//! Exact effect receipts for mounted session-refresh operations.

use tracedecay_application::retained_surfaces::{
    RetainedOutcomeStatusV1, RetainedSurfaceResultV1, SessionRefreshBeginResultV1,
    SessionRefreshCancelResultV1, SessionRefreshReceiptV1, SessionRefreshTerminalStateResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, EffectId, EffectReceipt, EffectResult, EffectTermination, IdempotencyKey,
    ReconciliationState, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    now_micros,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_usecases::session::{
    SessionRefreshCancelDispositionKind, SessionRefreshHandle, SessionRefreshOutcome,
};

use super::receipts::{authority_receipt, measured_budget};
use super::session_refresh::RetainedSessionRefreshExecutionV1;

pub(super) fn session_refresh_effect_outcome(
    configuration_digest: &ManifestDigest,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    result: RetainedSurfaceResultV1,
    refresh: RetainedSessionRefreshExecutionV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let handle = refresh
        .exact_handle
        .as_ref()
        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let (committed_state, effect_kind) = match (&result, &refresh.exact) {
        (
            RetainedSurfaceResultV1::SessionRefreshBegin(public),
            SessionRefreshOutcome::Started(exact),
        )
        | (
            RetainedSurfaceResultV1::SessionRefreshBegin(public),
            SessionRefreshOutcome::Joined(exact),
        ) => {
            ensure_begin_projection(public, exact)?;
            if handle != exact {
                return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
            }
            (
                canonical_sha256(&(
                    "tracedecay.application.retained.session-refresh.begin.v1",
                    exact.operation_id().as_str(),
                    exact.accepted_at().0,
                    exact.join_digest().as_bytes(),
                ))
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
                SessionRefreshEffectKind::Begin,
            )
        }
        (
            RetainedSurfaceResultV1::SessionRefreshCancel(public),
            SessionRefreshOutcome::Cancelled(exact),
        ) => {
            ensure_cancel_projection(public, exact)?;
            if exact.operation_id() != handle.operation_id()
                || exact.session_id() != handle.target().session_id()
            {
                return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
            }
            accepted_cancel_disposition(refresh.cancel_disposition)?;
            (
                canonical_sha256(&(
                    "tracedecay.application.retained.session-refresh.cancel.v1",
                    exact.operation_id().as_str(),
                    exact.session_id().as_str(),
                    exact.frontier().observed_through(),
                    exact.frontier().committed_through(),
                    exact.coverage(),
                    exact.terminal_at().0,
                ))
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
                SessionRefreshEffectKind::Cancel,
            )
        }
        (RetainedSurfaceResultV1::SessionRefreshCancel(_), SessionRefreshOutcome::Complete(_))
        | (RetainedSurfaceResultV1::SessionRefreshCancel(_), SessionRefreshOutcome::Failed(_)) => {
            return Err(RetainedSurfaceExecutionErrorV1::Conflict);
        }
        _ => return Err(RetainedSurfaceExecutionErrorV1::Unavailable),
    };
    let input_digest = match effect_kind {
        SessionRefreshEffectKind::Begin => {
            refresh_digest(handle.caller_idempotency_digest().as_bytes())?
        }
        SessionRefreshEffectKind::Cancel => canonical_sha256(&(
            "tracedecay.application.retained.session-refresh.cancel.input.v1",
            handle.operation_id().as_str(),
            handle.caller_idempotency_digest().as_bytes(),
            handle.join_digest().as_bytes(),
        ))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
    };
    let expected_state = refresh_digest(handle.join_digest().as_bytes())?;
    let idempotency_digest = canonical_sha256(&(
        "tracedecay.application.retained.session-refresh.effect-idempotency.v1",
        effect_kind.as_str(),
        handle.operation_id().as_str(),
        handle.caller_idempotency_digest().as_bytes(),
        handle.join_digest().as_bytes(),
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let idempotency_key = IdempotencyKey::new(idempotency_digest.as_str().to_owned())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect_id = EffectId::new(format!(
        "effect.retained.session-refresh.{}.{}",
        effect_kind.as_str(),
        handle.operation_id().as_str(),
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let finished_at = now_micros();
    let authority = authority_receipt(context, finished_at)?;
    let catalog = tracedecay_application::retained_surface_catalog_contribution()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect_class = catalog
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == context.operation.capability_id())
        .map(|manifest| manifest.effect())
        .filter(|effect| effect.is_effect())
        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let catalog_digest =
        canonical_sha256(&catalog).map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.application.retained-surface.privacy.v1",
        &context.request_context.grant().grant_id,
        &context.request_context.grant().digest,
        context.request_context.grant().disclosure,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let receipt = EffectReceipt {
        operation: context.operation.use_case_id().clone(),
        request_id: context.request_context.request_id().clone(),
        actor: context.request_context.actor().clone(),
        scope: context.request_context.scope().clone(),
        effect_class,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let execution = tracedecay_application::OperationReceipt::completed(
        context.observed_at,
        finished_at,
        context.request_context.deadline().clone(),
        measured_budget(context.observed_at, finished_at, &result)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let effect = EffectResult::new(
        effect_id,
        effect_class,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    Ok(ApplicationOutcome::Effect(effect))
}

#[derive(Clone, Copy)]
enum SessionRefreshEffectKind {
    Begin,
    Cancel,
}

impl SessionRefreshEffectKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Cancel => "cancel",
        }
    }
}

fn ensure_begin_projection(
    public: &SessionRefreshBeginResultV1,
    exact: &SessionRefreshHandle,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    (matches!(
        public.outcome,
        RetainedOutcomeStatusV1::Started | RetainedOutcomeStatusV1::Joined
    ) && public.operation_id.as_deref() == Some(exact.operation_id().as_str())
        && public
            .handle
            .as_deref()
            .is_some_and(|handle| !handle.is_empty())
        && public.accepted_at == Some(exact.accepted_at().0)
        && public.progress.is_none()
        && public.receipt.is_none()
        && public.error.is_none())
    .then_some(())
    .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)
}

fn ensure_cancel_projection(
    public: &SessionRefreshCancelResultV1,
    exact: &tracedecay_store::SessionRefreshReceiptV1,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    let state = match exact.state() {
        tracedecay_store::SessionRefreshTerminalStateV1::Complete => {
            SessionRefreshTerminalStateResultV1::Complete
        }
        tracedecay_store::SessionRefreshTerminalStateV1::Failed => {
            SessionRefreshTerminalStateResultV1::Failed
        }
        tracedecay_store::SessionRefreshTerminalStateV1::Cancelled => {
            SessionRefreshTerminalStateResultV1::Cancelled
        }
    };
    let receipt = public
        .receipt
        .as_ref()
        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    (public.outcome == RetainedOutcomeStatusV1::Cancelled
        && receipt.operation_id == exact.operation_id().as_str()
        && receipt.session_id == exact.session_id().as_str()
        && receipt.frontier.observed_through == exact.frontier().observed_through()
        && receipt.frontier.committed_through == exact.frontier().committed_through()
        && receipt.coverage.visible == exact.coverage().visible
        && receipt.coverage.hidden == exact.coverage().hidden
        && receipt.coverage.unknown == exact.coverage().unknown
        && receipt.coverage.redacted == exact.coverage().redacted
        && source_coverage_matches(receipt, exact)
        && receipt.state == state
        && receipt.failure_code == exact.failure_code().map(|code| code.as_str().to_owned())
        && receipt.terminal_at == exact.terminal_at().0
        && public.accepted_at.is_none()
        && public.handle.is_none()
        && public.operation_id.is_none()
        && public.progress.is_none()
        && public.error.is_none())
    .then_some(())
    .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)
}

fn accepted_cancel_disposition(
    disposition: Option<SessionRefreshCancelDispositionKind>,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    match disposition {
        Some(
            SessionRefreshCancelDispositionKind::Committed
            | SessionRefreshCancelDispositionKind::IdempotentReplay,
        ) => Ok(()),
        Some(SessionRefreshCancelDispositionKind::Reconciled) | None => {
            Err(RetainedSurfaceExecutionErrorV1::Conflict)
        }
    }
}

fn refresh_digest(bytes: &[u8; 32]) -> Result<ManifestDigest, RetainedSurfaceExecutionErrorV1> {
    ManifestDigest::new(format!("sha256:{}", hex::encode(bytes)))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

fn source_coverage_matches(
    public: &SessionRefreshReceiptV1,
    exact: &tracedecay_store::SessionRefreshReceiptV1,
) -> bool {
    match exact.source_coverage() {
        Some(coverage) => {
            public.source_coverage
                == coverage
                    .sources()
                    .iter()
                    .cloned()
                    .map(super::session::source_coverage)
                    .collect::<Vec<_>>()
        }
        None => public.source_coverage.is_empty(),
    }
}
