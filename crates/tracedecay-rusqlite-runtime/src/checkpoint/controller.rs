use std::time::{Duration, Instant};

use crate::maintenance::ExclusiveMaintenancePermit;

use super::driver::{RusqliteCheckpointDriver, RusqliteCheckpointError};
use super::types::{
    CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointError,
    CheckpointInterruption, CheckpointMode, CheckpointReport, CheckpointResult, WalPressure,
};

/// Checkpoint policy state owned by the persistent writer.
pub(crate) struct WriterCheckpointController {
    driver: RusqliteCheckpointDriver,
    config: CheckpointConfig,
    hard_drain_required: bool,
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
            hard_drain_required: false,
        })
    }

    pub(crate) const fn hard_drain_required(&self) -> bool {
        self.hard_drain_required
    }

    pub(crate) fn evaluate_scheduled(
        &mut self,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>> {
        self.evaluate_interruptible(snapshot_blockers, || None)
    }

    pub(crate) fn restart_scheduled(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>> {
        if !snapshot_blockers.is_clear() {
            return Err(CheckpointError::MaintenanceStillDraining(snapshot_blockers));
        }
        let sample = self.driver.sample_wal().map_err(CheckpointError::Driver)?;
        let decision = self.restart(sample.bytes, permit, snapshot_blockers)?;
        Ok(CheckpointResult::Decision { sample, decision })
    }

    pub(crate) fn truncate_scheduled(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>> {
        if !snapshot_blockers.is_clear() {
            return Err(CheckpointError::MaintenanceStillDraining(snapshot_blockers));
        }
        let sample = self.driver.sample_wal().map_err(CheckpointError::Driver)?;
        let decision = self.truncate(sample.bytes, permit, snapshot_blockers)?;
        Ok(CheckpointResult::Decision { sample, decision })
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

    /// Apply automatic WAL pressure policy. Soft and hard pressure both first
    /// attempt PASSIVE. An incomplete hard-pressure attempt requests a drain;
    /// the snapshot authority remains the source of blocker inventory.
    pub(crate) fn evaluate(
        &mut self,
        wal_bytes: u64,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<RusqliteCheckpointError>> {
        let pressure = self.pressure(wal_bytes);
        if pressure == WalPressure::BelowSoft && !self.hard_drain_required {
            return Ok(CheckpointDecision::BelowSoftLimit { wal_bytes });
        }
        self.run_checkpoint(
            CheckpointMode::Passive,
            pressure,
            wal_bytes,
            snapshot_blockers,
        )
    }

    /// RESTART and TRUNCATE are reachable only through the exclusive permit
    /// issued after maintenance drains admission, readers, snapshots, and
    /// writer work. PASSIVE remains available through [`Self::evaluate`].
    pub(crate) fn restart(
        &mut self,
        wal_bytes: u64,
        permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<RusqliteCheckpointError>> {
        self.run_exclusive(
            CheckpointMode::Restart,
            wal_bytes,
            permit,
            snapshot_blockers,
        )
    }

    pub(crate) fn truncate(
        &mut self,
        wal_bytes: u64,
        permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<RusqliteCheckpointError>> {
        self.run_exclusive(
            CheckpointMode::Truncate,
            wal_bytes,
            permit,
            snapshot_blockers,
        )
    }

    /// Returns the whole WAL to the database as the writer stops.
    ///
    /// Only the writer's own shutdown may call this: its admission is closed,
    /// its queues are empty, and the attachment released every reader before
    /// joining it, so the writer holds the exclusivity a maintenance permit
    /// would otherwise prove. A reader outside this attachment can still keep
    /// the checkpoint pending; the WAL then stays for the next open.
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

    fn run_exclusive(
        &mut self,
        mode: CheckpointMode,
        wal_bytes: u64,
        _permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<RusqliteCheckpointError>> {
        if !snapshot_blockers.is_clear() {
            return Err(CheckpointError::MaintenanceStillDraining(snapshot_blockers));
        }
        self.run_checkpoint(mode, self.pressure(wal_bytes), wal_bytes, snapshot_blockers)
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
                crate::observe::record_checkpoint_error(
                    checkpoint_attribution(mode),
                    started.elapsed(),
                );
                return Err(CheckpointError::Driver(error));
            }
        };
        let elapsed = started.elapsed();
        crate::observe::record_checkpoint(
            checkpoint_attribution(mode),
            elapsed,
            report.complete(),
            wal_bytes,
            report.checkpointed_frames,
        );
        let (decision, hard_drain_required) = checkpoint_decision(
            report,
            mode,
            pressure,
            wal_bytes,
            snapshot_blockers,
            self.hard_drain_required,
            elapsed,
        );
        self.hard_drain_required = hard_drain_required;
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

pub(super) fn checkpoint_decision(
    report: CheckpointReport,
    mode: CheckpointMode,
    pressure: WalPressure,
    wal_bytes: u64,
    snapshot_blockers: CheckpointBlockers,
    hard_drain_required: bool,
    elapsed: Duration,
) -> (CheckpointDecision, bool) {
    if report.complete() {
        return (
            CheckpointDecision::Complete {
                mode,
                pressure,
                wal_bytes,
                report,
                elapsed,
            },
            false,
        );
    }
    let hard_drain_required = pressure == WalPressure::Hard || hard_drain_required;
    (
        CheckpointDecision::Pending {
            mode,
            pressure,
            wal_bytes,
            report,
            snapshot_blockers,
            hard_drain_required,
            elapsed,
        },
        hard_drain_required,
    )
}

fn checkpoint_attribution(mode: CheckpointMode) -> crate::observe::CheckpointAttribution {
    match mode {
        CheckpointMode::Passive => crate::observe::CheckpointAttribution::Passive,
        CheckpointMode::Restart => crate::observe::CheckpointAttribution::Restart,
        CheckpointMode::Truncate => crate::observe::CheckpointAttribution::Truncate,
    }
}
