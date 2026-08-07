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
