use super::*;

impl DaemonSessionSyncService {
    pub(super) async fn recover_project(
        &self,
        context: &Arc<SessionSyncProjectContext>,
    ) -> crate::errors::Result<bool> {
        let scope = SessionSyncScopeV1::new(context.project_id.clone(), context.profile_id.clone());
        let prefix = journal_prefix(&scope);
        let journals = context
            .registry
            .list_session_sync_journals(&prefix)
            .await
            .map_err(store_error)?;
        let mut recovered_import = false;
        for (key, encoded) in journals {
            let mut journal: SessionSyncJournalV1 =
                serde_json::from_str(&encoded).map_err(journal_decode_error)?;
            if journal.scope != scope || journal.status == SessionSyncJournalStatusV1::Complete {
                continue;
            }
            journal = self.refresh_source_frontiers(context, &key).await?;
            if let Some(primary) = journal.coalesced_primary.clone() {
                recovered_import = true;
                let primary_key = journal_key(&scope, &primary);
                if self
                    .mirror_primary_terminal(context, &key, &primary_key)
                    .await?
                    .is_some()
                {
                    continue;
                }
                if journal.cancel_requested_at.is_some() {
                    self.persist_terminal(
                        context,
                        &key,
                        OperationTermination::Cancelled,
                        journal.stats,
                        journal.coverage,
                        journal.source_frontiers,
                        Vec::new(),
                    )
                    .await?;
                    continue;
                }
                if journal.deadline.is_elapsed_at(now_micros()) {
                    self.persist_terminal(
                        context,
                        &key,
                        OperationTermination::TimedOut,
                        journal.stats,
                        journal.coverage,
                        journal.source_frontiers,
                        Vec::new(),
                    )
                    .await?;
                    continue;
                }
                let cancellation = CancellationSignal::active(format!(
                    "session-sync.recovered-alias.{}",
                    journal.admission.operation_id.as_str()
                ))
                .map_err(contract_error)?;
                self.coalesce_import(Arc::clone(context), key, journal, primary_key, cancellation);
                continue;
            }
            if journal.cancel_requested_at.is_some() {
                self.persist_terminal(
                    context,
                    &key,
                    OperationTermination::Cancelled,
                    journal.stats,
                    journal.coverage,
                    journal.source_frontiers,
                    Vec::new(),
                )
                .await?;
                continue;
            }
            if journal.deadline.is_elapsed_at(now_micros()) {
                self.persist_terminal(
                    context,
                    &key,
                    OperationTermination::TimedOut,
                    journal.stats,
                    journal.coverage,
                    journal.source_frontiers,
                    Vec::new(),
                )
                .await?;
                continue;
            }
            let active_import =
                matches!(journal.source, SessionSyncCommandV1::ImportTranscripts(_))
                    .then(|| {
                        self.active_imports
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get(&import_scope_key(&journal.scope))
                            .cloned()
                    })
                    .flatten();
            if let Some(active_import) = active_import {
                recovered_import = true;
                journal = self
                    .update_journal(context, &key, |journal| {
                        journal.coalesced_primary =
                            Some(active_import.admission.idempotency_key.clone());
                        journal.updated_at = now_micros();
                    })
                    .await?;
                let cancellation = CancellationSignal::active(format!(
                    "session-sync.recovered-coalesced.{}",
                    journal.admission.operation_id.as_str()
                ))
                .map_err(contract_error)?;
                self.coalesce_import(
                    Arc::clone(context),
                    key,
                    journal,
                    active_import.journal_key,
                    cancellation,
                );
                continue;
            }
            let cancellation = CancellationSignal::active(format!(
                "session-sync.recovered.{}",
                journal.admission.operation_id.as_str()
            ))
            .map_err(contract_error)?;
            let admission = journal.admission.clone();
            let request = SessionSyncRequestV1::new(
                journal.admission.operation_id,
                journal.admission.idempotency_key,
                journal.scope,
                journal.deadline,
                cancellation,
                journal.source,
            );
            recovered_import |= matches!(
                request.command(),
                SessionSyncCommandV1::ImportTranscripts(_)
            );
            let _ = self.enqueue(Arc::clone(context), key, request, admission);
        }
        Ok(recovered_import)
    }

    pub(super) async fn mirror_primary_terminal(
        &self,
        context: &SessionSyncProjectContext,
        alias_key: &str,
        primary_key: &str,
    ) -> crate::errors::Result<Option<SessionSyncJournalV1>> {
        let Some(encoded) = context
            .registry
            .read_session_sync_journal(primary_key)
            .await
            .map_err(store_error)?
        else {
            return Ok(None);
        };
        let primary: SessionSyncJournalV1 =
            serde_json::from_str(&encoded).map_err(journal_decode_error)?;
        let Some(completion) = primary.completion else {
            return Ok(None);
        };
        self.persist_terminal(
            context,
            alias_key,
            completion.termination,
            completion.stats,
            completion.coverage,
            completion.source_frontiers,
            completion.failure_codes,
        )
        .await
        .map(Some)
    }

    pub(super) fn coalesce_import(
        &self,
        context: Arc<SessionSyncProjectContext>,
        key: String,
        journal: SessionSyncJournalV1,
        primary_key: String,
        cancellation: CancellationSignal,
    ) {
        {
            let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            if active.contains_key(&key) {
                return;
            }
            active.insert(key.clone(), cancellation.clone());
        }
        let service = self.clone();
        let key_for_cleanup = key.clone();
        let task = tokio::spawn(async move {
            let worker = async {
                loop {
                    if service.shutdown.is_cancelled() {
                        return;
                    }
                    match context
                        .registry
                        .read_session_sync_journal(&primary_key)
                        .await
                    {
                        Ok(Some(encoded)) => {
                            let primary: SessionSyncJournalV1 = match serde_json::from_str(&encoded)
                            {
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
                                            vec![
                                                "session_sync_coalesced_journal_invalid".to_owned(),
                                            ],
                                        )
                                        .await;
                                    return;
                                }
                            };
                            if let Some(completion) = primary.completion.clone() {
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
                            if let Some(termination) = coalesced_alias_local_interruption(
                                &primary,
                                &journal,
                                cancellation.is_cancelled(),
                                now_micros(),
                            ) {
                                let _ = service
                                    .persist_interruption(&context, &key, termination)
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
            };
            worker.await;
            service
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key_for_cleanup);
        });
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    pub(super) async fn cancel_request(
        &self,
        control: SessionSyncControlV1,
    ) -> SessionSyncOutcomeV1 {
        let Some(context) = self.context_for(control.scope()) else {
            return SessionSyncOutcomeV1::WrongScope;
        };
        let key = journal_key(control.scope(), control.idempotency_key());
        let initial = match self.refresh_source_frontiers(&context, &key).await {
            Ok(journal) => journal,
            Err(error) => {
                tracing::warn!(%error, "session sync cancellation journal read failed");
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_cancel_failed",
                };
            }
        };
        if initial.scope != *control.scope()
            || initial.admission.idempotency_key != *control.idempotency_key()
        {
            return SessionSyncOutcomeV1::WrongScope;
        }
        if initial.status == SessionSyncJournalStatusV1::Complete {
            return initial.outcome();
        }
        let primary_key = initial
            .coalesced_primary
            .as_ref()
            .map(|primary| journal_key(control.scope(), primary));
        if let Some(primary_key) = primary_key.as_deref() {
            match self
                .mirror_primary_terminal(&context, &key, primary_key)
                .await
            {
                Ok(Some(journal)) => return journal.outcome(),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "session sync cancellation reconciliation failed");
                    return SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_cancel_failed",
                    };
                }
            }
        }
        let mut cancellation_owned = false;
        let updated = self
            .update_journal(&context, &key, |journal| {
                cancellation_owned = false;
                if journal.scope == *control.scope()
                    && journal.admission.idempotency_key == *control.idempotency_key()
                    && journal.status != SessionSyncJournalStatusV1::Complete
                    && journal.cancel_requested_at.is_none()
                {
                    journal.cancel_requested_at = Some(now_micros());
                    journal.updated_at = now_micros();
                    cancellation_owned = true;
                }
            })
            .await;
        let journal = match updated {
            Ok(journal) => journal,
            Err(error) => {
                tracing::warn!(%error, "session sync cancellation journal write failed");
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_cancel_failed",
                };
            }
        };
        if journal.scope != *control.scope()
            || journal.admission.idempotency_key != *control.idempotency_key()
        {
            return SessionSyncOutcomeV1::WrongScope;
        }
        if journal.status == SessionSyncJournalStatusV1::Complete {
            return journal.outcome();
        }
        if !cancellation_owned {
            return journal.outcome();
        }
        if let Some(signal) = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            signal.cancel(now_micros());
            return journal.outcome();
        }
        if let Some(primary_key) = primary_key.as_deref() {
            match self
                .mirror_primary_terminal(&context, &key, primary_key)
                .await
            {
                Ok(Some(journal)) => return journal.outcome(),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "session sync cancellation reconciliation failed");
                    return SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_cancel_failed",
                    };
                }
            }
        }
        match self
            .persist_interruption(&context, &key, OperationTermination::Cancelled)
            .await
        {
            Ok(journal) => journal.outcome(),
            Err(error) => {
                tracing::warn!(%error, "session sync cancellation completion failed");
                SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_cancel_failed",
                }
            }
        }
    }
}

impl SessionSyncProjectContext {
    pub(super) async fn source_frontiers_for(
        &self,
        source: &SessionSyncCommandV1,
    ) -> crate::errors::Result<Vec<SessionSyncSourceFrontierV1>> {
        match source {
            SessionSyncCommandV1::ImportTranscripts(_) => self.source_frontiers().await,
            SessionSyncCommandV1::SynchronizeGit(_) => self.git_history_source_frontiers().await,
        }
    }

    pub(super) async fn source_frontiers(
        &self,
    ) -> crate::errors::Result<Vec<SessionSyncSourceFrontierV1>> {
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

    async fn git_history_source_frontiers(
        &self,
    ) -> crate::errors::Result<Vec<SessionSyncSourceFrontierV1>> {
        let store = GlobalDbGitCorrelationStore::new(Arc::clone(&self.project_sessions));
        let snapshot = store.read_snapshot().await.map_err(store_error)?;
        let activity_timestamp = crate::sessions::git_correlation::read_meta_value(
            &snapshot,
            crate::sessions::git_correlation::AUTO_BACKFILL_WATERMARK_KEY,
        )
        .await
        .map_err(store_error)?;
        let source_rowid = crate::sessions::git_correlation::read_meta_value(
            &snapshot,
            crate::sessions::git_correlation::GIT_HISTORY_ROWID_FRONTIER_KEY,
        )
        .await
        .map_err(store_error)?;
        Ok(
            git_history_frontier_from_meta(activity_timestamp, source_rowid)
                .map(|frontier| vec![git_history_source_frontier(&self.project_id, frontier)])
                .unwrap_or_default(),
        )
    }

    pub(super) async fn import_transcripts(
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

            let profile_sweep_satisfied = {
                let completed_profile_sweeps = service
                    .completed_profile_sweeps
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                completed_profile_sweep_covers(
                    completed_profile_sweeps.get(self.profile_id.as_str()),
                    admitted_at,
                )
            };
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

    pub(super) async fn synchronize_git(
        &self,
        request: &SessionSyncRequestV1,
        options: SessionGitSyncV1,
        shutdown: &crate::application::observation::ObservationCancellation,
    ) -> SessionSyncWorkResult {
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
        let backfill_options = crate::sessions::git_correlation::BackfillOptions {
            since: options.since_unix(),
            limit_sessions: options.max_sessions(),
            merge_gap_secs: crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
            max_commits_per_repo: usize::MAX,
            dry_run: options.dry_run(),
        };
        let backfill = store.run_bounded_history_index_page(&backfill_options, &control);
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
                    let reason_code = outcome
                        .interruption
                        .map(git_history_interruption_reason)
                        .unwrap_or("git_sync_interrupted");
                    let mut failure_codes = vec![reason_code.to_owned()];
                    if outcome.unresolved_failures > 0 {
                        failure_codes.push("git_source_failed".to_owned());
                    }
                    return requested_interruption.unwrap_or(SessionSyncWorkResult::Finished {
                        committed: false,
                        stats,
                        coverage: vec![SessionSyncSourceCoverageV1 {
                            store_scope: "git".to_owned(),
                            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
                        }],
                        source_frontiers: Vec::new(),
                        failure_codes,
                    });
                }
                let git_errors = saturating_usize_to_u64(outcome.stats.skipped_git_error);
                let remaining_work = outcome
                    .remaining_sessions
                    .max(git_errors)
                    .max(outcome.unresolved_failures)
                    .max(u64::from(interrupted));
                let coverage = if remaining_work > 0 {
                    SessionSyncCoverageV1::Partial {
                        deferred_units: remaining_work,
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
                        _ => outcome
                            .interruption
                            .map(git_history_interruption_reason)
                            .unwrap_or("git_sync_interrupted")
                            .to_owned(),
                    });
                }
                if git_errors > 0 || outcome.unresolved_failures > 0 {
                    failure_codes.push("git_source_failed".to_owned());
                }
                let source_frontiers = vec![git_history_source_frontier(
                    &self.project_id,
                    outcome.frontier,
                )];
                SessionSyncWorkResult::Finished {
                    committed: outcome.committed,
                    stats,
                    coverage: vec![SessionSyncSourceCoverageV1 {
                        store_scope: "git".to_owned(),
                        coverage,
                    }],
                    source_frontiers,
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

const fn git_history_interruption_reason(
    interruption: crate::sessions::git_correlation::BoundedBackfillInterruption,
) -> &'static str {
    use crate::sessions::git_correlation::BoundedBackfillInterruption;

    match interruption {
        BoundedBackfillInterruption::Cancelled => "git_sync_cancelled",
        BoundedBackfillInterruption::CommandTimedOut => "git_command_timed_out",
        BoundedBackfillInterruption::HistoryLimitReached => "git_history_limit_reached",
        BoundedBackfillInterruption::DryRunFrontierLimitReached => {
            "git_dry_run_frontier_limit_reached"
        }
        BoundedBackfillInterruption::UnsupportedSourceFraming => "git_unsupported_source_framing",
        BoundedBackfillInterruption::SourceChanged => "git_source_changed",
        BoundedBackfillInterruption::SourceUnavailable => "git_source_unavailable",
    }
}

pub(super) fn git_history_frontier_from_meta(
    activity_timestamp: Option<i64>,
    source_rowid: Option<i64>,
) -> Option<crate::sessions::git_correlation::GitHistoryIndexFrontier> {
    activity_timestamp.map(|activity_timestamp| {
        crate::sessions::git_correlation::GitHistoryIndexFrontier {
            activity_timestamp,
            source_rowid: source_rowid.unwrap_or(0),
        }
    })
}

pub(super) fn git_history_source_frontier(
    project_id: &ProjectId,
    frontier: crate::sessions::git_correlation::GitHistoryIndexFrontier,
) -> SessionSyncSourceFrontierV1 {
    SessionSyncSourceFrontierV1 {
        store_scope: "git".to_owned(),
        source_json: serde_json::json!({
            "authority": "git_history_index",
        })
        .to_string(),
        scope_json: serde_json::json!({
            "project_id": project_id.as_str(),
        })
        .to_string(),
        committed_cursor_json: serde_json::json!({
            "activity_timestamp": frontier.activity_timestamp,
            "source_rowid": frontier.source_rowid,
        })
        .to_string(),
    }
}

pub(super) fn coalesced_alias_local_interruption(
    primary: &SessionSyncJournalV1,
    alias: &SessionSyncJournalV1,
    cancellation_is_requested: bool,
    observed_at: UtcMicros,
) -> Option<OperationTermination> {
    if primary.completion.is_some() {
        None
    } else if alias.deadline.is_elapsed_at(observed_at) {
        Some(OperationTermination::TimedOut)
    } else if cancellation_is_requested {
        Some(OperationTermination::Cancelled)
    } else {
        None
    }
}

pub(super) fn log_session_sync_join(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result
        && !error.is_cancelled()
    {
        tracing::warn!(%error, "session sync worker join failed");
    }
}
