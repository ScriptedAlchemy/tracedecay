//! Hook-event notification handling: workspace route observation
//! and hook-event plan execution.

use super::*;
use crate::application::host_admission::HostAdmissionStatus;

/// When a settled branch write is allowed to refresh the file token map.
///
/// The three branch plans differ here and the differences are load-bearing, so
/// each one names its policy rather than inheriting a shared default.
#[derive(Clone, Copy)]
enum BranchTokenMapRefresh {
    /// Refresh whenever the branch was already tracked, whatever the writer asked.
    AlreadyTrackedAlways,
    /// Refresh when the branch was already tracked and the writer asked for it.
    AlreadyTrackedWhenRequested,
    /// Refresh for any settled outcome the writer flagged, before it is classified.
    AnyOutcomeWhenRequested,
}

/// The per-plan effects that survive the shared branch-write path.
#[derive(Clone, Copy)]
struct BranchEffectPolicy {
    refresh: BranchTokenMapRefresh,
    /// Whether a newly added branch reopens the retained handle.
    reopen_on_added: bool,
}

impl McpServer {
    /// Authorizes, writes, and classifies one branch effect.
    ///
    /// `effect_root` is the root the write targets and `live_root` the current
    /// project root; both are revalidated here so admit-time membership is never
    /// reused. Everything that differs between the branch plans is carried by
    /// `policy` rather than by branching on the plan again.
    async fn apply_branch_effect(
        &self,
        cg: &Arc<TraceDecay>,
        effect_root: &Path,
        live_root: &Path,
        branch: String,
        agent: Option<HookAgent>,
        policy: BranchEffectPolicy,
    ) -> HostAdmissionOutcome {
        let root =
            match hook_events::authorize_planned_branch_effect(effect_root, live_root, &branch) {
                Ok(authorized) => authorized,
                Err(error) => {
                    return match error {
                        hook_events::AddBranchAtRootAuthError::Unresolvable => {
                            HostAdmissionOutcome::retained_unavailable(error.reason_code())
                        }
                        _ => HostAdmissionOutcome::degraded(error.reason_code()),
                    };
                }
            };
        let request = HookBranchWriteRequest {
            // R4: resolve the live branch once, here, where the effect root is
            // final; every gate this write crosses reads it from the request.
            live_branch: crate::branch::BranchMemo::new(&root),
            graph: Arc::clone(cg),
            root,
            branch,
            incremental_sync_agent: agent,
        };
        let result = match (self.hook_branch_writer)(request).await {
            Ok(result) => result,
            Err(_) => {
                return HostAdmissionOutcome::retained_unavailable("canonical_admission_failed");
            }
        };
        if matches!(
            policy.refresh,
            BranchTokenMapRefresh::AnyOutcomeWhenRequested
        ) && result.refresh_file_token_map
        {
            self.refresh_file_token_map().await;
        }
        match result.branch_outcome {
            crate::branch::BranchAddOutcome::Added => {
                if policy.reopen_on_added {
                    self.reopen_after_branch_tracking_added().await;
                }
                HostAdmissionOutcome::replay_completed(true, false)
            }
            crate::branch::BranchAddOutcome::AlreadyTracked => {
                let refresh = match policy.refresh {
                    BranchTokenMapRefresh::AlreadyTrackedAlways => true,
                    BranchTokenMapRefresh::AlreadyTrackedWhenRequested => {
                        result.refresh_file_token_map
                    }
                    BranchTokenMapRefresh::AnyOutcomeWhenRequested => false,
                };
                if refresh {
                    self.refresh_file_token_map().await;
                }
                HostAdmissionOutcome::replay_completed(false, true)
            }
            crate::branch::BranchAddOutcome::Deferred => {
                HostAdmissionOutcome::retained_backpressured("daemon_backpressure")
            }
            crate::branch::BranchAddOutcome::NotIndexed => {
                HostAdmissionOutcome::retained_unavailable("canonical_admission_unavailable")
            }
        }
    }

    pub(crate) async fn update_hook_workspace_route(
        &self,
        event: &hook_events::HookEvent,
        route_cache: &mut HookProjectRouteCache,
    ) -> crate::errors::Result<()> {
        let route = match HookProjectRouteCache::route_cwd(event) {
            Some(cwd) => {
                let arguments = json!({
                    "project_selector": {
                        "path": cwd.to_string_lossy(),
                    }
                });
                match crate::mcp::tools::handlers::selected_registered_project_reader(
                    "tracedecay_files".to_owned(),
                    arguments,
                    self.registry_db.as_deref(),
                    self.retained_project_graph_resolver.clone(),
                    None,
                )
                .await
                {
                    Ok(Some(route)) => {
                        crate::mcp::project_route::WorkspaceProjectRoute::Resolved(Box::new(route))
                    }
                    Ok(None) => crate::mcp::project_route::WorkspaceProjectRoute::Failed(
                        crate::mcp::project_route::ProjectRouteFailure {
                            kind: crate::mcp::project_route::ProjectRouteFailureKind::NotFound,
                            detail: format!(
                                "workspace {} did not resolve to a registered project",
                                cwd.display()
                            ),
                        },
                    ),
                    Err(error) => crate::mcp::project_route::WorkspaceProjectRoute::Failed(
                        crate::mcp::project_route::ProjectRouteFailure::from_selection_error(
                            &error,
                        ),
                    ),
                }
            }
            None => crate::mcp::project_route::WorkspaceProjectRoute::Failed(
                crate::mcp::project_route::ProjectRouteFailure {
                    kind: crate::mcp::project_route::ProjectRouteFailureKind::Unavailable,
                    detail: "hook workspace route did not include a working directory".to_owned(),
                },
            ),
        };
        let failure = match &route {
            crate::mcp::project_route::WorkspaceProjectRoute::Resolved(_) => None,
            crate::mcp::project_route::WorkspaceProjectRoute::Failed(failure) => {
                Some(failure.clone())
            }
        };
        route_cache.observe_workspace_route(event, route);
        self.hook_project_routes.store(route_cache);
        failure.map_or(Ok(()), |failure| Err(failure.into_error()))
    }

    pub(crate) async fn run_hook_event_plan(
        &self,
        cg: Arc<TraceDecay>,
        root: &Path,
        plan: HookEventPlan,
    ) -> HostAdmissionOutcome {
        match plan {
            HookEventPlan::SyncFiles(rel_paths) => self.enqueue_hook_paths(root, rel_paths).await,
            HookEventPlan::AddBranch(branch) => {
                // Project-root plans must revalidate live root + current branch
                // immediately before effect — same strictness as AddBranchAt.
                self.apply_branch_effect(
                    &cg,
                    root,
                    root,
                    branch,
                    None,
                    BranchEffectPolicy {
                        refresh: BranchTokenMapRefresh::AlreadyTrackedAlways,
                        reopen_on_added: true,
                    },
                )
                .await
            }
            HookEventPlan::AddBranchAt {
                root: effect_root,
                branch,
                agent,
            } => {
                // Durable effect roots stay concrete (not hashed) and must be
                // freshly normalized, canonicalized, and reauthorized before
                // any write — admit-time membership/branch are never reused.
                self.apply_branch_effect(
                    &cg,
                    &effect_root,
                    root,
                    branch,
                    Some(agent),
                    BranchEffectPolicy {
                        refresh: BranchTokenMapRefresh::AnyOutcomeWhenRequested,
                        reopen_on_added: false,
                    },
                )
                .await
            }
            HookEventPlan::SyncCurrentBranch { branch, agent } => {
                // Session/workspace sync plans capture branch at admit time;
                // revalidate live root + current branch immediately before effect.
                self.apply_branch_effect(
                    &cg,
                    root,
                    root,
                    branch,
                    Some(agent),
                    BranchEffectPolicy {
                        refresh: BranchTokenMapRefresh::AlreadyTrackedWhenRequested,
                        reopen_on_added: true,
                    },
                )
                .await
            }
            HookEventPlan::DebouncedIncrementalSync(_) => {
                HostAdmissionOutcome::replay_completed(false, true)
            }
            HookEventPlan::RecordTerminalReceipt { route, receipt } => {
                match crate::automation::host_receipts::record(
                    &cg.store_layout().dashboard_root,
                    route,
                    receipt,
                )
                .await
                {
                    Ok(true) => {
                        if let Some(reconcile) = &self.automation_scheduler_reconciler {
                            let reconcile = Arc::clone(reconcile);
                            tokio::spawn(async move {
                                let _ = reconcile().await;
                            });
                        }
                        HostAdmissionOutcome::replay_completed(true, false)
                    }
                    Ok(false) => HostAdmissionOutcome::replay_completed(false, true),
                    Err(_) => {
                        HostAdmissionOutcome::retained_unavailable("canonical_admission_failed")
                    }
                }
            }
            HookEventPlan::MarkTurnIngested {
                route,
                transcript_watermark,
            } => {
                match crate::automation::host_receipts::mark_turn_ingested(
                    &cg.store_layout().dashboard_root,
                    route,
                    &transcript_watermark,
                )
                .await
                {
                    Ok(()) => {
                        if let Some(reconcile) = &self.automation_scheduler_reconciler {
                            let reconcile = Arc::clone(reconcile);
                            tokio::spawn(async move {
                                let _ = reconcile().await;
                            });
                        }
                        HostAdmissionOutcome::replay_completed(true, false)
                    }
                    Err(_) => {
                        HostAdmissionOutcome::retained_unavailable("canonical_admission_failed")
                    }
                }
            }
            HookEventPlan::CursorEvent(event) => {
                self.run_queued_cursor_event(cg, root, event).await
            }
            HookEventPlan::Noop => HostAdmissionOutcome::replay_completed(false, true),
        }
    }

    async fn run_queued_cursor_event(
        &self,
        cg: Arc<TraceDecay>,
        root: &Path,
        event: crate::mcp::tools::handlers::hook_runtime::CursorQueuedEventV1,
    ) -> HostAdmissionOutcome {
        let capture = match event.event_name.as_str() {
            "beforeSubmitPrompt" => match cg.reset_local_counter().await {
                Ok(()) => HostAdmissionOutcome::replay_completed(true, false),
                Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
            },
            "preCompact" => self.run_queued_cursor_compaction(&cg, root, &event).await,
            "sessionStart" | "sessionEnd" | "stop" => {
                self.run_queued_cursor_capture(&cg, root, &event).await
            }
            _ => HostAdmissionOutcome::replay_completed(false, true),
        };
        if !matches!(
            capture.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ) {
            return capture;
        }

        if event.event_name != "afterFileEdit" || event.rel_paths.is_empty() {
            return capture;
        }
        self.enqueue_hook_paths(root, event.rel_paths).await
    }

    fn queued_cursor_event_json(
        root: &Path,
        event: &crate::mcp::tools::handlers::hook_runtime::CursorQueuedEventV1,
    ) -> std::result::Result<String, ()> {
        let mut payload = event.event.clone();
        let Some(payload) = payload.as_object_mut() else {
            return Err(());
        };
        payload.insert(
            "cwd".to_owned(),
            Value::String(root.to_string_lossy().into_owned()),
        );
        serde_json::to_string(&payload).map_err(|_| ())
    }

    async fn run_queued_cursor_capture(
        &self,
        cg: &Arc<TraceDecay>,
        root: &Path,
        event: &crate::mcp::tools::handlers::hook_runtime::CursorQueuedEventV1,
    ) -> HostAdmissionOutcome {
        let Ok(event_json) = Self::queued_cursor_event_json(root, event) else {
            return HostAdmissionOutcome::degraded("durable_payload_malformed");
        };
        let args = serde_json::json!({
            "action": "ingest_transcript",
            "provider": "cursor",
            "user_scope": false,
            "event_json": event_json,
            "max_new_bytes": crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES,
        });
        let authorities = crate::mcp::tools::SessionAuthorities::new(
            self.session_db.as_ref(),
            self.user_session_db.as_ref(),
        )
        .with_profile_identity(self.profile_identity.as_ref())
        .with_registered_databases(
            self.registered_session_db.as_ref(),
            self.registered_user_session_db.as_ref(),
        );
        match crate::mcp::tools::handlers::hook_runtime::ingest_transcript(
            Some(cg.as_ref()),
            &args,
            self.profile_root.as_deref(),
            self.global_db.as_deref(),
            authorities,
        )
        .await
        {
            Ok(result) if result.get("completed").and_then(Value::as_bool) == Some(false) => {
                HostAdmissionOutcome::retained_backpressured("ingest_pass_backpressured")
            }
            Ok(result) => HostAdmissionOutcome::replay_completed(
                result
                    .get("messages_upserted")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0),
                true,
            ),
            Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
        }
    }

    async fn run_queued_cursor_compaction(
        &self,
        cg: &Arc<TraceDecay>,
        root: &Path,
        event: &crate::mcp::tools::handlers::hook_runtime::CursorQueuedEventV1,
    ) -> HostAdmissionOutcome {
        let Ok(event_json) = Self::queued_cursor_event_json(root, event) else {
            return HostAdmissionOutcome::degraded("durable_payload_malformed");
        };
        let args = serde_json::json!({
            "action": "cursor_compact",
            "event_json": event_json,
        });
        let authorities = crate::mcp::tools::SessionAuthorities::new(
            self.session_db.as_ref(),
            self.user_session_db.as_ref(),
        )
        .with_profile_identity(self.profile_identity.as_ref())
        .with_registered_databases(
            self.registered_session_db.as_ref(),
            self.registered_user_session_db.as_ref(),
        );
        match crate::mcp::tools::handlers::hook_runtime::cursor_compact(
            cg.as_ref(),
            &args,
            authorities,
        )
        .await
        {
            Ok(result) => HostAdmissionOutcome::replay_completed(
                result
                    .get("summary_nodes_created")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0),
                true,
            ),
            Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
        }
    }

    async fn enqueue_hook_paths(
        &self,
        root: &Path,
        rel_paths: Vec<String>,
    ) -> HostAdmissionOutcome {
        let Some(sink) = &self.code_index_hook_sink else {
            return HostAdmissionOutcome::retained_unavailable("code_index_scheduler_unavailable");
        };
        if sink(root.to_path_buf(), rel_paths).await {
            HostAdmissionOutcome::replay_completed(true, false)
        } else {
            HostAdmissionOutcome::retained_unavailable("code_index_scheduler_unavailable")
        }
    }

    pub(crate) async fn run_hook_incremental_sync(
        &self,
        cg: Arc<TraceDecay>,
        agent: HookAgent,
    ) -> HostAdmissionOutcome {
        match run_hook_incremental_sync_direct(&cg, agent).await {
            Ok(true) => {
                self.refresh_file_token_map().await;
                HostAdmissionOutcome::replay_completed(true, false)
            }
            Ok(false) => HostAdmissionOutcome::replay_completed(false, true),
            Err(_) => HostAdmissionOutcome::retained_unavailable("canonical_admission_failed"),
        }
    }
}
