//! In-process daemon invocation executor and its multi-root query helpers,
//! including the `multi_root_family_allows` kill-switch predicate.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope so the moved code resolves unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::application::operation_stream::OperationRequestControls;
use crate::application_surface::ApplicationSurfaceOperation;
use crate::errors::{Result, TraceDecayError};

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FederatedSurfaceRequestV1 {
    pub(super) operation: ApplicationSurfaceOperation,
    pub(super) request: Value,
}

pub(super) struct PrecomputedMultiRootQueryPort {
    pub(super) outcomes:
        BTreeMap<tracedecay_domain::ManifestDigest, tracedecay_domain::ScopeOutcome<Vec<Value>>>,
}

impl tracedecay_application::MultiRootQueryPort<Value, Value> for PrecomputedMultiRootQueryPort {
    fn query_root(
        &self,
        context: &tracedecay_application::RequestContext,
        _generation: &tracedecay_domain::RootGenerationV1,
        _query: &Value,
        _page: u64,
    ) -> tracedecay_domain::ScopeOutcome<Vec<Value>> {
        self.outcomes
            .get(&context.scope().scope_digest)
            .cloned()
            .unwrap_or(tracedecay_domain::ScopeOutcome::Unavailable {
                reason: tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
            })
    }
}

pub(super) fn denied_root_generation(
    scope: &tracedecay_application::ResolvedScope,
) -> std::result::Result<
    tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
    service::invocation::DaemonInvocationProblem,
> {
    tracedecay_domain::RootScopeOutcomeV1::new(
        scope.scope_digest.clone(),
        tracedecay_domain::ScopeOutcome::Denied,
    )
    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)
}

pub(super) fn unavailable_root_generation(
    scope: &tracedecay_application::ResolvedScope,
    reason: tracedecay_domain::ScopeUnavailableReasonV1,
) -> std::result::Result<
    tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
    service::invocation::DaemonInvocationProblem,
> {
    tracedecay_domain::RootScopeOutcomeV1::new(
        scope.scope_digest.clone(),
        tracedecay_domain::ScopeOutcome::Unavailable { reason },
    )
    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)
}

pub(super) fn frozen_root_generation(
    scope: &tracedecay_application::ResolvedScope,
    scope_set_digest: &tracedecay_domain::ManifestDigest,
    source_revision: &str,
    operation: &Value,
) -> std::result::Result<
    tracedecay_domain::RootGenerationV1,
    service::invocation::DaemonInvocationProblem,
> {
    let collection_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.multi-root.collection.v1",
        scope,
        source_revision,
    ))
    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
    let collection_revision = tracedecay_domain::CollectionRevision::new(collection_digest)
        .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
    let stack_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.multi-root.stack.v1",
        scope_set_digest,
        operation,
    ))
    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
    let stack_revision = tracedecay_domain::StackRevision::new(stack_digest)
        .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
    tracedecay_domain::RootGenerationV1::new(
        scope.scope_digest.clone(),
        collection_revision,
        stack_revision,
    )
    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)
}

pub(super) fn explicit_git_state(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C"])
        .arg(root)
        .args([
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8(output.stdout).ok()?;
    let head = head.trim();
    (!head.is_empty()).then(|| head.to_owned())
}

fn extract_application_payload<T: serde::Serialize>(
    outcome: &T,
) -> std::result::Result<Value, service::invocation::DaemonInvocationProblem> {
    serde_json::to_value(outcome)
        .ok()
        .and_then(|value| value.get("value")?.get("payload").cloned())
        .ok_or(service::invocation::DaemonInvocationProblem::Unavailable)
}

pub(super) fn extract_work_application_payload(
    outcome: &service::invocation::WorkApplicationOutcomeV1,
) -> std::result::Result<Value, service::invocation::DaemonInvocationProblem> {
    match outcome {
        service::invocation::WorkApplicationOutcomeV1::Snapshot(outcome) => {
            extract_application_payload(outcome)
        }
        service::invocation::WorkApplicationOutcomeV1::Delta(outcome) => {
            extract_application_payload(outcome)
        }
        _ => Err(service::invocation::DaemonInvocationProblem::InvalidRequest),
    }
}

pub(super) fn multi_root_family_allows(
    family: &tracedecay_application::MultiRootOperationV1,
    operation: ApplicationSurfaceOperation,
) -> bool {
    match family {
        tracedecay_application::MultiRootOperationV1::Git { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::GitStatus
                | ApplicationSurfaceOperation::GitDiff
                | ApplicationSurfaceOperation::GitHistory
                | ApplicationSurfaceOperation::GitBlame
                | ApplicationSurfaceOperation::GitHunks
        ),
        tracedecay_application::MultiRootOperationV1::Feedback { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::FeedbackDiagnostics
                | ApplicationSurfaceOperation::FeedbackGet
                | ApplicationSurfaceOperation::FeedbackExpand
                | ApplicationSurfaceOperation::FeedbackList
                | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
        ),
        tracedecay_application::MultiRootOperationV1::Impact { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::FeedbackImpact
                | ApplicationSurfaceOperation::AffectedTests
                | ApplicationSurfaceOperation::TestResults
        ),
        tracedecay_application::MultiRootOperationV1::Query { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::CodeExactOccurrence
                | ApplicationSurfaceOperation::CodePhraseSearch
                | ApplicationSurfaceOperation::CodeSymbolSearch
                | ApplicationSurfaceOperation::CodeSignatureSearch
                | ApplicationSurfaceOperation::CodeImplementations
                | ApplicationSurfaceOperation::CodeTypeHierarchy
                | ApplicationSurfaceOperation::CodeCallers
                | ApplicationSurfaceOperation::CodeCallees
                | ApplicationSurfaceOperation::CodeFacets
                | ApplicationSurfaceOperation::CodeTimeline
                | ApplicationSurfaceOperation::CodeDeclaration
                | ApplicationSurfaceOperation::CodeDefinition
                | ApplicationSurfaceOperation::CodeTypeDefinition
                | ApplicationSurfaceOperation::CodeReferences
        ),
        tracedecay_application::MultiRootOperationV1::Work { .. } => false,
    }
}

#[derive(Clone)]
pub(super) struct InProcessDaemonInvocationExecutor {
    invocation: DaemonInvocationState,
    store_administration: StoreAdministration,
    project_path: PathBuf,
    scope: tracedecay_application::ResolvedScope,
}

impl InProcessDaemonInvocationExecutor {
    pub(super) fn new(
        invocation: DaemonInvocationState,
        store_administration: StoreAdministration,
        project_path: PathBuf,
        scope: tracedecay_application::ResolvedScope,
    ) -> Self {
        Self {
            invocation,
            store_administration,
            project_path,
            scope,
        }
    }

    async fn invoke_once(&self, request: DaemonInvocationRequest) -> DaemonInvocationResponse {
        self.invocation
            .invoke_for_project(
                &self.store_administration,
                Some(&self.project_path),
                request,
            )
            .await
    }
}

impl tracedecay_application::ApplicationInvocationExecutor for InProcessDaemonInvocationExecutor {
    fn invoke(
        &self,
        invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async move {
            let (context, request) = invocation.into_parts();
            let (request_id, target, deadline, cancellation) = context.into_parts();
            if target.resolved().is_some_and(|scope| scope != &self.scope) {
                return Err(tracedecay_application::InvocationError::Denied);
            }
            let target = match target {
                tracedecay_application::InvocationTarget::CurrentProject => {
                    tracedecay_application::InvocationTarget::Resolved(self.scope.clone())
                }
                target @ tracedecay_application::InvocationTarget::Resolved(_) => target,
            };
            match request {
                tracedecay_application::ApplicationRequest::Surface { binding, payload } => {
                    let (_binding_id, surface, operation, result_contract, _page) =
                        binding.into_parts();
                    let operation =
                        crate::application_surface::ApplicationSurfaceOperation::from_tool_name(
                            operation.as_str(),
                        )
                        .ok_or(tracedecay_application::InvocationError::InvalidRequest)?;
                    let typed = crate::application_surface::parse_application_surface_request(
                        operation, payload,
                    )
                    .map_err(|_| tracedecay_application::InvocationError::InvalidRequest)?;
                    let observed_at = crate::daemon_client::invocation_now_micros();
                    let cancellation_context = cancellation.context();
                    let scope = match target {
                        tracedecay_application::InvocationTarget::CurrentProject => None,
                        tracedecay_application::InvocationTarget::Resolved(scope) => Some(scope),
                    };
                    let policy = if operation
                        == crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet
                    {
                        crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect
                    } else {
                        crate::daemon_client::InvocationCancellationPolicy::ReadOnly
                    };
                    let request = match (operation, typed) {
                        (
                            crate::application_surface::ApplicationSurfaceOperation::ConfigurationGet
                            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
                            crate::application_surface::ApplicationSurfaceRequest::Configuration(
                                request,
                            ),
                        ) => DaemonInvocationRequest::configuration(
                            request_id.as_str(),
                            operation,
                            request,
                            observed_at,
                            deadline.clone(),
                            cancellation_context,
                        )
                        .with_resolved_scope(scope),
                        (
                            crate::application_surface::ApplicationSurfaceOperation::FeedbackGet,
                            crate::application_surface::ApplicationSurfaceRequest::Feedback(
                                request,
                            ),
                        ) => DaemonInvocationRequest::feedback(
                            request_id.as_str(),
                            operation,
                            request.request_handle,
                            observed_at,
                            deadline.clone(),
                            cancellation_context,
                        )
                        .with_resolved_scope(scope),
                        _ => {
                            return Err(
                                tracedecay_application::InvocationError::InvalidRequest,
                            );
                        }
                    }
                    .with_delivery_route(crate::daemon_client::application_delivery_route(surface));
                    let response =
                        <Self as crate::daemon_client::DaemonInvocationExecutor>::invoke_controlled(
                            self,
                            request,
                            deadline,
                            cancellation,
                            policy,
                        )
                        .await
                        .map_err(crate::daemon_client::map_invocation_error)?;
                    crate::daemon_client::application_response(
                        request_id,
                        result_contract,
                        response.outcome,
                    )
                }
                tracedecay_application::ApplicationRequest::FeedbackObservation {
                    configuration_digest,
                    observed_at,
                    event,
                } => {
                    let event = serde_json::from_value(event)
                        .map_err(|_| tracedecay_application::InvocationError::InvalidRequest)?;
                    let response = self
                        .invoke_once(DaemonInvocationRequest::feedback_observation(
                            request_id.as_str(),
                            configuration_digest,
                            observed_at,
                            event,
                        ))
                        .await;
                    if matches!(
                        response.outcome,
                        DaemonInvocationOutcome::ObservationAccepted
                    ) {
                        Ok(tracedecay_application::ApplicationResponse::ObservationAccepted)
                    } else {
                        Err(tracedecay_application::InvocationError::Unavailable)
                    }
                }
                tracedecay_application::ApplicationRequest::OperationEvents {
                    operation_id,
                    max_events,
                    after_sequence,
                } => {
                    let operation_id =
                        crate::application::operation_stream::OperationId::from_request(
                            operation_id.clone(),
                        );
                    let observed_at = crate::daemon_client::invocation_now_micros();
                    let authority = self.invocation.service.operation_events();
                    let admitted = authority
                        .resolve_invocation_context(
                            &operation_id,
                            &target,
                            OperationRequestControls::new(
                                request_id,
                                deadline,
                                cancellation.context(),
                                observed_at,
                                None,
                            ),
                        )
                        .await
                        .map_err(map_operation_event_invocation_error)?;
                    let requested_next_sequence =
                        after_sequence.map_or(0, |sequence| sequence.saturating_add(1));
                    let subscription = authority
                        .subscribe(
                            &operation_id,
                            &admitted,
                            observed_at,
                            requested_next_sequence,
                            None,
                        )
                        .await
                        .map_err(map_operation_event_invocation_error)?;
                    let (_correlation_id, frontier, mut stream) = subscription.into_sse_parts();
                    let mut events = Vec::with_capacity(max_events as usize);
                    let mut terminated = false;
                    for _ in 0..max_events {
                        let Ok(Some(event)) =
                            timeout(Duration::from_millis(1), stream.next()).await
                        else {
                            break;
                        };
                        terminated = matches!(
                            &event.kind,
                            tracedecay_application::StreamEventKind::Terminal(_)
                        );
                        let kind = match event.kind {
                            tracedecay_application::StreamEventKind::Item(item) => {
                                tracedecay_application::StreamEventKind::Item(
                                    serde_json::to_value(item).map_err(|_| {
                                        tracedecay_application::InvocationError::Unavailable
                                    })?,
                                )
                            }
                            tracedecay_application::StreamEventKind::Progress {
                                completed,
                                total,
                            } => tracedecay_application::StreamEventKind::Progress {
                                completed,
                                total,
                            },
                            tracedecay_application::StreamEventKind::Gap(gap) => {
                                tracedecay_application::StreamEventKind::Gap(gap)
                            }
                            tracedecay_application::StreamEventKind::Terminal(terminal) => {
                                tracedecay_application::StreamEventKind::Terminal(terminal)
                            }
                        };
                        events.push(tracedecay_application::StreamEvent {
                            sequence: event.sequence,
                            kind,
                        });
                        if terminated {
                            break;
                        }
                    }
                    let next_sequence = (!terminated).then_some(frontier.next_sequence);
                    Ok(tracedecay_application::ApplicationResponse::Stream(
                        tracedecay_application::ApplicationStreamResponse {
                            stream: tracedecay_application::ApplicationStream {
                                operation_id: operation_id.request_id().clone(),
                                events,
                                frontier,
                                next_sequence,
                                terminated,
                            },
                        },
                    ))
                }
                tracedecay_application::ApplicationRequest::OperationCancel { operation_id } => {
                    let operation_id =
                        crate::application::operation_stream::OperationId::from_request(
                            operation_id.clone(),
                        );
                    let observed_at = crate::daemon_client::invocation_now_micros();
                    let authority = self.invocation.service.operation_events();
                    let admitted = authority
                        .resolve_invocation_context(
                            &operation_id,
                            &target,
                            OperationRequestControls::new(
                                request_id,
                                deadline,
                                cancellation.context(),
                                observed_at,
                                None,
                            ),
                        )
                        .await
                        .map_err(map_operation_event_invocation_error)?;
                    let cancelled = match authority
                        .cancel(&operation_id, &admitted, observed_at)
                        .await
                        .map_err(map_operation_event_invocation_error)?
                    {
                        crate::application::operation_stream::OperationCancelOutcome::Requested
                        | crate::application::operation_stream::OperationCancelOutcome::AlreadyRequested => true,
                        crate::application::operation_stream::OperationCancelOutcome::AlreadyTerminal => false,
                    };
                    Ok(tracedecay_application::ApplicationResponse::Cancellation(
                        tracedecay_application::InvocationCancellation {
                            operation_id: operation_id.request_id().clone(),
                            cancelled,
                        },
                    ))
                }
            }
        })
    }
}

fn map_operation_event_invocation_error(
    error: crate::application::operation_stream::OperationEventError,
) -> tracedecay_application::InvocationError {
    match error {
        crate::application::operation_stream::OperationEventError::NotFoundOrNotAuthorized => {
            tracedecay_application::InvocationError::Denied
        }
        crate::application::operation_stream::OperationEventError::RequestNotAdmitted => {
            tracedecay_application::InvocationError::DeadlineExceeded
        }
        crate::application::operation_stream::OperationEventError::InvalidFrontier
        | crate::application::operation_stream::OperationEventError::FrontierExpired
        | crate::application::operation_stream::OperationEventError::ResumeExpired => {
            tracedecay_application::InvocationError::Conflict
        }
        crate::application::operation_stream::OperationEventError::InvalidConfiguration
        | crate::application::operation_stream::OperationEventError::InvalidContext(_)
        | crate::application::operation_stream::OperationEventError::AlreadyBound
        | crate::application::operation_stream::OperationEventError::Saturated
        | crate::application::operation_stream::OperationEventError::ResumeUnavailable
        | crate::application::operation_stream::OperationEventError::InvalidProgress
        | crate::application::operation_stream::OperationEventError::TerminalAlreadyPublished
        | crate::application::operation_stream::OperationEventError::InvalidTerminal(_)
        | crate::application::operation_stream::OperationEventError::InvalidTestRunEvent => {
            tracedecay_application::InvocationError::Unavailable
        }
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for InProcessDaemonInvocationExecutor {
    fn invoke_controlled(
        &self,
        request: DaemonInvocationRequest,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<DaemonInvocationResponse, crate::daemon_client::DaemonInvocationError>,
    > {
        Box::pin(async move {
            use tracedecay_application::CancellationStage;

            if cancellation.is_cancelled() {
                return Err(crate::daemon_client::DaemonInvocationError::Cancelled {
                    stage: CancellationStage::BeforeAdmission,
                });
            }
            let remaining = crate::daemon_client::deadline_remaining(&deadline).ok_or(
                crate::daemon_client::DaemonInvocationError::TimedOut {
                    stage: CancellationStage::BeforeAdmission,
                },
            )?;
            let executor = self.clone();
            tokio::spawn(async move {
                let stage = match policy {
                    crate::daemon_client::InvocationCancellationPolicy::ReadOnly => {
                        CancellationStage::DuringRead
                    }
                    crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect => {
                        CancellationStage::EffectInFlight
                    }
                };
                if !policy.may_interrupt(stage) {
                    return Ok(executor.invoke_once(request).await);
                }
                let invocation = executor.invoke_once(request);
                tokio::pin!(invocation);
                let cancellation_wait = crate::daemon_client::wait_for_cancellation(cancellation);
                tokio::pin!(cancellation_wait);
                tokio::select! {
                    response = &mut invocation => Ok(response),
                    () = &mut cancellation_wait => {
                        Err(crate::daemon_client::DaemonInvocationError::Cancelled { stage })
                    }
                    () = tokio::time::sleep(remaining) => {
                        Err(crate::daemon_client::DaemonInvocationError::TimedOut { stage })
                    }
                }
            })
            .await
            .map_err(|_| crate::daemon_client::DaemonInvocationError::Unavailable)?
        })
    }

    fn observe_plan26_feedback(
        &self,
        subject_digest: tracedecay_domain::ManifestDigest,
        observed_at: tracedecay_domain::UtcMicros,
        event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, Result<()>> {
        Box::pin(async move {
            let request_id = crate::request_identity::mint_global_request_id(
                crate::request_identity::GlobalRequestSurface::FeedbackObservation,
            )
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
            let response = self
                .invoke_once(DaemonInvocationRequest::feedback_observation(
                    request_id.as_str(),
                    subject_digest,
                    observed_at,
                    event,
                ))
                .await;
            if matches!(
                response.outcome,
                DaemonInvocationOutcome::ObservationAccepted
            ) {
                Ok(())
            } else {
                Err(TraceDecayError::Config {
                    message: "daemon did not accept the feedback observation".to_owned(),
                })
            }
        })
    }
}

pub(super) fn invocation_is_git_operation(
    operation: service::invocation::DaemonInvocationOperation,
) -> bool {
    matches!(
        operation,
        service::invocation::DaemonInvocationOperation::GitStatus
            | service::invocation::DaemonInvocationOperation::GitDiff
            | service::invocation::DaemonInvocationOperation::GitHistory
            | service::invocation::DaemonInvocationOperation::GitBlame
            | service::invocation::DaemonInvocationOperation::GitHunks
            | service::invocation::DaemonInvocationOperation::GitPreview
            | service::invocation::DaemonInvocationOperation::GitApply
    )
}
