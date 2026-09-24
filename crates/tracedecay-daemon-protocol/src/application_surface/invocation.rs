//! Execution of one catalog-bound application-surface invocation.
//!
//! Surface adapters hand the executor the operation's request body. Socket
//! clients and daemon-local project servers both decode it here into the
//! closed daemon invocation, so cancellation policy, scope admission,
//! transport-failure observation, and result assembly have one owner.

use serde::Serialize;
use serde_json::Value;
use tracedecay_contracts::feedback::observations::{
    FeedbackDeliveryRouteV1, FeedbackOperationV1, FeedbackOutcomeV1, FeedbackSourceEventV1,
};
use tracedecay_contracts::retained_surfaces::RetainedSurfaceOperation;
use tracedecay_contracts::retrieval::PrimitiveRequest;
use tracedecay_contracts::{
    ApplicationEnvelope, ApplicationInvocationBinding, ApplicationInvocationContext,
    ApplicationOutcome, ApplicationProblem, ApplicationResponse, CallableCodeSurfaceRequest,
    InvocationError, NativeIntegrationSurfaceRequest, PageRequest, PrimitiveCodeSurfaceRequest,
    RequestId, ResultContractRef, RetryDirective, SafeDiagnostic, now_micros,
    retained_surface_operation_is_effect, retained_surface_outcome_matches_terminal,
    retained_surface_problem_matches_terminal, try_now_micros,
};
use tracedecay_domain::canonical_sha256;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use super::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceRequest, parse_application_surface_request,
};
use crate::client::{
    DaemonInvocationError, DaemonInvocationExecutor, InvocationCancellationPolicy,
};
use crate::contract::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonInvocationOutcome,
    DaemonInvocationProblem, DaemonInvocationRequest, DaemonInvocationResponse,
};

impl ApplicationSurfaceRequest {
    /// The operation's request body, the shape
    /// [`parse_application_surface_invocation_payload`] decodes.
    ///
    /// Adjacently tagged request families carry the body under `request`;
    /// Git reads carry their decoded bounds because their surface body is
    /// lossy (defaults and scope names are resolved at parse time).
    pub fn into_invocation_payload(self) -> Result<Value, ApplicationSurfaceAdapterError> {
        fn body(request: impl Serialize) -> Result<Value, ApplicationSurfaceAdapterError> {
            serde_json::to_value(request).map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        fn tagged_body(request: impl Serialize) -> Result<Value, ApplicationSurfaceAdapterError> {
            body(request)?
                .get_mut("request")
                .map(Value::take)
                .ok_or_else(|| {
                    ApplicationSurfaceAdapterError::invalid_request(
                        "tagged surface request has no body",
                    )
                })
        }
        match self {
            Self::GitRead(request) => body(request),
            Self::GitPreview(request) => body(request),
            Self::GitApply(request) => body(request),
            Self::GitHubStackSignalExpand(request) => body(request),
            Self::NativeIntegration(request) => match request {
                NativeIntegrationSurfaceRequest::StackSnapshot(request) => body(request),
                NativeIntegrationSurfaceRequest::Preflight(request) => body(request),
                NativeIntegrationSurfaceRequest::Approve(request) => body(request),
                NativeIntegrationSurfaceRequest::Apply(request) => body(request),
                NativeIntegrationSurfaceRequest::Status(request) => body(request),
                NativeIntegrationSurfaceRequest::Cancel(request) => body(request),
                NativeIntegrationSurfaceRequest::Worktree(request) => tagged_body(request),
            },
            Self::Feedback(request) => body(request),
            Self::FeedbackAdvisoryCycle(request) => body(request),
            Self::FeedbackProximity(request) => body(request),
            Self::TestResults(request) => body(request),
            Self::CallableCode(request) => match request {
                CallableCodeSurfaceRequest::ExactOccurrence(request) => body(request),
                CallableCodeSurfaceRequest::PhraseSearch(request) => body(request),
                CallableCodeSurfaceRequest::Callees(request) => body(request),
                CallableCodeSurfaceRequest::Facets(request) => body(request),
                CallableCodeSurfaceRequest::Timeline(request) => body(request),
                CallableCodeSurfaceRequest::Declaration(request)
                | CallableCodeSurfaceRequest::TypeDefinition(request)
                | CallableCodeSurfaceRequest::References(request) => body(request),
            },
            Self::PrimitiveCode(request) => match request {
                PrimitiveCodeSurfaceRequest::SymbolSearch(request) => body(request),
                PrimitiveCodeSurfaceRequest::SignatureSearch(request) => body(request),
                PrimitiveCodeSurfaceRequest::Implementations(request) => body(request),
                PrimitiveCodeSurfaceRequest::TypeHierarchy(request) => body(request),
                PrimitiveCodeSurfaceRequest::Callers(request) => body(request),
            },
            Self::Primitive(request) => tagged_body(request),
            Self::ObservatoryRead(request) => body(request),
            Self::Configuration(request) => tagged_body(request),
            Self::ContextScout(request) => tagged_body(request),
            Self::SourceEdit(request) => body(request),
            Self::SourceEditReconcile(request) => body(request),
            Self::SourceEditRollback(request) => body(request),
            Self::Retained(request) => body(request),
            Self::GraphTool(arguments) => Ok(Value::Object(arguments)),
        }
    }
}

/// Decode an invocation payload produced by
/// [`ApplicationSurfaceRequest::into_invocation_payload`].
///
/// Source edits carry their already-validated invocation, because their
/// public argument decoding resolves defaults and effect identities.
pub fn parse_application_surface_invocation_payload(
    operation: ApplicationSurfaceOperation,
    payload: Value,
) -> Result<ApplicationSurfaceRequest, ApplicationSurfaceAdapterError> {
    match operation {
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks => serde_json::from_value(payload)
            .map(ApplicationSurfaceRequest::GitRead)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::SourceEditReconcile => serde_json::from_value(payload)
            .map(ApplicationSurfaceRequest::SourceEditReconcile)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::SourceEditRollback => serde_json::from_value(payload)
            .map(ApplicationSurfaceRequest::SourceEditRollback)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        operation if super::is_source_edit_operation(operation) => serde_json::from_value(payload)
            .map(ApplicationSurfaceRequest::SourceEdit)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        operation if RetainedSurfaceOperation::from_application(operation).is_some() => {
            serde_json::from_value(payload)
                .map(ApplicationSurfaceRequest::Retained)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        _ => parse_application_surface_request(operation, payload),
    }
}

/// Effects past their commit point settle authoritatively; everything else
/// is abandoned on cancellation.
pub fn application_surface_cancellation_policy(
    operation: ApplicationSurfaceOperation,
) -> InvocationCancellationPolicy {
    if let Some(retained) = RetainedSurfaceOperation::from_application(operation) {
        return if retained_surface_operation_is_effect(retained) {
            InvocationCancellationPolicy::AuthoritativeEffect
        } else {
            InvocationCancellationPolicy::ReadOnly
        };
    }
    match operation {
        ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::GitApply
        | ApplicationSurfaceOperation::NativeIntegrationApprove
        | ApplicationSurfaceOperation::NativeIntegrationApply
        | ApplicationSurfaceOperation::NativeIntegrationCancel
        | ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume
        | ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutClaim
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback
        | ApplicationSurfaceOperation::StrReplace
        | ApplicationSurfaceOperation::MultiStrReplace
        | ApplicationSurfaceOperation::InsertAt
        | ApplicationSurfaceOperation::AstGrepRewrite
        | ApplicationSurfaceOperation::ReplaceSymbol
        | ApplicationSurfaceOperation::InsertAtSymbol
        | ApplicationSurfaceOperation::MoveSymbol
        | ApplicationSurfaceOperation::RenameSymbol
        | ApplicationSurfaceOperation::SourceEditReconcile
        | ApplicationSurfaceOperation::SourceEditRollback => {
            InvocationCancellationPolicy::AuthoritativeEffect
        }
        _ => InvocationCancellationPolicy::ReadOnly,
    }
}

fn daemon_invocation_request(
    request_id: &RequestId,
    operation: ApplicationSurfaceOperation,
    request: ApplicationSurfaceRequest,
    page: PageRequest,
    deadline: tracedecay_contracts::Deadline,
    cancellation: tracedecay_contracts::CancellationContext,
) -> DaemonInvocationRequest {
    let request_id = request_id.as_str();
    let observed_at = now_micros();
    match request {
        ApplicationSurfaceRequest::GitRead(request) => DaemonInvocationRequest::git_read(
            request_id,
            operation,
            request,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::GitPreview(request) => DaemonInvocationRequest::git_preview(
            request_id,
            request,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::GitApply(request) => DaemonInvocationRequest::git_apply(
            request_id,
            request,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::GitHubStackSignalExpand(request) => {
            DaemonInvocationRequest::github_stack_signal_expand(
                request_id,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::NativeIntegration(request) => {
            DaemonInvocationRequest::native_integration(
                request_id,
                operation,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::Feedback(request) => DaemonInvocationRequest::feedback(
            request_id,
            operation,
            request.request_handle,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request) => {
            DaemonInvocationRequest::feedback_advisory_cycle(
                request_id,
                request.document_uri,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::FeedbackProximity(request) => {
            DaemonInvocationRequest::feedback_proximity(request_id, request, deadline, cancellation)
        }
        ApplicationSurfaceRequest::TestResults(_) => DaemonInvocationRequest::primitive(
            request_id,
            operation,
            PrimitiveRequest::RecentTestResults(page),
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::CallableCode(request) => DaemonInvocationRequest::callable_code(
            request_id,
            operation,
            request,
            page,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::PrimitiveCode(request) => {
            DaemonInvocationRequest::primitive_code(
                request_id,
                operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::Primitive(request) => DaemonInvocationRequest::primitive(
            request_id,
            operation,
            request,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::ObservatoryRead(request) => {
            DaemonInvocationRequest::observatory_read(
                request_id,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::Configuration(request) => {
            DaemonInvocationRequest::configuration(
                request_id,
                operation,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::ContextScout(request) => DaemonInvocationRequest::context_scout(
            request_id,
            operation,
            request,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::SourceEdit(request) => DaemonInvocationRequest::source_edit(
            request_id,
            request,
            observed_at,
            deadline,
            cancellation,
        ),
        ApplicationSurfaceRequest::SourceEditReconcile(request) => {
            DaemonInvocationRequest::source_edit_reconcile(
                request_id,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::SourceEditRollback(request) => {
            DaemonInvocationRequest::source_edit_rollback(
                request_id,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::Retained(request) => {
            DaemonInvocationRequest::retained_application(
                request_id,
                request,
                observed_at,
                deadline,
                cancellation,
            )
        }
        ApplicationSurfaceRequest::GraphTool(arguments) => {
            DaemonInvocationRequest::graph_tool(
                request_id,
                operation,
                arguments,
                observed_at,
                deadline,
                cancellation,
            )
        }
    }
}

/// Execute one surface invocation through `executor`'s daemon transport.
///
/// An unreachable daemon never saw the request, so it stays a dispatch
/// failure; every other transport failure keeps its exact stage-bearing
/// problem and is reported to the feedback ledger for observable reads.
#[hotpath::measure(label = "application_surface.invoke", future = true)]
pub async fn invoke_application_surface<E: DaemonInvocationExecutor + ?Sized>(
    executor: &E,
    context: ApplicationInvocationContext,
    binding: ApplicationInvocationBinding,
    payload: Value,
) -> Result<ApplicationResponse, InvocationError> {
    let (request_id, target, deadline, cancellation) = context.into_parts();
    let (_binding_id, surface, operation, result_contract, page) = binding.into_parts();
    let operation = ApplicationSurfaceOperation::from_surface_name(surface, operation.as_str())
        .ok_or(InvocationError::InvalidRequest)?;
    let request = parse_application_surface_invocation_payload(operation, payload)
        .map_err(|_| InvocationError::InvalidRequest)?;
    if !request.matches(operation) {
        return Err(InvocationError::InvalidRequest);
    }
    let route = application_delivery_route(surface);
    let request = daemon_invocation_request(
        &request_id,
        operation,
        request,
        page,
        deadline.clone(),
        cancellation.context(),
    )
    .with_resolved_scope(target.resolved().cloned())
    .map_err(|_| InvocationError::InvalidRequest)?
    .with_delivery_route(route);
    let policy = application_surface_cancellation_policy(operation);
    match executor
        .invoke_controlled(request, deadline, cancellation, policy)
        .await
    {
        Ok(response) => match RetainedSurfaceOperation::from_application(operation) {
            Some(retained) => {
                retained_application_response(retained, request_id, result_contract, response)
            }
            None => application_response(request_id, result_contract, response.outcome),
        },
        Err(DaemonInvocationError::Unreachable {
            reason_code,
            detail,
        }) => Err(InvocationError::Unreachable {
            reason_code,
            detail,
        }),
        Err(error) => {
            observe_transport_failure(executor, &request_id, operation, route, &error).await;
            Err(InvocationError::Problem(Box::new(
                error.into_application_problem(),
            )))
        }
    }
}

async fn observe_transport_failure<E: DaemonInvocationExecutor + ?Sized>(
    executor: &E,
    request_id: &RequestId,
    operation: ApplicationSurfaceOperation,
    route: FeedbackDeliveryRouteV1,
    error: &DaemonInvocationError,
) {
    if !application_surface_feedback_is_observable(operation) {
        return;
    }
    let (Ok(subject_digest), Ok(observed_at)) = (
        canonical_sha256(&(
            "tracedecay.feedback.transport-observation.v1",
            request_id.as_str(),
            operation.as_str(),
            route,
        )),
        try_now_micros(),
    ) else {
        return;
    };
    let feedback_operation = application_surface_feedback_operation(operation);
    let event = match error {
        DaemonInvocationError::Cancelled { .. } => FeedbackSourceEventV1::Cancellation {
            operation: feedback_operation,
            outcome: FeedbackOutcomeV1::Cancelled,
        },
        DaemonInvocationError::TimedOut { .. } => FeedbackSourceEventV1::Cancellation {
            operation: feedback_operation,
            outcome: FeedbackOutcomeV1::TimedOut,
        },
        DaemonInvocationError::Unavailable | DaemonInvocationError::Unreachable { .. } => {
            FeedbackSourceEventV1::Delivery {
                operation: feedback_operation,
                route,
                outcome: FeedbackOutcomeV1::Unavailable,
                item_count: 0,
                duration_micros: None,
            }
        }
    };
    let _ = executor
        .observe_feedback(subject_digest, observed_at, event)
        .await;
}

pub fn application_delivery_route(surface: BindingSurface) -> FeedbackDeliveryRouteV1 {
    match surface {
        BindingSurface::Cli => FeedbackDeliveryRouteV1::Cli,
        BindingSurface::Mcp => FeedbackDeliveryRouteV1::Mcp,
        BindingSurface::Http | BindingSurface::Dashboard => FeedbackDeliveryRouteV1::Http,
        BindingSurface::Lsp => FeedbackDeliveryRouteV1::Lsp,
    }
}

/// Assemble the daemon's answer to a surface invocation.
pub fn application_response(
    request_id: RequestId,
    result_contract: ResultContractRef,
    outcome: DaemonInvocationOutcome,
) -> Result<ApplicationResponse, InvocationError> {
    let envelope = match outcome {
        DaemonInvocationOutcome::GitRead { scope, result }
        | DaemonInvocationOutcome::Feedback { scope, result }
        | DaemonInvocationOutcome::Primitive { scope, result }
        | DaemonInvocationOutcome::CallableCode { scope, result }
        | DaemonInvocationOutcome::ObservatoryRead { scope, result } => {
            ApplicationEnvelope::evidence(
                result_contract,
                request_id,
                scope,
                result.into_application(),
            )
        }
        DaemonInvocationOutcome::GitPreview { scope, preview } => ApplicationEnvelope::preview(
            result_contract,
            request_id,
            scope,
            preview
                .into_application_result()
                .map_err(|_| InvocationError::Unavailable)?,
        ),
        DaemonInvocationOutcome::GitApply { scope, effect } => ApplicationEnvelope::effect(
            result_contract,
            request_id,
            scope,
            effect
                .into_application_result()
                .map_err(|_| InvocationError::Unavailable)?,
        ),
        DaemonInvocationOutcome::Configuration { scope, outcome }
        | DaemonInvocationOutcome::GitHubStackSignalExpand { scope, outcome }
        | DaemonInvocationOutcome::NativeIntegration { scope, outcome }
        | DaemonInvocationOutcome::ContextScout { scope, outcome } => ApplicationEnvelope {
            contract: result_contract,
            request_id,
            scope,
            outcome,
            touched_files: Vec::new(),
            code_graph: None,
            analytics: None,
        },
        DaemonInvocationOutcome::SourceEdit { scope, result } => ApplicationEnvelope {
            contract: result_contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Result(
                serde_json::to_value(result).map_err(|_| InvocationError::Unavailable)?,
            ),
            touched_files: Vec::new(),
            code_graph: None,
            analytics: None,
        },
        DaemonInvocationOutcome::GraphTool { scope, completion } => ApplicationEnvelope {
            contract: result_contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Result(
                completion
                    .result
                    .result_value()
                    .map_err(|_| InvocationError::Unavailable)?,
            ),
            touched_files: completion.touched_files,
            code_graph: completion.code_graph,
            analytics: completion.analytics,
        },
        // The daemon already resolved this invocation to a typed problem
        // (e.g. `configuration.conflict`); carry it whole so surface adapters
        // republish that diagnostic instead of refabricating a generic one.
        DaemonInvocationOutcome::ApplicationProblem { problem } => {
            return Err(InvocationError::Problem(Box::new(problem)));
        }
        DaemonInvocationOutcome::Problem { problem } => {
            return Err(InvocationError::Problem(Box::new(
                daemon_problem_into_application(problem),
            )));
        }
        _ => return Err(InvocationError::Unavailable),
    };
    Ok(ApplicationResponse::unary(envelope))
}

/// Assemble a retained terminal only when it still belongs to the selected
/// operation, request, and authenticated scope.
fn retained_application_response(
    operation: RetainedSurfaceOperation,
    request_id: RequestId,
    result_contract: ResultContractRef,
    response: DaemonInvocationResponse,
) -> Result<ApplicationResponse, InvocationError> {
    let unavailable = |message: &str| {
        InvocationError::Problem(Box::new(ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.surface.invalid_response".to_owned(),
            message: message.to_owned(),
        })))
    };
    if response.protocol != DAEMON_INVOCATION_PROTOCOL
        || response.revision != DAEMON_INVOCATION_REVISION
        || response.request_id != request_id.as_str()
    {
        return Err(unavailable(
            "The daemon returned an invalid retained application envelope",
        ));
    }
    let invalid = || unavailable("The daemon returned an invalid retained application response");
    match response.outcome {
        DaemonInvocationOutcome::RetainedApplication { scope, outcome }
            if retained_surface_outcome_matches_terminal(
                operation,
                &request_id,
                &scope,
                &outcome,
            ) =>
        {
            Ok(ApplicationResponse::unary(ApplicationEnvelope {
                contract: result_contract,
                request_id,
                scope,
                outcome: application_outcome_value(outcome).map_err(|_| invalid())?,
                touched_files: Vec::new(),
                code_graph: None,
                analytics: None,
            }))
        }
        DaemonInvocationOutcome::RetainedApplicationProblem { scope, problem }
            if retained_surface_problem_matches_terminal(
                operation,
                &request_id,
                Some(&scope),
                &problem,
            ) =>
        {
            Err(InvocationError::Problem(Box::new(problem)))
        }
        DaemonInvocationOutcome::ApplicationProblem { problem }
            if retained_surface_problem_matches_terminal(
                operation,
                &request_id,
                None,
                &problem,
            ) =>
        {
            Err(InvocationError::Problem(Box::new(problem)))
        }
        DaemonInvocationOutcome::Problem { problem } => Err(InvocationError::Problem(Box::new(
            retained_daemon_problem(problem),
        ))),
        _ => Err(invalid()),
    }
}

fn retained_daemon_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    let diagnostic = |code: &str, message: &str| SafeDiagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
    };
    match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::invalid_request_without_action(
                "application.surface.invalid_request",
                "The daemon rejected the retained application request",
            )
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::ResetRequired => ApplicationProblem::reset_required(diagnostic(
            "application.surface.reset_required",
            "The retained application store requires an explicit reset",
        )),
        DaemonInvocationProblem::ApplicationContractViolation => {
            ApplicationProblem::unavailable(diagnostic(
                "application.surface.contract_violation",
                "The retained application result violated its canonical contract",
            ))
        }
        DaemonInvocationProblem::Unavailable => ApplicationProblem::unavailable(diagnostic(
            "application.surface.unavailable",
            "The retained application service is unavailable",
        )),
    }
}

/// Re-encode an outcome's typed payload as its JSON carrier.
pub fn application_outcome_value<T: Serialize>(
    outcome: ApplicationOutcome<T>,
) -> Result<ApplicationOutcome<Value>, serde_json::Error> {
    fn payload<T: Serialize>(payload: Option<T>) -> Result<Option<Value>, serde_json::Error> {
        payload.map(serde_json::to_value).transpose()
    }
    Ok(match outcome {
        ApplicationOutcome::Evidence(packet) => {
            ApplicationOutcome::Evidence(tracedecay_contracts::EvidencePacket {
                temporal: packet.temporal,
                authority: packet.authority,
                evidence_authorities: packet.evidence_authorities,
                coverage: packet.coverage,
                omissions: packet.omissions,
                scores: packet.scores,
                contributions: packet.contributions,
                page: packet.page,
                execution: packet.execution,
                payload: payload(packet.payload)?,
            })
        }
        ApplicationOutcome::Preview(preview) => {
            ApplicationOutcome::Preview(tracedecay_contracts::PreviewResult {
                preview_id: preview.preview_id,
                preview_digest: preview.preview_digest,
                effect_class: preview.effect_class,
                authority: preview.authority,
                expected_state: preview.expected_state,
                execution: preview.execution,
                payload: payload(preview.payload)?,
            })
        }
        ApplicationOutcome::Effect(effect) => {
            ApplicationOutcome::Effect(tracedecay_contracts::EffectResult {
                effect_id: effect.effect_id,
                effect_class: effect.effect_class,
                idempotency_key: effect.idempotency_key,
                authority: effect.authority,
                expected_state: effect.expected_state,
                execution: effect.execution,
                reconciliation: effect.reconciliation,
                receipt: effect.receipt,
                payload: payload(effect.payload)?,
            })
        }
        ApplicationOutcome::Result(result) => {
            ApplicationOutcome::Result(serde_json::to_value(result)?)
        }
    })
}

fn daemon_problem_into_application(problem: DaemonInvocationProblem) -> ApplicationProblem {
    let diagnostic = |code: &str, message: &str| SafeDiagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
    };
    match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::invalid_request_without_action(
                "application.surface.invalid_request",
                "The daemon rejected the application request",
            )
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::ResetRequired => ApplicationProblem::reset_required(diagnostic(
            "application.surface.reset_required",
            "The application store requires an explicit reset",
        )),
        DaemonInvocationProblem::ApplicationContractViolation => {
            ApplicationProblem::unavailable(diagnostic(
                "application.surface.contract_violation",
                "The application result violated its canonical contract",
            ))
        }
        DaemonInvocationProblem::Unavailable => ApplicationProblem::unavailable(diagnostic(
            "application.surface.unavailable",
            "The application service for this operation is unavailable",
        )),
    }
}

/// The feedback-ledger operation an application surface reports under.
pub fn application_surface_feedback_operation(
    operation: ApplicationSurfaceOperation,
) -> FeedbackOperationV1 {
    match operation {
        ApplicationSurfaceOperation::FeedbackDiagnostics => {
            FeedbackOperationV1::FeedbackDiagnostics
        }
        ApplicationSurfaceOperation::FeedbackGet => FeedbackOperationV1::FeedbackGet,
        ApplicationSurfaceOperation::FeedbackExpand => FeedbackOperationV1::FeedbackExpand,
        ApplicationSurfaceOperation::FeedbackList => FeedbackOperationV1::FeedbackList,
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => FeedbackOperationV1::FeedbackCycle,
        ApplicationSurfaceOperation::FeedbackProximity => FeedbackOperationV1::Proximity,
        ApplicationSurfaceOperation::FeedbackImpact => FeedbackOperationV1::PrimitiveImpact,
        ApplicationSurfaceOperation::AffectedTests => FeedbackOperationV1::PrimitiveAffectedTests,
        ApplicationSurfaceOperation::TestResults => FeedbackOperationV1::PrimitiveTestResults,
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks
        | ApplicationSurfaceOperation::GitPreview
        | ApplicationSurfaceOperation::GitApply
        | ApplicationSurfaceOperation::GitHubStackSignalExpand
        | ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
        | ApplicationSurfaceOperation::NativeIntegrationPreflight
        | ApplicationSurfaceOperation::NativeIntegrationApprove
        | ApplicationSurfaceOperation::NativeIntegrationApply
        | ApplicationSurfaceOperation::NativeIntegrationStatus
        | ApplicationSurfaceOperation::NativeIntegrationCancel
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile
        | ApplicationSurfaceOperation::CodeExactOccurrence
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
        | ApplicationSurfaceOperation::CodeTypeDefinition
        | ApplicationSurfaceOperation::CodeReferences
        | ApplicationSurfaceOperation::SessionLookup
        | ApplicationSurfaceOperation::QualifiedName
        | ApplicationSurfaceOperation::CallChain
        | ApplicationSurfaceOperation::FileDependents
        | ApplicationSurfaceOperation::SourceLines
        | ApplicationSurfaceOperation::SourceBody
        | ApplicationSurfaceOperation::SourceOutline
        | ApplicationSurfaceOperation::ModuleApi
        | ApplicationSurfaceOperation::Context
        | ApplicationSurfaceOperation::Node
        | ApplicationSurfaceOperation::Impact
        | ApplicationSurfaceOperation::Similar
        | ApplicationSurfaceOperation::Redundancy
        | ApplicationSurfaceOperation::RenamePreview
        | ApplicationSurfaceOperation::PortStatus
        | ApplicationSurfaceOperation::PortOrder
        | ApplicationSurfaceOperation::Todos
        | ApplicationSurfaceOperation::HealthRead
        | ApplicationSurfaceOperation::HealthDelta
        | ApplicationSurfaceOperation::StorageStatus
        | ApplicationSurfaceOperation::DiagnosticsRead
        | ApplicationSurfaceOperation::ObservatoryRead
        | ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit
        | ApplicationSurfaceOperation::ContextScoutStatus
        | ApplicationSurfaceOperation::ContextScoutRecent
        | ApplicationSurfaceOperation::ContextScoutExplain
        | ApplicationSurfaceOperation::ContextScoutCapability
        | ApplicationSurfaceOperation::ContextScoutBudget
        | ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume
        | ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutClaim
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback
        | ApplicationSurfaceOperation::StrReplace
        | ApplicationSurfaceOperation::MultiStrReplace
        | ApplicationSurfaceOperation::InsertAt
        | ApplicationSurfaceOperation::AstGrepRewrite
        | ApplicationSurfaceOperation::ReplaceSymbol
        | ApplicationSurfaceOperation::InsertAtSymbol
        | ApplicationSurfaceOperation::MoveSymbol
        | ApplicationSurfaceOperation::RenameSymbol
        | ApplicationSurfaceOperation::SourceEditReconcile
        | ApplicationSurfaceOperation::SourceEditRollback
        | ApplicationSurfaceOperation::FactStoreCurate
        | ApplicationSurfaceOperation::FactStoreAdd
        | ApplicationSurfaceOperation::FactStoreSearch
        | ApplicationSurfaceOperation::FactStoreProbe
        | ApplicationSurfaceOperation::FactStoreRelated
        | ApplicationSurfaceOperation::FactStoreReason
        | ApplicationSurfaceOperation::FactStoreContradict
        | ApplicationSurfaceOperation::FactStoreGet
        | ApplicationSurfaceOperation::FactStoreUpdate
        | ApplicationSurfaceOperation::FactStoreRemove
        | ApplicationSurfaceOperation::FactStoreSupersede
        | ApplicationSurfaceOperation::FactStoreList
        | ApplicationSurfaceOperation::FactFeedback
        | ApplicationSurfaceOperation::MemoryStatus
        | ApplicationSurfaceOperation::SessionRefreshStatus
        | ApplicationSurfaceOperation::SessionRefreshCancel
        | ApplicationSurfaceOperation::SessionRefreshBegin
        | ApplicationSurfaceOperation::MessageSearch
        | ApplicationSurfaceOperation::SessionsFor
        | ApplicationSurfaceOperation::Workflows
        | ApplicationSurfaceOperation::LcmStatus
        | ApplicationSurfaceOperation::LcmDoctor
        | ApplicationSurfaceOperation::LcmLoadSession
        | ApplicationSurfaceOperation::LcmGrep
        | ApplicationSurfaceOperation::LcmDescribe
        | ApplicationSurfaceOperation::LcmExpand
        | ApplicationSurfaceOperation::LcmExpandQuery => FeedbackOperationV1::FeedbackCycle,
    }
}

/// Surfaces whose rejections and transport failures feed the feedback ledger.
pub fn application_surface_feedback_is_observable(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::FeedbackDiagnostics
            | ApplicationSurfaceOperation::FeedbackGet
            | ApplicationSurfaceOperation::FeedbackExpand
            | ApplicationSurfaceOperation::FeedbackList
            | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
            | ApplicationSurfaceOperation::FeedbackProximity
            | ApplicationSurfaceOperation::FeedbackImpact
            | ApplicationSurfaceOperation::AffectedTests
            | ApplicationSurfaceOperation::TestResults
            | ApplicationSurfaceOperation::SessionLookup
            | ApplicationSurfaceOperation::QualifiedName
            | ApplicationSurfaceOperation::CallChain
            | ApplicationSurfaceOperation::FileDependents
            | ApplicationSurfaceOperation::SourceLines
            | ApplicationSurfaceOperation::SourceBody
            | ApplicationSurfaceOperation::SourceOutline
            | ApplicationSurfaceOperation::ModuleApi
            | ApplicationSurfaceOperation::HealthRead
            | ApplicationSurfaceOperation::HealthDelta
            | ApplicationSurfaceOperation::StorageStatus
            | ApplicationSurfaceOperation::DiagnosticsRead
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_contracts::{
        ApplicationProblem, InvocationError, LegalAction, RequestId, ResultContractRef,
        RetryDirective,
    };
    use tracedecay_tool_catalog::{ApplicationSurfaceOperation, SchemaId};

    use super::{application_response, parse_application_surface_invocation_payload};
    use crate::application_surface::parse_application_surface_request;
    use crate::contract::{DaemonInvocationOutcome, DaemonInvocationProblem};

    #[test]
    fn caller_bodies_encode_to_literal_payloads_the_executor_accepts() {
        for (operation, body, payload) in [
            (
                ApplicationSurfaceOperation::ConfigurationList,
                json!({}),
                json!({}),
            ),
            (
                ApplicationSurfaceOperation::FeedbackList,
                json!({"request_handle": "feedback.handle.v1"}),
                json!({"request_handle": "feedback.handle.v1"}),
            ),
            (
                ApplicationSurfaceOperation::GitStatus,
                json!({}),
                json!({
                    "max_entries": 1000,
                    "max_bytes": 4_194_304,
                    "request": {"query": "status"}
                }),
            ),
            (
                ApplicationSurfaceOperation::GitHistory,
                json!({"count": 5, "path": "src/lib.rs"}),
                json!({
                    "max_entries": 1000,
                    "max_bytes": 4_194_304,
                    "request": {
                        "query": "history",
                        "max_count": 5,
                        "path": "src/lib.rs",
                        "follow": false,
                        "first_parent": false
                    }
                }),
            ),
            (
                ApplicationSurfaceOperation::StorageStatus,
                json!({"include_details": false}),
                json!({"include_details": false}),
            ),
            (
                ApplicationSurfaceOperation::ObservatoryRead,
                json!({}),
                json!({"window_days": 14}),
            ),
        ] {
            let encoded = parse_application_surface_request(operation, body)
                .unwrap_or_else(|error| panic!("{operation:?} body: {error}"))
                .into_invocation_payload()
                .expect("payload");
            assert_eq!(encoded, payload, "{operation:?}");
            let decoded = parse_application_surface_invocation_payload(operation, payload)
                .unwrap_or_else(|error| panic!("{operation:?} payload: {error}"));
            assert!(decoded.matches(operation), "{operation:?}");
        }
    }

    #[test]
    fn configuration_payload_is_the_envelope_stripped_body() {
        let payload = parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationGet,
            json!({"key": "mcp.tool_timings"}),
        )
        .expect("get")
        .into_invocation_payload()
        .expect("payload");
        assert_eq!(payload, json!({"key": "mcp.tool_timings"}));
        assert!(
            parse_application_surface_invocation_payload(
                ApplicationSurfaceOperation::ConfigurationGet,
                json!({"operation": "get", "request": {"key": "mcp.tool_timings"}}),
            )
            .is_err(),
            "the tagged envelope is not an invocation payload"
        );
    }

    #[test]
    fn feedback_payloads_validate_handles_at_the_executor() {
        assert!(
            parse_application_surface_invocation_payload(
                ApplicationSurfaceOperation::FeedbackGet,
                json!({"request_handle": " leading"}),
            )
            .is_err()
        );
    }

    #[test]
    fn daemon_reset_response_remains_an_authoritative_typed_problem() {
        let error = application_response(
            RequestId::new("request.daemon-client.reset").expect("request"),
            ResultContractRef::new(
                SchemaId::new("schema.test.daemon-client-reset-result").expect("schema"),
                1,
            )
            .expect("contract"),
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::ResetRequired,
            },
        )
        .expect_err("reset-required must not become a successful response");

        let InvocationError::Problem(problem) = error else {
            panic!("reset-required must remain an authoritative typed problem");
        };
        let ApplicationProblem::ResetRequired {
            retry,
            legal_actions,
            ..
        } = *problem
        else {
            panic!("reset-required must keep its terminal kind");
        };
        assert_eq!(retry, RetryDirective::Never);
        assert_eq!(legal_actions, vec![LegalAction::Reset]);
    }
}
