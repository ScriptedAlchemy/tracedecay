use super::contract::{
    SessionRetrievalUnavailable, SessionRetrievalUnavailableReason, SessionRetrievalWorkerBlocker,
    SessionRetrievalWorkerRetryClass, SessionRetrievalWorkerStatusView,
};
use tracedecay_usecases::session::{
    SessionProjectionServingState, SessionProjectionServingStatus,
    SessionProjectionServingStatusPort, SessionProjectionStaleReason,
    SessionProjectionUnavailableReason, SessionProjectionWorkerBlocker,
    SessionProjectionWorkerRetryClass,
};

pub(super) fn not_current_unavailable(
    status_port: &dyn SessionProjectionServingStatusPort,
) -> Option<SessionRetrievalUnavailable> {
    let status = status_port.serving_status();
    let reason = match &status.state {
        SessionProjectionServingState::Current => return None,
        SessionProjectionServingState::Stale { reason } => match reason {
            SessionProjectionStaleReason::HistoricalConvergence => {
                SessionRetrievalUnavailableReason::HistoricalConvergence
            }
            SessionProjectionStaleReason::HistoricalRetry { .. } => {
                SessionRetrievalUnavailableReason::HistoricalRetry
            }
            SessionProjectionStaleReason::HistoricalBlocked { .. } => {
                SessionRetrievalUnavailableReason::HistoricalBlocked
            }
        },
        SessionProjectionServingState::Unavailable { reason } => match *reason {
            SessionProjectionUnavailableReason::WorkerMissing => {
                SessionRetrievalUnavailableReason::RefreshWorkerMissing
            }
            SessionProjectionUnavailableReason::WorkerRecovering => {
                SessionRetrievalUnavailableReason::RefreshWorkerRecovering
            }
            SessionProjectionUnavailableReason::WorkerStalled => {
                SessionRetrievalUnavailableReason::RefreshWorkerStalled
            }
            SessionProjectionUnavailableReason::WorkerStopped => {
                SessionRetrievalUnavailableReason::RefreshWorkerStopped
            }
        },
    };
    Some(SessionRetrievalUnavailable {
        reason,
        worker: Some(worker_status(&status)),
    })
}

fn worker_status(status: &SessionProjectionServingStatus) -> SessionRetrievalWorkerStatusView {
    SessionRetrievalWorkerStatusView {
        last_progress_at_unix_micros: status.last_progress_at_unix_micros,
        backlog: status.backlog,
        blocker: status.blocker.map(|blocker| match blocker {
            SessionProjectionWorkerBlocker::WorkerMissing => {
                SessionRetrievalWorkerBlocker::WorkerMissing
            }
            SessionProjectionWorkerBlocker::WorkerPanicked => {
                SessionRetrievalWorkerBlocker::WorkerPanicked
            }
            SessionProjectionWorkerBlocker::WorkerStopped => {
                SessionRetrievalWorkerBlocker::WorkerStopped
            }
            SessionProjectionWorkerBlocker::Storage => SessionRetrievalWorkerBlocker::Storage,
            SessionProjectionWorkerBlocker::Projector => SessionRetrievalWorkerBlocker::Projector,
            SessionProjectionWorkerBlocker::Deadline => SessionRetrievalWorkerBlocker::Deadline,
        }),
        retry_class: status.retry_class.map(|retry_class| match retry_class {
            SessionProjectionWorkerRetryClass::Storage => SessionRetrievalWorkerRetryClass::Storage,
            SessionProjectionWorkerRetryClass::Projector => {
                SessionRetrievalWorkerRetryClass::Projector
            }
            SessionProjectionWorkerRetryClass::Deadline => {
                SessionRetrievalWorkerRetryClass::Deadline
            }
        }),
    }
}

