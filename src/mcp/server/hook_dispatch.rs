//! Hook-event notification handling: workspace route observation
//! and hook-event plan execution.

use super::*;

impl McpServer {
    pub(crate) async fn update_hook_workspace_route(
        &self,
        event: &hook_events::HookEvent,
        route_cache: &mut HookProjectRouteCache,
    ) {
        let route_cwd = HookProjectRouteCache::route_cwd(event);
        let project_path = match route_cwd {
            Some(cwd) => self.registered_project_containing_path(cwd).await,
            None => None,
        };
        route_cache.observe_hook_event(event, project_path);
        self.hook_project_routes.store(route_cache);
    }

    pub(crate) async fn registered_project_containing_path(&self, cwd: &Path) -> Option<String> {
        let registry = self.registry_db.as_deref()?;
        let mut candidate = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        loop {
            if let Some(context) = registry.project_registry_context_by_alias(&candidate).await {
                return Some(context.project.canonical_root);
            }
            if !candidate.pop() {
                return None;
            }
        }
    }

    /// Resolves the registered project for a hook route `cwd`, first by the
    /// parent-directory alias walk (see
    /// [`Self::registered_project_containing_path`]) and, when that misses,
    /// by git-common-dir identity. A linked worktree lives at a sibling path,
    /// so the alias walk never reaches its main checkout; identity resolution
    /// maps it back to the registered project through the shared common dir.
    pub(crate) async fn registered_project_for_route_cwd(&self, cwd: &Path) -> Option<String> {
        if let Some(root) = self.registered_project_containing_path(cwd).await {
            return Some(root);
        }
        let registry = self.registry_db.as_deref()?;
        let git_common_dir = crate::worktree::git_common_dir(cwd);
        let context = registry
            .project_registry_context_by_identity(cwd, git_common_dir.as_deref())
            .await?;
        Some(context.project.canonical_root)
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
                    root,
                    branch,
                    open_options: cg.open_options(),
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
                    root,
                    branch,
                    open_options: cg.open_options(),
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
                    root,
                    branch,
                    open_options: cg.open_options(),
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
