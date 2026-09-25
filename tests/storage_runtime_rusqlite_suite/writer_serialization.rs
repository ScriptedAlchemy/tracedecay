use std::sync::Arc;

use tracedecay_store::RuntimeSubmitOutcomeV1;

use super::runtime_test_support::{
    Probe, TestDatabase, outbox_request, run, writer, writer_runtime_fixture,
};

#[test]
fn writer_serializes_concurrent_commits_into_ordered_receipts() {
    let fixture = writer_runtime_fixture();
    let database = TestDatabase::new("writer-serialized.sqlite3");
    let first = outbox_request(
        &fixture.origin_binding,
        &fixture.target_binding,
        "operation.runtime.serialized.first",
        &format!("{}.first", fixture.effect_id),
        &format!("{}.first", fixture.ordering_key),
    );
    let second = outbox_request(
        &fixture.origin_binding,
        &fixture.target_binding,
        "operation.runtime.serialized.second",
        &format!("{}.second", fixture.effect_id),
        &format!("{}.second", fixture.ordering_key),
    );
    let writer = Arc::new(writer(&database, &fixture.origin_binding));

    let mut sequences = run(async {
        let first_writer = Arc::clone(&writer);
        let first_probe = Probe::for_submit(&first);
        let first_task = tokio::spawn(async move { first_writer.submit(first, first_probe).await });
        let second_writer = Arc::clone(&writer);
        let second_probe = Probe::for_submit(&second);
        let second_task =
            tokio::spawn(async move { second_writer.submit(second, second_probe).await });
        [first_task.await, second_task.await]
            .into_iter()
            .map(|result| {
                let outcome = result
                    .expect("join serialized submit")
                    .expect("execute serialized submit");
                match outcome {
                    RuntimeSubmitOutcomeV1::Committed { receipt } => receipt.commit_sequence.0,
                    outcome => panic!("expected serialized commit, got {outcome:?}"),
                }
            })
            .collect::<Vec<_>>()
    });
    sequences.sort_unstable();
    assert_eq!(sequences, fixture.commit_sequences.to_vec());

    Arc::try_unwrap(writer)
        .unwrap_or_else(|_| panic!("submit tasks retained the writer"))
        .shutdown_and_join()
        .expect("close writer");
}
