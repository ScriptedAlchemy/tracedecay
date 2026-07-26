use std::sync::Arc;

use tracedecay_rusqlite_runtime::read_consistency::{CommitWatermarkSource, WatermarkSourceState};
use tracedecay_store::{CommitSequenceV1, RuntimeSubmitOutcomeV1};

use crate::cutover_support::{Probe, TestDatabase, fixture, outbox_request, run, writer};

#[test]
fn writer_serializes_commit_checkpoints_and_publishes_only_committed_watermarks() {
    let fixture = fixture();
    let s9 = fixture.s9;
    let s10 = fixture.s10;
    let database = TestDatabase::new("s10-serialized.sqlite3");
    let first = outbox_request(
        &s9.origin_binding,
        &s9.target_binding,
        "operation.cutover.serialized.first",
        &format!("{}.first", s10.effect_id),
        &format!("{}.first", s10.ordering_key),
    );
    let second = outbox_request(
        &s9.origin_binding,
        &s9.target_binding,
        "operation.cutover.serialized.second",
        &format!("{}.second", s10.effect_id),
        &format!("{}.second", s10.ordering_key),
    );
    let writer = Arc::new(writer(&database, &s9.origin_binding));
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
    assert_eq!(sequences, s10.commit_sequences);
    assert_eq!(
        watermarks.current(&s9.origin_binding.shard_id),
        WatermarkSourceState::Available(tracedecay_store::ShardWatermarkV1 {
            shard_id: s9.origin_binding.shard_id.clone(),
            incarnation: s9.origin_binding.incarnation,
            authority_epoch: s9.origin_binding.authority_epoch,
            commit_sequence: CommitSequenceV1(2),
        })
    );

    Arc::try_unwrap(writer)
        .unwrap_or_else(|_| panic!("submit tasks retained the S10 writer"))
        .shutdown_and_join()
        .expect("close S10 writer");
}
