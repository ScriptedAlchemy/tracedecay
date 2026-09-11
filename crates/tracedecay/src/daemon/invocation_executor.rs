//! In-process daemon invocation execution and multi-root query helpers.
//!
//! Captured project admission and cancellation authority stay attached to
//! nested multi-root calls so quiescence drains the whole request atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use tracedecay_application::operation_stream::OperationRequestControls;
use tracedecay_daemon_protocol::DAEMON_TOOL_RESPONSE_GRACE;
use tracedecay_daemon_service::{
    DaemonInvocationOperation, DaemonInvocationProblem, ProjectRuntimeRequestLeaseV1,
    WorkApplicationOutcomeV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::*;

#[cfg(test)]
mod controlled_invocation_tests;

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

impl tracedecay_contracts::MultiRootQueryPort<Value, Value> for PrecomputedMultiRootQueryPort {
    fn query_root(
        &self,
        context: &tracedecay_contracts::RequestContext,
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
    scope: &tracedecay_contracts::ResolvedScope,
) -> std::result::Result<
    tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
    DaemonInvocationProblem,
> {
    tracedecay_domain::RootScopeOutcomeV1::new(
        scope.scope_digest.clone(),
        tracedecay_domain::ScopeOutcome::Denied,
    )
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)
}

pub(super) fn unavailable_root_generation(
    scope: &tracedecay_contracts::ResolvedScope,
    reason: tracedecay_domain::ScopeUnavailableReasonV1,
) -> std::result::Result<
    tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
    DaemonInvocationProblem,
> {
    tracedecay_domain::RootScopeOutcomeV1::new(
        scope.scope_digest.clone(),
        tracedecay_domain::ScopeOutcome::Unavailable { reason },
    )
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)
}

pub(super) fn frozen_root_generation(
    scope: &tracedecay_contracts::ResolvedScope,
    scope_set_digest: &tracedecay_domain::ManifestDigest,
    source_revision: &str,
    operation: &Value,
) -> std::result::Result<tracedecay_domain::RootGenerationV1, DaemonInvocationProblem> {
    let collection_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.multi-root.collection.v1",
        scope,
        source_revision,
    ))
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let collection_revision = tracedecay_domain::CollectionRevision::new(collection_digest)
        .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let stack_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.multi-root.stack.v1",
        scope_set_digest,
        operation,
    ))
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let stack_revision = tracedecay_domain::StackRevision::new(stack_digest)
        .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    tracedecay_domain::RootGenerationV1::new(
        scope.scope_digest.clone(),
        collection_revision,
        stack_revision,
    )
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)
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
) -> std::result::Result<Value, DaemonInvocationProblem> {
    serde_json::to_value(outcome)
        .ok()
        .and_then(|value| value.get("value")?.get("payload").cloned())
        .ok_or(DaemonInvocationProblem::Unavailable)
}

pub(super) fn extract_work_application_payload(
    outcome: &WorkApplicationOutcomeV1,
) -> std::result::Result<Value, DaemonInvocationProblem> {
    match outcome {
        WorkApplicationOutcomeV1::Views(outcome) => extract_application_payload(outcome),
        _ => Err(DaemonInvocationProblem::InvalidRequest),
    }
}

pub(super) fn multi_root_family_allows(
    family: &tracedecay_contracts::MultiRootOperationV1,
    operation: ApplicationSurfaceOperation,
) -> bool {
    match family {
        tracedecay_contracts::MultiRootOperationV1::Git { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::GitStatus
                | ApplicationSurfaceOperation::GitDiff
                | ApplicationSurfaceOperation::GitHistory
                | ApplicationSurfaceOperation::GitBlame
                | ApplicationSurfaceOperation::GitHunks
        ),
        tracedecay_contracts::MultiRootOperationV1::Feedback { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::FeedbackDiagnostics
                | ApplicationSurfaceOperation::FeedbackGet
                | ApplicationSurfaceOperation::FeedbackExpand
                | ApplicationSurfaceOperation::FeedbackList
                | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
        ),
        tracedecay_contracts::MultiRootOperationV1::Impact { .. } => matches!(
            operation,
            ApplicationSurfaceOperation::FeedbackImpact
                | ApplicationSurfaceOperation::AffectedTests
                | ApplicationSurfaceOperation::TestResults
        ),
        tracedecay_contracts::MultiRootOperationV1::Query { .. } => matches!(
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
        tracedecay_contracts::MultiRootOperationV1::Work { .. } => false,
    }
}

#[derive(Clone)]
pub(super) struct InProcessDaemonInvocationExecutor {
    invocation: DaemonInvocationState,
    store_administration: StoreAdministration,
    project_path: PathBuf,
    scope: tracedecay_contracts::ResolvedScope,
    project_admission: Option<ProjectRuntimeRequestLeaseV1>,
    admitted_cancellation: Option<tracedecay_runtime_core::cancellation::CancellationToken>,
}

impl InProcessDaemonInvocationExecutor {
    pub(super) fn new(
        invocation: DaemonInvocationState,
        store_administration: StoreAdministration,
        project_path: PathBuf,
        scope: tracedecay_contracts::ResolvedScope,
    ) -> Self {
        Self {
            invocation,
            store_administration,
            project_path,
            scope,
            project_admission: None,
            admitted_cancellation: None,
        }
    }

    pub(super) fn with_project_admission(
        invocation: DaemonInvocationState,
        store_administration: StoreAdministration,
        project_path: PathBuf,
        scope: tracedecay_contracts::ResolvedScope,
        project_admission: ProjectRuntimeRequestLeaseV1,
        admitted_cancellation: Option<tracedecay_runtime_core::cancellation::CancellationToken>,
    ) -> Self {
        Self {
            invocation,
            store_administration,
            project_path,
            scope,
            project_admission: Some(project_admission),
            admitted_cancellation,
        }
    }

    #[hotpath::skip]
    async fn invoke_once(&self, request: DaemonInvocationRequest) -> DaemonInvocationResponse {
        if let Some(project_admission) = self.project_admission.as_ref() {
            let git_service = if invocation_is_git_operation(request.operation()) {
                git_service_for_project_path(&self.store_administration, Some(&self.project_path))
                    .await
            } else {
                None
            };
            let native_integration_service =
                if invocation_is_native_integration_operation(request.operation()) {
                    native_integration_service_for_project_path(
                        &self.store_administration,
                        Some(&self.project_path),
                    )
                    .await
                } else {
                    None
                };
            self.invocation
                .service
                .invoke_with_project_admission(
                    &self.invocation.lsp_session_registry,
                    &self.project_path,
                    git_service,
                    native_integration_service,
                    request,
                    self.admitted_cancellation.clone(),
                    project_admission,
                )
                .await
        } else {
            self.invocation
                .invoke_for_project(
                    &self.store_administration,
                    Some(&self.project_path),
                    request,
                    None,
                )
                .await
        }
    }
}

impl tracedecay_contracts::ApplicationInvocationExecutor for InProcessDaemonInvocationExecutor {
    #[expect(
        clippy::too_many_lines,
        reason = "One invocation: admit, route, execute, and settle the response."
    )]
    fn invoke(
        &self,
        invocation: tracedecay_contracts::ApplicationInvocation,
    ) -> tracedecay_contracts::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_contracts::ApplicationResponse,
            tracedecay_contracts::InvocationError,
        >,
    > {
        Box::pin(async move {
            let (context, request) = invocation.into_parts();
            let (request_id, target, deadline, cancellation) = context.into_parts();
            if target.resolved().is_some_and(|scope| scope != &self.scope) {
                return Err(tracedecay_contracts::InvocationError::Denied);
            }
            let target = match target {
                tracedecay_contracts::InvocationTarget::CurrentProject => {
                    tracedecay_contracts::InvocationTarget::Resolved(self.scope.clone())
                }
                target @ tracedecay_contracts::InvocationTarget::Resolved(_) => target,
            };
            match request {
                tracedecay_contracts::ApplicationRequest::Surface { binding, payload } => {
                    let (_binding_id, surface, operation, result_contract, _page) =
                        binding.into_parts();
                    let operation =
                        ApplicationSurfaceOperation::from_surface_name(surface, operation.as_str())
                            .ok_or(tracedecay_contracts::InvocationError::InvalidRequest)?;
                    let observed_at = tracedecay_daemon_protocol::invocation_now_micros();
                    let cancellation_context = cancellation.context();
                    let scope = match target {
                        tracedecay_contracts::InvocationTarget::CurrentProject => None,
                        tracedecay_contracts::InvocationTarget::Resolved(scope) => Some(scope),
                    };
                    let policy = if matches!(
                        operation,
                        ApplicationSurfaceOperation::ConfigurationSet
                            | ApplicationSurfaceOperation::ConfigurationUnset
                            | ApplicationSurfaceOperation::ConfigurationBatch
                    ) {
                        tracedecay_daemon_protocol::InvocationCancellationPolicy::AuthoritativeEffect
                    } else {
                        tracedecay_daemon_protocol::InvocationCancellationPolicy::ReadOnly
                    };
                    let request = match operation {
                        ApplicationSurfaceOperation::ConfigurationGet
                        | ApplicationSurfaceOperation::ConfigurationSet
                        | ApplicationSurfaceOperation::ConfigurationUnset
                        | ApplicationSurfaceOperation::ConfigurationBatch => {
                            let request = tracedecay_contracts::configuration_wire_request_from_invocation_payload(
                                operation.as_str(),
                                payload,
                            )
                            .map_err(|_| {
                                tracedecay_contracts::InvocationError::InvalidRequest
                            })?;
                            DaemonInvocationRequest::configuration(
                                request_id.as_str(),
                                operation,
                                request,
                                observed_at,
                                deadline.clone(),
                                cancellation_context,
                            )
                            .with_resolved_scope(scope)
                        }
                        ApplicationSurfaceOperation::FeedbackGet => {
                            let typed = tracedecay_daemon_protocol::parse_application_surface_request(
                                operation, payload,
                            )
                            .map_err(|_| {
                                tracedecay_contracts::InvocationError::InvalidRequest
                            })?;
                            let tracedecay_daemon_protocol::ApplicationSurfaceRequest::Feedback(
                                request,
                            ) = typed
                            else {
                                return Err(
                                    tracedecay_contracts::InvocationError::InvalidRequest,
                                );
                            };
                            DaemonInvocationRequest::feedback(
                                request_id.as_str(),
                                operation,
                                request.request_handle,
                                observed_at,
                                deadline.clone(),
                                cancellation_context,
                            )
                            .with_resolved_scope(scope)
                        }
                        _ => {
                            return Err(
                                tracedecay_contracts::InvocationError::InvalidRequest,
                            );
                        }
                    }
                    .with_delivery_route(tracedecay_daemon_protocol::application_delivery_route(surface));
                    let response =
                        <Self as tracedecay_daemon_protocol::DaemonInvocationExecutor>::invoke_controlled(
                            self,
                            request,
                            deadline,
                            cancellation,
                            policy,
                        )
                        .await
                        .map_err(tracedecay_daemon_protocol::map_invocation_error)?;
                    tracedecay_daemon_protocol::application_response(
                        request_id,
                        result_contract,
                        response.outcome,
                    )
                }
                tracedecay_contracts::ApplicationRequest::FeedbackObservation {
                    configuration_digest,
                    observed_at,
                    event,
                } => {
                    let event = serde_json::from_value(event)
                        .map_err(|_| tracedecay_contracts::InvocationError::InvalidRequest)?;
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
                        Ok(tracedecay_contracts::ApplicationResponse::ObservationAccepted)
                    } else {
                        Err(tracedecay_contracts::InvocationError::Unavailable)
                    }
                }
                tracedecay_contracts::ApplicationRequest::OperationEvents {
                    operation_id,
                    max_events,
                    after_sequence,
                } => {
                    let operation_id =
                        tracedecay_application::operation_stream::OperationId::from_request(
                            operation_id.clone(),
                        );
                    let observed_at = tracedecay_daemon_protocol::invocation_now_micros();
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
                            tracedecay_contracts::StreamEventKind::Terminal(_)
                        );
                        let kind = match event.kind {
                            tracedecay_contracts::StreamEventKind::Item(item) => {
                                tracedecay_contracts::StreamEventKind::Item(
                                    serde_json::to_value(item).map_err(|_| {
                                        tracedecay_contracts::InvocationError::Unavailable
                                    })?,
                                )
                            }
                            tracedecay_contracts::StreamEventKind::Progress {
                                completed,
                                total,
                            } => {
                                tracedecay_contracts::StreamEventKind::Progress { completed, total }
                            }
                            tracedecay_contracts::StreamEventKind::Gap(gap) => {
                                tracedecay_contracts::StreamEventKind::Gap(gap)
                            }
                            tracedecay_contracts::StreamEventKind::Terminal(terminal) => {
                                tracedecay_contracts::StreamEventKind::Terminal(terminal)
                            }
                        };
                        events.push(tracedecay_contracts::StreamEvent {
                            sequence: event.sequence,
                            kind,
                        });
                        if terminated {
                            break;
                        }
                    }
                    let next_sequence = (!terminated).then_some(frontier.next_sequence);
                    Ok(tracedecay_contracts::ApplicationResponse::Stream(
                        tracedecay_contracts::ApplicationStreamResponse {
                            stream: tracedecay_contracts::ApplicationStream {
                                operation_id: operation_id.request_id().clone(),
                                events,
                                frontier,
                                next_sequence,
                                terminated,
                            },
                        },
                    ))
                }
                tracedecay_contracts::ApplicationRequest::OperationCancel { operation_id } => {
                    let operation_id =
                        tracedecay_application::operation_stream::OperationId::from_request(
                            operation_id.clone(),
                        );
                    let observed_at = tracedecay_daemon_protocol::invocation_now_micros();
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
                        tracedecay_application::operation_stream::OperationCancelOutcome::Requested
                        | tracedecay_application::operation_stream::OperationCancelOutcome::AlreadyRequested => true,
                        tracedecay_application::operation_stream::OperationCancelOutcome::AlreadyTerminal => false,
                    };
                    Ok(tracedecay_contracts::ApplicationResponse::Cancellation(
                        tracedecay_contracts::InvocationCancellation {
                            operation_id: operation_id.request_id().clone(),
                            cancelled,
                        },
                    ))
                }
            }
        })
    }
}

/// Settles one in-process invocation. `request_cancellations` is the owning
/// service's table: a caller-side cancellation or deadline is relayed to the
/// daemon-side registration through it when no pre-admitted token exists.
async fn settle_in_process_invocation(
    request_cancellations: &tracedecay_daemon_service::RequestCancellationRegistryV1,
    request_id: &str,
    invocation: tokio::task::JoinHandle<DaemonInvocationResponse>,
    remaining: Duration,
    cancellation: tracedecay_contracts::CancellationSignal,
    admitted_cancellation: Option<tracedecay_runtime_core::cancellation::CancellationToken>,
    policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
) -> std::result::Result<DaemonInvocationResponse, tracedecay_daemon_protocol::DaemonInvocationError>
{
    use tracedecay_contracts::CancellationStage;

    let stage = match policy {
        tracedecay_daemon_protocol::InvocationCancellationPolicy::ReadOnly => {
            CancellationStage::DuringRead
        }
        tracedecay_daemon_protocol::InvocationCancellationPolicy::AuthoritativeEffect => {
            CancellationStage::EffectInFlight
        }
    };
    let invocation = invocation;
    tokio::pin!(invocation);
    let cancellation_wait = tracedecay_daemon_protocol::wait_for_cancellation(cancellation);
    tokio::pin!(cancellation_wait);
    let has_admitted_cancellation = admitted_cancellation.is_some();
    let admitted_cancellation_wait = async move {
        match admitted_cancellation {
            Some(cancellation) => cancellation.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(admitted_cancellation_wait);
    let timed_out = tokio::select! {
        response = &mut invocation => {
            return response.map_err(|_| tracedecay_daemon_protocol::DaemonInvocationError::Unavailable);
        }
        () = &mut cancellation_wait => false,
        () = &mut admitted_cancellation_wait => false,
        () = tokio::time::sleep(remaining) => true,
    };
    if !has_admitted_cancellation {
        request_cancellations.cancel(request_id);
    }
    match policy {
        tracedecay_daemon_protocol::InvocationCancellationPolicy::ReadOnly => {
            if tokio::time::timeout(Duration::from_secs(1), &mut invocation)
                .await
                .is_err()
            {
                invocation.abort();
            }
            if timed_out {
                Err(tracedecay_daemon_protocol::DaemonInvocationError::TimedOut { stage })
            } else {
                Err(tracedecay_daemon_protocol::DaemonInvocationError::Cancelled { stage })
            }
        }
        tracedecay_daemon_protocol::InvocationCancellationPolicy::AuthoritativeEffect => {
            // An authoritative effect settles itself: its own budget bounds it,
            // and when that budget expires after the commit point it reports
            // `PartialEffect` with a committed receipt. Waiting only
            // `DAEMON_TASK_ABORT_DEADLINE` — two seconds, a *shutdown* bound —
            // replaced that answer with `ResetRequired` whenever settlement
            // took a moment longer than the deadline, which tells the operator
            // their store is corrupt and must be reset when in truth one
            // effect merely outlived its budget. Wait for the authoritative
            // settlement over the same grace the daemon's own clients keep
            // reading for, so the effect's real terminal is the one reported.
            match tokio::time::timeout(DAEMON_TOOL_RESPONSE_GRACE, &mut invocation).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) | Err(_) => Ok(DaemonInvocationResponse::problem(
                    request_id,
                    tracedecay_daemon_protocol::DaemonInvocationProblem::ResetRequired,
                )),
            }
        }
    }
}

fn map_operation_event_invocation_error(
    error: tracedecay_application::operation_stream::OperationEventError,
) -> tracedecay_contracts::InvocationError {
    match error {
        tracedecay_application::operation_stream::OperationEventError::NotFoundOrNotAuthorized => {
            tracedecay_contracts::InvocationError::Denied
        }
        tracedecay_application::operation_stream::OperationEventError::RequestNotAdmitted => {
            tracedecay_contracts::InvocationError::DeadlineExceeded
        }
        tracedecay_application::operation_stream::OperationEventError::InvalidFrontier
        | tracedecay_application::operation_stream::OperationEventError::FrontierExpired
        | tracedecay_application::operation_stream::OperationEventError::ResumeExpired => {
            tracedecay_contracts::InvocationError::Conflict
        }
        tracedecay_application::operation_stream::OperationEventError::InvalidConfiguration
        | tracedecay_application::operation_stream::OperationEventError::InvalidContext(_)
        | tracedecay_application::operation_stream::OperationEventError::AlreadyBound
        | tracedecay_application::operation_stream::OperationEventError::Saturated
        | tracedecay_application::operation_stream::OperationEventError::ResumeUnavailable
        | tracedecay_application::operation_stream::OperationEventError::InvalidProgress
        | tracedecay_application::operation_stream::OperationEventError::TerminalAlreadyPublished
        | tracedecay_application::operation_stream::OperationEventError::InvalidTerminal(_)
        | tracedecay_application::operation_stream::OperationEventError::InvalidTestRunEvent => {
            tracedecay_contracts::InvocationError::Unavailable
        }
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for InProcessDaemonInvocationExecutor {
    fn invoke_controlled(
        &self,
        request: DaemonInvocationRequest,
        deadline: tracedecay_contracts::Deadline,
        cancellation: tracedecay_contracts::CancellationSignal,
        policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        Box::pin(async move {
            use tracedecay_contracts::CancellationStage;

            if cancellation.is_cancelled()
                || self.admitted_cancellation.as_ref().is_some_and(
                    tracedecay_runtime_core::cancellation::CancellationToken::is_cancelled,
                )
            {
                return Err(
                    tracedecay_daemon_protocol::DaemonInvocationError::Cancelled {
                        stage: CancellationStage::BeforeAdmission,
                    },
                );
            }
            let remaining = tracedecay_daemon_protocol::deadline_remaining(&deadline).ok_or(
                tracedecay_daemon_protocol::DaemonInvocationError::TimedOut {
                    stage: CancellationStage::BeforeAdmission,
                },
            )?;
            let executor = self.clone();
            let admitted_cancellation = self.admitted_cancellation.clone();
            let request_cancellations = self.invocation.service.request_cancellations().clone();
            tokio::spawn(async move {
                let request_id = request.request_id.clone();
                let invocation = tokio::spawn(hotpath::future!(
                    async move { executor.invoke_once(request).await },
                    label = "daemon.invocation.invoke_once"
                ));
                settle_in_process_invocation(
                    &request_cancellations,
                    &request_id,
                    invocation,
                    remaining,
                    cancellation,
                    admitted_cancellation,
                    policy,
                )
                .await
            })
            .await
            .map_err(|_| tracedecay_daemon_protocol::DaemonInvocationError::Unavailable)?
        })
    }

    fn observe_feedback(
        &self,
        subject_digest: tracedecay_domain::ManifestDigest,
        observed_at: tracedecay_domain::UtcMicros,
        event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<'_, Result<()>> {
        Box::pin(async move {
            let request_id = tracedecay_contracts::request_identity::mint_global_request_id(
                tracedecay_contracts::request_identity::GlobalRequestSurface::FeedbackObservation,
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

pub(super) fn invocation_is_git_operation(operation: DaemonInvocationOperation) -> bool {
    matches!(
        operation,
        DaemonInvocationOperation::GitStatus
            | DaemonInvocationOperation::GitDiff
            | DaemonInvocationOperation::GitHistory
            | DaemonInvocationOperation::GitBlame
            | DaemonInvocationOperation::GitHunks
            | DaemonInvocationOperation::GitPreview
            | DaemonInvocationOperation::GitApply
    )
}

pub(super) fn invocation_is_native_integration_operation(
    operation: DaemonInvocationOperation,
) -> bool {
    matches!(
        operation,
        DaemonInvocationOperation::GitHubStackSignalExpand
            | DaemonInvocationOperation::NativeIntegrationStackSnapshot
            | DaemonInvocationOperation::NativeIntegrationPreflight
            | DaemonInvocationOperation::NativeIntegrationApprove
            | DaemonInvocationOperation::NativeIntegrationApply
            | DaemonInvocationOperation::NativeIntegrationStatus
            | DaemonInvocationOperation::NativeIntegrationCancel
            | DaemonInvocationOperation::NativeIntegrationWorktreeInventory
            | DaemonInvocationOperation::NativeIntegrationWorktreeInspect
            | DaemonInvocationOperation::NativeIntegrationWorktreeConfirm
            | DaemonInvocationOperation::NativeIntegrationWorktreeRemove
            | DaemonInvocationOperation::NativeIntegrationWorktreeReconcile
    )
}
