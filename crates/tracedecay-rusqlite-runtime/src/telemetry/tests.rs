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
        lock_held_micros: 1,
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
    assert_eq!(snapshot.batches.lock_held_micros, 1);
    assert_eq!(snapshot.client_services.len(), 1);
}

#[test]
fn transaction_close_records_commands_rows_lock_and_outcomes() {
    let recorder = WriterTelemetry::default();
    recorder.transaction_closed(WriterTransactionMetrics {
        outcome: WriterTransactionOutcome::Committed,
        commands: 3,
        rows: 8,
        lock_held_micros: 4,
        transaction_micros: 9,
        sqlite_vm: SqliteVmSnapshot {
            fullscan_steps: 1,
            sort_steps: 2,
            vm_steps: 6,
        },
        lock_work: WriterLockWorkSnapshot {
            bytes_encoded: 16,
            bytes_decoded: 32,
        },
    });
    recorder.transaction_closed(WriterTransactionMetrics {
        outcome: WriterTransactionOutcome::RolledBack,
        commands: 1,
        rows: 0,
        lock_held_micros: 2,
        transaction_micros: 2,
        sqlite_vm: SqliteVmSnapshot::default(),
        lock_work: WriterLockWorkSnapshot::default(),
    });
    recorder.transaction_closed(WriterTransactionMetrics {
        outcome: WriterTransactionOutcome::Busy,
        commands: 1,
        rows: 0,
        lock_held_micros: 0,
        transaction_micros: 1,
        sqlite_vm: SqliteVmSnapshot::default(),
        lock_work: WriterLockWorkSnapshot::default(),
    });

    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.transactions.committed_transactions, 1);
    assert_eq!(snapshot.transactions.rolled_back_transactions, 2);
    assert_eq!(snapshot.transactions.commands, 5);
    assert_eq!(snapshot.transactions.rows, 8);
    assert_eq!(snapshot.transactions.lock_held_micros, 6);
    assert_eq!(snapshot.transactions.transaction_micros, 12);
    assert!(
        snapshot.transactions.lock_held_micros <= snapshot.transactions.transaction_micros,
        "lock-held time cannot exceed total transaction time"
    );
    assert_eq!(snapshot.sqlite_vm.fullscan_steps, 1);
    assert_eq!(snapshot.sqlite_vm.sort_steps, 2);
    assert_eq!(snapshot.sqlite_vm.vm_steps, 6);
    assert_eq!(snapshot.lock_work.bytes_encoded, 16);
    assert_eq!(snapshot.lock_work.bytes_decoded, 32);
}

#[test]
fn checkpoint_sample_records_wal_blockers_and_reclaimed_frames() {
    let recorder = WriterTelemetry::default();
    recorder.checkpoint(WalCheckpointSample {
        wal_frames: 12,
        wal_bytes: 4096,
        checkpointed_frames: 12,
        reclaimed_frames: 12,
        busy: true,
        blocker_count: 2,
        hard_pressure: true,
        completed: true,
    });
    recorder.checkpoint_hard_retry();
    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.wal.wal_frames, 12);
    assert_eq!(snapshot.wal.wal_bytes, 4096);
    assert_eq!(snapshot.wal.checkpointed_frames, 12);
    assert_eq!(snapshot.wal.reclaimed_frames, 12);
    assert_eq!(snapshot.wal.busy_events, 1);
    assert_eq!(snapshot.wal.blocker_count, 2);
    assert_eq!(snapshot.wal.hard_pressure_events, 1);
    assert_eq!(snapshot.wal.hard_retry_wakes, 1);
}
