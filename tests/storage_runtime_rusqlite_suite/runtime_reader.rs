use std::time::Duration;

use tracedecay_rusqlite_runtime::{
    WriterState,
    read_consistency::{CommitWatermarkSource, WatermarkSourceState},
    reader::{ReaderAcquireError, ReaderPool, ReaderPoolState},
    runtime::{IntegrityResult, SqliteDoctorHealthLane},
    watermark::CommittedWatermarkPublisher,
};
use tracedecay_store::{OperationPriorityV1, StoreCommitReceiptV1, UnavailableReasonV1};

use super::runtime_test_support::{
    CountExecutor, Probe, TestDatabase, acceptance_reader_budget, read_request, reader_locator,
    reader_runtime_fixture, seed_acceptance_rows, shard_watermark,
};

#[test]
fn reader_drain_preserves_inflight_and_reserved_health_capacity() {
    let fixture = reader_runtime_fixture();
    let database = TestDatabase::new("runtime-reader.sqlite3");
    seed_acceptance_rows(&database.connect(), false);

    let pool = ReaderPool::start(
        reader_locator(&fixture.binding, &database.path),
        acceptance_reader_budget(&fixture),
        CountExecutor,
    )
    .expect("start reader pool");

    let regular = read_request(&fixture.binding, "foreground");
    let regular_probe = Probe::for_read(&regular);
    let mut inflight = pool
        .acquire(&regular, &regular_probe, Duration::ZERO)
        .expect("acquire in-flight general reader");
    pool.begin_drain();

    assert_eq!(pool.snapshot().state, ReaderPoolState::Draining);
    assert!(matches!(
        pool.acquire(&regular, &regular_probe, Duration::ZERO),
        Err(ReaderAcquireError::Interrupted {
            reason: UnavailableReasonV1::Draining
        })
    ));
    let mut snapshot = inflight
        .begin_snapshot()
        .expect("existing lease may finish its snapshot");
    assert!(
        snapshot
            .execute(regular, &regular_probe)
            .expect("execute admitted read")
            .value()
            .is_some()
    );

    let health = read_request(&fixture.binding, "health");
    let health_probe = Probe::for_read(&health);
    let _health = pool
        .acquire(&health, &health_probe, Duration::ZERO)
        .expect("reserved health lane remains available while draining");
    assert_eq!(pool.snapshot().leased_health, 1);
}

#[test]
fn doctor_health_and_commit_watermark_report_the_same_runtime_binding() {
    let fixture = reader_runtime_fixture();
    let database = TestDatabase::new("runtime-health.sqlite3");
    seed_acceptance_rows(&database.connect(), false);

    let pool = ReaderPool::start(
        reader_locator(&fixture.binding, &database.path),
        acceptance_reader_budget(&fixture),
        CountExecutor,
    )
    .expect("start reader pool");
    let health =
        SqliteDoctorHealthLane::from_health_connection(fixture.binding.clone(), database.connect())
            .inspect(WriterState::Ready, pool.snapshot(), true)
            .expect("inspect health lane");
    assert_eq!(health.binding, fixture.binding);
    assert_eq!(health.quick_check, IntegrityResult::Healthy);
    assert_eq!(health.integrity_check, Some(IntegrityResult::Healthy));
    assert_eq!(health.available_health_readers, 1);

    let publisher = CommittedWatermarkPublisher::with_initial_watermarks([shard_watermark(
        &fixture.binding,
        fixture.initial_commit_sequence,
    )])
    .expect("seed committed watermark");
    let receipt: StoreCommitReceiptV1 = serde_json::from_value(serde_json::json!({
        "operation_id": "operation.runtime.watermark",
        "idempotency": {
            "key": "key.runtime.watermark",
            "command_digest": format!("sha256:{}", "a".repeat(64))
        },
        "shard_id": fixture.binding.shard_id,
        "incarnation": fixture.binding.incarnation,
        "authority_epoch": fixture.binding.authority_epoch,
        "commit_sequence": fixture.published_commit_sequence,
        "committed_at": 1
    }))
    .expect("construct committed receipt");
    publisher
        .publish_committed(&receipt)
        .expect("publish monotonic watermark");
    assert_eq!(
        publisher.subscribe().current(&fixture.binding.shard_id),
        WatermarkSourceState::Available(shard_watermark(
            &fixture.binding,
            fixture.published_commit_sequence
        ))
    );

    let health_request = read_request(&fixture.binding, "health");
    assert_eq!(health_request.priority(), OperationPriorityV1::Health);
}
