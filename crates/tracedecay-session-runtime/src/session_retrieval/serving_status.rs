use super::contract::{
    SessionRetrievalUnavailable, SessionRetrievalUnavailableReason, SessionRetrievalWorkerBlocker,
    SessionRetrievalWorkerRetryClass, SessionRetrievalWorkerStatusView,
};
use tracedecay_sessions::serving::{
    SessionProjectionServingState, SessionProjectionServingStatus,
    SessionProjectionServingStatusPort, SessionProjectionStaleReason,
    SessionProjectionUnavailableReason, SessionProjectionWorkerBlocker,
    SessionProjectionWorkerRetryClass,
};

pub(super) fn not_current_unavailable(
    status_port: &dyn SessionProjectionServingStatusPort,
) -> Option<SessionRetrievalUnavailable> {
    let status = status_port.serving_status();
    // Each refusal reason increments its own static counter so a profile can
    // separate historical-convergence staleness from refresh-worker outages
    // without ever recording session or project identity.
    let reason = match &status.state {
        SessionProjectionServingState::Current => return None,
        SessionProjectionServingState::Stale { reason } => match reason {
            SessionProjectionStaleReason::HistoricalConvergence => {
                hotpath::gauge!("daemon.session_retrieval.stale.historical_convergence").inc(1.0);
                SessionRetrievalUnavailableReason::HistoricalConvergence
            }
            SessionProjectionStaleReason::HistoricalRetry { .. } => {
                hotpath::gauge!("daemon.session_retrieval.stale.historical_retry").inc(1.0);
                SessionRetrievalUnavailableReason::HistoricalRetry
            }
            SessionProjectionStaleReason::HistoricalBlocked { .. } => {
                hotpath::gauge!("daemon.session_retrieval.stale.historical_blocked").inc(1.0);
                SessionRetrievalUnavailableReason::HistoricalBlocked
            }
        },
        SessionProjectionServingState::Unavailable { reason } => match *reason {
            SessionProjectionUnavailableReason::WorkerMissing => {
                hotpath::gauge!("daemon.session_retrieval.refused.worker_missing").inc(1.0);
                SessionRetrievalUnavailableReason::RefreshWorkerMissing
            }
            SessionProjectionUnavailableReason::WorkerRecovering => {
                hotpath::gauge!("daemon.session_retrieval.refused.worker_recovering").inc(1.0);
                SessionRetrievalUnavailableReason::RefreshWorkerRecovering
            }
            SessionProjectionUnavailableReason::WorkerStalled => {
                hotpath::gauge!("daemon.session_retrieval.refused.worker_stalled").inc(1.0);
                SessionRetrievalUnavailableReason::RefreshWorkerStalled
            }
            SessionProjectionUnavailableReason::WorkerStopped => {
                hotpath::gauge!("daemon.session_retrieval.refused.worker_stopped").inc(1.0);
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
