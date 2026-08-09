//! Application-owned execution boundary for retained memory and temporal operations.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_domain::UtcMicros;

use super::{
    FactFeedbackRequestV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreGetRequestV1, FactStoreListRequestV1, FactStoreProbeRequestV1,
    FactStoreReasonRequestV1, FactStoreRelatedRequestV1, FactStoreRemoveRequestV1,
    FactStoreSearchRequestV1, FactStoreUpdateRequestV1, LcmDescribeRequestV1, LcmDoctorRequestV1,
    LcmExpandQueryRequestV1, LcmExpandRequestV1, LcmGrepRequestV1, LcmLoadSessionRequestV1,
    LcmStatusRequestV1, MemoryStatusRequestV1, MessageSearchRequestV1, RetainedSurfaceOperation,
    RetainedSurfaceRequestV1, RetainedSurfaceResultV1, SessionRefreshRequestV1,
    SessionsForRequestV1, WorkflowsRequestV1, retained_surface_application_operation,
};
use crate::{
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, CancellationSignal,
    EffectReceipt, LegalAction, RequestAdmission, RequestContext, RetryDirective, SafeDiagnostic,
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
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    PartialEffect {
        reason_code: String,
        committed_receipt: EffectReceipt,
        detail: String,
    },
    Stale,
    Unsupported,
    Saturated,
    Unavailable,
    ProfileResetRequired,
    ProjectResetRequired,
    Cancelled,
    TimedOut,
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
    FactStoreList(&'a FactStoreListRequestV1),
    FactFeedback(&'a FactFeedbackRequestV1),
    MemoryStatus(&'a MemoryStatusRequestV1),
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
    memory: Option<Arc<dyn RetainedMemoryExecutionPortV1 + 'a>>,
    session: Option<Arc<dyn RetainedSessionExecutionPortV1 + 'a>>,
    lcm: Option<Arc<dyn RetainedLcmExecutionPortV1 + 'a>>,
}

impl<'a> RetainedSurfacePortsV1<'a> {
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
        let execution_context = || RetainedSurfaceExecutionContextV1 {
            request_context: context,
            cancellation_signal: cancellation,
            operation: &operation,
            observed_at,
        };
        let outcome = async {
            match request {
                RetainedSurfaceRequestV1::FactStoreAdd(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreAdd(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreSearch(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreSearch(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreProbe(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreProbe(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreRelated(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreRelated(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreReason(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreReason(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreContradict(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreContradict(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreGet(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreGet(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreUpdate(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreUpdate(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreRemove(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreRemove(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactStoreList(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactStoreList(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::FactFeedback(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::FactFeedback(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::MemoryStatus(request) => {
                    self.ports
                        .memory
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_memory(
                            execution_context(),
                            RetainedMemoryRequestV1::MemoryStatus(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::SessionRefresh(request) => {
                    self.ports
                        .session
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_session(
                            execution_context(),
                            RetainedSessionRequestV1::SessionRefresh(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::MessageSearch(request) => {
                    self.ports
                        .session
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_session(
                            execution_context(),
                            RetainedSessionRequestV1::MessageSearch(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::SessionsFor(request) => {
                    self.ports
                        .session
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_session(
                            execution_context(),
                            RetainedSessionRequestV1::SessionsFor(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::Workflows(request) => {
                    self.ports
                        .session
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_session(
                            execution_context(),
                            RetainedSessionRequestV1::Workflows(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::LcmStatus(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(execution_context(), RetainedLcmRequestV1::Status(request))
                        .await
                }
                RetainedSurfaceRequestV1::LcmDoctor(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(execution_context(), RetainedLcmRequestV1::Doctor(request))
                        .await
                }
                RetainedSurfaceRequestV1::LcmLoadSession(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(
                            execution_context(),
                            RetainedLcmRequestV1::LoadSession(request),
                        )
                        .await
                }
                RetainedSurfaceRequestV1::LcmGrep(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(execution_context(), RetainedLcmRequestV1::Grep(request))
                        .await
                }
                RetainedSurfaceRequestV1::LcmDescribe(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(execution_context(), RetainedLcmRequestV1::Describe(request))
                        .await
                }
                RetainedSurfaceRequestV1::LcmExpand(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(execution_context(), RetainedLcmRequestV1::Expand(request))
                        .await
                }
                RetainedSurfaceRequestV1::LcmExpandQuery(request) => {
                    self.ports
                        .lcm
                        .as_ref()
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?
                        .execute_lcm(
                            execution_context(),
                            RetainedLcmRequestV1::ExpandQuery(request),
                        )
                        .await
                }
            }
        }
        .await
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

fn ensure_post_execution_cancellation(
    operation: RetainedSurfaceOperation,
    cancellation: &CancellationSignal,
) -> Result<(), ApplicationProblem> {
    if !retained_surface_operation_is_effect(operation) && cancellation.is_cancelled() {
        Err(retained_surface_execution_problem(
            RetainedSurfaceExecutionErrorV1::Cancelled,
        ))
    } else {
        Ok(())
    }
}

fn outcome_matches_operation(
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
        RetainedSurfaceOperation::FactStoreAdd
            | RetainedSurfaceOperation::FactStoreUpdate
            | RetainedSurfaceOperation::FactStoreRemove
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
        RetainedSurfaceExecutionErrorV1::Unavailable => unavailable_problem(
            "application.retained.authority-unavailable",
            "The retained operation authority is unavailable.",
        ),
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
        RetainedSurfaceExecutionErrorV1::Cancelled => {
            ApplicationProblem::cancelled_before_admission()
        }
        RetainedSurfaceExecutionErrorV1::TimedOut => {
            ApplicationProblem::timed_out_before_admission()
        }
    }
}

fn unavailable_problem(code: &'static str, message: &'static str) -> ApplicationProblem {
    ApplicationProblem::Unavailable {
        diagnostic: diagnostic(code, message),
        retry: RetryDirective::AfterDelay,
        legal_actions: vec![LegalAction::Retry],
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
    use super::*;
    use crate::ApplicationProblemKind;

    #[test]
    fn runtime_terminal_states_remain_typed() {
        for (error, expected) in [
            (
                RetainedSurfaceExecutionErrorV1::Cancelled,
                ApplicationProblemKind::Cancelled,
            ),
            (
                RetainedSurfaceExecutionErrorV1::TimedOut,
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
