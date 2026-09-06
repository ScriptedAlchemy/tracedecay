//! The daemon invocation dispatcher: `DaemonInvocationService::invoke`.

use super::*;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// Upper bound for the size of the `DaemonInvocationService::invoke` future.
///
/// `invoke` matches over every daemon payload, so without boxing its coroutine
/// is as large as the widest payload arm (~46 KiB). That future is embedded by
/// value in every caller's future, so the cost multiplies across call sites and
/// exhausts the default 2 MiB thread stack that both tokio workers and libtest
/// threads use. The large arms are therefore `Box::pin`ned before being awaited.
#[cfg(test)]
const INVOKE_FUTURE_SIZE_BUDGET: usize = 24 * 1024;

/// In-flight accounting for the daemon invocation front door. Entering counts
/// one request; dropping settles it, so cancellation, panic, and every early
/// denial path release the in-flight gauge.
struct InvocationDispatchGaugeGuard;

impl InvocationDispatchGaugeGuard {
    fn enter() -> Self {
        hotpath::gauge!("daemon.service.invocation.active").inc(1_u64);
        hotpath::gauge!("daemon.service.invocation.requests_total").inc(1_u64);
        Self
    }
}

impl Drop for InvocationDispatchGaugeGuard {
    fn drop(&mut self) {
        hotpath::gauge!("daemon.service.invocation.active").dec(1_u64);
        hotpath::gauge!("daemon.service.invocation.settled_total").inc(1_u64);
    }
}

/// Counts a request the front door denied before any payload handler ran.
/// The reason set is the closed [`DaemonInvocationProblem`] enum, so every
/// key is static and bounded.
fn observe_front_door_denial(problem: DaemonInvocationProblem) {
    match problem {
        DaemonInvocationProblem::InvalidRequest => {
            hotpath::gauge!("daemon.service.invocation.denied.invalid_request").inc(1_u64);
        }
        DaemonInvocationProblem::UnsupportedRevision => {
            hotpath::gauge!("daemon.service.invocation.denied.unsupported_revision").inc(1_u64);
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            hotpath::gauge!("daemon.service.invocation.denied.not_authorized").inc(1_u64);
        }
        DaemonInvocationProblem::ResetRequired => {
            hotpath::gauge!("daemon.service.invocation.denied.reset_required").inc(1_u64);
        }
        DaemonInvocationProblem::ApplicationContractViolation => {
            hotpath::gauge!("daemon.service.invocation.denied.contract_violation").inc(1_u64);
        }
        DaemonInvocationProblem::Unavailable => {
            hotpath::gauge!("daemon.service.invocation.denied.unavailable").inc(1_u64);
        }
    }
}

impl DaemonInvocationService {
    pub fn operation_events(&self) -> OperationEventAuthority {
        self.operation_events.clone()
    }

    pub fn admit_project_request(
        &self,
        project_root: &Path,
    ) -> Option<crate::project_runtime::ProjectRuntimeRequestLeaseV1> {
        self.admit_project_request_resolved(project_root, None)
    }

    pub fn admit_project_request_resolved(
        &self,
        project_root: &Path,
        canonical_root: Option<&Path>,
    ) -> Option<crate::project_runtime::ProjectRuntimeRequestLeaseV1> {
        let resolved;
        let canonical_root = match canonical_root {
            Some(canonical_root) => Some(canonical_root),
            None => {
                resolved = project_root.canonicalize().ok();
                resolved.as_deref()
            }
        };
        self.project_runtimes
            .admit_request(project_root, canonical_root)
    }

    /// Executes a closed request after daemon socket authentication.
    /// `lsp_workspace` is supplied only after the daemon has resolved every
    /// requested root through registered project ownership.
    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn invoke(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: Option<&Path>,
        lsp_workspace: Option<AuthorizedLspWorkspace>,
        git_service: Option<DaemonGitInvocationOwner>,
        native_integration_service: Option<DaemonNativeIntegrationOwner>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        Box::pin(self.invoke_with_cancellation(
            lsp_registry,
            project_root,
            lsp_workspace,
            git_service,
            native_integration_service,
            request,
            None,
        ))
        .await
    }

    /// Executes a request with a cancellation lease that was admitted before a
    /// route-local project-open wait. The ordinary `invoke` entry point keeps
    /// owning registration for callers that do not need a pre-admission wait.
    #[hotpath::skip]
    pub async fn invoke_with_cancellation(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: Option<&Path>,
        lsp_workspace: Option<AuthorizedLspWorkspace>,
        git_service: Option<DaemonGitInvocationOwner>,
        native_integration_service: Option<DaemonNativeIntegrationOwner>,
        request: DaemonInvocationRequest,
        admitted_cancellation: Option<CancellationToken>,
    ) -> DaemonInvocationResponse {
        self.invoke_with_admission(
            lsp_registry,
            project_root,
            lsp_workspace,
            git_service,
            native_integration_service,
            request,
            admitted_cancellation,
            None,
        )
        .await
    }

    #[hotpath::skip]
    pub async fn invoke_with_project_admission(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: &Path,
        git_service: Option<DaemonGitInvocationOwner>,
        native_integration_service: Option<DaemonNativeIntegrationOwner>,
        request: DaemonInvocationRequest,
        admitted_cancellation: Option<CancellationToken>,
        project_admission: &crate::project_runtime::ProjectRuntimeRequestLeaseV1,
    ) -> DaemonInvocationResponse {
        self.invoke_with_admission(
            lsp_registry,
            Some(project_root),
            None,
            git_service,
            native_integration_service,
            request,
            admitted_cancellation,
            Some(project_admission),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "daemon.service.invocation.dispatch", future = true)]
    async fn invoke_with_admission(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: Option<&Path>,
        lsp_workspace: Option<AuthorizedLspWorkspace>,
        git_service: Option<DaemonGitInvocationOwner>,
        native_integration_service: Option<DaemonNativeIntegrationOwner>,
        request: DaemonInvocationRequest,
        admitted_cancellation: Option<CancellationToken>,
        project_admission: Option<&crate::project_runtime::ProjectRuntimeRequestLeaseV1>,
    ) -> DaemonInvocationResponse {
        let _dispatch_gauges = InvocationDispatchGaugeGuard::enter();
        let request_id = request.request_id.clone();
        let cancellation_lease = if admitted_cancellation.is_none() {
            let Some(lease) = crate::request_cancellation::register(&request_id) else {
                observe_front_door_denial(DaemonInvocationProblem::InvalidRequest);
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            };
            Some(lease)
        } else {
            None
        };
        let request_cancellation = match (admitted_cancellation, cancellation_lease.as_ref()) {
            (Some(token), _) => token,
            (None, Some(lease)) => lease.token(),
            (None, None) => {
                observe_front_door_denial(DaemonInvocationProblem::InvalidRequest);
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let operation = request.operation();
        let delivery_route = request.delivery_route;
        // Every per-project component this request may need, taken in one pass
        // so dispatch sees one consistent view of the project. A pre-admitted
        // lease already stored the canonicalize result; reuse it.
        let canonical_root = match (project_root, project_admission) {
            (Some(project_root), Some(project_admission)) => project_admission
                .admitted_canonical_root()
                .map(ToOwned::to_owned)
                .or_else(|| project_root.canonicalize().ok()),
            (Some(project_root), None) => project_root.canonicalize().ok(),
            (None, _) => None,
        };
        let runtimes = match (project_root, project_admission) {
            (Some(project_root), Some(project_admission)) => {
                self.project_runtimes.request_runtimes_with_admission(
                    project_root,
                    canonical_root.as_deref(),
                    project_admission,
                )
            }
            _ => {
                hotpath::future!(
                    self.project_runtimes
                        .request_runtimes(project_root, canonical_root.as_deref()),
                    label = "daemon.service.invocation.admission_wait"
                )
                .await
            }
        };
        let project_runtime_admitted = runtimes.is_admitted();
        let feedback_runtime = runtimes.feedback;
        let observations = feedback_runtime
            .as_ref()
            .map(|runtime| runtime.source_observation_port());
        let observation_subject =
            invocation_observation_subject(&request_id, operation, delivery_route);
        let validated =
            hotpath::measure_block!("daemon.service.invocation.validate", request.validate());
        if let Err(problem) = validated {
            if is_observable_operation(operation)
                && let Some((argument, rejection)) = invocation_problem_rejected_argument(problem)
            {
                emit_invocation_observation(
                    observations.as_ref(),
                    observation_subject.as_ref(),
                    current_micros(),
                    FeedbackSourceEventV1::SurfaceArgumentRejected {
                        operation: feedback_observation_operation(operation),
                        route: delivery_route,
                        argument,
                        rejection,
                        schema_revision: 1,
                        outcome: FeedbackOutcomeV1::Rejected,
                    },
                );
            }
            observe_front_door_denial(problem);
            return DaemonInvocationResponse::problem(request_id, problem);
        }
        let pre_admission_response = match &request.payload {
            DaemonInvocationPayload::LspOpen {
                deadline,
                cancellation,
                ..
            } => lsp::admit_lsp_control(request_id.clone(), deadline, cancellation).err(),
            _ => None,
        };
        if let Some(response) = pre_admission_response {
            return *response;
        }
        if request.requires_project() && !project_runtime_admitted {
            // The wire problem stays deliberately non-diagnostic; the cause
            // must still land in the operator journal, because from the
            // client this refusal is indistinguishable from every other
            // `Unavailable`.
            tracing::warn!(
                operation = ?operation,
                project_root = ?project_root,
                "invocation refused at the front door: the project runtime is \
                 not admitted (project graph runtime unavailable or its \
                 activation is pending)"
            );
            observe_front_door_denial(DaemonInvocationProblem::Unavailable);
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        let dispatched_at = current_micros();
        if is_observable_operation(operation) {
            emit_invocation_observation(
                observations.as_ref(),
                observation_subject.as_ref(),
                dispatched_at,
                FeedbackSourceEventV1::Dispatch {
                    operation: feedback_observation_operation(operation),
                    outcome: FeedbackOutcomeV1::Admitted,
                    capacity: 1,
                    admitted: 1,
                },
            );
        }
        let now_ms = now_millis();
        hotpath::future!(
            self.expire_sessions(now_ms),
            label = "daemon.service.invocation.expire_sessions"
        )
        .await;
        let feedback_service = runtimes.feedback_owner;
        let advisory_cycle = runtimes.advisory_cycle;
        let configuration_runtime = runtimes.configuration;
        let work_runtime = runtimes.work;
        let retained_runtime = runtimes.retained;
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
                    request_cancellation,
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
            DaemonInvocationPayload::GitHubStackSignalExpand {
                request,
                observed_at,
                deadline,
                cancellation,
            } => execute_github_stack_signal_expand(
                request_id,
                configuration_runtime.clone(),
                native_integration_service,
                request,
                observed_at,
                deadline,
                cancellation,
            ),
            DaemonInvocationPayload::NativeIntegration {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let observability_producer = self.observability_producer(project_root).await;
                let status_broadcast = match project_root {
                    Some(project_root) => {
                        Some(self.native_integration_status_broadcast(project_root).await)
                    }
                    None => None,
                };
                Box::pin(execute_native_integration(
                    request_id,
                    configuration_runtime.clone(),
                    native_integration_service,
                    observability_producer,
                    status_broadcast,
                    surface_operation,
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
            DaemonInvocationPayload::FeedbackAdvisoryCycle {
                document_uri,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback_advisory_cycle(
                    request_id,
                    advisory_cycle,
                    document_uri,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
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
                    ApplicationSurfaceOperation::FeedbackImpact,
                    PrimitiveRequest::Impact(request),
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
                    ApplicationSurfaceOperation::AffectedTests,
                    PrimitiveRequest::AffectedFileTests(request),
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
                    ApplicationSurfaceOperation::TestResults,
                    PrimitiveRequest::RecentTestResults(page),
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
                let request = match tracedecay_application::primitive_code_into_primitive(
                    request,
                    tracedecay_code_index_runtime::code_index_scheduler::queries::callable_query_sanitizer_revision(
                    ),
                    tracedecay_code_index_runtime::code_index_scheduler::queries::callable_query_normalization_revision(
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
                let Some(configuration_runtime) = configuration_runtime else {
                    return match project_root {
                        Some(project_root) => missing_registered_owner_problem(
                            &self.project_runtimes,
                            project_root,
                            request_id,
                        ),
                        None => runtime_mounting_problem(request_id),
                    };
                };
                Box::pin(execute_configuration(
                    request_id,
                    Some(configuration_runtime),
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
            DaemonInvocationPayload::ObservatoryRead {
                request,
                resolved_scope,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_observatory_read(
                    self,
                    project_root,
                    request_id,
                    request,
                    resolved_scope,
                    observed_at,
                    deadline,
                    cancellation,
                    request_cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::RetainedApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                Box::pin(execute_retained_application(
                    request_id,
                    retained_runtime,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                    request_cancellation,
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
                let observability_producer = self.observability_producer(project_root).await;
                Box::pin(execute_work_application(
                    registered,
                    Arc::clone(&self.work_attempt_processes),
                    observability_producer,
                    project_root.map(Path::to_path_buf),
                    request_id,
                    *request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
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
                let Some(registered) = work_runtime.clone() else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                let observability_producer = self.observability_producer(Some(project_root)).await;
                Box::pin(execute_workflow_application(
                    registered,
                    Arc::clone(&self.work_attempt_processes),
                    observability_producer,
                    project_root.to_path_buf(),
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                    self.worktree_holder_admission.clone(),
                ))
                .await
            }
            DaemonInvocationPayload::HandoffApplication {
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
                Box::pin(execute_handoff_application(
                    registered,
                    feedback_runtime,
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ))
                .await
            }
            DaemonInvocationPayload::SemanticEvaluateAndPublish {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            } => {
                self.execute_semantic_evaluation(
                    project_root,
                    request_id,
                    evaluated_profile_id,
                    observed_at,
                    deadline,
                    cancellation,
                    request_cancellation.clone(),
                )
                .await
            }
            DaemonInvocationPayload::SemanticActivate {
                evaluated_profile_id,
                set_rollback,
                observed_at,
                deadline,
                cancellation,
            } => {
                self.execute_semantic_activation(
                    project_root,
                    request_id,
                    evaluated_profile_id,
                    set_rollback,
                    observed_at,
                    deadline,
                    cancellation,
                    request_cancellation.clone(),
                )
                .await
            }
            DaemonInvocationPayload::SemanticQualify {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            } => {
                self.execute_semantic_qualification(
                    project_root,
                    request_id,
                    evaluated_profile_id,
                    observed_at,
                    deadline,
                    cancellation,
                    request_cancellation.clone(),
                )
                .await
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
                deadline,
                cancellation,
            } => match lsp::admit_lsp_control(request_id.clone(), &deadline, &cancellation) {
                Ok(()) => {
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
                Err(response) => *response,
            },
            DaemonInvocationPayload::LspFrame {
                session,
                frame,
                deadline,
                cancellation,
            } => match lsp::admit_lsp_control(request_id.clone(), &deadline, &cancellation) {
                Ok(()) => {
                    self.send_lsp_frame(lsp_registry, request_id, session, frame, now_ms)
                        .await
                }
                Err(response) => *response,
            },
            DaemonInvocationPayload::LspPoll {
                session,
                deadline,
                cancellation,
            } => match lsp::admit_lsp_control(request_id.clone(), &deadline, &cancellation) {
                Ok(()) => {
                    self.poll_lsp_frame(lsp_registry, request_id, session, now_ms)
                        .await
                }
                Err(response) => *response,
            },
            DaemonInvocationPayload::LspAcknowledge {
                session,
                deadline,
                cancellation,
            } => match lsp::admit_lsp_control(request_id.clone(), &deadline, &cancellation) {
                Ok(()) => {
                    self.acknowledge_lsp_frame(lsp_registry, request_id, session, now_ms)
                        .await
                }
                Err(response) => *response,
            },
            DaemonInvocationPayload::LspReconnect {
                session,
                deadline,
                cancellation,
            } => match lsp::admit_lsp_control(request_id.clone(), &deadline, &cancellation) {
                Ok(()) => {
                    self.reconnect_lsp_session(lsp_registry, request_id, session, now_ms)
                        .await
                }
                Err(response) => *response,
            },
            DaemonInvocationPayload::LspDetach {
                session,
                deadline,
                cancellation,
            } => match lsp::admit_lsp_control(request_id.clone(), &deadline, &cancellation) {
                Ok(()) => {
                    self.detach_lsp_session(lsp_registry, request_id, session, now_ms)
                        .await
                }
                Err(response) => *response,
            },
        };
        if is_observable_operation(operation) {
            hotpath::measure_block!(
                "daemon.service.invocation.observe_response",
                observe_invocation_response(
                    observations.as_ref(),
                    observation_subject.as_ref(),
                    operation,
                    delivery_route,
                    dispatched_at,
                    &response,
                )
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
