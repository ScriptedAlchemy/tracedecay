use std::time::Duration;

use tokio::time::{Instant, timeout};
use tracedecay_store::{
    CommitSequenceV1, ConsistencyModeV1, FrozenWatermarkCoverageV1, FrozenWatermarkVectorV1,
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeRequestProbeV1, ShardWatermarkV1,
    SnapshotLeaseV1, StoreRuntimeBindingV1, UnavailableReasonV1, WatermarkCoverageStatusV1,
};

pub use super::ports::SystemConsistencyClock;
use super::ports::{
    CommitWatermarkSource, ConsistencyClock, RetainedSnapshotRegistry, RetainedSnapshotState,
    WatermarkSourceState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadConsistencyConfig {
    pub max_wait: Duration,
    pub cancellation_poll_interval: Duration,
}

impl Default for ReadConsistencyConfig {
    fn default() -> Self {
        Self {
            max_wait: Duration::from_secs(5),
            cancellation_poll_interval: Duration::from_millis(10),
        }
    }
}

/// Resolves only wait and coverage truth. Reader workers and snapshot lease
/// ownership remain outside this coordinator.
pub struct ReadConsistencyCoordinator<C = SystemConsistencyClock> {
    config: ReadConsistencyConfig,
    clock: C,
}

impl ReadConsistencyCoordinator<SystemConsistencyClock> {
    pub fn new(config: ReadConsistencyConfig) -> Self {
        Self::with_clock(config, SystemConsistencyClock)
    }
}

impl<C> ReadConsistencyCoordinator<C>
where
    C: ConsistencyClock,
{
    pub fn with_clock(config: ReadConsistencyConfig, clock: C) -> Self {
        Self { config, clock }
    }

    pub async fn resolve(
        &self,
        binding: &StoreRuntimeBindingV1,
        mode: &ConsistencyModeV1,
        commits: &dyn CommitWatermarkSource,
        snapshots: &dyn RetainedSnapshotRegistry,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> RuntimeReadCoverageV1 {
        if let Some(reason) = interruption_reason(probe) {
            return unavailable(None, reason);
        }

        match mode {
            ConsistencyModeV1::LatestAvailable => self.latest(binding, commits),
            ConsistencyModeV1::AtLeast { commit_sequence } => {
                self.at_least(binding, *commit_sequence, commits, probe)
                    .await
            }
            ConsistencyModeV1::ExactSnapshot { lease } => {
                self.exact_snapshot(binding, lease, snapshots)
            }
            ConsistencyModeV1::FrozenWatermarkVector { vector } => {
                self.frozen_vector(vector, commits, probe).await
            }
        }
    }

    fn latest(
        &self,
        binding: &StoreRuntimeBindingV1,
        commits: &dyn CommitWatermarkSource,
    ) -> RuntimeReadCoverageV1 {
        match commits.current(&binding.shard_id) {
            WatermarkSourceState::Available(observed) => match history_reason(binding, &observed) {
                Some(reason) => unavailable(None, reason),
                None => RuntimeReadCoverageV1::Latest {
                    observed: Some(observed),
                },
            },
            WatermarkSourceState::Unavailable(reason) => unavailable(None, reason),
        }
    }

    async fn at_least(
        &self,
        binding: &StoreRuntimeBindingV1,
        commit_sequence: CommitSequenceV1,
        commits: &dyn CommitWatermarkSource,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> RuntimeReadCoverageV1 {
        let required = ShardWatermarkV1 {
            shard_id: binding.shard_id.clone(),
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            commit_sequence,
        };
        let deadline = Instant::now() + self.config.max_wait;
        match self.wait_for(&required, commits, probe, deadline).await {
            WaitResult::Observed {
                watermark,
                unavailable_reason,
            } => classify(
                FrozenWatermarkVectorV1::new([required]).expect("one watermark is non-empty"),
                watermark.into_iter().collect(),
                unavailable_reason,
            ),
            WaitResult::Interrupted(reason) => unavailable(None, reason),
        }
    }

    fn exact_snapshot(
        &self,
        binding: &StoreRuntimeBindingV1,
        requested: &SnapshotLeaseV1,
        snapshots: &dyn RetainedSnapshotRegistry,
    ) -> RuntimeReadCoverageV1 {
        if let Some(reason) = history_reason(binding, &requested.watermark) {
            return unavailable(None, reason);
        }
        if self.clock.utc_now_micros() >= requested.expires_at.0 {
            return unavailable(None, UnavailableReasonV1::SnapshotExpired);
        }

        match snapshots.lookup(&requested.lease_id) {
            RetainedSnapshotState::Retained(retained) => {
                if self.clock.utc_now_micros() >= retained.expires_at.0 {
                    return unavailable(None, UnavailableReasonV1::SnapshotExpired);
                }
                if *retained != *requested {
                    if let Some(reason) = history_reason(binding, &retained.watermark) {
                        return unavailable(None, reason);
                    }
                    return unavailable(None, UnavailableReasonV1::SnapshotNotRetained);
                }
                let required = FrozenWatermarkVectorV1::new([requested.watermark.clone()])
                    .expect("one watermark is non-empty");
                let coverage =
                    FrozenWatermarkCoverageV1::new(required, [retained.watermark.clone()])
                        .expect("retained watermark belongs to the requested vector");
                RuntimeReadCoverageV1::Complete { coverage }
            }
            RetainedSnapshotState::Expired => {
                unavailable(None, UnavailableReasonV1::SnapshotExpired)
            }
            RetainedSnapshotState::NotRetained => {
                unavailable(None, UnavailableReasonV1::SnapshotNotRetained)
            }
            RetainedSnapshotState::Unavailable(reason) => unavailable(None, reason),
        }
    }

    async fn frozen_vector(
        &self,
        required: &FrozenWatermarkVectorV1,
        commits: &dyn CommitWatermarkSource,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> RuntimeReadCoverageV1 {
        // One shared monotonic deadline bounds the complete vector. Iteration is
        // canonical by StoreShardId; each shard is independently observed.
        let deadline = Instant::now() + self.config.max_wait;
        let mut observed = Vec::new();
        let mut unavailable_reason = None;
        for (_, target) in required.iter() {
            match self.wait_for(target, commits, probe, deadline).await {
                WaitResult::Observed {
                    watermark: Some(watermark),
                    unavailable_reason: reason,
                } => {
                    unavailable_reason = unavailable_reason.or(reason);
                    observed.push(watermark);
                }
                WaitResult::Observed {
                    watermark: None,
                    unavailable_reason: reason,
                } => {
                    unavailable_reason = unavailable_reason
                        .or(reason)
                        .or(Some(UnavailableReasonV1::WatermarkNotReached));
                }
                WaitResult::Interrupted(reason) => return unavailable(None, reason),
            }
        }
        classify(required.clone(), observed, unavailable_reason)
    }

    async fn wait_for(
        &self,
        required: &ShardWatermarkV1,
        commits: &dyn CommitWatermarkSource,
        probe: &dyn RuntimeRequestProbeV1,
        deadline: Instant,
    ) -> WaitResult {
        let mut state = commits.current(&required.shard_id);
        loop {
            match state {
                WatermarkSourceState::Available(ref observed) => {
                    if !observed.same_history_as(required)
                        || observed.commit_sequence >= required.commit_sequence
                    {
                        return WaitResult::Observed {
                            unavailable_reason: history_reason_for(required, observed),
                            watermark: Some(observed.clone()),
                        };
                    }
                }
                WatermarkSourceState::Unavailable(reason) => {
                    return WaitResult::Observed {
                        watermark: None,
                        unavailable_reason: Some(reason),
                    };
                }
            }
            if let Some(reason) = interruption_reason(probe) {
                return WaitResult::Interrupted(reason);
            }
            if Instant::now() >= deadline {
                return WaitResult::Observed {
                    watermark: available(state),
                    unavailable_reason: None,
                };
            }

            let after = match &state {
                WatermarkSourceState::Available(after) => after.clone(),
                WatermarkSourceState::Unavailable(_) => {
                    unreachable!("unavailable source state returns above")
                }
            };
            let changed = commits.wait_for_change(&required.shard_id, &after);
            tokio::pin!(changed);
            loop {
                let now = Instant::now();
                if now >= deadline {
                    return WaitResult::Observed {
                        watermark: available(state),
                        unavailable_reason: None,
                    };
                }
                let poll = nonzero_poll(self.config.cancellation_poll_interval)
                    .min(deadline.saturating_duration_since(now));
                match timeout(poll, &mut changed).await {
                    Ok(next) => {
                        state = next;
                        break;
                    }
                    Err(_) => {
                        if let Some(reason) = interruption_reason(probe) {
                            return WaitResult::Interrupted(reason);
                        }
                    }
                }
            }
        }
    }
}

enum WaitResult {
    Observed {
        watermark: Option<ShardWatermarkV1>,
        unavailable_reason: Option<UnavailableReasonV1>,
    },
    Interrupted(UnavailableReasonV1),
}

fn available(state: WatermarkSourceState) -> Option<ShardWatermarkV1> {
    match state {
        WatermarkSourceState::Available(watermark) => Some(watermark),
        WatermarkSourceState::Unavailable(_) => None,
    }
}

fn nonzero_poll(interval: Duration) -> Duration {
    if interval.is_zero() {
        Duration::from_millis(1)
    } else {
        interval
    }
}

fn interruption_reason(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
    probe.interruption().map(|interruption| match interruption {
        RuntimeInterruptionV1::Cancelled => UnavailableReasonV1::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded => UnavailableReasonV1::DeadlineExceeded,
    })
}

fn history_reason(
    binding: &StoreRuntimeBindingV1,
    observed: &ShardWatermarkV1,
) -> Option<UnavailableReasonV1> {
    if observed.shard_id != binding.shard_id {
        Some(UnavailableReasonV1::MissingAuthority)
    } else if observed.incarnation != binding.incarnation {
        Some(UnavailableReasonV1::WrongIncarnation)
    } else if observed.authority_epoch != binding.authority_epoch {
        Some(UnavailableReasonV1::WrongAuthorityEpoch)
    } else {
        None
    }
}

fn history_reason_for(
    required: &ShardWatermarkV1,
    observed: &ShardWatermarkV1,
) -> Option<UnavailableReasonV1> {
    history_reason(
        &StoreRuntimeBindingV1::new(
            required.shard_id.clone(),
            required.incarnation,
            required.authority_epoch,
        ),
        observed,
    )
}

fn classify(
    required: FrozenWatermarkVectorV1,
    observed: Vec<ShardWatermarkV1>,
    unavailable_reason: Option<UnavailableReasonV1>,
) -> RuntimeReadCoverageV1 {
    let coverage = FrozenWatermarkCoverageV1::new(required, observed)
        .expect("coordinator only records watermarks for required shards");
    let has_stale = coverage
        .required
        .iter()
        .any(|(shard, _)| coverage.status_for(shard) == WatermarkCoverageStatusV1::Stale);
    if coverage.is_complete() {
        RuntimeReadCoverageV1::Complete { coverage }
    } else if coverage.is_partial() {
        RuntimeReadCoverageV1::Partial { coverage }
    } else if has_stale {
        RuntimeReadCoverageV1::Stale { coverage }
    } else {
        RuntimeReadCoverageV1::Unavailable {
            coverage: Some(coverage),
            reason: unavailable_reason.unwrap_or(UnavailableReasonV1::WatermarkNotReached),
        }
    }
}

fn unavailable(
    coverage: Option<FrozenWatermarkCoverageV1>,
    reason: UnavailableReasonV1,
) -> RuntimeReadCoverageV1 {
    RuntimeReadCoverageV1::Unavailable { coverage, reason }
}
