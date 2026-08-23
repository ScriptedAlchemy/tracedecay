use std::{collections::VecDeque, fmt::Debug, time::Duration};

use tracedecay_store::{
    BrainId, ProjectId, RuntimePublicationIdV1, SnapshotLeaseIdV1, StoreAuthorityEpochV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreRuntimeRegistryPublicationV1, StoreShardIdV1,
    UserProfileId,
};

use crate::maintenance::{
    DrainBlockers, DrainedStateProof, ExclusiveMaintenancePermit, MaintenanceOwnerId,
};

use super::*;

#[derive(Debug, Eq, PartialEq)]
enum FakeError {
    Configure,
    Sample,
    Checkpoint,
}

impl std::fmt::Display for FakeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for FakeError {}

#[derive(Default)]
struct FakeDriver {
    configure_error: bool,
    samples: VecDeque<Result<WalSample, FakeError>>,
    reports: VecDeque<Result<CheckpointReport, FakeError>>,
}

impl FakeDriver {
    fn with_reports(reports: impl IntoIterator<Item = CheckpointReport>) -> Self {
        Self {
            reports: reports.into_iter().map(Ok).collect(),
            ..Self::default()
        }
    }

    fn with_sample_and_reports(
        sample: WalSample,
        reports: impl IntoIterator<Item = CheckpointReport>,
    ) -> Self {
        Self {
            samples: [Ok(sample)].into(),
            reports: reports.into_iter().map(Ok).collect(),
            ..Self::default()
        }
    }
}

impl CheckpointDriver for FakeDriver {
    type Error = FakeError;

    fn disable_auto_checkpoint(&mut self) -> Result<(), Self::Error> {
        (!self.configure_error)
            .then_some(())
            .ok_or(FakeError::Configure)
    }

    fn sample_wal(&mut self) -> Result<WalSample, Self::Error> {
        self.samples.pop_front().unwrap_or(Err(FakeError::Sample))
    }

    fn checkpoint(&mut self, _mode: CheckpointMode) -> Result<CheckpointReport, Self::Error> {
        self.reports
            .pop_front()
            .unwrap_or(Err(FakeError::Checkpoint))
    }
}

fn report(busy: bool, log_frames: u64, checkpointed_frames: u64) -> CheckpointReport {
    CheckpointReport {
        busy,
        log_frames,
        checkpointed_frames,
    }
}

fn inventory(id: &str) -> CheckpointBlockers {
    CheckpointBlockers {
        blockers: vec![CheckpointBlocker {
            lease_id: SnapshotLeaseIdV1::try_from(id.to_owned()).unwrap(),
            age: Duration::from_secs(3),
        }],
        omitted: 0,
    }
}

fn controller(
    reports: impl IntoIterator<Item = CheckpointReport>,
) -> WriterCheckpointController<FakeDriver> {
    WriterCheckpointController::new(
        FakeDriver::with_reports(reports),
        CheckpointConfig::default(),
    )
    .expect("fake driver configures")
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

/// Test authority that admits one externally canonical publication and issues
/// a permit only from an observed clear drain. It never derives an identity
/// from a path or allocates a replacement fence.
struct FakeCanonicalAuthority {
    publication: StoreRuntimeRegistryPublicationV1,
}

impl FakeCanonicalAuthority {
    fn new() -> Self {
        let binding = StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                id::<BrainId>("brain.checkpoint"),
                id::<UserProfileId>("profile.checkpoint"),
                id::<ProjectId>("project.checkpoint"),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        );
        Self {
            publication: serde_json::from_value(serde_json::json!({
                "publication_id": RuntimePublicationIdV1::new(
                    "publication.checkpoint".to_owned()
                ).unwrap(),
                "binding": binding,
                "published_at": 1,
            }))
            .unwrap(),
        }
    }

    fn permit_after_drain(&self) -> ExclusiveMaintenancePermit {
        let proof =
            DrainedStateProof::observe(self.publication.clone(), DrainBlockers::default()).unwrap();
        ExclusiveMaintenancePermit::issue_after_drain(
            MaintenanceOwnerId::new(1).unwrap(),
            self.publication.clone(),
            proof,
        )
        .unwrap()
    }
}

#[test]
fn below_soft_is_a_noop_and_soft_pressure_is_passive() {
    let config = CheckpointConfig::default();
    let mut controller = controller([report(false, 100, 100)]);
    assert_eq!(
        controller
            .evaluate(config.soft_wal_bytes - 1, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::BelowSoftLimit {
            wal_bytes: config.soft_wal_bytes - 1,
        }
    );
    assert!(matches!(
        controller
            .evaluate(config.soft_wal_bytes, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::Complete {
            mode: CheckpointMode::Passive,
            pressure: WalPressure::Soft,
            ..
        }
    ));
}

#[test]
fn controller_reports_inventory_without_owning_snapshot_state() {
    let blockers = inventory("lease.soft");
    let mut controller = controller([report(true, 100, 40)]);
    assert!(matches!(
        controller
            .evaluate(CheckpointConfig::default().soft_wal_bytes, blockers.clone())
            .unwrap(),
        CheckpointDecision::Pending {
            snapshot_blockers,
            hard_drain_required: false,
            ..
        } if snapshot_blockers == blockers
    ));
}

#[test]
fn scheduled_checkpoint_samples_frames_and_bytes_before_passive() {
    let config = CheckpointConfig::default();
    let sample = WalSample {
        frames: 17,
        bytes: config.soft_wal_bytes,
    };
    let driver = FakeDriver::with_sample_and_reports(sample, [report(false, 17, 17)]);
    let mut controller =
        WriterCheckpointController::new(driver, config).expect("fake driver configures");

    assert!(matches!(
        controller
            .evaluate_scheduled(CheckpointBlockers::default())
            .unwrap(),
        CheckpointResult::Decision {
            sample: actual,
            decision: CheckpointDecision::Complete {
                mode: CheckpointMode::Passive,
                ..
            },
        } if actual == sample
    ));
}

#[test]
fn scheduled_checkpoint_surfaces_typed_cancellation_before_driver_work() {
    let sample = WalSample {
        frames: 1,
        bytes: CheckpointConfig::default().soft_wal_bytes,
    };
    let driver = FakeDriver::with_sample_and_reports(sample, [report(false, 1, 1)]);
    let mut controller =
        WriterCheckpointController::new(driver, CheckpointConfig::default()).unwrap();

    assert_eq!(
        controller
            .evaluate_interruptible(CheckpointBlockers::default(), || Some(
                CheckpointInterruption::DeadlineExceeded
            ),)
            .unwrap(),
        CheckpointResult::Interrupted {
            reason: CheckpointInterruption::DeadlineExceeded,
            sample: None,
            snapshot_blockers: CheckpointBlockers::default(),
        }
    );
}

#[test]
fn incomplete_hard_checkpoint_requires_drain_until_passive_completes() {
    let config = CheckpointConfig::default();
    let mut controller = controller([report(false, 1_000, 500), report(false, 500, 500)]);
    assert!(matches!(
        controller
            .evaluate(config.hard_wal_bytes, inventory("lease.hard"))
            .unwrap(),
        CheckpointDecision::Pending {
            pressure: WalPressure::Hard,
            hard_drain_required: true,
            ..
        }
    ));
    assert!(controller.hard_drain_required());
    assert!(matches!(
        controller
            .evaluate(config.soft_wal_bytes - 1, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::Complete {
            mode: CheckpointMode::Passive,
            pressure: WalPressure::BelowSoft,
            ..
        }
    ));
    assert!(!controller.hard_drain_required());
}

#[test]
fn invalid_config_and_driver_configuration_fail_closed() {
    let result = WriterCheckpointController::new(
        FakeDriver::default(),
        CheckpointConfig {
            soft_wal_bytes: 10,
            hard_wal_bytes: 10,
        },
    );
    assert!(matches!(
        result,
        Err(CheckpointError::InvalidConfig(
            CheckpointConfigError::HardLimitNotAboveSoftLimit
        ))
    ));
    let result = WriterCheckpointController::new(
        FakeDriver {
            configure_error: true,
            ..FakeDriver::default()
        },
        CheckpointConfig::default(),
    );
    assert!(matches!(
        result,
        Err(CheckpointError::Driver(FakeError::Configure))
    ));
}

#[test]
fn exclusive_modes_borrow_one_canonical_linear_permit() {
    let authority = FakeCanonicalAuthority::new();
    let permit = authority.permit_after_drain();
    let config = CheckpointConfig::default();
    let mut controller = controller([report(false, 2, 2), report(false, 0, 0)]);

    assert!(matches!(
        controller
            .restart(
                config.soft_wal_bytes,
                &permit,
                CheckpointBlockers::default(),
            )
            .unwrap(),
        CheckpointDecision::Complete {
            mode: CheckpointMode::Restart,
            ..
        }
    ));
    assert!(matches!(
        controller
            .truncate(0, &permit, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::Complete {
            mode: CheckpointMode::Truncate,
            ..
        }
    ));
}

#[test]
fn exclusive_checkpoint_rejects_a_nonempty_drain_inventory() {
    let authority = FakeCanonicalAuthority::new();
    let permit = authority.permit_after_drain();
    let blockers = inventory("lease.exclusive");
    let mut controller = controller([]);

    assert!(matches!(
        controller.restart(0, &permit, blockers.clone()),
        Err(CheckpointError::MaintenanceStillDraining(actual)) if actual == blockers
    ));
}

#[test]
fn rusqlite_driver_samples_and_checkpoints_its_owned_writer_connection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkpoint.db");
    std::fs::File::create(&path).unwrap();
    let connection =
        crate::connection::open(&path, crate::connection::ConnectionMode::Writer).unwrap();
    connection
        .execute_batch("CREATE TABLE item (value INTEGER); INSERT INTO item VALUES (1);")
        .unwrap();
    let mut driver = RusqliteCheckpointDriver::new(connection);

    driver.disable_auto_checkpoint().unwrap();
    let sample = driver.sample_wal().unwrap();
    let report = driver.checkpoint(CheckpointMode::Passive).unwrap();

    assert!(sample.frames > 0);
    assert!(sample.bytes > 0);
    assert!(report.checkpointed_frames <= report.log_frames);
}
