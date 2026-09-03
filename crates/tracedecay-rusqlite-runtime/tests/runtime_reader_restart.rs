//! Reader restart and drain acceptance through public runtime authorities.

use std::time::Duration;

use tracedecay_rusqlite_runtime::reader::{ReaderAcquireError, ReaderPool};
use tracedecay_store::{AdmissionConfigV1, UnavailableReasonV1};

#[path = "../../../tests/storage_runtime_rusqlite_suite/runtime_test_support.rs"]
mod runtime_test_support;

use runtime_test_support::{
    CountExecutor, Probe, ReaderRuntimeFixture, TestDatabase, read_request, reader_locator,
    reader_runtime_fixture,
};

#[test]
fn drain_rejects_new_general_work_but_finishes_inflight_and_keeps_health_reserved() {
    let fixture = reader_runtime_fixture();
    let database = TestDatabase::new("reader-bounded-drain.sqlite3");
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

fn reader_budget(fixture: &ReaderRuntimeFixture) -> tracedecay_store::ReaderBudgetV1 {
    let mut budget = AdmissionConfigV1::default().readers;
    budget.min_per_hot_shard = fixture.reader_budget.min_per_hot_shard;
    budget.max_per_hot_shard = fixture.reader_budget.max_per_hot_shard;
    budget.idle_burst_retire_ms = fixture.reader_budget.idle_burst_retire_ms;
    budget
}
