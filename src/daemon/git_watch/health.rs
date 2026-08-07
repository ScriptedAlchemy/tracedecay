use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

const HEARTBEAT_STALE_SECS: u64 = 120;
#[cfg(test)]
pub(super) const HEARTBEAT_STALE_MILLIS: u64 = HEARTBEAT_STALE_SECS * 1_000;
#[cfg(not(test))]
const HEARTBEAT_STALE_MILLIS: u64 = HEARTBEAT_STALE_SECS * 1_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum ProjectWatchStatus {
    #[default]
    Initializing,
    Active,
    WatchPlanCapacity,
    WatchPlanUnavailable,
    NotifyCapacity,
    NotifyBackend,
}

impl ProjectWatchStatus {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Active,
            2 => Self::WatchPlanCapacity,
            3 => Self::WatchPlanUnavailable,
            4 => Self::NotifyCapacity,
            5 => Self::NotifyBackend,
            _ => Self::Initializing,
        }
    }

    pub(super) fn is_degraded(self) -> bool {
        !matches!(self, Self::Initializing | Self::Active)
    }
}

#[derive(Debug, Default)]
pub(super) struct ProjectHealth {
    last_heartbeat: AtomicU64,
    status: AtomicU8,
    #[cfg(test)]
    last_freshness_request: AtomicU64,
}

impl ProjectHealth {
    pub(super) fn beat(&self) {
        self.last_heartbeat
            .store(monotonic_health_millis(), Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn mark_requested(&self) {
        self.last_freshness_request
            .store(monotonic_health_millis(), Ordering::Relaxed);
    }

    pub(super) fn set_status(&self, status: ProjectWatchStatus) {
        self.status.store(status as u8, Ordering::Release);
    }

    pub(super) fn snapshot(&self) -> ProjectHealthSnapshot {
        let status = ProjectWatchStatus::from_raw(self.status.load(Ordering::Acquire));
        ProjectHealthSnapshot {
            last_heartbeat: self.last_heartbeat.load(Ordering::Relaxed),
            status,
            #[cfg(test)]
            last_freshness_request: self.last_freshness_request.load(Ordering::Relaxed),
            #[cfg(test)]
            degraded: status.is_degraded(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ProjectHealthSnapshot {
    pub(super) last_heartbeat: u64,
    pub(super) status: ProjectWatchStatus,
    #[cfg(test)]
    pub(super) last_freshness_request: u64,
    #[cfg(test)]
    pub(super) degraded: bool,
}

impl ProjectHealthSnapshot {
    pub(super) fn heartbeat_stale(&self) -> bool {
        self.heartbeat_stale_at(monotonic_health_millis())
    }

    pub(super) fn heartbeat_stale_at(&self, now_millis: u64) -> bool {
        let heartbeat = self.last_heartbeat;
        heartbeat == 0 || now_millis.saturating_sub(heartbeat) > HEARTBEAT_STALE_MILLIS
    }
}

fn monotonic_health_millis() -> u64 {
    static PROCESS_HEALTH_EPOCH: OnceLock<Instant> = OnceLock::new();
    let elapsed = PROCESS_HEALTH_EPOCH.get_or_init(Instant::now).elapsed();
    let capped = elapsed.as_millis().min(u128::from(u64::MAX - 1)) as u64;
    capped + 1
}
