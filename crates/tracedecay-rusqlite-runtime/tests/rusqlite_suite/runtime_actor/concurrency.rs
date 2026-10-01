use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, OperationPriorityV1, RuntimeSubmitOutcomeV1,
    StoreCommitReceiptV1,
};

use super::support::{
    ExecutorControl, ReleaseOnDrop, TestBinding, TestDatabase, TestProbe, marker_count, release,
    request, runtime, unwrap_arc, writer,
};

#[test]
fn independent_shards_make_progress_on_distinct_threads_and_connections() {
    let database_a = TestDatabase::new();
    let database_b = TestDatabase::new();
    let request_a = request(
        TestBinding::project("project.shard.a"),
        "operation.shard.a",
        "key.shard.a",
        'a',
        OperationPriorityV1::Foreground,
    );
    let request_b = request(
        TestBinding::project("project.shard.b"),
        "operation.shard.b",
        "key.shard.b",
        'b',
        OperationPriorityV1::Foreground,
    );
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writer_a = Arc::new(writer(
        &database_a,
        &request_a,
        AdmissionConfigV1::default(),
        ExecutorControl {
            entered: Some(entered_tx),
            release: Some(Arc::clone(&gate)),
            ..ExecutorControl::default()
        },
    ));
    let writer_b = writer(
        &database_b,
        &request_b,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    );
    let _open_gate_on_failure = ReleaseOnDrop(Arc::clone(&gate));
    runtime().block_on(async {
        let task_writer = Arc::clone(&writer_a);
        let probe_a = TestProbe::fixed(&request_a);
        let task_a = tokio::spawn(async move { task_writer.submit(request_a, probe_a).await });
        tokio::task::yield_now().await;
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // Writer A stays parked inside its open transaction until the gate
        // opens, so B can only commit on a thread and connection of its own.
        assert!(matches!(
            writer_b
                .submit(request_b.clone(), TestProbe::fixed(&request_b))
                .await
                .unwrap(),
            RuntimeSubmitOutcomeV1::Committed {
                receipt: StoreCommitReceiptV1 {
                    commit_sequence: CommitSequenceV1(1),
                    ..
                }
            }
        ));
        assert!(!task_a.is_finished());
        assert_eq!(marker_count(&database_a), 0);
        assert_eq!(marker_count(&database_b), 1);
        release(&gate);
        assert!(matches!(
            task_a.await.unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed {
                receipt: StoreCommitReceiptV1 {
                    commit_sequence: CommitSequenceV1(1),
                    ..
                }
            }
        ));
    });
    unwrap_arc(writer_a).shutdown_and_join().unwrap();
    writer_b.shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database_a), 1);
    assert_eq!(marker_count(&database_b), 1);
}
