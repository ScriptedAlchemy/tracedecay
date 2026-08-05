use super::*;
use tracedecay_store::{RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeSubmitOutcomeV1};

fn batch(priority: OperationPriorityV1) -> WriterBatchMetrics {
    WriterBatchMetrics {
        priority,
        durability: DurabilityClassV1::Full,
        batch_operations: 1,
        batch_bytes: 8,
        queue_wait_micros: 2,
        transaction_micros: 3,
    }
}

#[test]
fn one_recorder_owns_admission_and_completion_snapshot() {
    let recorder = WriterTelemetry::default();
    recorder.offered();
    recorder.admitted(8);
    recorder.released(1, 8);
    recorder.completed(&Ok(RuntimeSubmitOutcomeV1::Unavailable {
        reason: tracedecay_store::UnavailableReasonV1::Closed,
    }));
    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.operations.offered_operations, 1);
    assert_eq!(snapshot.operations.admitted_operations, 1);
    assert_eq!(snapshot.operations.completed_operations, 1);
    assert_eq!(snapshot.queue, WriterQueueSnapshot::default());
}

#[test]
fn interruption_outcomes_remain_distinct_in_writer_telemetry() {
    let recorder = WriterTelemetry::default();
    recorder.completed(&Ok(RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit {
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new("deadline.telemetry").unwrap(),
        },
    }));

    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.operations.completed_operations, 1);
    assert_eq!(snapshot.operations.deadline_exceeded_operations, 1);
    assert_eq!(snapshot.operations.cancelled_operations, 0);
}

#[test]
fn commit_metrics_and_clients_are_recorded_together() {
    let recorder = WriterTelemetry::default();
    recorder.committed(
        CommitSequenceV1(1),
        batch(OperationPriorityV1::Foreground),
        [(
            StoreClientIdV1::new("client.telemetry").unwrap(),
            OperationPriorityV1::Foreground,
        )],
    );
    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.commit_sequence, CommitSequenceV1(1));
    assert_eq!(snapshot.batches.total_latency_micros, 5);
    assert_eq!(snapshot.client_services.len(), 1);
}
