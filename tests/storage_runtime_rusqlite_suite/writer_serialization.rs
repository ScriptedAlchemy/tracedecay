use std::sync::Arc;

use tracedecay_rusqlite_runtime::read_consistency::{CommitWatermarkSource, WatermarkSourceState};
use tracedecay_store::{CommitSequenceV1, RuntimeSubmitOutcomeV1};

use crate::runtime_test_support::{
    Probe, TestDatabase, outbox_request, run, writer, writer_runtime_fixture,
};

#[test]
fn writer_serializes_commit_checkpoints_and_publishes_only_committed_watermarks() {
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
    let watermarks = writer.commit_watermark_source();

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
    assert_eq!(
        watermarks.current(&fixture.origin_binding.shard_id),
        WatermarkSourceState::Available(tracedecay_store::ShardWatermarkV1 {
            shard_id: fixture.origin_binding.shard_id.clone(),
            incarnation: fixture.origin_binding.incarnation,
            authority_epoch: fixture.origin_binding.authority_epoch,
            commit_sequence: CommitSequenceV1(2),
        })
    );

    Arc::try_unwrap(writer)
        .unwrap_or_else(|_| panic!("submit tasks retained the writer"))
        .shutdown_and_join()
        .expect("close writer");
}
