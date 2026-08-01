//! Hook-event notification handling: workspace route observation
//! and hook-event plan execution.

use super::*;

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
            HookEventPlan::SyncFiles(rel_paths) => {
                match cg.sync_if_stale_silent(&rel_paths).await {
                    Ok(()) => {
                        self.refresh_file_token_map().await;
                        HostAdmissionOutcome::replay_completed(true, false)
                    }
                    Err(TraceDecayError::SyncLock { .. }) => {
                        HostAdmissionOutcome::retained_backpressured("daemon_backpressure")
                    }
                    Err(_) => {
                        HostAdmissionOutcome::retained_unavailable("canonical_admission_failed")
                    }
                }
            }
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
            HookEventPlan::DebouncedIncrementalSync(agent) => {
                self.run_hook_incremental_sync(cg, agent).await
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
            HookEventPlan::Noop => HostAdmissionOutcome::replay_completed(false, true),
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
