use std::time::Instant;

use crate::maintenance::ExclusiveMaintenancePermit;

use super::driver::{CheckpointDriver, RusqliteCheckpointDriver};
use super::types::{
    CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointError,
    CheckpointInterruption, CheckpointMode, CheckpointResult, WalPressure,
};

/// Checkpoint policy state owned by the persistent writer.
pub(crate) struct WriterCheckpointController<D> {
    driver: D,
    config: CheckpointConfig,
    hard_drain_required: bool,
}

impl<D: CheckpointDriver> WriterCheckpointController<D> {
    /// Construct policy state and disable SQLite's connection-local automatic
    /// checkpointing. Startup fails closed when this cannot be established.
    pub(crate) fn new(
        mut driver: D,
        config: CheckpointConfig,
    ) -> Result<Self, CheckpointError<D::Error>> {
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

    #[hotpath::skip]
    pub(crate) const fn hard_drain_required(&self) -> bool {
        self.hard_drain_required
    }

    pub(crate) fn evaluate_scheduled(
        &mut self,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointResult, CheckpointError<D::Error>> {
        self.evaluate_interruptible(snapshot_blockers, || None)
    }

    pub(crate) fn restart_scheduled(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointResult, CheckpointError<D::Error>> {
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
    ) -> Result<CheckpointResult, CheckpointError<D::Error>> {
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
    ) -> Result<CheckpointResult, CheckpointError<D::Error>>
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
    ) -> Result<CheckpointDecision, CheckpointError<D::Error>> {
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
    ) -> Result<CheckpointDecision, CheckpointError<D::Error>> {
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
    ) -> Result<CheckpointDecision, CheckpointError<D::Error>> {
        self.run_exclusive(
            CheckpointMode::Truncate,
            wal_bytes,
            permit,
            snapshot_blockers,
        )
    }

    fn run_exclusive(
        &mut self,
        mode: CheckpointMode,
        wal_bytes: u64,
        _permit: &ExclusiveMaintenancePermit,
        snapshot_blockers: CheckpointBlockers,
    ) -> Result<CheckpointDecision, CheckpointError<D::Error>> {
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
    ) -> Result<CheckpointDecision, CheckpointError<D::Error>> {
        let started = Instant::now();
        let report = match self.driver.checkpoint(mode) {
            Ok(report) => report,
            Err(error) => {
                crate::hotpath_observe::record_checkpoint_error(
                    checkpoint_attribution(mode),
                    started.elapsed(),
                );
                return Err(CheckpointError::Driver(error));
            }
        };
        let elapsed = started.elapsed();
        crate::hotpath_observe::record_checkpoint(
            checkpoint_attribution(mode),
            elapsed,
            report.complete(),
            wal_bytes,
            report.checkpointed_frames,
        );

        if report.complete() {
            self.hard_drain_required = false;
            return Ok(CheckpointDecision::Complete {
                mode,
                pressure,
                wal_bytes,
                report,
                elapsed,
            });
        }

        if pressure == WalPressure::Hard || self.hard_drain_required {
            self.hard_drain_required = true;
        }
        Ok(CheckpointDecision::Pending {
            mode,
            pressure,
            wal_bytes,
            report,
            snapshot_blockers,
            hard_drain_required: self.hard_drain_required,
            elapsed,
        })
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
}

fn checkpoint_attribution(mode: CheckpointMode) -> crate::hotpath_observe::CheckpointAttribution {
    match mode {
        CheckpointMode::Passive => crate::hotpath_observe::CheckpointAttribution::Passive,
        CheckpointMode::Restart => crate::hotpath_observe::CheckpointAttribution::Restart,
        CheckpointMode::Truncate => crate::hotpath_observe::CheckpointAttribution::Truncate,
    }
}

impl WriterCheckpointController<RusqliteCheckpointDriver> {
    pub(crate) fn connection_mut(&mut self) -> &mut rusqlite::Connection {
        self.driver.connection_mut()
    }
}
