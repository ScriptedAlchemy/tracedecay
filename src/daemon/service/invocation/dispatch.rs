//! The daemon invocation dispatcher: `DaemonInvocationService::invoke`.

use super::*;

/// Upper bound for the size of the `DaemonInvocationService::invoke` future.
///
/// `invoke` matches over every daemon payload, so without boxing its coroutine
/// is as large as the widest payload arm (~46 KiB). That future is embedded by
/// value in every caller's future, so the cost multiplies across call sites and
/// exhausts the default 2 MiB thread stack that both tokio workers and libtest
/// threads use. The large arms are therefore `Box::pin`ned before being awaited.
#[cfg(test)]
const INVOKE_FUTURE_SIZE_BUDGET: usize = 24 * 1024;

#[allow(dead_code)] // PR12 primitive + Plan 37 feedback publication — staged
impl DaemonInvocationService {
    pub(crate) fn operation_events(&self) -> OperationEventAuthority {
        self.operation_events.clone()
    }

    /// Executes a closed request after daemon socket authentication.
    /// `lsp_workspace` is supplied only after the daemon has resolved every
    /// requested root through registered project ownership.
    pub(crate) async fn invoke(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: Option<&Path>,
        lsp_workspace: Option<AuthorizedLspWorkspace>,
        git_service: Option<DaemonGitInvocationOwner>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        let request_id = request.request_id.clone();
        let operation = request.operation();
        let delivery_route = request.delivery_route;
        // Every per-project component this request may need, taken in one pass
        // so dispatch sees one consistent view of the project.
        let runtimes = self
            .project_runtimes
            .request_runtimes(
                project_root,
                project_root
                    .and_then(|root| root.canonicalize().ok())
                    .as_deref(),
            )
            .await;
        let feedback_runtime = runtimes.feedback;
        let observations = feedback_runtime
            .as_ref()
            .map(|runtime| runtime.source_observation_port());
        let observation_subject = plan26_invocation_subject(&request_id, operation, delivery_route);
        if let Err(problem) = request.validate() {
            if plan26_observable_operation(operation)
                && let Some((argument, rejection)) =
                    plan26_invocation_problem_rejected_argument(problem)
            {
                emit_plan26_invocation_event(
                    observations.as_ref(),
                    observation_subject.as_ref(),
                    current_micros(),
                    Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                        operation: plan26_feedback_operation(operation),
                        route: delivery_route,
                        argument,
                        rejection,
                        schema_revision: 1,
                        outcome: Plan26FeedbackOutcomeV1::Rejected,
                    },
                );
            }
            return DaemonInvocationResponse::problem(request_id, problem);
        }
        let dispatched_at = current_micros();
        if plan26_observable_operation(operation) {
            emit_plan26_invocation_event(
                observations.as_ref(),
                observation_subject.as_ref(),
                dispatched_at,
                Plan26FeedbackSourceEventV1::Dispatch {
                    operation: plan26_feedback_operation(operation),
                    outcome: Plan26FeedbackOutcomeV1::Admitted,
                    capacity: 1,
                    admitted: 1,
                },
            );
        }
        let now_ms = now_millis();
        self.expire_sessions(now_ms).await;
        let feedback_service = runtimes.feedback_owner;
        let configuration_runtime = runtimes.configuration;
        let work_runtime = runtimes.work;
        let lsp_owner = runtimes.lsp_owner;

        let response = match request.payload {
            DaemonInvocationPayload::GitRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_git_read(
                    request_id,
                    project_root,
                    git_service,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::GitPreview {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_git_preview(
                    &self.operation_events,
                    request_id,
                    git_service,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::GitApply {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_git_apply(
                    &self.operation_events,
                    request_id,
                    git_service,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::FeedbackDiagnostics {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackDiagnostics,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackGet {
                request_handle,
                resolved_scope,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !feedback_scope_matches(
                    resolved_scope.as_ref(),
                    project_root,
                    feedback_service.as_ref(),
                ) {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                }
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackGet,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackExpand {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackExpand,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackList {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackList,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackAdvisoryCycle { .. } => {
                execute_feedback_advisory_cycle(request_id).await
            }
            DaemonInvocationPayload::FeedbackImpact {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackImpact,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::AffectedTests {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::AffectedTests,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            } => {
                if let Some(observations) = observations.as_ref() {
                    observations.observe_source_event_for_subject(
                        subject_digest,
                        observed_at,
                        event,
                    );
                    DaemonInvocationResponse::with_outcome(
                        request_id,
                        DaemonInvocationOutcome::ObservationAccepted,
                    )
                } else {
                    DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    )
                }
            }
            DaemonInvocationPayload::PrimitiveImpact {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                    Pr12PrimitiveRequest::Impact(request),
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                    Pr12PrimitiveRequest::AffectedFileTests(request),
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::PrimitiveTestResults {
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::TestResults,
                    Pr12PrimitiveRequest::RecentTestResults(page),
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::PrimitiveRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_primitive(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::PrimitiveCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                let request = match request.into_primitive(
                    crate::daemon::code_index_scheduler::queries::callable_query_sanitizer_revision(
                    ),
                    crate::daemon::code_index_scheduler::queries::callable_query_normalization_revision(
                    ),
                    page,
                ) {
                    Ok(request) => request,
                    Err(_) => {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            DaemonInvocationProblem::InvalidRequest,
                        );
                    }
                };
                Box::pin(execute_primitive(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::CallableCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_callable_code(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    page,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::Configuration {
                surface_operation,
                request,
                resolved_scope,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !resolved_scope.as_ref().is_none_or(|scope| {
                    configuration_runtime
                        .as_ref()
                        .is_some_and(|registered| &registered.scope == scope)
                }) {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                }
                Box::pin(execute_configuration(
                    request_id,
                    configuration_runtime,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::ContextScout {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_context_scout(
                    self,
                    request_id,
                    configuration_runtime,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::MultiRootScopeSetRead { .. }
            | DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. }
            | DaemonInvocationPayload::MultiRootExecute { .. } => {
                DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                )
            }
            DaemonInvocationPayload::WorkApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let Some(registered) = work_runtime else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                execute_work_application(
                    registered,
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
            }
            DaemonInvocationPayload::WorkflowApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let Some(project_root) = project_root else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                };
                let Some(registered) = self.work_runtime(Some(project_root)).await else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                Box::pin(execute_workflow_application(
                    registered,
                    project_root,
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::WorkAttempt {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let Some(registered) = work_runtime else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                Box::pin(execute_work_attempt(
                    registered,
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::SemanticEvaluateAndPublish { candidate } => {
                self.execute_semantic_evaluation(project_root, request_id, *candidate)
                    .await
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
            } => {
                self.open_lsp_session(
                    lsp_registry,
                    lsp_workspace,
                    request_id,
                    client_revision,
                    requested_root_uri,
                    workspace_folders,
                    now_ms,
                    lsp_owner,
                )
                .await
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                self.send_lsp_frame(lsp_registry, request_id, session, frame, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspPoll { session } => {
                self.poll_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspAcknowledge { session } => {
                self.acknowledge_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspReconnect { session } => {
                self.reconnect_lsp_session(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspDetach { session } => {
                self.detach_lsp_session(lsp_registry, request_id, session, now_ms)
                    .await
            }
        };
        if plan26_observable_operation(operation) {
            observe_plan26_invocation_response(
                observations.as_ref(),
                observation_subject.as_ref(),
                operation,
                delivery_route,
                dispatched_at,
                &response,
            );
        }
        response
    }
}

#[cfg(test)]
mod future_size_guard {
    use super::*;

    fn future_size<F: std::future::Future>(_: impl FnOnce() -> F) -> usize {
        std::mem::size_of::<F>()
    }

    fn type_only<T>() -> T {
        unreachable!("type-only future size placeholder must never execute")
    }

    /// Fails if a payload arm stops boxing its awaited future and the dispatch
    /// coroutine grows back toward the size that overflowed default stacks.
    #[test]
    fn invoke_future_stays_within_budget() {
        let size = future_size(|| {
            DaemonInvocationService::invoke(
                type_only(),
                type_only(),
                type_only(),
                type_only(),
                type_only(),
                type_only(),
            )
        });
        assert!(
            size <= INVOKE_FUTURE_SIZE_BUDGET,
            "DaemonInvocationService::invoke future is {size} bytes, over the \
             {INVOKE_FUTURE_SIZE_BUDGET} byte budget; box the awaited future of \
             any large payload arm in the dispatch match",
        );
    }
}
