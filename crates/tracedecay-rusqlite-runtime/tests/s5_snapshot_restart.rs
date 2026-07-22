//! S5 restart and drain acceptance through public runtime authorities.

use std::time::Duration;

use tracedecay_rusqlite_runtime::{
    read_consistency::{
        CommitWatermarkSource, RetainedSnapshotRegistry, RetainedSnapshotState,
        WatermarkSourceState,
    },
    reader::{ReaderAcquireError, ReaderPool, SqliteRetainedSnapshotRegistry},
    watermark::CommittedWatermarkPublisher,
};
use tracedecay_store::{AdmissionConfigV1, SnapshotLeaseV1, UnavailableReasonV1};

#[path = "../../../tests/storage_runtime_rusqlite_suite/cutover_support.rs"]
mod cutover_support;

use cutover_support::{CountExecutor, Probe, TestDatabase, fixture, read_request, reader_locator};

#[test]
fn restart_reseeds_commit_truth_without_resurrecting_process_local_snapshots() {
    let fixture = fixture().s5;
    let database = TestDatabase::new("s5-snapshot-restart.sqlite3");
    database
        .connect()
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE acceptance_rows(value INTEGER NOT NULL);
             INSERT INTO acceptance_rows(value) VALUES (1);",
        )
        .expect("seed restart authority");

    let budget = reader_budget(&fixture);
    let locator = reader_locator(&fixture.binding, &database.path);
    let pool = ReaderPool::start(locator.clone(), budget.clone(), CountExecutor)
        .expect("start original reader pool");
    let registry = SqliteRetainedSnapshotRegistry::new(pool.clone());
    let lease = snapshot_lease(&fixture);
    let exact = exact_request(&fixture, &lease);
    let probe = Probe::for_read(&exact);
    registry
        .retain(lease.clone(), &exact, &probe, Duration::ZERO)
        .expect("retain original process-local snapshot");
    assert!(matches!(
        registry.lookup(&lease.lease_id),
        RetainedSnapshotState::Retained(found) if *found == lease
    ));

    drop(registry);
    drop(pool);

    let restarted =
        ReaderPool::start(locator, budget, CountExecutor).expect("restart reader authority");
    let restarted_registry = SqliteRetainedSnapshotRegistry::new(restarted);
    assert_eq!(
        restarted_registry.lookup(&lease.lease_id),
        RetainedSnapshotState::NotRetained,
        "a restart must not synthesize an exact SQLite snapshot from metadata"
    );

    let publisher = CommittedWatermarkPublisher::with_initial_watermarks([lease.watermark.clone()])
        .expect("seed the public committed-watermark authority");
    assert_eq!(
        publisher.subscribe().current(&fixture.binding.shard_id),
        WatermarkSourceState::Available(lease.watermark),
        "restart recovery must seed commit truth through the public watermark API"
    );
}

#[test]
fn drain_rejects_new_general_work_but_finishes_inflight_and_keeps_health_reserved() {
    let fixture = fixture().s5;
    let database = TestDatabase::new("s5-bounded-drain.sqlite3");
    database
        .connect()
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE acceptance_rows(value INTEGER NOT NULL);
             INSERT INTO acceptance_rows(value) VALUES (1);",
        )
        .expect("seed drain authority");
    let pool = ReaderPool::start(
        reader_locator(&fixture.binding, &database.path),
        reader_budget(&fixture),
        CountExecutor,
    )
    .expect("start drain reader pool");

    let regular = read_request(&fixture.binding, "foreground");
    let regular_probe = Probe::for_read(&regular);
    let mut inflight = pool
        .acquire(&regular, &regular_probe, Duration::ZERO)
        .expect("acquire inflight reader");
    pool.begin_drain();

    assert!(matches!(
        pool.acquire(&regular, &regular_probe, Duration::from_secs(1)),
        Err(ReaderAcquireError::Interrupted {
            reason: UnavailableReasonV1::Draining
        })
    ));
    let mut snapshot = inflight
        .begin_snapshot()
        .expect("inflight reader may finish after drain begins");
    assert!(
        snapshot
            .execute(regular, &regular_probe)
            .expect("finish inflight snapshot")
            .value()
            .is_some()
    );
    drop(snapshot);
    drop(inflight);

    let health = read_request(&fixture.binding, "health");
    let health_probe = Probe::for_read(&health);
    let health_lease = pool
        .acquire(&health, &health_probe, Duration::ZERO)
        .expect("reserved health reader remains available");
    assert_eq!(pool.snapshot().leased_health, 1);
    drop(health_lease);
    assert_eq!(pool.snapshot().leased_health, 0);
}

fn reader_budget(fixture: &cutover_support::S5Fixture) -> tracedecay_store::ReaderBudgetV1 {
    let mut budget = AdmissionConfigV1::default().readers;
    budget.min_per_hot_shard = fixture.reader_budget.min_per_hot_shard;
    budget.max_per_hot_shard = fixture.reader_budget.max_per_hot_shard;
    budget.idle_burst_retire_ms = fixture.reader_budget.idle_burst_retire_ms;
    budget
}

fn snapshot_lease(fixture: &cutover_support::S5Fixture) -> SnapshotLeaseV1 {
    serde_json::from_value(serde_json::json!({
        "lease_id": "lease.s5.restart",
        "snapshot_id": "snapshot.s5.restart",
        "watermark": {
            "shard_id": fixture.binding.shard_id,
            "incarnation": fixture.binding.incarnation,
            "authority_epoch": fixture.binding.authority_epoch,
            "commit_sequence": fixture.initial_commit_sequence
        },
        "acquired_at": 1,
        "expires_at": i64::MAX
    }))
    .expect("construct S5 snapshot lease")
}

fn exact_request(
    fixture: &cutover_support::S5Fixture,
    lease: &SnapshotLeaseV1,
) -> tracedecay_store::RuntimeReadRequestV1 {
    serde_json::from_value(serde_json::json!({
        "binding": fixture.binding,
        "consistency": {
            "kind": "exact_snapshot",
            "lease": lease
        },
        "operation": { "kind": "graph_quick_check" },
        "priority": "foreground",
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": "deadline.s5.restart" },
            "cancellation": {
                "cancellation_id": "cancellation.s5.restart",
                "generation": 1
            }
        }
    }))
    .expect("construct exact S5 read request")
}
