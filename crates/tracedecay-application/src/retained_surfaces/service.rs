//! Application-owned execution boundary for retained memory and temporal operations.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_domain::{CursorManifestLimitKindV1, UtcMicros};

use super::{
    FactFeedbackRequestV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreCurateRequestV1, FactStoreGetRequestV1, FactStoreListRequestV1,
    FactStoreProbeRequestV1, FactStoreReasonRequestV1, FactStoreRelatedRequestV1,
    FactStoreRemoveRequestV1, FactStoreSearchRequestV1, FactStoreSupersedeRequestV1,
    FactStoreUpdateRequestV1, LcmDescribeRequestV1, LcmDoctorRequestV1, LcmExpandQueryRequestV1,
    LcmExpandRequestV1, LcmGrepRequestV1, LcmLoadSessionRequestV1, LcmStatusRequestV1,
    MemoryStatusRequestV1, MessageSearchRequestV1, RetainedSurfaceOperation,
    RetainedSurfaceRequestV1, RetainedSurfaceResultV1, SessionRefreshRequestV1,
    SessionsForRequestV1, WorkflowsRequestV1, retained_surface_application_operation,
};
use crate::{
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, CancellationSignal,
    CancellationStage, EffectReceipt, LegalAction, RequestAdmission, RequestContext,
    RetryDirective, SafeDiagnostic,
};

pub type RetainedSurfaceExecutionFutureV1<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<
                    ApplicationOutcome<RetainedSurfaceResultV1>,
                    RetainedSurfaceExecutionErrorV1,
                >,
            > + Send
            + 'a,
    >,
>;

/// Bounded error classes a retained runtime may return to the application owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetainedSurfaceExecutionErrorV1 {
    ApplicationProblem(ApplicationProblem),
    StructuralRefusal(RetainedStructuralRefusalV1),
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    PartialEffect {
        reason_code: String,
        committed_receipt: Box<EffectReceipt>,
        detail: String,
    },
    Stale,
    Unsupported,
    Saturated,
    /// The authority cannot serve the request right now. `detail` names the
    /// exact cause (the underlying error or the absent authority) so every
    /// dispatch surface can hand the caller a corrective message instead of a
    /// blank terminal — mirroring the decode-request diagnostic contract.
    Unavailable {
        detail: String,
    },
    ProfileResetRequired,
    ProjectResetRequired,
    Cancelled(CancellationStage),
    TimedOut(CancellationStage),
}

/// Bounded structural refusals that callers must correct rather than retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedStructuralRefusalV1 {
    SessionRetrievalBudget,
    SessionCursorManifestLimit {
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
}

/// Exact admitted input handed to the daemon-owned retained runtime.
pub struct RetainedSurfaceExecutionContextV1<'a> {
    pub request_context: &'a RequestContext,
    pub cancellation_signal: &'a CancellationSignal,
    pub operation: &'a ApplicationOperation,
    pub observed_at: UtcMicros,
}

/// Typed memory operation selected after application admission.
pub enum RetainedMemoryRequestV1<'a> {
    FactStoreAdd(&'a FactStoreAddRequestV1),
    FactStoreSearch(&'a FactStoreSearchRequestV1),
    FactStoreProbe(&'a FactStoreProbeRequestV1),
    FactStoreRelated(&'a FactStoreRelatedRequestV1),
    FactStoreReason(&'a FactStoreReasonRequestV1),
    FactStoreContradict(&'a FactStoreContradictRequestV1),
    FactStoreGet(&'a FactStoreGetRequestV1),
    FactStoreUpdate(&'a FactStoreUpdateRequestV1),
    FactStoreRemove(&'a FactStoreRemoveRequestV1),
    FactStoreSupersede(&'a FactStoreSupersedeRequestV1),
    FactStoreList(&'a FactStoreListRequestV1),
    FactFeedback(&'a FactFeedbackRequestV1),
    MemoryStatus(&'a MemoryStatusRequestV1),
}

/// Automatic curation authority mounted independently from direct fact CRUD.
pub trait RetainedAutomationExecutionPortV1: Send + Sync {
    fn execute_fact_store_curate<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: &'a FactStoreCurateRequestV1,
    ) -> RetainedSurfaceExecutionFutureV1<'a>;
}

/// Typed session operation selected after application admission.
pub enum RetainedSessionRequestV1<'a> {
    SessionRefresh(&'a SessionRefreshRequestV1),
    MessageSearch(&'a MessageSearchRequestV1),
    SessionsFor(&'a SessionsForRequestV1),
    Workflows(&'a WorkflowsRequestV1),
}

/// Typed LCM operation selected after application admission.
pub enum RetainedLcmRequestV1<'a> {
    Status(&'a LcmStatusRequestV1),
    Doctor(&'a LcmDoctorRequestV1),
    LoadSession(&'a LcmLoadSessionRequestV1),
    Grep(&'a LcmGrepRequestV1),
    Describe(&'a LcmDescribeRequestV1),
    Expand(&'a LcmExpandRequestV1),
    ExpandQuery(&'a LcmExpandQueryRequestV1),
}

/// Memory authority mounted independently from session and LCM authorities.
pub trait RetainedMemoryExecutionPortV1: Send + Sync {
    fn execute_memory<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: RetainedMemoryRequestV1<'a>,
    ) -> RetainedSurfaceExecutionFutureV1<'a>;
}

/// Session authority mounted independently from memory and LCM authorities.
pub trait RetainedSessionExecutionPortV1: Send + Sync {
    fn execute_session<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: RetainedSessionRequestV1<'a>,
    ) -> RetainedSurfaceExecutionFutureV1<'a>;
}

/// LCM authority mounted independently from memory and session authorities.
pub trait RetainedLcmExecutionPortV1: Send + Sync {
    fn execute_lcm<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: RetainedLcmRequestV1<'a>,
    ) -> RetainedSurfaceExecutionFutureV1<'a>;
}

/// Independently mounted retained authorities. A missing operation family is a
/// typed unavailable result for that request, never a mount failure for peers.
#[derive(Clone, Default)]
pub struct RetainedSurfacePortsV1<'a> {
    automation: Option<Arc<dyn RetainedAutomationExecutionPortV1 + 'a>>,
    memory: Option<Arc<dyn RetainedMemoryExecutionPortV1 + 'a>>,
    session: Option<Arc<dyn RetainedSessionExecutionPortV1 + 'a>>,
    lcm: Option<Arc<dyn RetainedLcmExecutionPortV1 + 'a>>,
}

impl<'a> RetainedSurfacePortsV1<'a> {
    pub fn with_automation(
        mut self,
        port: Arc<dyn RetainedAutomationExecutionPortV1 + 'a>,
    ) -> Self {
        self.automation = Some(port);
        self
    }

    pub fn with_memory(mut self, port: Arc<dyn RetainedMemoryExecutionPortV1 + 'a>) -> Self {
        self.memory = Some(port);
        self
    }

    pub fn with_session(mut self, port: Arc<dyn RetainedSessionExecutionPortV1 + 'a>) -> Self {
        self.session = Some(port);
        self
    }

    pub fn with_lcm(mut self, port: Arc<dyn RetainedLcmExecutionPortV1 + 'a>) -> Self {
        self.lcm = Some(port);
        self
    }
}

/// One application owner shared by HTTP, MCP, CLI, and generated SDK calls.
#[derive(Clone)]
pub struct RetainedSurfaceServiceV1<'a> {
    ports: RetainedSurfacePortsV1<'a>,
}

impl<'a> RetainedSurfaceServiceV1<'a> {
    #[hotpath::skip]
    pub const fn new(ports: RetainedSurfacePortsV1<'a>) -> Self {
        Self { ports }
    }

    pub async fn execute(
        &self,
        context: &RequestContext,
        cancellation: &CancellationSignal,
        observed_at: UtcMicros,
        request: &RetainedSurfaceRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, ApplicationProblem> {
        admit(context, observed_at)?;
        if cancellation.context().token_id != context.cancellation().token_id {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                RetryDirective::Never,
            ));
        }
        if cancellation.is_cancelled() {
            return Err(ApplicationProblem::cancelled_before_admission());
        }
        let operation =
            retained_surface_application_operation(request.operation()).map_err(|_| {
                unavailable_problem(
                    "application.retained.catalog-unavailable",
                    "The retained application catalog is unavailable.",
                )
            })?;
        if !context.allows(operation.capability_id(), operation.use_case_id()) {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                RetryDirective::Never,
            ));
        }
        let execution_context = RetainedSurfaceExecutionContextV1 {
            request_context: context,
            cancellation_signal: cancellation,
            operation: &operation,
            observed_at,
        };
        let outcome = match classify_retained_surface_request(request) {
            RetainedSurfaceDispatch::Automation(request) => {
                if !request.validate() {
                    Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
                } else {
                    match self.ports.automation.as_ref() {
                        Some(port) => {
                            port.execute_fact_store_curate(execution_context, request)
                                .await
                        }
                        None => Err(RetainedSurfaceExecutionErrorV1::unavailable(
                            "the retained automation authority is not mounted for this scope",
                        )),
                    }
                }
            }
            RetainedSurfaceDispatch::Memory(request) => match self.ports.memory.as_ref() {
                Some(port) => port.execute_memory(execution_context, request).await,
                None => Err(RetainedSurfaceExecutionErrorV1::unavailable(
                    "the retained memory authority is not mounted for this scope",
                )),
            },
            RetainedSurfaceDispatch::Session(request) => match self.ports.session.as_ref() {
                Some(port) => port.execute_session(execution_context, request).await,
                None => Err(RetainedSurfaceExecutionErrorV1::unavailable(
                    "the retained session authority is not mounted for this scope",
                )),
            },
            RetainedSurfaceDispatch::Lcm(request) => match self.ports.lcm.as_ref() {
                Some(port) => port.execute_lcm(execution_context, request).await,
                None => Err(RetainedSurfaceExecutionErrorV1::unavailable(
                    "the retained LCM authority is not mounted for this scope",
                )),
            },
        }
        .map_err(retained_surface_execution_problem)?;
        ensure_post_execution_cancellation(request.operation(), cancellation)?;
        if outcome_matches_operation(request.operation(), &outcome) {
            Ok(outcome)
        } else {
            Err(unavailable_problem(
                "application.retained.invalid-outcome",
                "The retained authority returned an outcome with the wrong effect class.",
            ))
        }
    }
}

enum RetainedSurfaceDispatch<'a> {
    Automation(&'a FactStoreCurateRequestV1),
    Memory(RetainedMemoryRequestV1<'a>),
    Session(RetainedSessionRequestV1<'a>),
    Lcm(RetainedLcmRequestV1<'a>),
}

fn classify_retained_surface_request(
    request: &RetainedSurfaceRequestV1,
) -> RetainedSurfaceDispatch<'_> {
    match request {
        RetainedSurfaceRequestV1::FactStoreCurate(request) => {
            RetainedSurfaceDispatch::Automation(request)
        }
        RetainedSurfaceRequestV1::FactStoreAdd(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreAdd(request))
        }
        RetainedSurfaceRequestV1::FactStoreSearch(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreSearch(request))
        }
        RetainedSurfaceRequestV1::FactStoreProbe(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreProbe(request))
        }
        RetainedSurfaceRequestV1::FactStoreRelated(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreRelated(request))
        }
        RetainedSurfaceRequestV1::FactStoreReason(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreReason(request))
        }
        RetainedSurfaceRequestV1::FactStoreContradict(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreContradict(request))
        }
        RetainedSurfaceRequestV1::FactStoreGet(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreGet(request))
        }
        RetainedSurfaceRequestV1::FactStoreUpdate(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreUpdate(request))
        }
        RetainedSurfaceRequestV1::FactStoreRemove(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreRemove(request))
        }
        RetainedSurfaceRequestV1::FactStoreSupersede(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreSupersede(request))
        }
        RetainedSurfaceRequestV1::FactStoreList(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactStoreList(request))
        }
        RetainedSurfaceRequestV1::FactFeedback(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::FactFeedback(request))
        }
        RetainedSurfaceRequestV1::MemoryStatus(request) => {
            RetainedSurfaceDispatch::Memory(RetainedMemoryRequestV1::MemoryStatus(request))
        }
        RetainedSurfaceRequestV1::SessionRefresh(request) => {
            RetainedSurfaceDispatch::Session(RetainedSessionRequestV1::SessionRefresh(request))
        }
        RetainedSurfaceRequestV1::MessageSearch(request) => {
            RetainedSurfaceDispatch::Session(RetainedSessionRequestV1::MessageSearch(request))
        }
        RetainedSurfaceRequestV1::SessionsFor(request) => {
            RetainedSurfaceDispatch::Session(RetainedSessionRequestV1::SessionsFor(request))
        }
        RetainedSurfaceRequestV1::Workflows(request) => {
            RetainedSurfaceDispatch::Session(RetainedSessionRequestV1::Workflows(request))
        }
        RetainedSurfaceRequestV1::LcmStatus(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::Status(request))
        }
        RetainedSurfaceRequestV1::LcmDoctor(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::Doctor(request))
        }
        RetainedSurfaceRequestV1::LcmLoadSession(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::LoadSession(request))
        }
        RetainedSurfaceRequestV1::LcmGrep(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::Grep(request))
        }
        RetainedSurfaceRequestV1::LcmDescribe(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::Describe(request))
        }
        RetainedSurfaceRequestV1::LcmExpand(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::Expand(request))
        }
        RetainedSurfaceRequestV1::LcmExpandQuery(request) => {
            RetainedSurfaceDispatch::Lcm(RetainedLcmRequestV1::ExpandQuery(request))
        }
    }
}

fn ensure_post_execution_cancellation(
    operation: RetainedSurfaceOperation,
    cancellation: &CancellationSignal,
) -> Result<(), ApplicationProblem> {
    if !retained_surface_operation_is_effect(operation) && cancellation.is_cancelled() {
        Err(retained_surface_execution_problem(
            RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::DuringRead),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn outcome_matches_operation(
    operation: RetainedSurfaceOperation,
    outcome: &ApplicationOutcome<RetainedSurfaceResultV1>,
) -> bool {
    let effect = retained_surface_operation_is_effect(operation);
    let class_matches = matches!(
        (effect, outcome),
        (true, ApplicationOutcome::Effect(_)) | (false, ApplicationOutcome::Evidence(_))
    );
    let result = match outcome {
        ApplicationOutcome::Evidence(packet) => packet.payload.as_ref(),
        ApplicationOutcome::Effect(effect) => effect.payload.as_ref(),
        ApplicationOutcome::Preview(_) => None,
    };
    if let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = result {
        return operation == RetainedSurfaceOperation::FactStoreCurate
            && class_matches
            && result.matches_terminal();
    }
    class_matches
        && matches!(
            (operation, result),
            (
                RetainedSurfaceOperation::FactStoreAdd,
                Some(RetainedSurfaceResultV1::FactStoreAdd(_))
            ) | (
                RetainedSurfaceOperation::FactStoreSearch,
                Some(RetainedSurfaceResultV1::FactStoreSearch(_))
            ) | (
                RetainedSurfaceOperation::FactStoreProbe,
                Some(RetainedSurfaceResultV1::FactStoreProbe(_))
            ) | (
                RetainedSurfaceOperation::FactStoreRelated,
                Some(RetainedSurfaceResultV1::FactStoreRelated(_))
            ) | (
                RetainedSurfaceOperation::FactStoreReason,
                Some(RetainedSurfaceResultV1::FactStoreReason(_))
            ) | (
                RetainedSurfaceOperation::FactStoreContradict,
                Some(RetainedSurfaceResultV1::FactStoreContradict(_))
            ) | (
                RetainedSurfaceOperation::FactStoreGet,
                Some(RetainedSurfaceResultV1::FactStoreGet(_))
            ) | (
                RetainedSurfaceOperation::FactStoreUpdate,
                Some(RetainedSurfaceResultV1::FactStoreUpdate(_))
            ) | (
                RetainedSurfaceOperation::FactStoreRemove,
                Some(RetainedSurfaceResultV1::FactStoreRemove(_))
            ) | (
                RetainedSurfaceOperation::FactStoreSupersede,
                Some(RetainedSurfaceResultV1::FactStoreSupersede(_))
            ) | (
                RetainedSurfaceOperation::FactStoreList,
                Some(RetainedSurfaceResultV1::FactStoreList(_))
            ) | (
                RetainedSurfaceOperation::FactFeedback,
                Some(RetainedSurfaceResultV1::FactFeedback(_))
            ) | (
                RetainedSurfaceOperation::MemoryStatus,
                Some(RetainedSurfaceResultV1::MemoryStatus(_))
            ) | (
                RetainedSurfaceOperation::SessionRefreshStatus,
                Some(RetainedSurfaceResultV1::SessionRefreshStatus(_))
            ) | (
                RetainedSurfaceOperation::SessionRefreshCancel,
                Some(RetainedSurfaceResultV1::SessionRefreshCancel(_))
            ) | (
                RetainedSurfaceOperation::SessionRefreshBegin,
                Some(RetainedSurfaceResultV1::SessionRefreshBegin(_))
            ) | (
                RetainedSurfaceOperation::MessageSearch,
                Some(RetainedSurfaceResultV1::MessageSearch(_))
            ) | (
                RetainedSurfaceOperation::SessionsFor,
                Some(RetainedSurfaceResultV1::SessionsFor(_))
            ) | (
                RetainedSurfaceOperation::Workflows,
                Some(RetainedSurfaceResultV1::Workflows(_))
            ) | (
                RetainedSurfaceOperation::LcmStatus,
                Some(RetainedSurfaceResultV1::LcmStatus(_))
            ) | (
                RetainedSurfaceOperation::LcmDoctor,
                Some(RetainedSurfaceResultV1::LcmDoctor(_))
            ) | (
                RetainedSurfaceOperation::LcmLoadSession,
                Some(RetainedSurfaceResultV1::LcmLoadSession(_))
            ) | (
                RetainedSurfaceOperation::LcmGrep,
                Some(RetainedSurfaceResultV1::LcmGrep(_))
            ) | (
                RetainedSurfaceOperation::LcmDescribe,
                Some(RetainedSurfaceResultV1::LcmDescribe(_))
            ) | (
                RetainedSurfaceOperation::LcmExpand,
                Some(RetainedSurfaceResultV1::LcmExpand(_))
            ) | (
                RetainedSurfaceOperation::LcmExpandQuery,
                Some(RetainedSurfaceResultV1::LcmExpandQuery(_))
            )
        )
}

/// Whether a retained operation can cross its durable effect boundary.
pub const fn retained_surface_operation_is_effect(operation: RetainedSurfaceOperation) -> bool {
    matches!(
        operation,
        RetainedSurfaceOperation::FactStoreCurate
            | RetainedSurfaceOperation::FactStoreAdd
            | RetainedSurfaceOperation::FactStoreUpdate
            | RetainedSurfaceOperation::FactStoreRemove
            | RetainedSurfaceOperation::FactStoreSupersede
            | RetainedSurfaceOperation::FactFeedback
            | RetainedSurfaceOperation::SessionRefreshCancel
            | RetainedSurfaceOperation::SessionRefreshBegin
    )
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

/// Canonical semantic problem projection for a retained runtime failure.
pub fn retained_surface_execution_problem(
    error: RetainedSurfaceExecutionErrorV1,
) -> ApplicationProblem {
    match error {
        RetainedSurfaceExecutionErrorV1::ApplicationProblem(problem) => problem,
        RetainedSurfaceExecutionErrorV1::StructuralRefusal(refusal) => {
            structural_refusal_problem(refusal)
        }
        RetainedSurfaceExecutionErrorV1::InvalidRequest => ApplicationProblem::InvalidRequest {
            diagnostic: diagnostic(
                "application.retained.invalid-request",
                "The retained operation request is invalid.",
            ),
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        RetainedSurfaceExecutionErrorV1::Conflict => ApplicationProblem::Conflict {
            diagnostic: diagnostic(
                "application.retained.conflict",
                "The retained operation conflicts with current state.",
            ),
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code,
            committed_receipt,
            detail,
        } => ApplicationProblem::PartialEffect {
            diagnostic: SafeDiagnostic {
                code: reason_code,
                message: detail,
            },
            committed_receipt,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::Reconcile],
        },
        RetainedSurfaceExecutionErrorV1::Stale => ApplicationProblem::stale(diagnostic(
            "application.retained.stale",
            "The retained authority is stale for this request.",
        )),
        RetainedSurfaceExecutionErrorV1::Unsupported => ApplicationProblem::Unsupported {
            diagnostic: diagnostic(
                "application.retained.unsupported",
                "The retained authority does not support this request.",
            ),
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        RetainedSurfaceExecutionErrorV1::Saturated => ApplicationProblem::Saturated {
            diagnostic: diagnostic(
                "application.retained.saturated",
                "The retained authority cannot admit more work right now.",
            ),
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
        // Structural budget refusal uses InvalidRequest so the wire kind stays
        // unchanged and non-retryable. Callers must narrow scope or limit.
        RetainedSurfaceExecutionErrorV1::Unavailable { detail } => {
            ApplicationProblem::Unavailable {
                classification: crate::ApplicationUnavailableClassV1::Authority,
                diagnostic: SafeDiagnostic {
                    code: "application.retained.authority-unavailable".to_owned(),
                    message: unavailable_authority_message(&detail),
                },
                retry: RetryDirective::AfterDelay,
                legal_actions: vec![LegalAction::Retry],
            }
        }
        RetainedSurfaceExecutionErrorV1::ProfileResetRequired => {
            ApplicationProblem::reset_required(diagnostic(
                "application.retained.profile-reset-required",
                "The retained profile store requires an explicit reset before it can serve requests.",
            ))
        }
        RetainedSurfaceExecutionErrorV1::ProjectResetRequired => {
            ApplicationProblem::reset_required(diagnostic(
                "application.retained.project-reset-required",
                "The retained project store requires an explicit reset before it can serve requests.",
            ))
        }
        RetainedSurfaceExecutionErrorV1::Cancelled(stage) => ApplicationProblem::Cancelled {
            stage,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        RetainedSurfaceExecutionErrorV1::TimedOut(stage) => ApplicationProblem::TimedOut {
            stage,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    }
}

fn unavailable_problem(code: &'static str, message: &'static str) -> ApplicationProblem {
    ApplicationProblem::Unavailable {
        classification: crate::ApplicationUnavailableClassV1::Authority,
        diagnostic: diagnostic(code, message),
        retry: RetryDirective::AfterDelay,
        legal_actions: vec![LegalAction::Retry],
    }
}

const UNAVAILABLE_AUTHORITY_MESSAGE: &str = "The retained operation authority is unavailable";

/// SafeDiagnostic validation refuses empty/untrimmed text, control characters,
/// and messages over 512 bytes, so the threaded cause is normalized at this
/// single projection choke point instead of at every producer.
fn unavailable_authority_message(detail: &str) -> String {
    let sanitized = detail
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        return format!("{UNAVAILABLE_AUTHORITY_MESSAGE}.");
    }
    // ": " joiner; 512 is the SafeDiagnostic message byte limit.
    let budget = 512 - (UNAVAILABLE_AUTHORITY_MESSAGE.len() + 2);
    let mut end = sanitized.len().min(budget);
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{UNAVAILABLE_AUTHORITY_MESSAGE}: {}",
        sanitized[..end].trim_end()
    )
}

impl RetainedSurfaceExecutionErrorV1 {
    /// Typed unavailability that names its cause. Producers must pass the
    /// underlying error text or an honest description of the absent
    /// authority; the projection above sanitizes it for the diagnostic.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::Unavailable {
            detail: detail.into(),
        }
    }

    pub fn cursor_stale_refusal() -> Self {
        Self::ApplicationProblem(ApplicationProblem::Stale {
            diagnostic: diagnostic(
                "application.retained.cursor-stale",
                "The retrieval cursor no longer matches the current candidate cohort. Restart the request without a cursor.",
            ),
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::RestartWithoutCursor],
        })
    }

    /// Fail-closed structural budget refusal. True concurrent saturation stays
    /// [`Self::Saturated`] and retryable; this path never is.
    pub fn structural_budget_refusal() -> Self {
        Self::StructuralRefusal(RetainedStructuralRefusalV1::SessionRetrievalBudget)
    }

    pub fn cursor_manifest_limit_refusal(
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    ) -> Self {
        Self::StructuralRefusal(RetainedStructuralRefusalV1::SessionCursorManifestLimit {
            kind,
            observed,
            maximum,
        })
    }
}

fn structural_refusal_problem(refusal: RetainedStructuralRefusalV1) -> ApplicationProblem {
    let diagnostic = match refusal {
        RetainedStructuralRefusalV1::SessionRetrievalBudget => diagnostic(
            "application.retained.budget-refused",
            "The request exceeds the admitted retrieval budget. Narrow the scope or limit.",
        ),
        RetainedStructuralRefusalV1::SessionCursorManifestLimit {
            kind: CursorManifestLimitKindV1::Participants,
            ..
        } => diagnostic(
            "application.retained.session-cursor-manifest-participants-limit-exceeded",
            "The authorized session scope contains too many cursor participants. Narrow the session scope.",
        ),
        RetainedStructuralRefusalV1::SessionCursorManifestLimit {
            kind: CursorManifestLimitKindV1::CanonicalBytes,
            ..
        } => diagnostic(
            "application.retained.session-cursor-manifest-canonical-bytes-limit-exceeded",
            "The authorized session scope exceeds the cursor manifest byte limit. Narrow the session scope.",
        ),
    };
    ApplicationProblem::InvalidRequest {
        diagnostic,
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn diagnostic(code: &'static str, message: &'static str) -> SafeDiagnostic {
    SafeDiagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::*;
    use crate::retained_surfaces::{FactReadOptionsV1, FactStoreSearchRequestV1};
    use crate::{
        ApplicationProblemEnvelope, ApplicationProblemKind, CancellationContext, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, EffectTermination, IdempotencyKey, ProblemTerminality,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, WorktreeId};
    use tracedecay_tool_catalog::EffectClass;

    struct ErrorMemoryPort(RetainedSurfaceExecutionErrorV1);

    impl RetainedMemoryExecutionPortV1 for ErrorMemoryPort {
        fn execute_memory<'a>(
            &'a self,
            _context: RetainedSurfaceExecutionContextV1<'a>,
            request: RetainedMemoryRequestV1<'a>,
        ) -> RetainedSurfaceExecutionFutureV1<'a> {
            assert!(matches!(
                request,
                RetainedMemoryRequestV1::FactStoreSearch(_)
            ));
            let error = self.0.clone();
            Box::pin(async move { Err(error) })
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture identity is valid")
    }

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64)))
            .expect("fixture digest is valid")
    }

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            id::<ProjectId>("project.retained.fixture"),
            id::<RepositoryId>("repository.retained.fixture"),
            id::<WorktreeId>("worktree.retained.fixture"),
            None,
        )
        .expect("fixture scope is valid")
    }

    fn context_for(operation: &ApplicationOperation) -> RequestContext {
        let scope = scope();
        let grant = CapabilityGrantSnapshot::new(
            id::<CapabilityGrantId>("grant.retained.fixture"),
            1,
            digest('a'),
            id::<ActorId>("actor.retained.issuer"),
            UtcMicros(1),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([operation.capability_id().clone()]),
            BTreeSet::from([operation.use_case_id().clone()]),
            crate::DisclosureClass::Evidence,
        )
        .expect("fixture grant is valid");
        RequestContext::new(
            id::<ActorId>("actor.retained.requester"),
            scope,
            grant,
            RequestId::new("request.retained.fixture").expect("fixture request id"),
            Deadline::new(UtcMicros(500)).expect("fixture deadline"),
            CancellationContext::active("cancel.retained.fixture")
                .expect("fixture cancellation context"),
        )
        .expect("fixture context is valid")
    }

    fn request() -> RetainedSurfaceRequestV1 {
        RetainedSurfaceRequestV1::FactStoreSearch(FactStoreSearchRequestV1 {
            query: "retained fixture".to_owned(),
            options: FactReadOptionsV1::default(),
            after: None,
        })
    }

    fn partial_receipt(
        operation: &ApplicationOperation,
        context: &RequestContext,
    ) -> EffectReceipt {
        EffectReceipt {
            operation: operation.use_case_id().clone(),
            request_id: context.request_id().clone(),
            actor: context.actor().clone(),
            scope: context.scope().clone(),
            effect_class: EffectClass::Administrative,
            idempotency_key: IdempotencyKey::new("idempotency.retained.fixture")
                .expect("fixture idempotency key"),
            input_digest: digest('a'),
            expected_state: digest('b'),
            policy_digest: digest('c'),
            configuration_digest: digest('d'),
            catalog_digest: digest('e'),
            privacy_digest: digest('f'),
            outcome: EffectTermination::Partial,
            committed_state: Some(digest('a')),
            external_proof: None,
        }
    }

    fn service_for(error: RetainedSurfaceExecutionErrorV1) -> RetainedSurfaceServiceV1<'static> {
        RetainedSurfaceServiceV1::new(
            RetainedSurfacePortsV1::default().with_memory(Arc::new(ErrorMemoryPort(error))),
        )
    }

    #[test]
    fn structural_budget_refusal_is_non_retryable_invalid_request() {
        let problem = retained_surface_execution_problem(
            RetainedSurfaceExecutionErrorV1::structural_budget_refusal(),
        );
        assert_eq!(problem.kind(), ApplicationProblemKind::InvalidRequest);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(
            problem
                .diagnostic()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("application.retained.budget-refused")
        );
        assert_eq!(problem.legal_actions(), &[LegalAction::CorrectRequest]);
    }

    #[test]
    fn cursor_manifest_refusals_have_distinct_non_retryable_diagnostics() {
        for (kind, expected_code) in [
            (
                CursorManifestLimitKindV1::Participants,
                "application.retained.session-cursor-manifest-participants-limit-exceeded",
            ),
            (
                CursorManifestLimitKindV1::CanonicalBytes,
                "application.retained.session-cursor-manifest-canonical-bytes-limit-exceeded",
            ),
        ] {
            let problem = retained_surface_execution_problem(
                RetainedSurfaceExecutionErrorV1::cursor_manifest_limit_refusal(kind, 257, 256),
            );
            assert_eq!(problem.kind(), ApplicationProblemKind::InvalidRequest);
            assert_eq!(problem.retry(), RetryDirective::Never);
            assert_eq!(
                problem
                    .diagnostic()
                    .map(|diagnostic| diagnostic.code.as_str()),
                Some(expected_code)
            );
            assert_eq!(problem.legal_actions(), &[LegalAction::CorrectRequest]);
        }
    }

    #[test]
    fn cursor_stale_refusal_requires_restart_without_cursor() {
        let problem = retained_surface_execution_problem(
            RetainedSurfaceExecutionErrorV1::cursor_stale_refusal(),
        );
        assert_eq!(problem.kind(), ApplicationProblemKind::Stale);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(
            problem
                .diagnostic()
                .map(|diagnostic| diagnostic.code.as_str()),
            Some("application.retained.cursor-stale")
        );
        assert_eq!(
            problem.legal_actions(),
            &[LegalAction::RestartWithoutCursor]
        );
    }

    #[test]
    fn runtime_terminal_states_remain_typed() {
        for (error, expected) in [
            (
                RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::BeforeRead),
                ApplicationProblemKind::Cancelled,
            ),
            (
                RetainedSurfaceExecutionErrorV1::TimedOut(CancellationStage::BeforeRead),
                ApplicationProblemKind::TimedOut,
            ),
            (
                RetainedSurfaceExecutionErrorV1::Stale,
                ApplicationProblemKind::Stale,
            ),
            (
                RetainedSurfaceExecutionErrorV1::Unsupported,
                ApplicationProblemKind::Unsupported,
            ),
            (
                RetainedSurfaceExecutionErrorV1::Saturated,
                ApplicationProblemKind::Saturated,
            ),
            (
                RetainedSurfaceExecutionErrorV1::structural_budget_refusal(),
                ApplicationProblemKind::InvalidRequest,
            ),
            (
                RetainedSurfaceExecutionErrorV1::ProfileResetRequired,
                ApplicationProblemKind::ResetRequired,
            ),
            (
                RetainedSurfaceExecutionErrorV1::ProjectResetRequired,
                ApplicationProblemKind::ResetRequired,
            ),
        ] {
            assert_eq!(retained_surface_execution_problem(error).kind(), expected);
        }
    }

    #[test]
    fn reset_required_is_not_retryable_unavailability() {
        let problem = retained_surface_execution_problem(
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired,
        );
        assert_eq!(problem.kind(), ApplicationProblemKind::ResetRequired);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(problem.legal_actions(), &[LegalAction::Reset]);
    }

    #[tokio::test]
    async fn memory_dispatch_preserves_partial_effect_receipt_as_an_admitted_terminal() {
        let operation =
            retained_surface_application_operation(RetainedSurfaceOperation::FactStoreSearch)
                .expect("fact search has a catalog operation");
        let context = context_for(&operation);
        let cancellation = CancellationSignal::active("cancel.retained.fixture")
            .expect("fixture cancellation signal");
        let receipt = partial_receipt(&operation, &context);
        let service = service_for(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.retained.partial-effect".to_owned(),
            committed_receipt: Box::new(receipt.clone()),
            detail: "The lower authority committed before delivery failed.".to_owned(),
        });

        let problem = service
            .execute(&context, &cancellation, UtcMicros(2), &request())
            .await
            .expect_err("partial lower effect must remain a problem terminal");

        assert_eq!(problem.kind(), ApplicationProblemKind::PartialEffect);
        assert_eq!(problem.terminality(), ProblemTerminality::AdmittedTerminal);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(problem.legal_actions(), &[LegalAction::Reconcile]);
        assert_eq!(problem.committed_receipt(), Some(&receipt));
        let envelope = ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            context.request_id().clone(),
            problem,
        )
        .expect("partial-effect envelope is valid");
        envelope
            .problem
            .validate()
            .expect("partial-effect envelope keeps its exact receipt");
        assert_eq!(envelope.problem.committed_receipt.as_ref(), Some(&receipt));
    }

    #[tokio::test]
    async fn memory_dispatch_preserves_reset_required_as_an_admitted_terminal() {
        let operation =
            retained_surface_application_operation(RetainedSurfaceOperation::FactStoreSearch)
                .expect("fact search has a catalog operation");
        let context = context_for(&operation);
        let cancellation = CancellationSignal::active("cancel.retained.fixture")
            .expect("fixture cancellation signal");
        let service = service_for(RetainedSurfaceExecutionErrorV1::ProfileResetRequired);

        let problem = service
            .execute(&context, &cancellation, UtcMicros(2), &request())
            .await
            .expect_err("reset-required lower state must remain a problem terminal");

        assert_eq!(problem.kind(), ApplicationProblemKind::ResetRequired);
        assert_eq!(problem.terminality(), ProblemTerminality::AdmittedTerminal);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(problem.legal_actions(), &[LegalAction::Reset]);
        assert!(problem.committed_receipt().is_none());
        let envelope = ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            context.request_id().clone(),
            problem,
        )
        .expect("reset-required envelope is valid");
        envelope
            .problem
            .validate()
            .expect("reset-required envelope remains a canonical terminal");
        assert!(envelope.problem.committed_receipt.is_none());
    }

    #[test]
    fn operation_effect_authority_matches_the_catalog() {
        for spec in super::super::surface_specs() {
            assert_eq!(
                retained_surface_operation_is_effect(spec.operation),
                spec.effect.is_effect(),
                "{} effect classification diverged from its catalog contract",
                spec.operation.as_str(),
            );
        }
    }

    #[test]
    fn unavailable_problem_carries_the_producer_detail() {
        let problem =
            retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::unavailable(
                "database error: lcm store open failed (operation: lcm_store_open)",
            ));
        assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
        assert_eq!(problem.retry(), RetryDirective::AfterDelay);
        let diagnostic = problem.diagnostic().expect("unavailable diagnostic");
        assert_eq!(
            diagnostic.code,
            "application.retained.authority-unavailable"
        );
        assert_eq!(
            diagnostic.message,
            "The retained operation authority is unavailable: database error: \
             lcm store open failed (operation: lcm_store_open)"
        );
        problem
            .validate()
            .expect("threaded detail must satisfy the SafeDiagnostic contract");
    }

    #[test]
    fn unavailable_detail_is_sanitized_for_the_safe_diagnostic() {
        // Control characters, surrounding whitespace, and oversized text must
        // never invalidate the diagnostic — the caller would then lose the
        // problem entirely instead of just the tail of the detail.
        for detail in [
            "  multi\nline\tcause  ".to_owned(),
            "x".repeat(4_096),
            "é".repeat(1_024),
            String::new(),
            "\n\t".to_owned(),
        ] {
            let problem = retained_surface_execution_problem(
                RetainedSurfaceExecutionErrorV1::unavailable(detail),
            );
            problem
                .validate()
                .expect("every sanitized detail must satisfy the SafeDiagnostic contract");
            let diagnostic = problem.diagnostic().expect("unavailable diagnostic");
            assert!(
                diagnostic
                    .message
                    .starts_with("The retained operation authority is unavailable"),
            );
        }
        let multiline = retained_surface_execution_problem(
            RetainedSurfaceExecutionErrorV1::unavailable("multi\nline\tcause"),
        );
        assert_eq!(
            multiline.diagnostic().expect("diagnostic").message,
            "The retained operation authority is unavailable: multi line cause"
        );
    }

    #[test]
    fn cancellation_after_port_execution_blocks_only_evidence_projection() {
        let signal = CancellationSignal::active("cancellation.retained.after-execution")
            .expect("valid cancellation identity");
        assert!(
            ensure_post_execution_cancellation(RetainedSurfaceOperation::MessageSearch, &signal,)
                .is_ok()
        );
        assert!(signal.cancel(UtcMicros(17)));
        let problem =
            ensure_post_execution_cancellation(RetainedSurfaceOperation::MessageSearch, &signal)
                .expect_err("cancelled lower read cannot project success");
        assert_eq!(problem.kind(), ApplicationProblemKind::Cancelled);
        assert!(
            ensure_post_execution_cancellation(
                RetainedSurfaceOperation::SessionRefreshBegin,
                &signal,
            )
            .is_ok(),
            "effect outcomes must preserve exact receipt and reconciliation state"
        );
    }
}
