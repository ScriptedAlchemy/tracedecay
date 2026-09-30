//! Session-projection serving status: current / stale / unavailable, plus the
//! port refresh workers implement so retrieval can surface a typed refusal.

use tracedecay_contracts::{SessionTemporalRefreshWakePort, UnavailableSessionTemporalRefreshWake};

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

/// Whether the worker has settled historical discovery, projection
/// publication, and summary convergence for everything its sources hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionConvergenceState {
    /// A stage still owes committed work, or the worker has not yet observed
    /// that it owes none.
    Converging,
    /// The last pass committed the final owed item and no stage has work
    /// pending.
    Converged,
    /// Every runnable stage settled, but historical discovery is blocked.
    Blocked { reason_code: String },
    /// No worker is serving this store.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConvergenceStatus {
    pub state: SessionConvergenceState,
    /// Times this worker has reached `Converged`; a new value is a new
    /// convergence event.
    pub epoch: u64,
    pub converged_at_unix_micros: Option<i64>,
}

impl SessionConvergenceStatus {
    pub const fn unavailable() -> Self {
        Self {
            state: SessionConvergenceState::Unavailable,
            epoch: 0,
            converged_at_unix_micros: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionProjectionServingStatus {
    pub state: SessionProjectionServingState,
    pub last_progress_at_unix_micros: Option<i64>,
    pub backlog: usize,
    pub blocker: Option<SessionProjectionWorkerBlocker>,
    pub retry_class: Option<SessionProjectionWorkerRetryClass>,
    pub convergence: SessionConvergenceStatus,
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
            convergence: SessionConvergenceStatus::unavailable(),
        }
    }
}

impl SessionProjectionServingStatusPort for UnavailableSessionTemporalRefreshWake {
    fn serving_status(&self) -> SessionProjectionServingStatus {
        RefreshWorkerMissing.serving_status()
    }
}

/// One mounted refresh worker. Wake and serving status are the same object.
/// Do not split them into parallel ports: retrieval and refresh then have to
/// be threaded as two signals and can disagree about whether a worker exists.
pub trait SessionRefreshWorkerPort:
    SessionTemporalRefreshWakePort + SessionProjectionServingStatusPort
{
}

impl<T> SessionRefreshWorkerPort for T where
    T: SessionTemporalRefreshWakePort + SessionProjectionServingStatusPort + ?Sized
{
}
