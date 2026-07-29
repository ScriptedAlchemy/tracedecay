//! Hook-event notification handling: workspace route observation
//! and hook-event plan execution.

use super::*;

impl McpServer {
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
                let root = match hook_events::authorize_planned_branch_effect(root, root, &branch) {
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
                    graph: Arc::clone(&cg),
                    root,
                    branch,
                    incremental_sync_agent: None,
                };
                match (self.hook_branch_writer)(request).await {
                    Ok(result) => match result.branch_outcome {
                        crate::branch::BranchAddOutcome::Added => {
                            self.reopen_after_branch_tracking_added().await;
                            HostAdmissionOutcome::replay_completed(true, false)
                        }
                        crate::branch::BranchAddOutcome::AlreadyTracked => {
                            self.refresh_file_token_map().await;
                            HostAdmissionOutcome::replay_completed(false, true)
                        }
                        crate::branch::BranchAddOutcome::Deferred => {
                            HostAdmissionOutcome::retained_backpressured("daemon_backpressure")
                        }
                        crate::branch::BranchAddOutcome::NotIndexed => {
                            HostAdmissionOutcome::retained_unavailable(
                                "canonical_admission_unavailable",
                            )
                        }
                    },
                    Err(_) => {
                        HostAdmissionOutcome::retained_unavailable("canonical_admission_failed")
                    }
                }
            }
            HookEventPlan::AddBranchAt {
                root: effect_root,
                branch,
                agent,
            } => {
                // Durable effect roots stay concrete (not hashed) and must be
                // freshly normalized, canonicalized, and reauthorized before
                // any write — admit-time membership/branch are never reused.
                let root =
                    match hook_events::authorize_planned_branch_effect(&effect_root, root, &branch)
                    {
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
                    graph: Arc::clone(&cg),
                    root,
                    branch,
                    incremental_sync_agent: Some(agent),
                };
                match (self.hook_branch_writer)(request).await {
                    Ok(result) => {
                        if result.refresh_file_token_map {
                            self.refresh_file_token_map().await;
                        }
                        match result.branch_outcome {
                            crate::branch::BranchAddOutcome::Added => {
                                HostAdmissionOutcome::replay_completed(true, false)
                            }
                            crate::branch::BranchAddOutcome::AlreadyTracked => {
                                HostAdmissionOutcome::replay_completed(false, true)
                            }
                            crate::branch::BranchAddOutcome::Deferred => {
                                HostAdmissionOutcome::retained_backpressured("daemon_backpressure")
                            }
                            crate::branch::BranchAddOutcome::NotIndexed => {
                                HostAdmissionOutcome::retained_unavailable(
                                    "canonical_admission_unavailable",
                                )
                            }
                        }
                    }
                    Err(_) => {
                        HostAdmissionOutcome::retained_unavailable("canonical_admission_failed")
                    }
                }
            }
            HookEventPlan::SyncCurrentBranch { branch, agent } => {
                // Session/workspace sync plans capture branch at admit time;
                // revalidate live root + current branch immediately before effect.
                let root = match hook_events::authorize_planned_branch_effect(root, root, &branch) {
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
                    graph: Arc::clone(&cg),
                    root,
                    branch,
                    incremental_sync_agent: Some(agent),
                };
                match (self.hook_branch_writer)(request).await {
                    Ok(result) => match result.branch_outcome {
                        crate::branch::BranchAddOutcome::Added => {
                            self.reopen_after_branch_tracking_added().await;
                            HostAdmissionOutcome::replay_completed(true, false)
                        }
                        crate::branch::BranchAddOutcome::AlreadyTracked => {
                            if result.refresh_file_token_map {
                                self.refresh_file_token_map().await;
                            }
                            HostAdmissionOutcome::replay_completed(false, true)
                        }
                        crate::branch::BranchAddOutcome::Deferred => {
                            HostAdmissionOutcome::retained_backpressured("daemon_backpressure")
                        }
                        crate::branch::BranchAddOutcome::NotIndexed => {
                            HostAdmissionOutcome::retained_unavailable(
                                "canonical_admission_unavailable",
                            )
                        }
                    },
                    Err(_) => {
                        HostAdmissionOutcome::retained_unavailable("canonical_admission_failed")
                    }
                }
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
