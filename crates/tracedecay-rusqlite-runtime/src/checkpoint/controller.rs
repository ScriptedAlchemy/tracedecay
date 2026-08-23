use std::time::Instant;

use crate::maintenance::ExclusiveMaintenancePermit;

use super::driver::{CheckpointDriver, RusqliteCheckpointDriver};
use super::types::{
    CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointError,
    CheckpointInterruption, CheckpointMode, CheckpointResult, WalPressure, WalSample,
};

/// Checkpoint policy state owned by the persistent writer.
pub(crate) struct WriterCheckpointController<D> {
    driver: D,
    config: CheckpointConfig,
    hard_drain_required: bool,
    last_wal_bytes: Option<u64>,
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
            last_wal_bytes: None,
        })
    }

    pub(crate) const fn hard_drain_required(&self) -> bool {
        self.hard_drain_required
    }

    /// Scheduled WAL sampling after a product write. Hard-drain recovery and
    /// an unseen or near-soft estimate still sample; otherwise the write is
    /// charged against the last sample so later writes cannot hide a crossing.
    pub(crate) fn scheduled_sample_required(&self, additional_bytes: u64) -> bool {
        self.hard_drain_required
            || self
                .estimated_wal_after_write(additional_bytes)
                .is_none_or(|estimated| estimated >= self.config.soft_wal_bytes)
    }

    pub(crate) fn note_unsampled_write(&mut self, additional_bytes: u64) {
        if let Some(estimated) = self.estimated_wal_after_write(additional_bytes) {
            self.last_wal_bytes = Some(estimated);
        }
    }

    fn estimated_wal_after_write(&self, additional_bytes: u64) -> Option<u64> {
        Some(
            self.last_wal_bytes?
                .saturating_add(additional_bytes)
                .saturating_add(self.growth_slack()),
        )
    }

    fn growth_slack(&self) -> u64 {
        // 1/32 of the soft limit (1 MiB at the default 32 MiB threshold).
        self.config.soft_wal_bytes.saturating_div(32).max(1)
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
        let sample = self.sample_wal()?;
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
        let sample = self.sample_wal()?;
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
        let sample = self.sample_wal()?;
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
        self.last_wal_bytes = Some(wal_bytes);
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
        let report = self
            .driver
            .checkpoint(mode)
            .map_err(CheckpointError::Driver)?;
        let elapsed = started.elapsed();

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

    fn sample_wal(&mut self) -> Result<WalSample, CheckpointError<D::Error>> {
        let sample = self.driver.sample_wal().map_err(CheckpointError::Driver)?;
        self.last_wal_bytes = Some(sample.bytes);
        Ok(sample)
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

impl WriterCheckpointController<RusqliteCheckpointDriver> {
    pub(crate) fn connection_mut(&mut self) -> &mut rusqlite::Connection {
        self.driver.connection_mut()
    }
}
