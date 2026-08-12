//! Per-project invocation admission and direct multi-root routing.

use super::*;

impl DaemonInvocationState {
    pub(super) async fn invoke_for_project(
        &self,
        store_administration: &StoreAdministration,
        project_path: Option<&Path>,
        request: DaemonInvocationRequest,
        request_cancellation: Option<CancellationToken>,
    ) -> DaemonInvocationResponse {
        if let Some(response) = invalid_multi_root_invocation_response(&request) {
            return response;
        }
        let direct_request_cancellation_lease = if matches!(
            &request.payload,
            service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead { .. }
                | service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. }
                | service::invocation::DaemonInvocationPayload::MultiRootExecute { .. }
        ) && request_cancellation.is_none()
        {
            let Some(lease) = crate::daemon::request_cancellation::register(&request.request_id)
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            Some(lease)
        } else {
            None
        };
        let direct_request_cancellation = request_cancellation.clone().or_else(|| {
            direct_request_cancellation_lease
                .as_ref()
                .map(crate::daemon::request_cancellation::Lease::token)
        });
        let request_project_path = request.requires_project().then_some(project_path).flatten();
        if let service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead {
            request: scope_set_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id.clone(),
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let Some(_project_request_lease) =
                self.service.admit_project_request(active_project_root)
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            if cancellation.is_cancelled()
                || direct_request_cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(*observed_at)
                || deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
            let scope_set = self
                .service
                .persisted_scope_set(active_project_root, &scope_set_request.scope_set_id)
                .await;
            if direct_request_cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(tracedecay_application::clock::now_micros()) {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
            let Ok(application_request_id) =
                tracedecay_application::RequestId::new(request.request_id.clone())
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            let Some((scope, outcome)) = self
                .service
                .multi_root_evidence(
                    active_project_root,
                    application_request_id,
                    "scope_set_read",
                    scope_set,
                    *observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                )
                .await
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            return DaemonInvocationResponse::with_outcome(
                request.request_id,
                service::invocation::DaemonInvocationOutcome::MultiRootScopeSetRead {
                    scope,
                    outcome,
                },
            );
        }
        if let service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
            request: scope_set_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id.clone(),
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let Some(_project_request_lease) =
                self.service.admit_project_request(active_project_root)
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            if cancellation.is_cancelled()
                || direct_request_cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(*observed_at)
                || deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
            let mut _selected_project_request_leases =
                Vec::with_capacity(scope_set_request.roots.len());
            for selector in &scope_set_request.roots {
                let Some(lease) = self.service.admit_project_request(&selector.root) else {
                    return DaemonInvocationResponse::problem(
                        request.request_id.clone(),
                        service::invocation::DaemonInvocationProblem::Unavailable,
                    );
                };
                _selected_project_request_leases.push(lease);
            }
            let roots = match resolve_multi_root_projects(
                store_administration,
                &self.service,
                &scope_set_request.roots,
            )
            .await
            {
                Ok(roots) => roots,
                Err(problem) => {
                    return DaemonInvocationResponse::problem(request.request_id.clone(), problem);
                }
            };
            let Ok(application_request_id) =
                tracedecay_application::RequestId::new(request.request_id.clone())
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            return match self
                .service
                .compare_and_swap_scope_set(
                    active_project_root,
                    scope_set_request.clone(),
                    roots,
                    *observed_at,
                    deadline,
                    direct_request_cancellation.as_ref(),
                )
                .await
            {
                Some((_scope, result)) => {
                    let Some((scope, outcome)) = self
                        .service
                        .multi_root_evidence(
                            active_project_root,
                            application_request_id,
                            "scope_set_compare_and_swap",
                            result,
                            *observed_at,
                            deadline.clone(),
                            cancellation.clone(),
                        )
                        .await
                    else {
                        return DaemonInvocationResponse::problem(
                            request.request_id,
                            service::invocation::DaemonInvocationProblem::Unavailable,
                        );
                    };
                    DaemonInvocationResponse::with_outcome(
                        request.request_id,
                        service::invocation::DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap {
                            scope,
                            outcome,
                        },
                    )
                }
                None if direct_request_cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled) =>
                {
                    DaemonInvocationResponse::application_problem(
                        request.request_id,
                        tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                    )
                }
                None if deadline.is_elapsed_at(tracedecay_application::clock::now_micros()) => {
                    DaemonInvocationResponse::application_problem(
                        request.request_id,
                        tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                    )
                }
                None => DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                ),
            };
        }
        if let service::invocation::DaemonInvocationPayload::MultiRootExecute {
            request: execute_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let Some(_project_request_lease) =
                self.service.admit_project_request(active_project_root)
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            if cancellation.is_cancelled()
                || direct_request_cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(*observed_at)
                || deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
            {
                return DaemonInvocationResponse::application_problem(
                    request.request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
            return self
                .execute_multi_root_for_project(
                    store_administration,
                    active_project_root,
                    request.request_id,
                    execute_request.clone(),
                    *observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                    direct_request_cancellation,
                )
                .await;
        }
        let lsp_workspace =
            if request.operation() == service::invocation::DaemonInvocationOperation::LspOpen {
                match request_project_path {
                    Some(project_path) => {
                        admitted_lsp_workspace_for_request(
                            store_administration,
                            &self.service,
                            project_path,
                            &request,
                        )
                        .await
                    }
                    None => None,
                }
            } else {
                None
            };
        let git_service = if invocation_is_git_operation(request.operation()) {
            git_service_for_project_path(store_administration, request_project_path).await
        } else {
            None
        };
        let native_integration_service =
            if invocation_is_native_integration_operation(request.operation()) {
                native_integration_service_for_project_path(
                    store_administration,
                    request_project_path,
                )
                .await
            } else {
                None
            };
        let lsp_frame_session = match &request.payload {
            service::invocation::DaemonInvocationPayload::LspFrame { session, .. } => {
                Some(session.clone())
            }
            _ => None,
        };
        let response = Box::pin(self.service.invoke_with_cancellation(
            &self.lsp_session_registry,
            request_project_path,
            lsp_workspace,
            git_service,
            native_integration_service,
            request,
            request_cancellation,
        ))
        .await;
        // A client frame may have parsed a fenced workspace-folder change; the
        // daemon settles it here because only this layer can resolve and
        // authorize the next root set.
        if let (Some(session), Some(project_path)) = (lsp_frame_session, request_project_path) {
            settle_pending_lsp_workspace_mutation(
                store_administration,
                &self.service,
                project_path,
                &session,
            )
            .await;
        }
        response
    }
}
