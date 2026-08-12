//! Typed retained projections for daemon-owned session refresh outcomes.

use tracedecay_application::retained_surfaces::{
    RetainedErrorV1, RetainedOutcomeStatusV1, RetainedSurfaceResultV1, SessionRefreshBeginResultV1,
    SessionRefreshCancelResultV1, SessionRefreshProgressV1, SessionRefreshReceiptV1,
    SessionRefreshStatusResultV1,
};
use tracedecay_application::{CancellationStage, RetainedSurfaceExecutionErrorV1};

use crate::mcp::tools::{
    SessionRefreshProgressView, SessionRefreshReceiptView, SessionRefreshServiceOutcome,
};

pub(super) fn status_result(
    outcome: SessionRefreshServiceOutcome,
) -> Result<RetainedSurfaceResultV1, RetainedSurfaceExecutionErrorV1> {
    let (outcome, progress, receipt, error) = match outcome {
        SessionRefreshServiceOutcome::Running(progress) => (
            RetainedOutcomeStatusV1::Running,
            progress.map(refresh_progress).transpose()?,
            None,
            None,
        ),
        SessionRefreshServiceOutcome::Complete(receipt) => (
            RetainedOutcomeStatusV1::Complete,
            None,
            Some(refresh_receipt(receipt)?),
            None,
        ),
        SessionRefreshServiceOutcome::Failed(receipt) => (
            RetainedOutcomeStatusV1::Failed,
            None,
            Some(refresh_receipt(receipt)?),
            Some(refresh_error(
                "refresh_failed",
                "the durable session refresh failed",
            )),
        ),
        SessionRefreshServiceOutcome::Cancelled(receipt) => (
            RetainedOutcomeStatusV1::Cancelled,
            None,
            Some(refresh_receipt(receipt)?),
            None,
        ),
        SessionRefreshServiceOutcome::Denied => refresh_problem(
            RetainedOutcomeStatusV1::Denied,
            "refresh_denied",
            "the caller is not authorized for this session refresh",
        ),
        SessionRefreshServiceOutcome::WrongScope => refresh_problem(
            RetainedOutcomeStatusV1::WrongScope,
            "refresh_wrong_scope",
            "the refresh handle does not belong to the requested scope",
        ),
        SessionRefreshServiceOutcome::Stale => refresh_problem(
            RetainedOutcomeStatusV1::Stale,
            "refresh_handle_stale",
            "the refresh handle is no longer current",
        ),
        SessionRefreshServiceOutcome::NotFound => refresh_problem(
            RetainedOutcomeStatusV1::NotFound,
            "refresh_handle_not_found",
            "the refresh handle was not found",
        ),
        SessionRefreshServiceOutcome::Aborted => refresh_problem(
            RetainedOutcomeStatusV1::Aborted,
            "refresh_aborted",
            "the session refresh request was aborted",
        ),
        SessionRefreshServiceOutcome::DeadlineExceeded => refresh_problem(
            RetainedOutcomeStatusV1::DeadlineExceeded,
            "refresh_deadline_exceeded",
            "the session refresh request deadline was exceeded",
        ),
        SessionRefreshServiceOutcome::Unavailable => refresh_problem(
            RetainedOutcomeStatusV1::Unavailable,
            "refresh_service_unavailable",
            "the daemon-owned session refresh service is unavailable",
        ),
        SessionRefreshServiceOutcome::Busy => refresh_problem(
            RetainedOutcomeStatusV1::Busy,
            "refresh_busy",
            "a conflicting refresh target is already running",
        ),
        SessionRefreshServiceOutcome::Started { .. }
        | SessionRefreshServiceOutcome::StartedReconciliationRequired { .. }
        | SessionRefreshServiceOutcome::Joined { .. }
        | SessionRefreshServiceOutcome::JoinedReconciliationRequired { .. }
        | SessionRefreshServiceOutcome::CancelledReconciliationRequired(_) => refresh_problem(
            RetainedOutcomeStatusV1::Unavailable,
            "refresh_contract_violation",
            "the refresh status authority returned a begin outcome",
        ),
    };
    Ok(RetainedSurfaceResultV1::SessionRefreshStatus(
        SessionRefreshStatusResultV1 {
            outcome,
            scope: "project".to_owned(),
            tool: "tracedecay_session_refresh".to_owned(),
            progress,
            receipt,
            error,
        },
    ))
}

pub(super) struct EffectProjection {
    pub(super) operation_id: String,
    pub(super) result: RetainedSurfaceResultV1,
    pub(super) reconciliation_required: bool,
}

pub(super) fn begin_result(
    outcome: SessionRefreshServiceOutcome,
) -> Result<EffectProjection, RetainedSurfaceExecutionErrorV1> {
    let (outcome, operation_id, handle, accepted_at, reconciliation_required) = match outcome {
        SessionRefreshServiceOutcome::Started {
            operation_id,
            handle,
            accepted_at,
        } => (
            RetainedOutcomeStatusV1::Started,
            operation_id,
            handle,
            accepted_at,
            false,
        ),
        SessionRefreshServiceOutcome::StartedReconciliationRequired {
            operation_id,
            handle,
            accepted_at,
        } => (
            RetainedOutcomeStatusV1::Started,
            operation_id,
            handle,
            accepted_at,
            true,
        ),
        SessionRefreshServiceOutcome::Joined {
            operation_id,
            handle,
            accepted_at,
        } => (
            RetainedOutcomeStatusV1::Joined,
            operation_id,
            handle,
            accepted_at,
            false,
        ),
        SessionRefreshServiceOutcome::JoinedReconciliationRequired {
            operation_id,
            handle,
            accepted_at,
        } => (
            RetainedOutcomeStatusV1::Joined,
            operation_id,
            handle,
            accepted_at,
            true,
        ),
        outcome => return Err(effect_error(outcome)),
    };
    let result = RetainedSurfaceResultV1::SessionRefreshBegin(SessionRefreshBeginResultV1 {
        outcome,
        scope: "project".to_owned(),
        tool: "tracedecay_session_refresh".to_owned(),
        accepted_at: Some(accepted_at),
        handle: Some(handle),
        operation_id: Some(operation_id.clone()),
        progress: None,
        receipt: None,
        error: None,
    });
    Ok(EffectProjection {
        operation_id,
        result,
        reconciliation_required,
    })
}

pub(super) fn cancel_result(
    outcome: SessionRefreshServiceOutcome,
    handle: Option<&str>,
) -> Result<EffectProjection, RetainedSurfaceExecutionErrorV1> {
    let (outcome, receipt, error, reconciliation_required) = match outcome {
        SessionRefreshServiceOutcome::Cancelled(receipt) => {
            (RetainedOutcomeStatusV1::Cancelled, receipt, None, false)
        }
        SessionRefreshServiceOutcome::CancelledReconciliationRequired(receipt) => {
            (RetainedOutcomeStatusV1::Cancelled, receipt, None, true)
        }
        SessionRefreshServiceOutcome::Complete(receipt) => {
            (RetainedOutcomeStatusV1::Complete, receipt, None, false)
        }
        SessionRefreshServiceOutcome::Failed(receipt) => (
            RetainedOutcomeStatusV1::Failed,
            receipt,
            Some(refresh_error(
                "refresh_failed",
                "the durable session refresh had already failed",
            )),
            false,
        ),
        outcome => return Err(effect_error(outcome)),
    };
    let operation_id = receipt.operation_id.clone();
    let result = RetainedSurfaceResultV1::SessionRefreshCancel(SessionRefreshCancelResultV1 {
        outcome,
        scope: "project".to_owned(),
        tool: "tracedecay_session_refresh".to_owned(),
        accepted_at: None,
        handle: handle.map(ToOwned::to_owned),
        operation_id: Some(operation_id.clone()),
        progress: None,
        receipt: Some(refresh_receipt(receipt)?),
        error,
    });
    Ok(EffectProjection {
        operation_id,
        result,
        reconciliation_required,
    })
}

fn effect_error(outcome: SessionRefreshServiceOutcome) -> RetainedSurfaceExecutionErrorV1 {
    match outcome {
        SessionRefreshServiceOutcome::Busy => RetainedSurfaceExecutionErrorV1::Saturated,
        SessionRefreshServiceOutcome::Denied
        | SessionRefreshServiceOutcome::WrongScope
        | SessionRefreshServiceOutcome::NotFound => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        SessionRefreshServiceOutcome::Stale => RetainedSurfaceExecutionErrorV1::Stale,
        SessionRefreshServiceOutcome::Aborted => {
            RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::EffectInFlight)
        }
        SessionRefreshServiceOutcome::DeadlineExceeded => {
            RetainedSurfaceExecutionErrorV1::TimedOut(CancellationStage::EffectInFlight)
        }
        SessionRefreshServiceOutcome::Running(_) => RetainedSurfaceExecutionErrorV1::Conflict,
        SessionRefreshServiceOutcome::Unavailable
        | SessionRefreshServiceOutcome::Complete(_)
        | SessionRefreshServiceOutcome::Failed(_)
        | SessionRefreshServiceOutcome::Cancelled(_)
        | SessionRefreshServiceOutcome::CancelledReconciliationRequired(_)
        | SessionRefreshServiceOutcome::Started { .. }
        | SessionRefreshServiceOutcome::StartedReconciliationRequired { .. }
        | SessionRefreshServiceOutcome::Joined { .. }
        | SessionRefreshServiceOutcome::JoinedReconciliationRequired { .. } => {
            RetainedSurfaceExecutionErrorV1::Unavailable
        }
    }
}

fn refresh_problem(
    outcome: RetainedOutcomeStatusV1,
    code: &str,
    message: &str,
) -> (
    RetainedOutcomeStatusV1,
    Option<SessionRefreshProgressV1>,
    Option<SessionRefreshReceiptV1>,
    Option<RetainedErrorV1>,
) {
    (outcome, None, None, Some(refresh_error(code, message)))
}

fn refresh_error(code: &str, message: &str) -> RetainedErrorV1 {
    RetainedErrorV1 {
        code: code.to_owned(),
        message: message.to_owned(),
        kind: None,
        maximum: None,
        observed: None,
        reason: None,
        retryable: None,
    }
}

fn refresh_progress(
    value: SessionRefreshProgressView,
) -> Result<SessionRefreshProgressV1, RetainedSurfaceExecutionErrorV1> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

fn refresh_receipt(
    value: SessionRefreshReceiptView,
) -> Result<SessionRefreshReceiptV1, RetainedSurfaceExecutionErrorV1> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_projects_the_durable_operation_and_closed_handle() {
        let projected = begin_result(SessionRefreshServiceOutcome::Started {
            operation_id: "refresh.operation.fixture".to_owned(),
            handle: "srh_fixture".to_owned(),
            accepted_at: 42,
        })
        .expect("begin projection");

        assert_eq!(projected.operation_id, "refresh.operation.fixture");
        assert!(!projected.reconciliation_required);
        let RetainedSurfaceResultV1::SessionRefreshBegin(result) = projected.result else {
            panic!("expected begin result");
        };
        assert_eq!(result.outcome, RetainedOutcomeStatusV1::Started);
        assert_eq!(result.handle.as_deref(), Some("srh_fixture"));
        assert_eq!(result.accepted_at, Some(42));
    }

    #[test]
    fn begin_preserves_committed_delivery_failure_for_reconciliation() {
        let projected = begin_result(
            SessionRefreshServiceOutcome::StartedReconciliationRequired {
                operation_id: "refresh.operation.fixture".to_owned(),
                handle: "srh_fixture".to_owned(),
                accepted_at: 42,
            },
        )
        .expect("begin projection");

        assert!(projected.reconciliation_required);
        assert_eq!(projected.operation_id, "refresh.operation.fixture");
    }

    #[test]
    fn a_running_cancel_is_a_conflict_not_a_fabricated_effect() {
        let error = cancel_result(
            SessionRefreshServiceOutcome::Running(None),
            Some("srh_fixture"),
        )
        .err()
        .expect("running cancel must not fabricate a receipt");
        assert_eq!(error, RetainedSurfaceExecutionErrorV1::Conflict);
    }
}
