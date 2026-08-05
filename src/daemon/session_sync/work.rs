use super::*;

impl DaemonSessionSyncService {
    fn coalesce_recovered_import(
        &self,
        context: Arc<SessionSyncProjectContext>,
        key: String,
        journal: SessionSyncJournalV1,
        primary_key: String,
    ) {
        let service = self.clone();
        let task = tokio::spawn(async move {
            loop {
                if service.shutdown.is_cancelled() {
                    return;
                }
                if journal.deadline.is_elapsed_at(now_micros()) {
                    let _ = service
                        .persist_interruption(&context, &key, OperationTermination::TimedOut)
                        .await;
                    return;
                }
                match context
                    .registry
                    .read_session_sync_journal(&primary_key)
                    .await
                {
                    Ok(Some(encoded)) => {
                        let primary: SessionSyncJournalV1 = match serde_json::from_str(&encoded) {
                            Ok(primary) => primary,
                            Err(error) => {
                                tracing::warn!(%error, "coalesced session sync journal invalid");
                                let _ = service
                                    .persist_terminal(
                                        &context,
                                        &key,
                                        OperationTermination::Unavailable,
                                        journal.stats.clone(),
                                        journal.coverage.clone(),
                                        journal.source_frontiers.clone(),
                                        vec!["session_sync_coalesced_journal_invalid".to_owned()],
                                    )
                                    .await;
                                return;
                            }
                        };
                        if let Some(completion) = primary.completion {
                            let _ = service
                                .persist_terminal(
                                    &context,
                                    &key,
                                    completion.termination,
                                    completion.stats,
                                    completion.coverage,
                                    completion.source_frontiers,
                                    completion.failure_codes,
                                )
                                .await;
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = service
                            .persist_terminal(
                                &context,
                                &key,
                                OperationTermination::Unavailable,
                                journal.stats.clone(),
                                journal.coverage.clone(),
                                journal.source_frontiers.clone(),
                                vec!["session_sync_coalesced_journal_missing".to_owned()],
                            )
                            .await;
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "coalesced session sync journal read failed");
                        let _ = service
                            .persist_terminal(
                                &context,
                                &key,
                                OperationTermination::Unavailable,
                                journal.stats.clone(),
                                journal.coverage.clone(),
                                journal.source_frontiers.clone(),
                                vec!["session_sync_coalesced_journal_read_failed".to_owned()],
                            )
                            .await;
                        return;
                    }
                }
                tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL).await;
            }
        });
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
}

impl SessionSyncProjectContext {
    async fn source_frontiers(&self) -> crate::errors::Result<Vec<SessionSyncSourceFrontierV1>> {
        let mut frontiers = Vec::new();
        for (store_scope, database) in [
            ("project", &self.project_sessions),
            ("profile", &self.user_sessions),
        ] {
            for (source_json, scope_json, committed_cursor_json) in database
                .list_session_sync_source_frontiers()
                .await
                .map_err(store_error)?
            {
                frontiers.push(SessionSyncSourceFrontierV1 {
                    store_scope: store_scope.to_owned(),
                    source_json,
                    scope_json,
                    committed_cursor_json,
                });
            }
        }
        frontiers.sort_by(|left, right| {
            (
                &left.store_scope,
                &left.source_json,
                &left.scope_json,
                &left.committed_cursor_json,
            )
                .cmp(&(
                    &right.store_scope,
                    &right.source_json,
                    &right.scope_json,
                    &right.committed_cursor_json,
                ))
        });
        Ok(frontiers)
    }

    async fn import_transcripts(
        &self,
        service: &DaemonSessionSyncService,
        journal_key: &str,
        admitted_at: UtcMicros,
        request: &SessionSyncRequestV1,
        shutdown: &crate::application::observation::ObservationCancellation,
    ) -> SessionSyncWorkResult {
        let cancellation = crate::application::observation::ObservationCancellation::default();
        let pass_cancellation = cancellation.clone();
        let pass = async {
            let project_authority =
                GlobalDbSessionIngestAuthority::new(Arc::clone(&self.project_sessions));
            let project = crate::sessions::ingest_project_sources_for_provider_with_cancellation(
                &self.brain_id,
                &self.profile_id,
                &project_authority,
                &self.project_root,
                Some(self.project_id.clone()),
                None,
                true,
                &pass_cancellation,
            )
            .await;
            let project_stats = SessionSyncStatsV1 {
                sessions_imported: project.stats.sessions_upserted,
                messages_imported: project.stats.messages_upserted,
                ..SessionSyncStatsV1::default()
            };
            let project_coverage = vec![source_coverage("project", project.coverage)];
            let project_progress = service
                .persist_progress(
                    self,
                    journal_key,
                    project_stats.clone(),
                    project_coverage.clone(),
                )
                .await;
            let project_progress_failed = project_progress.is_err();
            let project_frontiers = project_progress.unwrap_or_default();

            let completed_profile_sweeps = service
                .completed_profile_sweeps
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let profile_sweep_satisfied = completed_profile_sweep_covers(
                completed_profile_sweeps.get(self.profile_id.as_str()),
                admitted_at,
            );
            drop(completed_profile_sweeps);
            let (user, profile_sweep_started_at) = if profile_sweep_satisfied
                || pass_cancellation.is_cancelled()
            {
                (None, None)
            } else {
                let profile_sweep_started_at = now_micros();
                let user_authority =
                    GlobalDbSessionIngestAuthority::new(Arc::clone(&self.user_sessions));
                let registry_authority =
                    GlobalDbSessionIngestAuthority::new(Arc::clone(&self.registry));
                let user =
                    crate::sessions::ingest_user_global_sources_for_provider_with_authorities_and_cancellation(
                        &self.brain_id,
                        &self.profile_id,
                        &user_authority,
                        &registry_authority,
                        &self.profile_root,
                        None,
                        &pass_cancellation,
                    )
                    .await;
                (Some(user), Some(profile_sweep_started_at))
            };
            if let Some(user) = user.as_ref()
                && user.coverage.is_complete()
                && user.failures.is_empty()
                && let Some(profile_sweep_started_at) = profile_sweep_started_at
            {
                service
                    .completed_profile_sweeps
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(
                        self.profile_id.as_str().to_owned(),
                        profile_sweep_started_at,
                    );
            }
            let combined = user
                .as_ref()
                .map_or(project.stats, |user| project.stats.merge(user.stats));
            let stats = SessionSyncStatsV1 {
                sessions_imported: combined.sessions_upserted,
                messages_imported: combined.messages_upserted,
                ..SessionSyncStatsV1::default()
            };
            let mut coverage = project_coverage;
            coverage.push(user.as_ref().map_or_else(
                || {
                    source_coverage(
                        "profile",
                        if profile_sweep_satisfied {
                            crate::sessions::IngestPassCoverage::Complete
                        } else {
                            crate::sessions::IngestPassCoverage::Partial { deferred_units: 1 }
                        },
                    )
                },
                |user| source_coverage("profile", user.coverage),
            ));
            let source_frontiers = service
                .persist_progress(self, journal_key, stats.clone(), coverage.clone())
                .await;
            (
                project,
                user,
                stats,
                coverage,
                source_frontiers,
                project_frontiers,
                project_progress_failed,
            )
        };
        let pass = async {
            match &self.transcript_source_home {
                Some(home) => {
                    crate::sessions::with_transcript_source_home(home.clone(), pass).await
                }
                None => pass.await,
            }
        };
        tokio::pin!(pass);
        let mut interrupted = None;
        let (
            project,
            user,
            stats,
            coverage,
            source_frontiers,
            project_frontiers,
            project_progress_failed,
        ) = loop {
            tokio::select! {
                outcomes = &mut pass => break outcomes,
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if shutdown.is_cancelled() {
                        cancellation.cancel();
                        interrupted = Some(SessionSyncWorkResult::Shutdown);
                    } else if request.cancellation().is_cancelled() {
                        cancellation.cancel();
                        interrupted = Some(SessionSyncWorkResult::Cancelled);
                    } else if request.deadline().is_elapsed_at(now_micros()) {
                        cancellation.cancel();
                        interrupted = Some(SessionSyncWorkResult::TimedOut);
                    }
                }
            }
        };
        let committed = project.scheduling_state_written
            || user
                .as_ref()
                .is_some_and(|outcome| outcome.scheduling_state_written)
            || stats != SessionSyncStatsV1::default();
        let mut failure_codes = project
            .failures
            .into_iter()
            .chain(user.into_iter().flat_map(|outcome| outcome.failures))
            .map(|failure| failure.reason_code.to_owned())
            .collect::<Vec<_>>();
        if project_progress_failed || source_frontiers.is_err() {
            failure_codes.push("session_sync_frontier_persist_failed".to_owned());
        }
        let source_frontiers = source_frontiers.unwrap_or(project_frontiers);
        if committed {
            return SessionSyncWorkResult::Finished {
                committed: true,
                stats,
                coverage,
                source_frontiers,
                failure_codes,
            };
        }
        match interrupted {
            Some(interrupted) => interrupted,
            None => SessionSyncWorkResult::Finished {
                committed: false,
                stats,
                coverage,
                source_frontiers,
                failure_codes,
            },
        }
    }

    async fn synchronize_git(
        &self,
        request: &SessionSyncRequestV1,
        options: SessionGitSyncV1,
        shutdown: &crate::application::observation::ObservationCancellation,
    ) -> SessionSyncWorkResult {
        let Some(analytics) = self.analytics.as_ref() else {
            return SessionSyncWorkResult::Finished {
                committed: false,
                stats: SessionSyncStatsV1::default(),
                coverage: vec![SessionSyncSourceCoverageV1 {
                    store_scope: "git".to_owned(),
                    coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
                }],
                source_frontiers: Vec::new(),
                failure_codes: vec!["analytics_authority_unavailable".to_owned()],
            };
        };
        let project_key = RegisteredGlobalDb::canonical_project_key(&self.project_root);
        let query = analytics.query_analytics_events(&AnalyticsEventQuery {
            project_id: Some(project_key),
            since: Some(options.since_unix()),
            limit: GIT_SYNC_ANALYTICS_LIMIT,
            ..AnalyticsEventQuery::default()
        });
        tokio::pin!(query);
        let events = loop {
            tokio::select! {
                result = &mut query => {
                    match result {
                        Ok(events) => break events,
                        Err(error) => {
                            tracing::warn!(%error, "session git sync analytics read failed");
                            return SessionSyncWorkResult::Finished {
                                committed: false,
                                stats: SessionSyncStatsV1::default(),
                                coverage: vec![SessionSyncSourceCoverageV1 {
                                    store_scope: "git".to_owned(),
                                    coverage: SessionSyncCoverageV1::Partial {
                                        deferred_units: 1,
                                    },
                                }],
                                source_frontiers: Vec::new(),
                                failure_codes: vec!["analytics_read_failed".to_owned()],
                            };
                        }
                    }
                }
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if shutdown.is_cancelled() {
                        return SessionSyncWorkResult::Shutdown;
                    }
                    if request.cancellation().is_cancelled() {
                        return SessionSyncWorkResult::Cancelled;
                    }
                    if request.deadline().is_elapsed_at(now_micros()) {
                        return SessionSyncWorkResult::TimedOut;
                    }
                }
            }
        };
        if shutdown.is_cancelled() {
            return SessionSyncWorkResult::Shutdown;
        }
        if request.cancellation().is_cancelled() {
            return SessionSyncWorkResult::Cancelled;
        }
        if request.deadline().is_elapsed_at(now_micros()) {
            return SessionSyncWorkResult::TimedOut;
        }
        let cancellation = crate::application::observation::ObservationCancellation::default();
        let control = crate::sessions::git_correlation::BoundedGitControl::new(
            cancellation.clone(),
            GIT_SYNC_COMMAND_DEADLINE,
        );
        let store = GlobalDbGitCorrelationStore::new(Arc::clone(&self.project_sessions));
        let backfill = store.run_bounded_backfill(
            &events,
            &crate::sessions::git_correlation::BackfillOptions {
                since: options.since_unix(),
                limit_sessions: options.max_sessions(),
                merge_gap_secs: crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                max_commits_per_repo: 5_000,
                dry_run: options.dry_run(),
            },
            &control,
        );
        tokio::pin!(backfill);
        let mut requested_interruption = None;
        let result = loop {
            tokio::select! {
                result = &mut backfill => break result,
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if shutdown.is_cancelled() {
                        cancellation.cancel();
                        requested_interruption = Some(SessionSyncWorkResult::Shutdown);
                    } else if request.cancellation().is_cancelled() {
                        cancellation.cancel();
                        requested_interruption = Some(SessionSyncWorkResult::Cancelled);
                    } else if request.deadline().is_elapsed_at(now_micros()) {
                        cancellation.cancel();
                        requested_interruption = Some(SessionSyncWorkResult::TimedOut);
                    }
                }
            }
        };
        match result {
            Ok(outcome) => {
                let stats = SessionSyncStatsV1 {
                    sessions_scanned: saturating_usize_to_u64(outcome.stats.sessions_scanned),
                    spans_written: saturating_usize_to_u64(outcome.stats.spans_written),
                    commits_attributed: saturating_usize_to_u64(outcome.stats.commits_attributed),
                    skipped: saturating_usize_to_u64(outcome.stats.skipped_total()),
                    ..SessionSyncStatsV1::default()
                };
                let interrupted =
                    outcome.interruption.is_some() || requested_interruption.is_some();
                if interrupted && !outcome.committed {
                    return requested_interruption.unwrap_or(SessionSyncWorkResult::Finished {
                        committed: false,
                        stats,
                        coverage: vec![SessionSyncSourceCoverageV1 {
                            store_scope: "git".to_owned(),
                            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
                        }],
                        source_frontiers: Vec::new(),
                        failure_codes: vec!["git_command_timed_out".to_owned()],
                    });
                }
                let git_errors = saturating_usize_to_u64(outcome.stats.skipped_git_error);
                let coverage = if interrupted || git_errors > 0 {
                    SessionSyncCoverageV1::Partial {
                        deferred_units: git_errors.max(1),
                    }
                } else {
                    SessionSyncCoverageV1::Complete
                };
                let mut failure_codes = Vec::new();
                if interrupted {
                    failure_codes.push(match requested_interruption {
                        Some(SessionSyncWorkResult::Cancelled) => {
                            "git_sync_cancelled_after_commit".to_owned()
                        }
                        Some(SessionSyncWorkResult::TimedOut) => {
                            "git_sync_timed_out_after_commit".to_owned()
                        }
                        Some(SessionSyncWorkResult::Shutdown) => {
                            "git_sync_shutdown_after_commit".to_owned()
                        }
                        _ => "git_command_timed_out".to_owned(),
                    });
                }
                if git_errors > 0 {
                    failure_codes.push("git_source_failed".to_owned());
                }
                SessionSyncWorkResult::Finished {
                    committed: outcome.committed,
                    stats,
                    coverage: vec![SessionSyncSourceCoverageV1 {
                        store_scope: "git".to_owned(),
                        coverage,
                    }],
                    source_frontiers: Vec::new(),
                    failure_codes,
                }
            }
            Err(error) => {
                tracing::warn!(%error, "session git sync failed");
                SessionSyncWorkResult::Finished {
                    committed: false,
                    stats: SessionSyncStatsV1::default(),
                    coverage: vec![SessionSyncSourceCoverageV1 {
                        store_scope: "git".to_owned(),
                        coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
                    }],
                    source_frontiers: Vec::new(),
                    failure_codes: vec!["git_sync_failed".to_owned()],
                }
            }
        }
    }
}
