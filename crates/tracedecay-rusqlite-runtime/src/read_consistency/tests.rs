use std::collections::BTreeMap;
use std::future::{Future, pending};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use serde_json::json;
use tracedecay_store::{
    BrainId, CommitSequenceV1, ConsistencyModeV1, FrozenWatermarkVectorV1, ProjectId,
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeRequestProbeV1, ShardWatermarkV1,
    SnapshotLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreShardIdV1, UnavailableReasonV1, UserProfileId, WatermarkCoverageStatusV1,
};

use super::*;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn shard(project: &str) -> StoreShardIdV1 {
    StoreShardIdV1::project(
        id::<BrainId>("brain.primary"),
        id::<UserProfileId>("profile.primary"),
        id::<ProjectId>(project),
    )
}

fn incarnation(value: u64) -> StoreIncarnationV1 {
    StoreIncarnationV1::new(value).unwrap()
}

fn epoch(value: u64) -> StoreAuthorityEpochV1 {
    StoreAuthorityEpochV1::new(value).unwrap()
}

fn watermark(
    shard_id: StoreShardIdV1,
    incarnation: u64,
    epoch: u64,
    sequence: u64,
) -> ShardWatermarkV1 {
    ShardWatermarkV1 {
        shard_id,
        incarnation: self::incarnation(incarnation),
        authority_epoch: self::epoch(epoch),
        commit_sequence: CommitSequenceV1(sequence),
    }
}

fn binding(shard_id: StoreShardIdV1) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(shard_id, incarnation(1), epoch(7))
}

#[derive(Clone)]
struct FixedSource(BTreeMap<StoreShardIdV1, WatermarkSourceState>);

impl CommitWatermarkSource for FixedSource {
    fn current(&self, shard_id: &StoreShardIdV1) -> WatermarkSourceState {
        self.0
            .get(shard_id)
            .cloned()
            .unwrap_or(WatermarkSourceState::Unavailable(
                UnavailableReasonV1::MissingAuthority,
            ))
    }

    fn wait_for_change<'a>(
        &'a self,
        _shard_id: &'a StoreShardIdV1,
        _after: &'a ShardWatermarkV1,
    ) -> super::ports::WatermarkFuture<'a> {
        Box::pin(pending())
    }
}

struct FixedSnapshots(RetainedSnapshotState);

impl RetainedSnapshotRegistry for FixedSnapshots {
    fn lookup(&self, _lease_id: &tracedecay_store::SnapshotLeaseIdV1) -> RetainedSnapshotState {
        self.0.clone()
    }
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl ConsistencyClock for FixedClock {
    fn utc_now_micros(&self) -> i64 {
        self.0
    }
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    state: Arc<AtomicU8>,
}

impl Probe {
    fn active() -> Self {
        Self {
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new("cancel.test").unwrap(),
                generation: 1,
            },
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new("deadline.test").unwrap(),
            },
            state: Arc::new(AtomicU8::new(0)),
        }
    }
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match self.state.load(Ordering::Acquire) {
            1 => Some(RuntimeInterruptionV1::Cancelled),
            2 => Some(RuntimeInterruptionV1::DeadlineExceeded),
            _ => None,
        }
    }
}

fn coordinator(max_wait: Duration) -> ReadConsistencyCoordinator<FixedClock> {
    ReadConsistencyCoordinator::with_clock(
        ReadConsistencyConfig {
            max_wait,
            cancellation_poll_interval: Duration::from_millis(1),
        },
        FixedClock(50),
    )
}

fn no_snapshots() -> FixedSnapshots {
    FixedSnapshots(RetainedSnapshotState::NotRetained)
}

fn run<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn history_mismatch_is_typed_unavailable_not_stale() {
    run(async {
        let shard_id = shard("project.history");
        for (observed, expected) in [
            (
                watermark(shard_id.clone(), 2, 7, 99),
                UnavailableReasonV1::WrongIncarnation,
            ),
            (
                watermark(shard_id.clone(), 1, 8, 99),
                UnavailableReasonV1::WrongAuthorityEpoch,
            ),
        ] {
            let source = FixedSource(BTreeMap::from([(
                shard_id.clone(),
                WatermarkSourceState::Available(observed),
            )]));
            let result = coordinator(Duration::from_secs(1))
                .resolve(
                    &binding(shard_id.clone()),
                    &ConsistencyModeV1::AtLeast {
                        commit_sequence: CommitSequenceV1(10),
                    },
                    &source,
                    &no_snapshots(),
                    &Probe::active(),
                )
                .await;

            assert!(matches!(
                result,
                RuntimeReadCoverageV1::Unavailable { reason, .. } if reason == expected
            ));
        }
    });
}

#[test]
fn monotonic_wait_times_out_with_truthful_stale_coverage() {
    run(async {
        let shard_id = shard("project.timeout");
        let source = FixedSource(BTreeMap::from([(
            shard_id.clone(),
            WatermarkSourceState::Available(watermark(shard_id.clone(), 1, 7, 4)),
        )]));
        let result = coordinator(Duration::ZERO)
            .resolve(
                &binding(shard_id),
                &ConsistencyModeV1::AtLeast {
                    commit_sequence: CommitSequenceV1(5),
                },
                &source,
                &no_snapshots(),
                &Probe::active(),
            )
            .await;

        assert!(matches!(result, RuntimeReadCoverageV1::Stale { .. }));
    });
}

#[test]
fn cancellation_interrupts_a_pending_consistency_wait() {
    run(async {
        let shard_id = shard("project.cancel");
        let source = FixedSource(BTreeMap::from([(
            shard_id.clone(),
            WatermarkSourceState::Available(watermark(shard_id.clone(), 1, 7, 1)),
        )]));
        let probe = Probe::active();
        probe.state.store(1, Ordering::Release);
        let result = coordinator(Duration::from_secs(2))
            .resolve(
                &binding(shard_id),
                &ConsistencyModeV1::AtLeast {
                    commit_sequence: CommitSequenceV1(2),
                },
                &source,
                &no_snapshots(),
                &probe,
            )
            .await;

        assert!(matches!(
            result,
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::Cancelled,
                coverage: None,
            }
        ));
    });
}

#[test]
fn caller_deadline_interrupts_before_waiting() {
    run(async {
        let shard_id = shard("project.deadline");
        let source = FixedSource(BTreeMap::from([(
            shard_id.clone(),
            WatermarkSourceState::Available(watermark(shard_id.clone(), 1, 7, 1)),
        )]));
        let probe = Probe::active();
        probe.state.store(2, Ordering::Release);

        let result = coordinator(Duration::from_secs(1))
            .resolve(
                &binding(shard_id),
                &ConsistencyModeV1::AtLeast {
                    commit_sequence: CommitSequenceV1(2),
                },
                &source,
                &no_snapshots(),
                &probe,
            )
            .await;

        assert!(matches!(
            result,
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::DeadlineExceeded,
                coverage: None,
            }
        ));
    });
}

fn lease(shard_id: &StoreShardIdV1, expires_at: i64) -> SnapshotLeaseV1 {
    serde_json::from_value(json!({
        "lease_id": "lease.test",
        "snapshot_id": "snapshot.test",
        "watermark": {
            "shard_id": shard_id,
            "incarnation": 1,
            "authority_epoch": 7,
            "commit_sequence": 8
        },
        "acquired_at": 1,
        "expires_at": expires_at
    }))
    .unwrap()
}

#[test]
fn expired_exact_snapshot_is_unavailable_even_if_registry_retains_it() {
    run(async {
        let shard_id = shard("project.snapshot");
        let lease = lease(&shard_id, 50);
        let result = coordinator(Duration::ZERO)
            .resolve(
                &binding(shard_id),
                &ConsistencyModeV1::ExactSnapshot {
                    lease: Box::new(lease.clone()),
                },
                &FixedSource(BTreeMap::new()),
                &FixedSnapshots(RetainedSnapshotState::Retained(Box::new(lease))),
                &Probe::active(),
            )
            .await;

        assert!(matches!(
            result,
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::SnapshotExpired,
                ..
            }
        ));
    });
}

#[test]
fn retained_exact_snapshot_reports_complete_coverage() {
    run(async {
        let shard_id = shard("project.retained");
        let lease = lease(&shard_id, 100);
        let result = coordinator(Duration::ZERO)
            .resolve(
                &binding(shard_id),
                &ConsistencyModeV1::ExactSnapshot {
                    lease: Box::new(lease.clone()),
                },
                &FixedSource(BTreeMap::new()),
                &FixedSnapshots(RetainedSnapshotState::Retained(Box::new(lease.clone()))),
                &Probe::active(),
            )
            .await;

        assert!(matches!(result, RuntimeReadCoverageV1::Complete { .. }));
    });
}

#[test]
fn frozen_vector_reports_partial_stale_and_unavailable_truth() {
    run(async {
        let satisfied = shard("project.a");
        let stale = shard("project.b");
        let unavailable = shard("project.c");
        let required = FrozenWatermarkVectorV1::new([
            watermark(unavailable.clone(), 1, 7, 3),
            watermark(satisfied.clone(), 1, 7, 3),
            watermark(stale.clone(), 1, 7, 3),
        ])
        .unwrap();
        let source = FixedSource(BTreeMap::from([
            (
                satisfied.clone(),
                WatermarkSourceState::Available(watermark(satisfied.clone(), 1, 7, 4)),
            ),
            (
                stale.clone(),
                WatermarkSourceState::Available(watermark(stale.clone(), 1, 7, 2)),
            ),
            (
                unavailable.clone(),
                WatermarkSourceState::Available(watermark(unavailable.clone(), 1, 9, 20)),
            ),
        ]));
        let result = coordinator(Duration::ZERO)
            .resolve(
                &binding(satisfied.clone()),
                &ConsistencyModeV1::FrozenWatermarkVector { vector: required },
                &source,
                &no_snapshots(),
                &Probe::active(),
            )
            .await;

        let RuntimeReadCoverageV1::Partial { coverage } = result else {
            panic!("mixed vector must be partial");
        };
        assert_eq!(
            coverage.status_for(&satisfied),
            WatermarkCoverageStatusV1::Satisfied
        );
        assert_eq!(
            coverage.status_for(&stale),
            WatermarkCoverageStatusV1::Stale
        );
        assert_eq!(
            coverage.status_for(&unavailable),
            WatermarkCoverageStatusV1::Unavailable
        );
        let canonical = coverage
            .required
            .iter()
            .map(|(shard, _)| shard.clone())
            .collect::<Vec<_>>();
        let mut sorted = canonical.clone();
        sorted.sort();
        assert_eq!(canonical, sorted);
    });
}
