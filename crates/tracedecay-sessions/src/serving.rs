//! Session-projection serving status: current / stale / unavailable, plus the
//! port refresh workers implement so retrieval can surface a typed refusal.

use std::collections::BTreeMap;

use tracedecay_contracts::{SessionTemporalRefreshWakePort, UnavailableSessionTemporalRefreshWake};

use crate::ProviderPassUnits;

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

/// Consecutive historical passes that leave work behind and commit nothing
/// before catch-up reads as stalled instead of slow.
pub const SESSION_HISTORY_STALLED_PASSES: u64 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionHistoryProgressState {
    /// The worker has not run a historical pass since it mounted.
    AwaitingFirstPass,
    /// The latest passes committed work or finished the backlog.
    Advancing,
    /// [`SESSION_HISTORY_STALLED_PASSES`] or more consecutive passes left work
    /// behind without committing any.
    Stalled,
}

/// Historical catch-up progress the refresh worker accumulates over the
/// passes it ran since it mounted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionHistoryCatchUpProgress {
    pub passes: u64,
    /// Consecutive passes, ending with the latest, that left work for a later
    /// pass and committed nothing. A pass that commits work or completes the
    /// backlog resets it.
    pub passes_without_progress: u64,
    pub last_progress_at_unix_micros: Option<i64>,
    /// Unit dispositions summed per provider over every pass.
    pub provider_units: BTreeMap<&'static str, ProviderPassUnits>,
}

impl SessionHistoryCatchUpProgress {
    pub fn record_pass(
        &mut self,
        complete: bool,
        made_progress: bool,
        provider_units: &BTreeMap<&'static str, ProviderPassUnits>,
        now_unix_micros: i64,
    ) {
        self.passes = self.passes.saturating_add(1);
        if made_progress {
            self.last_progress_at_unix_micros = Some(now_unix_micros);
        }
        self.passes_without_progress = if made_progress || complete {
            0
        } else {
            self.passes_without_progress.saturating_add(1)
        };
        for (provider, units) in provider_units {
            let total = self.provider_units.entry(provider).or_default();
            *total = total.merge(*units);
        }
    }

    pub const fn state(&self) -> SessionHistoryProgressState {
        if self.passes == 0 {
            SessionHistoryProgressState::AwaitingFirstPass
        } else if self.passes_without_progress >= SESSION_HISTORY_STALLED_PASSES {
            SessionHistoryProgressState::Stalled
        } else {
            SessionHistoryProgressState::Advancing
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
    /// Historical catch-up progress of the mounted worker; absent when no
    /// worker is mounted, so status never reports a first pass as pending.
    pub history: Option<SessionHistoryCatchUpProgress>,
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
            history: None,
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
