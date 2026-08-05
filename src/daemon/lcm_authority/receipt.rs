use tracedecay_application::{
    CancellationObservation, CancellationStage, OperationBudgetUsage, OperationReceipt,
    OperationTermination, RequestContext,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_usecases::context::{RequestInterruption, application_observed_at};
use tracedecay_usecases::session::lcm::{
    LcmAuthorityOperation, LcmAuthorityOutcome, LcmAuthorityPayload, LcmAuthorityReceipt,
    LcmAuthorityResponse, LcmAuthorityUnavailableReason,
};

pub(super) fn unavailable(
    context: &RequestContext,
    operation: LcmAuthorityOperation,
    started_at: UtcMicros,
    reason: LcmAuthorityUnavailableReason,
) -> LcmAuthorityResponse {
    terminal(
        context,
        operation,
        started_at,
        LcmAuthorityOutcome::Unavailable { reason },
        OperationTermination::Unavailable,
        None,
        None,
        None,
    )
}

pub(super) fn unavailable_with_state(
    context: &RequestContext,
    operation: LcmAuthorityOperation,
    started_at: UtcMicros,
    reason: LcmAuthorityUnavailableReason,
    committed_state: ManifestDigest,
) -> LcmAuthorityResponse {
    terminal(
        context,
        operation,
        started_at,
        LcmAuthorityOutcome::Unavailable { reason },
        OperationTermination::Unavailable,
        Some(committed_state),
        None,
        None,
    )
}

pub(super) fn terminal_failure(
    context: &RequestContext,
    operation: LcmAuthorityOperation,
    started_at: UtcMicros,
    diagnostic: &'static str,
) -> LcmAuthorityResponse {
    terminal(
        context,
        operation,
        started_at,
        LcmAuthorityOutcome::Failed {
            diagnostic: diagnostic.to_owned(),
        },
        OperationTermination::Failed,
        None,
        None,
        None,
    )
}

pub(super) fn terminal_interruption(
    context: &RequestContext,
    operation: LcmAuthorityOperation,
    started_at: UtcMicros,
    interruption: RequestInterruption,
    stage: CancellationStage,
    committed_state: Option<ManifestDigest>,
) -> LcmAuthorityResponse {
    let (outcome, termination) = match interruption {
        RequestInterruption::Cancelled => (
            LcmAuthorityOutcome::Cancelled,
            OperationTermination::Cancelled,
        ),
        RequestInterruption::DeadlineExceeded => (
            LcmAuthorityOutcome::TimedOut,
            OperationTermination::TimedOut,
        ),
    };
    terminal(
        context,
        operation,
        started_at,
        outcome,
        termination,
        committed_state,
        None,
        Some(stage),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn terminal(
    context: &RequestContext,
    operation: LcmAuthorityOperation,
    started_at: UtcMicros,
    outcome: LcmAuthorityOutcome,
    termination: OperationTermination,
    committed_state: Option<ManifestDigest>,
    payload: Option<LcmAuthorityPayload>,
    cancellation_stage: Option<CancellationStage>,
) -> LcmAuthorityResponse {
    let ended_at = application_observed_at().max(started_at);
    let cancellation = cancellation_stage.map(|stage| CancellationObservation {
        stage,
        observed_at: ended_at,
    });
    let execution = OperationReceipt {
        started_at,
        ended_at,
        effective_deadline: context.deadline().clone(),
        cancellation,
        budget: OperationBudgetUsage::default(),
        termination,
    };
    LcmAuthorityResponse {
        outcome,
        receipt: LcmAuthorityReceipt {
            request_id: context.request_id().clone(),
            operation,
            grant_id: context.grant().grant_id.clone(),
            grant_revision: context.grant().revision,
            grant_digest: context.grant().digest.clone(),
            authorized_scope_digest: context.scope().scope_digest.clone(),
            cancellation_token_id: context.cancellation().token_id.clone(),
            committed_state,
            execution,
        },
        payload,
    }
}
