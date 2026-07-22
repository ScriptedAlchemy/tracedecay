use tracedecay_store::{AdmissionConfigV1, OperationPriorityV1, RuntimeSubmitOutcomeV1};

use crate::support::{
    ExecutorControl, TestBinding, TestDatabase, TestProbe, marker_count, request, runtime,
    table_count, writer,
};

#[test]
fn restart_is_durable_and_replay_or_conflict_returns_the_original_receipt() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.restart");
    let original = request(
        binding,
        "operation.restart.original",
        "key.restart",
        'a',
        OperationPriorityV1::Foreground,
    );
    let first_writer = writer(
        &database,
        &original,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    );
    let original_receipt = match runtime()
        .block_on(first_writer.submit(original.clone(), TestProbe::fixed(&original)))
        .unwrap()
    {
        RuntimeSubmitOutcomeV1::Committed { receipt } => receipt,
        outcome => panic!("expected commit, got {outcome:?}"),
    };
    first_writer.shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database), 1);

    let retry = request(
        binding,
        "operation.restart.retry",
        "key.restart",
        'a',
        OperationPriorityV1::Foreground,
    );
    let restarted = writer(
        &database,
        &retry,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    );
    assert_eq!(
        runtime()
            .block_on(restarted.submit(retry.clone(), TestProbe::fixed(&retry)))
            .unwrap(),
        RuntimeSubmitOutcomeV1::ExactReplay {
            receipt: original_receipt.clone()
        }
    );
    let conflict = request(
        binding,
        "operation.restart.conflict",
        "key.restart",
        'b',
        OperationPriorityV1::Foreground,
    );
    assert_eq!(
        runtime()
            .block_on(restarted.submit(conflict.clone(), TestProbe::fixed(&conflict)))
            .unwrap(),
        RuntimeSubmitOutcomeV1::IdempotencyConflict {
            existing_receipt: original_receipt
        }
    );
    restarted.shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database), 1, "replay must not execute again");

    for table in [
        "td_runtime_writer_checkpoint_v1",
        "td_runtime_writer_idempotency_v1",
        "td_runtime_writer_outbox_v1",
    ] {
        assert_eq!(
            table_count(&database, table),
            1,
            "{table} must co-commit exactly once"
        );
    }
}
