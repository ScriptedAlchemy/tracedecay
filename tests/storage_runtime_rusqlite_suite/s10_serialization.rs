use std::sync::Arc;

use rusqlite::Savepoint;
use tracedecay_rusqlite_runtime::{
    StorageOperationExecutor,
    effects::{
        EffectCoordinator, EffectDispatchOutcome, EffectUnknownCause,
        SqliteOriginEffectTransactions, SqliteTargetEffectTransactions,
    },
    read_consistency::{CommitWatermarkSource, WatermarkSourceState},
};
use tracedecay_store::{
    CommitSequenceV1, RepositoryWritePayloadV1, RuntimeSubmitOutcomeV1, StoreEffectIdV1,
    TransactionalOutboxEntryV1,
};

use crate::cutover_support::{
    Probe, RecordingEffect, TestDatabase, fixture, outbox_request, run, writer,
    writer_with_executor,
};

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

#[test]
fn uncertain_target_publication_is_durable_and_requires_explicit_retry() {
    let fixture = fixture();
    let s9 = fixture.s9;
    let s10 = fixture.s10;
    let origin = TestDatabase::new("s10-origin.sqlite3");
    let target = TestDatabase::new("s10-target.sqlite3");
    let request = outbox_request(
        &s9.origin_binding,
        &s9.target_binding,
        "operation.cutover.uncertain",
        &s10.effect_id,
        &s10.ordering_key,
    );
    let origin_writer = writer(&origin, &s9.origin_binding);
    assert!(matches!(
        run(origin_writer.submit(request.clone(), Probe::for_submit(&request),))
            .expect("commit uncertain-publication outbox"),
        RuntimeSubmitOutcomeV1::Committed { .. }
    ));
    let effect_id = StoreEffectIdV1::new(s10.effect_id).expect("valid uncertain effect identity");
    let failing_target = writer_with_executor(&target, &s9.target_binding, FailingEffect);
    run(async {
        let mut origin_transactions = SqliteOriginEffectTransactions::open(&origin_writer)
            .expect("open uncertain origin effect view");
        let mut target_transactions = SqliteTargetEffectTransactions::new(&failing_target);
        match EffectCoordinator
            .dispatch(
                &effect_id,
                &s9.origin_binding,
                &s9.target_binding,
                &mut origin_transactions,
                &mut target_transactions,
            )
            .await
        {
            Ok(EffectDispatchOutcome::EffectUnknown(unknown)) => {
                assert!(matches!(unknown.cause, EffectUnknownCause::Target(_)));
                assert_eq!(
                    unknown.entry.state,
                    tracedecay_store::OutboxEffectStateV1::EffectUnknown
                );
            }
            outcome => panic!("expected durable uncertain-publication state: {outcome:?}"),
        }
    });
    failing_target
        .shutdown_and_join()
        .expect("close failing target writer");

    let target_writer = writer_with_executor(&target, &s9.target_binding, RecordingEffect);
    run(async {
        let mut origin_transactions = SqliteOriginEffectTransactions::open(&origin_writer)
            .expect("reopen uncertain origin effect view");
        let mut target_transactions = SqliteTargetEffectTransactions::new(&target_writer);
        assert!(matches!(
            EffectCoordinator
                .dispatch(
                    &effect_id,
                    &s9.origin_binding,
                    &s9.target_binding,
                    &mut origin_transactions,
                    &mut target_transactions,
                )
                .await,
            Ok(EffectDispatchOutcome::Acknowledged {
                replayed: false,
                ..
            })
        ));
    });
    origin_writer
        .shutdown_and_join()
        .expect("close uncertain-publication writer");
    target_writer
        .shutdown_and_join()
        .expect("close successful target writer");
}

#[derive(Clone, Copy)]
struct FailingEffect;

impl StorageOperationExecutor for FailingEffect {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }

    fn apply_inbox(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _entry: &TransactionalOutboxEntryV1,
    ) -> rusqlite::Result<()> {
        Err(rusqlite::Error::InvalidQuery)
    }
}
