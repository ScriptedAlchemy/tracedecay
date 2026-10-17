use std::time::{Duration, Instant};

use super::driver::{RusqliteCheckpointDriver, RusqliteCheckpointError};
use super::types::{
    CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointError,
    CheckpointInterruption, CheckpointMode, CheckpointReport, CheckpointResult, EXPIRED_RETRY_MAX,
    EXPIRED_RETRY_MIN, FENCE_LIMIT, FENCE_READ_DEADLINE, FENCED_RETRY_INTERVAL, WalPressure,
};

/// Where a hard-pressure drain stands.
///
/// An incomplete checkpoint at hard pressure raises a fence: new general reads
/// and writes wait while the writer retries. In-flight snapshots get one read
/// deadline, then are revoked. A fence never outlives [`FENCE_LIMIT`]: whatever
/// still pins the WAL after it (a wedged reader, another process) is named,
/// the fence drops and writes resume, and the checkpoint retries with backoff.
/// Writes block again only past twice the hard limit, so the WAL stays bounded
/// and nothing spins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Drain {
    Open,
    Fenced {
        since: Instant,
    },
    Expired {
        retry_at: Instant,
        backoff: Duration,
        wal_bytes: u64,
    },
}

/// Checkpoint policy state owned by the persistent writer.
pub(crate) struct WriterCheckpointController {
    driver: RusqliteCheckpointDriver,
    config: CheckpointConfig,
    drain: Drain,
}

impl WriterCheckpointController {
    /// Construct policy state and disable SQLite's connection-local automatic
    /// checkpointing. Startup fails closed when this cannot be established.
    pub(crate) fn new(
        mut driver: RusqliteCheckpointDriver,
        config: CheckpointConfig,
    ) -> Result<Self, CheckpointError<RusqliteCheckpointError>> {
        let config = config.validate().map_err(CheckpointError::InvalidConfig)?;
        driver
            .disable_auto_checkpoint()
            .map_err(CheckpointError::Driver)?;
        Ok(Self {
            driver,
            config,
            drain: Drain::Open,
        })
    }

    /// Whether general writes wait for the checkpoint: while the fence is up,
    /// and after it expired only once the WAL passed twice the hard limit.
    pub(crate) const fn hard_drain_required(&self) -> bool {
        blocks_general(self.drain, self.config.hard_wal_bytes)
    }

    /// When the writer should try the checkpoint again, if a drain is open.
    pub(crate) fn retry_due(&self, now: Instant) -> Option<Instant> {
        match self.drain {
            Drain::Open => None,
            Drain::Fenced { .. } => Some(now + FENCED_RETRY_INTERVAL),
            Drain::Expired { retry_at, .. } => Some(retry_at),
        }
    }

    /// The fence has waited one read deadline: in-flight snapshots must end.
    pub(crate) fn revocation_due(&self, now: Instant) -> bool {
        matches!(self.drain, Drain::Fenced { since } if now.saturating_duration_since(since) >= FENCE_READ_DEADLINE)
    }

    /// The fence has expired or the next incomplete attempt expires it, so
    /// the attempt should name whatever still holds the WAL.
    pub(crate) fn names_holders(&self, now: Instant) -> bool {
        match self.drain {
            Drain::Open => false,
            Drain::Fenced { since } => now.saturating_duration_since(since) >= FENCE_LIMIT,
            Drain::Expired { .. } => true,
        }
    }

    /// After the fence expired, the checkpoint after each batch waits for the
    /// backoff instead of retrying on every commit.
    pub(crate) fn defers_scheduled(&self, now: Instant) -> bool {
        matches!(self.drain, Drain::Expired { retry_at, .. } if now < retry_at)
    }

    pub(crate) fn evaluate_scheduled(
        &mut self,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>> {
        self.evaluate_interruptible(snapshot_blockers, || None)
    }

    pub(crate) fn evaluate_interruptible<F>(
        &mut self,
        snapshot_blockers: CheckpointBlockers,
        mut interruption: F,
    ) -> Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>>
    where
        F: FnMut() -> Option<CheckpointInterruption>,
    {
        if let Some(reason) = interruption() {
            return Ok(CheckpointResult::Interrupted {
                reason,
                sample: None,
                snapshot_blockers,
            });
        }
        let sample = self.driver.sample_wal().map_err(CheckpointError::Driver)?;
        if let Some(reason) = interruption() {
            return Ok(CheckpointResult::Interrupted {
                reason,
                sample: Some(sample),
                snapshot_blockers,
            });
        }
        let decision = self.evaluate(sample.bytes, snapshot_blockers)?;
        Ok(CheckpointResult::Decision { sample, decision })
    }

    /// Apply automatic WAL pressure policy. Soft and hard pressure both
    /// attempt RESTART. Backfilling every frame does not shrink the log:
    /// SQLite restarts it only once no reader holds a WAL read slot, and only
    /// RESTART reports a held slot as busy. An incomplete hard-pressure
    /// attempt raises the fence ([`Drain`]); the snapshot authority remains
    /// the source of blocker inventory.
    pub(crate) fn evaluate(
        &mut self,
        wal_bytes: u64,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<RusqliteCheckpointError>> {
        let pressure = self.pressure(wal_bytes);
        if pressure == WalPressure::BelowSoft && self.drain == Drain::Open {
            return Ok(CheckpointDecision::BelowSoftLimit { wal_bytes });
        }
        self.run_checkpoint(
            CheckpointMode::Restart,
            pressure,
            wal_bytes,
            snapshot_blockers,
        )
    }

    /// Returns the whole WAL to the database as the writer stops.
    ///
    /// Only the writer's own shutdown may call this: its admission is closed,
    /// its queues are empty, and the attachment released every reader before
    /// joining it, so the writer holds the exclusivity TRUNCATE needs. A reader
    /// outside this attachment can still keep the checkpoint pending; the WAL
    /// then stays for the next open.
    pub(crate) fn truncate_at_shutdown(
        &mut self,
    ) -> Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>> {
        let sample = self.driver.sample_wal().map_err(CheckpointError::Driver)?;
        let decision = self.run_checkpoint(
            CheckpointMode::Truncate,
            self.pressure(sample.bytes),
            sample.bytes,
            CheckpointBlockers::default(),
        )?;
        Ok(CheckpointResult::Decision { sample, decision })
    }

    fn run_checkpoint(
        &mut self,
        mode: CheckpointMode,
        pressure: WalPressure,
        wal_bytes: u64,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<RusqliteCheckpointError>> {
        let started = Instant::now();
        let report = match self.driver.checkpoint(mode) {
            Ok(report) => report,
            Err(error) => {
                return Err(CheckpointError::Driver(error));
            }
        };
        let elapsed = started.elapsed();
        let (decision, drain) = checkpoint_decision(
            CheckpointAttempt {
                report,
                mode,
                pressure,
                wal_bytes,
                snapshot_blockers,
                elapsed,
            },
            self.drain,
            self.config.hard_wal_bytes,
            Instant::now(),
        );
        self.drain = drain;
        Ok(decision)
    }

    fn pressure(&self, wal_bytes: u64) -> WalPressure {
        if wal_bytes >= self.config.hard_wal_bytes {
            WalPressure::Hard
        } else if wal_bytes >= self.config.soft_wal_bytes {
            WalPressure::Soft
        } else {
            WalPressure::BelowSoft
        }
    }

    pub(crate) fn connection_mut(&mut self) -> &mut rusqlite::Connection {
        self.driver.connection_mut()
    }
}

/// One checkpoint the driver ran, before the drain policy reads it.
pub(super) struct CheckpointAttempt {
    pub(super) report: CheckpointReport,
    pub(super) mode: CheckpointMode,
    pub(super) pressure: WalPressure,
    pub(super) wal_bytes: u64,
    pub(super) snapshot_blockers: CheckpointBlockers,
    pub(super) elapsed: Duration,
}

const fn blocks_general(drain: Drain, hard_wal_bytes: u64) -> bool {
    match drain {
        Drain::Open => false,
        Drain::Fenced { .. } => true,
        Drain::Expired { wal_bytes, .. } => wal_bytes >= hard_wal_bytes.saturating_mul(2),
    }
}

/// The drain policy: the decision this attempt reports and the drain state it
/// leaves. Pure over `now`, so every transition is testable without a clock.
pub(super) fn checkpoint_decision(
    attempt: CheckpointAttempt,
    drain: Drain,
    hard_wal_bytes: u64,
    now: Instant,
) -> (CheckpointDecision, Drain) {
    let CheckpointAttempt {
        report,
        mode,
        pressure,
        wal_bytes,
        snapshot_blockers,
        elapsed,
    } = attempt;
    if report.complete() {
        return (
            CheckpointDecision::Complete {
                mode,
                pressure,
                wal_bytes,
                report,
                elapsed,
            },
            Drain::Open,
        );
    }
    let next = match drain {
        Drain::Open if pressure == WalPressure::Hard => Drain::Fenced { since: now },
        Drain::Open => Drain::Open,
        Drain::Fenced { since } if now.saturating_duration_since(since) < FENCE_LIMIT => drain,
        Drain::Fenced { .. } => Drain::Expired {
            retry_at: now + EXPIRED_RETRY_MIN,
            backoff: EXPIRED_RETRY_MIN,
            wal_bytes,
        },
        Drain::Expired { backoff, .. } => {
            let backoff = backoff.saturating_mul(2).min(EXPIRED_RETRY_MAX);
            Drain::Expired {
                retry_at: now + backoff,
                backoff,
                wal_bytes,
            }
        }
    };
    (
        CheckpointDecision::Pending {
            mode,
            pressure,
            wal_bytes,
            report,
            snapshot_blockers,
            hard_drain_required: blocks_general(next, hard_wal_bytes),
            fence_expired: matches!(next, Drain::Expired { .. }),
            elapsed,
        },
        next,
    )
}
