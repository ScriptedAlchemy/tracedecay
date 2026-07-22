use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use tracedecay_store::{
    AdmissionConfigV1, BatchBudgetV1, OperationPriorityV1, QueueBudgetV1, RuntimeSubmitOutcomeV1,
};

use crate::support::{
    ExecutorControl, TestBinding, TestDatabase, TestProbe, release, request, runtime, unwrap_arc,
    writer,
};

#[test]
fn saturation_is_immediate_while_reserved_health_work_remains_admissible() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.overload");
    let first = request(
        binding,
        "operation.overload.first",
        "key.overload.first",
        'a',
        OperationPriorityV1::Foreground,
    );
    let defaults = AdmissionConfigV1::default();
    let config = AdmissionConfigV1 {
        per_shard_queue: QueueBudgetV1 {
            max_operations: 1,
            max_bytes: 1_024,
        },
        foreground_batch: BatchBudgetV1 {
            max_operations: 1,
            max_bytes: 1_024,
            ..defaults.foreground_batch
        },
        background_batch: BatchBudgetV1 {
            max_operations: 1,
            max_bytes: 1_024,
            ..defaults.background_batch
        },
        ..defaults
    };
    config.validate().unwrap();

    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writer = Arc::new(writer(
        &database,
        &first,
        config,
        ExecutorControl {
            entered: Some(entered_tx),
            release: Some(Arc::clone(&gate)),
            ..ExecutorControl::default()
        },
    ));
    runtime().block_on(async {
        let first_writer = Arc::clone(&writer);
        let first_probe = TestProbe::fixed(&first);
        let first_task = tokio::spawn(async move { first_writer.submit(first, first_probe).await });
        tokio::task::yield_now().await;
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let overflow = request(
            binding,
            "operation.overload.shed",
            "key.overload.shed",
            'b',
            OperationPriorityV1::Foreground,
        );
        let started = Instant::now();
        let outcome = writer
            .submit(overflow.clone(), TestProbe::fixed(&overflow))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(matches!(outcome, RuntimeSubmitOutcomeV1::Saturated { .. }));

        let health = request(
            binding,
            "operation.overload.health",
            "key.overload.health",
            'c',
            OperationPriorityV1::Health,
        );
        let health_writer = Arc::clone(&writer);
        let health_probe = TestProbe::fixed(&health);
        let health_task =
            tokio::spawn(async move { health_writer.submit(health, health_probe).await });
        release(&gate);
        assert!(matches!(
            first_task.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed { .. }
        ));
        assert!(matches!(
            health_task.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed { .. }
        ));
    });
    let telemetry = writer.telemetry_snapshot();
    assert_eq!(telemetry.operations.offered_operations, 3);
    assert_eq!(telemetry.operations.admitted_operations, 2);
    assert_eq!(telemetry.operations.completed_operations, 2);
    assert_eq!(telemetry.operations.shed_operations, 1);
    assert_eq!(telemetry.health_lane_services, 1);
    unwrap_arc(writer).shutdown_and_join().unwrap();
}

#[test]
fn foreground_request_uses_foreground_ceiling_when_background_ceiling_is_smaller() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.priority-ceiling");
    let foreground = request(
        binding,
        "operation.priority-ceiling.foreground",
        "key.priority-ceiling.foreground",
        'a',
        OperationPriorityV1::Foreground,
    );
    let defaults = AdmissionConfigV1::default();
    let config = AdmissionConfigV1 {
        foreground_batch: BatchBudgetV1 {
            max_operations: 2,
            max_bytes: 1_024,
            ..defaults.foreground_batch.clone()
        },
        background_batch: BatchBudgetV1 {
            max_operations: 1,
            max_bytes: 64,
            ..defaults.background_batch.clone()
        },
        ..defaults
    };
    config.validate().unwrap();
    let writer = writer(&database, &foreground, config, ExecutorControl::default());

    assert!(matches!(
        runtime().block_on(writer.submit(foreground.clone(), TestProbe::fixed(&foreground),)),
        Ok(RuntimeSubmitOutcomeV1::Committed { .. })
    ));
    let commit = writer
        .telemetry_snapshot()
        .latest_commit
        .expect("foreground commit telemetry");
    assert_eq!(commit.batch.priority, OperationPriorityV1::Foreground);
    assert_eq!(commit.batch.batch_bytes, 128);
    writer.shutdown_and_join().unwrap();
}
