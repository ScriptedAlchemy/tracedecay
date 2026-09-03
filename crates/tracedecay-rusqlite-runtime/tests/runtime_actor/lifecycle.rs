use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use tracedecay_rusqlite_runtime::{
    CheckpointBlockers, CheckpointOutcome, CheckpointPressure, CheckpointRequest, CheckpointStatus,
    WriterState,
};
use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, OperationPriorityV1, RuntimeCancellationStageV1,
    RuntimeSubmitOutcomeV1, StoreCommitReceiptV1, UnavailableReasonV1,
};

use crate::support::{
    ExecutorControl, LifecycleBarrier, TestBinding, TestDatabase, TestProbe, marker_count, release,
    request, runtime, table_count, unwrap_arc, writer,
};

#[test]
fn cancellation_before_commit_rolls_back_and_after_commit_returns_the_receipt() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.cancel");
    let before = request(
        binding,
        "operation.cancel.before",
        "key.cancel.before",
        'a',
        OperationPriorityV1::Foreground,
    );
    let before_state = Arc::new(AtomicU8::new(0));
    let before_barrier = LifecycleBarrier::default();
    let before_writer = Arc::new(writer(
        &database,
        &before,
        AdmissionConfigV1::default(),
        ExecutorControl {
            after_mutation: Some(before_barrier.clone()),
            ..ExecutorControl::default()
        },
    ));
    runtime().block_on(async {
        let task_writer = Arc::clone(&before_writer);
        let before_probe = TestProbe::controlled(&before, Arc::clone(&before_state));
        let task = tokio::spawn(async move { task_writer.submit(before, before_probe).await });
        tokio::task::yield_now().await;
        before_barrier.wait_until_arrived();
        before_state.store(1, Ordering::SeqCst);
        before_barrier.release();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
                stage: RuntimeCancellationStageV1::BeforeCommit,
                ..
            }
        ));
    });
    unwrap_arc(before_writer).shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database), 0);
    for table in [
        "td_runtime_writer_checkpoint_v1",
        "td_runtime_writer_idempotency_v1",
        "td_runtime_writer_outbox_v1",
    ] {
        assert_eq!(
            table_count(&database, table),
            0,
            "{table} must roll back with a pre-commit cancellation"
        );
    }

    let after = request(
        binding,
        "operation.cancel.after",
        "key.cancel.after",
        'b',
        OperationPriorityV1::Foreground,
    );
    let after_writer = Arc::new(writer(
        &database,
        &after,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    ));
    let after_state = Arc::new(AtomicU8::new(0));
    let after_barrier = LifecycleBarrier::default();
    runtime().block_on(async {
        let task_writer = Arc::clone(&after_writer);
        let probe = TestProbe::pause_after_commit(
            &after,
            Arc::clone(&after_state),
            &database,
            after_barrier.clone(),
        );
        let task = tokio::spawn(async move { task_writer.submit(after, probe).await });
        tokio::task::yield_now().await;
        after_barrier.wait_until_arrived();
        after_state.store(1, Ordering::SeqCst);
        after_barrier.release();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::CommittedAfterCancellation {
                receipt: StoreCommitReceiptV1 {
                    commit_sequence: CommitSequenceV1(1),
                    ..
                },
                ..
            }
        ));
    });
    unwrap_arc(after_writer).shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database), 1);
}

#[test]
fn drain_rejects_new_work_but_joins_after_accepted_work_replies() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.drain");
    let accepted = request(
        binding,
        "operation.drain.accepted",
        "key.drain.accepted",
        'a',
        OperationPriorityV1::Foreground,
    );
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writer = Arc::new(writer(
        &database,
        &accepted,
        AdmissionConfigV1::default(),
        ExecutorControl {
            entered: Some(entered_tx),
            release: Some(Arc::clone(&gate)),
            ..ExecutorControl::default()
        },
    ));
    runtime().block_on(async {
        let task_writer = Arc::clone(&writer);
        let probe = TestProbe::fixed(&accepted);
        let task = tokio::spawn(async move { task_writer.submit(accepted, probe).await });
        tokio::task::yield_now().await;
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.begin_drain();
        assert_eq!(writer.state(), WriterState::Draining);

        let rejected = request(
            binding,
            "operation.drain.rejected",
            "key.drain.rejected",
            'b',
            OperationPriorityV1::Foreground,
        );
        assert_eq!(
            writer
                .submit(rejected.clone(), TestProbe::fixed(&rejected))
                .await
                .unwrap(),
            RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::Draining
            }
        );
        release(&gate);
        assert!(matches!(
            task.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed { .. }
        ));
    });
    unwrap_arc(writer).shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database), 1);
}

#[test]
fn checkpoint_handle_is_the_external_mount_surface() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.checkpoint");
    let request = request(
        binding,
        "operation.checkpoint",
        "key.checkpoint",
        'c',
        OperationPriorityV1::Health,
    );
    let writer = writer(
        &database,
        &request,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    );
    let checkpoint = writer.checkpoint_handle();

    assert_eq!(checkpoint.binding(), writer.binding());
    assert_eq!(checkpoint.status(), CheckpointStatus::default());
    assert_eq!(checkpoint.pressure(), CheckpointPressure::Open);
    let runtime = runtime();
    runtime
        .block_on(writer.submit(request.clone(), TestProbe::fixed(&request)))
        .unwrap();
    let outcome = runtime
        .block_on(async {
            checkpoint
                .trigger(CheckpointRequest::new(
                    CheckpointBlockers::default(),
                    TestProbe::fixed(&request),
                ))
                .unwrap()
                .wait()
                .await
        })
        .unwrap();
    assert!(matches!(outcome, CheckpointOutcome::BelowSoft { .. }));

    writer.shutdown_and_join().unwrap();
    assert_eq!(checkpoint.pressure(), CheckpointPressure::Open);
}
