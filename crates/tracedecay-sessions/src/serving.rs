//! Session-projection serving status: current / stale / unavailable, plus the
//! port refresh workers implement so retrieval can surface a typed refusal.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionProjectionServingState {
    Current,
    Stale {
        reason: SessionProjectionStaleReason,
    },
    Unavailable {
        reason: SessionProjectionUnavailableReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionProjectionStaleReason {
    HistoricalConvergence,
    HistoricalRetry { reason_code: String },
    HistoricalBlocked { reason_code: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionProjectionUnavailableReason {
    WorkerMissing,
    WorkerRecovering,
    WorkerStalled,
    WorkerStopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionProjectionWorkerBlocker {
    WorkerMissing,
    WorkerPanicked,
    WorkerStopped,
    Storage,
    Projector,
    Deadline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionProjectionWorkerRetryClass {
    Storage,
    Projector,
    Deadline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionProjectionServingStatus {
    pub state: SessionProjectionServingState,
    pub last_progress_at_unix_micros: Option<i64>,
    pub backlog: usize,
    pub blocker: Option<SessionProjectionWorkerBlocker>,
    pub retry_class: Option<SessionProjectionWorkerRetryClass>,
}

pub trait SessionProjectionServingStatusPort: Send + Sync {
    fn serving_status(&self) -> SessionProjectionServingStatus;
}

/// The serving status of a store no refresh worker is mounted for.
///
/// A retrieval service that has no worker cannot know whether its projection
/// is current, so it reports that as the typed `WorkerMissing` state rather
/// than carrying the worker as an `Option` and reading its absence as fresh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefreshWorkerMissing;

impl SessionProjectionServingStatusPort for RefreshWorkerMissing {
    fn serving_status(&self) -> SessionProjectionServingStatus {
        SessionProjectionServingStatus {
            state: SessionProjectionServingState::Unavailable {
                reason: SessionProjectionUnavailableReason::WorkerMissing,
            },
            last_progress_at_unix_micros: None,
            backlog: 0,
            blocker: Some(SessionProjectionWorkerBlocker::WorkerMissing),
            retry_class: None,
        }
    }
}
