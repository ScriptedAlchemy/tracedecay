use std::time::Duration;

use tracedecay_store::SnapshotLeaseIdV1;

use super::*;

fn inventory(id: &str) -> CheckpointBlockers {
    CheckpointBlockers {
        blockers: vec![CheckpointBlocker::SnapshotLease {
            lease_id: SnapshotLeaseIdV1::try_from(id.to_owned()).unwrap(),
            age: Duration::from_secs(3),
        }],
        omitted: 0,
    }
}

struct CheckpointFixture {
    _directory: tempfile::TempDir,
    controller: WriterCheckpointController,
}

impl CheckpointFixture {
    fn open(config: CheckpointConfig) -> Self {
        let directory = tempfile::tempdir().expect("checkpoint directory");
        let path = directory.path().join("checkpoint.db");
        std::fs::File::create(&path).expect("checkpoint file");
        let connection = crate::connection::open(&path, crate::connection::ConnectionMode::Writer)
            .expect("writer connection");
        connection
            .execute_batch("CREATE TABLE item (value INTEGER); INSERT INTO item VALUES (1);")
            .expect("seed checkpoint database");
        let controller =
            WriterCheckpointController::new(RusqliteCheckpointDriver::new(connection), config)
                .expect("writer disables automatic checkpointing");
        Self {
            _directory: directory,
            controller,
        }
    }
}

fn report(busy: bool, log_frames: u64, checkpointed_frames: u64) -> CheckpointReport {
    CheckpointReport {
        busy,
        log_frames,
        checkpointed_frames,
    }
}

#[test]
fn below_soft_is_a_noop_and_soft_pressure_is_passive() {
    let config = CheckpointConfig::default();
    let mut fixture = CheckpointFixture::open(config);
    assert_eq!(
        fixture
            .controller
            .evaluate(config.soft_wal_bytes - 1, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::BelowSoftLimit {
            wal_bytes: config.soft_wal_bytes - 1,
        }
    );
    assert!(matches!(
        fixture
            .controller
            .evaluate(config.soft_wal_bytes, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::Complete {
            mode: CheckpointMode::Passive,
            pressure: WalPressure::Soft,
            ..
        }
    ));
}

/// A configured budget must move the thresholds the controller actually
/// decides on, not merely be stored beside them. A WAL span that is idle under
/// the contract default has to become soft pressure under a tightened budget,
/// and hard pressure at the tightened hard limit.
#[test]
fn a_configured_wal_budget_moves_the_controller_thresholds() {
    let budget = tracedecay_store::WalBudgetV1 {
        soft_limit_bytes: 4 * 1024 * 1024,
        hard_limit_bytes: 16 * 1024 * 1024,
    };
    budget.validate().expect("tightened budget is well formed");
    let config = CheckpointConfig::from(&budget);
    let mut fixture = CheckpointFixture::open(config);

    assert_eq!(
        fixture
            .controller
            .evaluate(budget.soft_limit_bytes - 1, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::BelowSoftLimit {
            wal_bytes: budget.soft_limit_bytes - 1,
        }
    );
    assert!(matches!(
        fixture
            .controller
            .evaluate(budget.soft_limit_bytes, CheckpointBlockers::default())
            .unwrap(),
        CheckpointDecision::Complete {
            pressure: WalPressure::Soft,
            ..
        }
    ));
    // A live PASSIVE checkpoint of this small WAL finishes completely, including
    // while another writer holds BEGIN IMMEDIATE, so the partial report is applied
    // through the decision function the controller uses after the driver returns.
    let (decision, hard_drain_required) = super::controller::checkpoint_decision(
        report(true, 100, 40),
        CheckpointMode::Passive,
        WalPressure::Hard,
        budget.hard_limit_bytes,
        CheckpointBlockers::default(),
        false,
        Duration::ZERO,
    );
    assert!(matches!(
        decision,
        CheckpointDecision::Pending {
            pressure: WalPressure::Hard,
            hard_drain_required: true,
            ..
        }
    ));
    assert!(hard_drain_required);
}

#[test]
fn controller_reports_inventory_without_owning_snapshot_state() {
    let blockers = inventory("lease.soft");
    let (decision, hard_drain_required) = super::controller::checkpoint_decision(
        report(true, 100, 40),
        CheckpointMode::Passive,
        WalPressure::Soft,
        CheckpointConfig::default().soft_wal_bytes,
        blockers.clone(),
        false,
        Duration::ZERO,
    );
    assert!(!hard_drain_required);
    assert!(matches!(
        decision,
        CheckpointDecision::Pending {
            snapshot_blockers,
            hard_drain_required: false,
            ..
        } if snapshot_blockers == blockers
    ));
}

#[test]
fn scheduled_checkpoint_samples_frames_and_bytes_before_passive() {
    let mut idle = CheckpointFixture::open(CheckpointConfig::default());
    assert!(matches!(
        idle.controller
            .evaluate_scheduled(CheckpointBlockers::default())
            .unwrap(),
        CheckpointResult::Decision {
            sample,
            decision: CheckpointDecision::BelowSoftLimit { wal_bytes },
        } if sample.frames >= 1 && sample.bytes >= 1 && wal_bytes == sample.bytes
    ));

    let config = CheckpointConfig {
        soft_wal_bytes: 1,
        hard_wal_bytes: 1024 * 1024 * 1024,
    };
    let mut fixture = CheckpointFixture::open(config);
    assert!(matches!(
        fixture
            .controller
            .evaluate_scheduled(CheckpointBlockers::default())
            .unwrap(),
        CheckpointResult::Decision {
            sample,
            decision: CheckpointDecision::Complete {
                mode: CheckpointMode::Passive,
                pressure: WalPressure::Soft,
                wal_bytes,
                ..
            },
        } if sample.frames >= 1 && sample.bytes >= 1 && wal_bytes == sample.bytes
    ));
}

#[test]
fn scheduled_checkpoint_surfaces_typed_cancellation_before_driver_work() {
    let mut fixture = CheckpointFixture::open(CheckpointConfig::default());

    assert_eq!(
        fixture
            .controller
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
    let (pending, hard_drain_required) = super::controller::checkpoint_decision(
        report(false, 1_000, 500),
        CheckpointMode::Passive,
        WalPressure::Hard,
        config.hard_wal_bytes,
        inventory("lease.hard"),
        false,
        Duration::ZERO,
    );
    assert!(matches!(
        pending,
        CheckpointDecision::Pending {
            pressure: WalPressure::Hard,
            hard_drain_required: true,
            ..
        }
    ));
    assert!(hard_drain_required);
    let (complete, hard_drain_required) = super::controller::checkpoint_decision(
        report(false, 500, 500),
        CheckpointMode::Passive,
        WalPressure::BelowSoft,
        config.soft_wal_bytes - 1,
        CheckpointBlockers::default(),
        hard_drain_required,
        Duration::ZERO,
    );
    assert!(matches!(
        complete,
        CheckpointDecision::Complete {
            mode: CheckpointMode::Passive,
            pressure: WalPressure::BelowSoft,
            ..
        }
    ));
    assert!(!hard_drain_required);
}

#[test]
fn invalid_config_and_driver_configuration_fail_closed() {
    let directory = tempfile::tempdir().expect("checkpoint directory");
    let path = directory.path().join("checkpoint.db");
    std::fs::File::create(&path).expect("checkpoint file");
    let connection = crate::connection::open(&path, crate::connection::ConnectionMode::Writer)
        .expect("writer connection");
    let result = WriterCheckpointController::new(
        RusqliteCheckpointDriver::new(connection),
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

    let reader = crate::connection::open(&path, crate::connection::ConnectionMode::Reader)
        .expect("reader connection");
    let result = WriterCheckpointController::new(
        RusqliteCheckpointDriver::new(reader),
        CheckpointConfig::default(),
    );
    assert!(matches!(
        result,
        Err(CheckpointError::Driver(RusqliteCheckpointError::Sqlite(_)))
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

    assert!(sample.frames >= 1);
    assert!(sample.bytes >= 1);
    assert!(report.checkpointed_frames <= report.log_frames);
    assert!(report.complete());
}

static FOREIGN_CHECKPOINTER_WAITING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static FOREIGN_CHECKPOINTER_RELEASED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn park_foreign_checkpointer(_attempt: i32) -> bool {
    use std::sync::atomic::Ordering;
    FOREIGN_CHECKPOINTER_WAITING.store(true, Ordering::Release);
    while !FOREIGN_CHECKPOINTER_RELEASED.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// `SQLite` answers a checkpoint that cannot take the checkpoint lock with the
/// row `(1, -1, -1)`. Another connection checkpointing the same file is an
/// ordinary deferral, so the scheduled checkpoint must report it pending
/// rather than fault the writer (#4007).
#[test]
fn scheduled_checkpoint_defers_while_another_connection_checkpoints() {
    use std::sync::atomic::Ordering;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkpoint.db");
    std::fs::File::create(&path).unwrap();
    let connection =
        crate::connection::open(&path, crate::connection::ConnectionMode::Writer).unwrap();
    connection
        .execute_batch("CREATE TABLE item (value INTEGER); INSERT INTO item VALUES (1);")
        .unwrap();
    let mut controller = WriterCheckpointController::new(
        RusqliteCheckpointDriver::new(connection),
        CheckpointConfig {
            soft_wal_bytes: 1,
            hard_wal_bytes: 1024 * 1024 * 1024,
        },
    )
    .unwrap();

    // A FULL checkpoint holds the checkpoint lock while its busy handler waits
    // out the open write transaction.
    let lock_holder = rusqlite::Connection::open(&path).unwrap();
    lock_holder
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO item VALUES (2);")
        .unwrap();
    let checkpointer_path = path.clone();
    let checkpointer = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(checkpointer_path).unwrap();
        connection
            .busy_handler(Some(park_foreign_checkpointer))
            .unwrap();
        connection
            .query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    });
    while !FOREIGN_CHECKPOINTER_WAITING.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }

    let result = controller.evaluate_scheduled(CheckpointBlockers::default());

    FOREIGN_CHECKPOINTER_RELEASED.store(true, Ordering::Release);
    assert_eq!(
        checkpointer.join().unwrap(),
        1,
        "the foreign FULL gave up busy"
    );
    lock_holder.execute_batch("ROLLBACK").unwrap();
    assert!(
        matches!(
            result,
            Ok(CheckpointResult::Decision {
                decision: CheckpointDecision::Pending {
                    mode: CheckpointMode::Passive,
                    report: CheckpointReport { busy: true, .. },
                    ..
                },
                ..
            })
        ),
        "a busy checkpoint lock is a pending checkpoint, not a driver fault: {result:?}"
    );
}
