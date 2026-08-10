//! Hook-event notification handling: workspace route observation
//! and hook-event plan execution.

use super::*;

impl McpServer {
    /// Authorizes and admits one branch reconciliation without waiting for indexing.
    ///
    /// `effect_root` is the root the write targets and `live_root` the current
    /// project root; both are revalidated here so admit-time membership is never
    /// reused. Everything that differs between the branch plans is carried by
    /// `policy` rather than by branching on the plan again.
    async fn apply_branch_effect(
        &self,
        effect_root: &Path,
        live_root: &Path,
        branch: String,
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
        match &self.code_index_reconcile_sink {
            Some(sink) if sink(root).await => HostAdmissionOutcome::replay_completed(true, false),
            Some(_) | None => {
                HostAdmissionOutcome::retained_unavailable("code_index_scheduler_unavailable")
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
                if rel_paths.is_empty() {
                    return HostAdmissionOutcome::replay_completed(false, true);
                }
                match self.code_index_hook_sink.as_ref() {
                    Some(sink) if sink(root.to_path_buf(), rel_paths).await => {
                        HostAdmissionOutcome::replay_completed(true, false)
                    }
                    Some(_) | None => HostAdmissionOutcome::retained_unavailable(
                        "code_index_scheduler_unavailable",
                    ),
                }
            }
            HookEventPlan::AddBranch(branch) => {
                // Project-root plans must revalidate live root + current branch
                // immediately before effect — same strictness as AddBranchAt.
                self.apply_branch_effect(root, root, branch).await
            }
            HookEventPlan::AddBranchAt {
                root: effect_root,
                branch,
                agent: _,
            } => {
                // Durable effect roots stay concrete (not hashed) and must be
                // freshly normalized, canonicalized, and reauthorized before
                // any write — admit-time membership/branch are never reused.
                self.apply_branch_effect(&effect_root, root, branch).await
            }
            HookEventPlan::SyncCurrentBranch { branch, agent: _ } => {
                // Session/workspace sync plans capture branch at admit time;
                // revalidate live root + current branch immediately before effect.
                self.apply_branch_effect(root, root, branch).await
            }
            HookEventPlan::DebouncedIncrementalSync(agent) => {
                self.run_hook_incremental_sync(cg, agent).await
            }
            HookEventPlan::RecordTerminalReceipt { route, receipt } => {
                match tracedecay_agent_hosts::automation::host_receipts::record(
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
                match tracedecay_agent_hosts::automation::host_receipts::mark_turn_ingested(
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
        match self.accept_debounced_code_index_reconcile(&cg, agent).await {
            Ok(changed) => HostAdmissionOutcome::replay_completed(changed, !changed),
            Err(outcome) => outcome,
        }
    }

    async fn accept_debounced_code_index_reconcile(
        &self,
        cg: &TraceDecay,
        agent: HookAgent,
    ) -> std::result::Result<bool, HostAdmissionOutcome> {
        let marker = hook_events::sync_marker_path(&cg.store_layout().data_root, agent);
        let now = crate::tracedecay::current_timestamp();
        if !hook_events::should_run_sync(&marker, now, 3) {
            return Ok(false);
        }
        let Some(sink) = &self.code_index_reconcile_sink else {
            return Err(HostAdmissionOutcome::retained_unavailable(
                "code_index_scheduler_unavailable",
            ));
        };
        if !sink(cg.project_root().to_path_buf()).await {
            return Err(HostAdmissionOutcome::retained_unavailable(
                "code_index_scheduler_unavailable",
            ));
        }
        hook_events::write_sync_marker(&marker, now);
        Ok(true)
    }
}
