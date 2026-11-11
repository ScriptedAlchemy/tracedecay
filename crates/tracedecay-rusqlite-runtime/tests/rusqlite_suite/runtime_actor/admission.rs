use std::pin::pin;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::task::{Context, Poll, Waker};

use tracedecay_store::{
    AdmissionConfigV1, BatchBudgetV1, CommitSequenceV1, OperationPriorityV1, QueueBudgetV1,
    RuntimeSubmitOutcomeV1, SaturationScopeV1, StoreCommitReceiptV1,
};

use super::support::{
    ExecutorControl, ReleaseOnDrop, TestBinding, TestDatabase, TestProbe, marker_count, release,
    request, runtime, unwrap_arc, writer,
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
    let _open_gate_on_failure = ReleaseOnDrop(Arc::clone(&gate));
    runtime().block_on(async {
        let first_writer = Arc::clone(&writer);
        let first_probe = TestProbe::fixed(&first);
        let first_task = tokio::spawn(async move { first_writer.submit(first, first_probe).await });
        tokio::task::yield_now().await;
        entered_rx.recv().unwrap();

        let overflow = request(
            binding,
            "operation.overload.shed",
            "key.overload.shed",
            'b',
            OperationPriorityV1::Foreground,
        );
        // Immediate means the overflow is refused on its first poll, without
        // ever waiting for the occupied queue to drain.
        let outcome = match pin!(writer.submit(overflow.clone(), TestProbe::fixed(&overflow)))
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(outcome) => outcome.unwrap(),
            Poll::Pending => panic!("saturated overflow waited for queue capacity"),
        };
        assert!(
            matches!(
                outcome,
                RuntimeSubmitOutcomeV1::Saturated {
                    scope: SaturationScopeV1::ShardOperations,
                    ..
                }
            ),
            "occupied general lane must refuse overflow immediately: {outcome:?}"
        );

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
            RuntimeSubmitOutcomeV1::Committed {
                receipt: StoreCommitReceiptV1 {
                    commit_sequence: CommitSequenceV1(1),
                    ..
                }
            }
        ));
        assert!(matches!(
            health_task.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed {
                receipt: StoreCommitReceiptV1 {
                    commit_sequence: CommitSequenceV1(2),
                    ..
                }
            }
        ));
    });
    unwrap_arc(writer).shutdown_and_join().unwrap();
    assert_eq!(
        marker_count(&database),
        2,
        "first and health commits persist; the shed overflow must not"
    );
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
        Ok(RuntimeSubmitOutcomeV1::Committed {
            receipt: StoreCommitReceiptV1 {
                commit_sequence: CommitSequenceV1(1),
                ..
            }
        })
    ));
    let background = request(
        binding,
        "operation.priority-ceiling.background",
        "key.priority-ceiling.background",
        'b',
        OperationPriorityV1::Background,
    );
    assert!(matches!(
        runtime().block_on(writer.submit(background.clone(), TestProbe::fixed(&background))),
        Ok(RuntimeSubmitOutcomeV1::Saturated {
            scope: SaturationScopeV1::ShardBytes,
            ..
        })
    ));
    writer.shutdown_and_join().unwrap();
    assert_eq!(
        marker_count(&database),
        1,
        "the over-ceiling background request must not persist"
    );
}
