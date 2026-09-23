//! Writer-transaction counters for observation admission batching.
//!
//! These tests count durable product transactions (`RuntimeTransactionScopeV1`
//! rows in the rusqlite-runtime idempotency ledger), not elapsed time.

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationCollisionOutcomeV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, RetrievalAnchorRecord,
    RetrievalAnchorRecordParts, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, FOREGROUND_BATCH_MAX_OPERATIONS,
    ObservationBatchPersistOutcome, ObservationPersistOutcome, ObservationStore,
    ObservationStoreError, ObservationWrite,
};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Event, Metadata, Subscriber};

use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

const BATCH_PROVIDER: &str = "observation-batch-test";
const BATCH_SIZE: usize = 8;
const ABOVE_WRITER_COALESCING_LIMIT: usize = FOREGROUND_BATCH_MAX_OPERATIONS as usize + 1;
const OBSERVATION_DISPATCH_TRACE_TARGET: &str = "tracedecay::observation_admission_work";
const OBSERVATION_SNAPSHOT_TRACE_TARGET: &str = "tracedecay::observation_snapshot_query";

#[derive(Default)]
struct ObservationWorkTrace {
    commands: AtomicU64,
    snapshot_probes: AtomicU64,
}

struct ObservationWorkSubscriber {
    trace: Arc<ObservationWorkTrace>,
}

struct ObservationWorkVisitor<'a> {
    trace: &'a ObservationWorkTrace,
}

impl Visit for ObservationWorkVisitor<'_> {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "work" && value == "runtime_command" {
            self.trace.commands.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Subscriber for ObservationWorkSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == OBSERVATION_DISPATCH_TRACE_TARGET
            || metadata.target() == OBSERVATION_SNAPSHOT_TRACE_TARGET
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if event.metadata().target() == OBSERVATION_SNAPSHOT_TRACE_TARGET {
            self.trace.snapshot_probes.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut visitor = ObservationWorkVisitor { trace: &self.trace };
        event.record(&mut visitor);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

async fn persist_with_work_census(
    store: &impl ObservationStore,
    writes: Vec<AnchoredObservationWrite>,
) -> (Vec<ObservationBatchPersistOutcome>, u64, u64) {
    // Keeps this census immune to another test thread poisoning the
    // process-global callsite interest cache; see the helper's documentation.
    crate::tests::harness::install_tracing_callsite_keepalive();
    let trace = Arc::new(ObservationWorkTrace::default());
    let dispatch = Dispatch::new(ObservationWorkSubscriber {
        trace: Arc::clone(&trace),
    });
    let guard = tracing::dispatcher::set_default(&dispatch);
    let outcomes = store.persist_observations(writes).await.unwrap();
    drop(guard);
    (
        outcomes,
        trace.commands.load(Ordering::Relaxed),
        trace.snapshot_probes.load(Ordering::Relaxed),
    )
}

/// Committed writer operations, read from the shard checkpoint: every
/// operation advances its commit sequence by one and opens its own
/// `RuntimeTransactionScopeV1`, whether or not the ledger keeps a replay row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WriterTxnCensus {
    operations: i64,
}

async fn writer_txn_census(runtime: &HostAdmissionTestRuntimeV1) -> WriterTxnCensus {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT COALESCE(SUM(commit_sequence), 0) FROM td_runtime_writer_checkpoint_v1",
            (),
        )
        .await
        .expect("query writer idempotency ledger");
    let row = rows
        .next()
        .await
        .expect("read writer ledger census")
        .expect("writer ledger census row");
    WriterTxnCensus {
        operations: row.get::<i64>(0).expect("operation count"),
    }
}

async fn initialize_writer_authority(
    runtime: &HostAdmissionTestRuntimeV1,
    store: &impl ObservationStore,
) {
    let session_id = SessionId::new("session.observation-batch.writer-authority").unwrap();
    let write = sequential_writes(&session_id, 1)
        .pop()
        .expect("writer authority bootstrap observation");
    assert!(matches!(
        store.persist_observation(write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    assert_eq!(
        writer_txn_census(runtime).await,
        WriterTxnCensus { operations: 1 }
    );
}

fn fixture_receipt(receipt_id: &str, payload: &serde_json::Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.observation-batch.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn sequential_observation(
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
) -> DurableObservationV1 {
    let provider = ProviderId::new(BATCH_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let record = ObservationId::new(format!("record.batch.{ordinal}")).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new(format!("message.batch.{ordinal}")).unwrap());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::FileBytes,
        record,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(
            &format!("receipt.batch.{}.{ordinal}", session_id.as_str()),
            &payload,
        ),
        RetentionClass::new("retention.observation-batch").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_write(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.observation-batch.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "observation-batch-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

fn sequential_writes(session_id: &SessionId, count: usize) -> Vec<AnchoredObservationWrite> {
    sequential_writes_with_text(session_id, count, |ordinal| format!("frame {ordinal}"))
}

fn sequential_writes_with_text(
    session_id: &SessionId,
    count: usize,
    text: impl Fn(u64) -> String,
) -> Vec<AnchoredObservationWrite> {
    let mut writes = Vec::with_capacity(count);
    let mut expected = None;
    for ordinal in 0..u64::try_from(count).expect("batch fits u64") {
        let observation = sequential_observation(session_id, ordinal, &text(ordinal));
        let write = anchored_write(observation, expected);
        expected = Some(write.next_cursor().clone());
        writes.push(write);
    }
    writes
}

fn with_retrieval_alias(
    write: &AnchoredObservationWrite,
    alias: tracedecay_domain::NativeAlias,
) -> AnchoredObservationWrite {
    let retained = write.retrieval_anchor();
    let anchor = RetrievalAnchorRecord::new(RetrievalAnchorRecordParts {
        target: retained.target().clone(),
        owner: retained.owner().clone(),
        aliases: vec![alias],
        occurred_at: retained.occurred_at(),
        ingested_at: retained.ingested_at(),
        evidence_class: retained.evidence_class(),
        source_generation: retained.source_generation().clone(),
        projection_generation: retained.projection_generation().clone(),
        projection_watermark: retained.projection_watermark().clone(),
        coverage: retained.coverage().clone(),
        source_observations: retained.source_observations().to_vec(),
        source_anchors: retained.source_anchors().to_vec(),
        authorization: retained.authorization().clone(),
        payload_access: retained.payload_access(),
        retention_class: retained.retention_class().clone(),
        durability: retained.durability().clone(),
    })
    .unwrap();
    AnchoredObservationWrite::new(
        write.write().clone(),
        anchor,
        write.projection_generation().clone(),
    )
    .unwrap()
}

fn colliding_rewrite(
    session_id: &SessionId,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let provider = ProviderId::new(BATCH_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record = ObservationId::new("record.batch.0").unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new("message.batch.0").unwrap());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": "rewritten colliding frame"}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(2).unwrap(),
        range,
        ObservationOrderingDomainV1::FileBytes,
        record,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt("receipt.batch.collision", &payload),
        RetentionClass::new("retention.observation-batch").unwrap(),
        payload,
    )
    .unwrap();
    anchored_write(observation, expected_cursor)
}

#[tokio::test]
async fn n_persist_observation_calls_open_n_writer_transactions() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    initialize_writer_authority(&runtime, &store).await;
    let session_id = SessionId::new("session.observation-batch.one-by-one").unwrap();
    let writes = sequential_writes(&session_id, BATCH_SIZE);
    let before = writer_txn_census(&runtime).await;
    for write in writes {
        assert!(matches!(
            store.persist_observation(write).await.unwrap(),
            ObservationPersistOutcome::Committed(_)
        ));
    }
    let after = writer_txn_census(&runtime).await;
    assert_eq!(after.operations - before.operations, BATCH_SIZE as i64);
}

#[tokio::test]
async fn persist_observations_opens_one_writer_transaction_for_the_batch() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    initialize_writer_authority(&runtime, &store).await;
    let session_id = SessionId::new("session.observation-batch.one-txn").unwrap();
    let writes = sequential_writes(&session_id, ABOVE_WRITER_COALESCING_LIMIT);
    let expected_observation_ids = writes
        .iter()
        .map(|write| write.observation().observation_id().clone())
        .collect::<Vec<_>>();
    let before = writer_txn_census(&runtime).await;
    let outcomes = store.persist_observations(writes).await.unwrap();
    assert_eq!(outcomes.len(), ABOVE_WRITER_COALESCING_LIMIT);
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome(), ObservationPersistOutcome::Committed(_)))
    );
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.outcome().receipt().observation().observation_id())
            .collect::<Vec<_>>(),
        expected_observation_ids.iter().collect::<Vec<_>>(),
        "batch receipt hydration must preserve caller order"
    );
    assert!(
        outcomes.windows(2).all(|pair| {
            pair[0].outcome().receipt().sequence() < pair[1].outcome().receipt().sequence()
        }),
        "writer authority must commit the ordered input frontier"
    );
    let after = writer_txn_census(&runtime).await;
    assert_eq!(
        after.operations - before.operations,
        1,
        "the bounded batch must be one admitted writer operation"
    );
}

#[tokio::test]
async fn intra_batch_exact_duplicate_is_hydrated_as_a_duplicate() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    initialize_writer_authority(&runtime, &store).await;
    let session_id = SessionId::new("session.observation-batch.intra-exact").unwrap();
    let first = sequential_writes(&session_id, 1)
        .pop()
        .expect("intra-batch exact observation");
    let before = writer_txn_census(&runtime).await;

    let outcomes = store
        .persist_observations(vec![first.clone(), first.clone()])
        .await
        .unwrap();

    assert!(matches!(
        outcomes[0].outcome(),
        ObservationPersistOutcome::Committed(_)
    ));
    assert!(matches!(
        outcomes[1].outcome(),
        ObservationPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(outcomes[0].stored(), outcomes[1].stored());
    let after = writer_txn_census(&runtime).await;
    assert_eq!(after.operations - before.operations, 1);
}

async fn open_profile_store() -> (
    TempDir,
    HostAdmissionTestRuntimeV1,
    crate::GlobalDbObservationStore,
) {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    initialize_writer_authority(&runtime, &store).await;
    (tmp, runtime, store)
}

/// Durable effect of admitting writes in order: each settled outcome kind up
/// to the first error, that error, which observations are retained, and the
/// resulting source cursor of the first write's source.
#[derive(Debug, PartialEq, Eq)]
struct AdmissionEffect {
    outcomes: Vec<std::mem::Discriminant<ObservationPersistOutcome>>,
    error: Option<String>,
    retained: Vec<bool>,
    cursor: Option<String>,
}

async fn admission_effect(
    store: &crate::GlobalDbObservationStore,
    writes: &[AnchoredObservationWrite],
    outcomes: Vec<std::mem::Discriminant<ObservationPersistOutcome>>,
    error: Option<String>,
) -> AdmissionEffect {
    let mut retained = Vec::with_capacity(writes.len());
    for write in writes {
        retained.push(
            store
                .get_observation(write.observation().observation_id())
                .await
                .unwrap()
                .is_some(),
        );
    }
    let source = writes[0].observation();
    let cursor = store
        .get_source_cursor(source.source(), source.scope())
        .await
        .unwrap()
        .map(|cursor| format!("{cursor:?}"));
    AdmissionEffect {
        outcomes,
        error,
        retained,
        cursor,
    }
}

/// Admits `writes` as one batch and, on a separate fresh store, as sequential
/// single writes, and requires both to leave the same durable effect. Returns
/// the batch result for case-specific assertions.
async fn assert_batch_settles_like_sequential_writes(
    writes: Vec<AnchoredObservationWrite>,
) -> (
    Result<Vec<ObservationBatchPersistOutcome>, ObservationStoreError>,
    AdmissionEffect,
) {
    let (_batch_tmp, _batch_runtime, batch_store) = open_profile_store().await;
    let batch = batch_store.persist_observations(writes.clone()).await;
    let batch_effect = match &batch {
        Ok(outcomes) => {
            admission_effect(
                &batch_store,
                &writes,
                outcomes
                    .iter()
                    .map(|outcome| std::mem::discriminant(outcome.outcome()))
                    .collect(),
                None,
            )
            .await
        }
        Err(error) => {
            admission_effect(&batch_store, &writes, Vec::new(), Some(format!("{error:?}"))).await
        }
    };

    let (_scalar_tmp, _scalar_runtime, scalar_store) = open_profile_store().await;
    let mut outcomes = Vec::new();
    let mut error = None;
    for write in writes.iter().cloned() {
        match scalar_store.persist_observation(write).await {
            Ok(outcome) => outcomes.push(std::mem::discriminant(&outcome)),
            Err(scalar_error) => {
                error = Some(format!("{scalar_error:?}"));
                outcomes.clear();
                break;
            }
        }
    }
    let scalar_effect = admission_effect(&scalar_store, &writes, outcomes, error).await;
    assert_eq!(
        batch_effect, scalar_effect,
        "a batch must settle exactly as the same writes admitted one at a time"
    );
    (batch, batch_effect)
}

#[tokio::test]
async fn intra_batch_identity_rewrite_settles_like_sequential_writes() {
    let session_id = SessionId::new("session.observation-batch.intra-rewrite").unwrap();
    let first = sequential_writes(&session_id, 1)
        .pop()
        .expect("intra-batch retained observation");
    let rewritten = colliding_rewrite(&session_id, Some(first.next_cursor().clone()));

    let (batch, effect) = assert_batch_settles_like_sequential_writes(vec![first, rewritten]).await;

    assert!(matches!(
        batch,
        Err(ObservationStoreError::ObservationCollision { .. })
    ));
    assert!(
        effect.retained[0],
        "the committed predecessor stays durable"
    );
}

#[tokio::test]
async fn intra_batch_receipt_collision_settles_like_sequential_writes() {
    let session_id = SessionId::new("session.observation-batch.intra-receipt").unwrap();
    let mut writes = sequential_writes(&session_id, 2);
    let first = writes.remove(0);
    let second = writes.remove(0);
    let observation = second.observation();
    let conflicting_observation = DurableObservationV1::new(
        observation.identity().clone(),
        fixture_receipt(
            first
                .observation()
                .receipt()
                .receipt()
                .receipt_id()
                .as_str(),
            observation.payload(),
        ),
        observation.retention_class().clone(),
        observation.payload().clone(),
    )
    .unwrap();
    let conflicting = anchored_write(conflicting_observation, second.expected_cursor().cloned());

    let (batch, effect) =
        assert_batch_settles_like_sequential_writes(vec![first, conflicting]).await;

    assert!(batch.is_err(), "a reused receipt id must not commit");
    assert_eq!(effect.retained, vec![true, false]);
}

#[tokio::test]
async fn intra_batch_alias_collision_settles_like_sequential_writes() {
    let session_id = SessionId::new("session.observation-batch.intra-alias").unwrap();
    let mut writes = sequential_writes(&session_id, 2);
    let first = writes.remove(0);
    let alias = first
        .retrieval_anchor()
        .aliases()
        .first()
        .cloned()
        .expect("observation retrieval alias");
    let second = with_retrieval_alias(&writes.remove(0), alias);

    let (batch, effect) = assert_batch_settles_like_sequential_writes(vec![first, second]).await;

    assert!(matches!(
        batch,
        Err(ObservationStoreError::RetrievalAnchorAliasCollision { .. })
    ));
    assert_eq!(effect.retained, vec![true, false]);
}

#[tokio::test(flavor = "current_thread")]
async fn persist_observations_dispatches_one_runtime_command_independent_of_batch_size() {
    let mut expected_snapshot_probes = None;
    for count in [1, BATCH_SIZE, ABOVE_WRITER_COALESCING_LIMIT] {
        let tmp = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        initialize_writer_authority(&runtime, &store).await;
        let session_id =
            SessionId::new(format!("session.observation-batch.dispatch-census.{count}")).unwrap();
        let before = writer_txn_census(&runtime).await;

        let (outcomes, runtime_commands, snapshot_probes) =
            persist_with_work_census(&store, sequential_writes(&session_id, count)).await;

        assert_eq!(outcomes.len(), count);
        assert!(
            outcomes.iter().all(|outcome| matches!(
                outcome.outcome(),
                ObservationPersistOutcome::Committed(_)
            ))
        );
        assert_eq!(
            runtime_commands, 1,
            "preflight and receipt hydration must use one bounded snapshot; only the writer batch is dispatched"
        );
        match expected_snapshot_probes {
            Some(expected) => assert_eq!(
                snapshot_probes, expected,
                "snapshot query probes must remain constant as batch size grows"
            ),
            None => {
                assert!(
                    snapshot_probes > 0,
                    "the census must observe snapshot reads"
                );
                expected_snapshot_probes = Some(snapshot_probes);
            }
        }
        let after = writer_txn_census(&runtime).await;
        assert_eq!(after.operations - before.operations, 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn persist_observations_partitions_large_windows_by_exact_admission_bytes() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    initialize_writer_authority(&runtime, &store).await;
    let session_id = SessionId::new("session.observation-batch.byte-partition").unwrap();
    let writes = sequential_writes_with_text(&session_id, 256, |ordinal| {
        format!("large frame {ordinal} {}", "x".repeat(15_000))
    });
    let before = writer_txn_census(&runtime).await;

    let (outcomes, runtime_commands, _) = persist_with_work_census(&store, writes).await;

    assert_eq!(outcomes.len(), 256);
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.outcome(), ObservationPersistOutcome::Committed(_)))
    );
    assert!(
        runtime_commands > 1,
        "the oversized source window must be split before runtime admission"
    );
    let after = writer_txn_census(&runtime).await;
    assert_eq!(
        after.operations - before.operations,
        runtime_commands as i64
    );
}

#[tokio::test]
async fn failed_batch_preflight_commits_no_valid_prefix() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    initialize_writer_authority(&runtime, &store).await;
    let session_id = SessionId::new("session.observation-batch.atomic-preflight").unwrap();
    let writes = sequential_writes(&session_id, 2);
    let first = writes[0].clone();
    let second = writes[1].clone();
    let conflicting_expected = first
        .next_cursor()
        .clone()
        .with_resume_checkpoint(0xdead_beef, 0xbaad_f00d);
    let invalid_second = anchored_write(second.observation().clone(), Some(conflicting_expected));
    let before = writer_txn_census(&runtime).await;

    let error = store
        .persist_observations(vec![first.clone(), invalid_second])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ObservationStoreError::CursorConflict { .. }
    ));
    assert_eq!(
        writer_txn_census(&runtime).await,
        before,
        "a failed ordered preflight must not admit a writer transaction"
    );
    assert!(
        store
            .get_observation(first.observation().observation_id())
            .await
            .unwrap()
            .is_none(),
        "a rejected batch must not commit its valid prefix"
    );
    assert!(
        store
            .get_source_cursor(first.observation().source(), first.observation().scope())
            .await
            .unwrap()
            .is_none(),
        "a rejected batch must not advance its source cursor"
    );
}

#[tokio::test]
async fn persist_observations_keeps_cursor_cas_collision_and_file_identity() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.observation-batch.authority").unwrap();
    let writes = sequential_writes(&session_id, 2);
    let first = writes[0].clone();
    let second = writes[1].clone();
    store.persist_observation(first.clone()).await.unwrap();

    let stale = store
        .persist_observations(vec![first.clone()])
        .await
        .unwrap();
    assert!(matches!(
        stale[0].outcome(),
        ObservationPersistOutcome::ExactDuplicate(_)
    ));

    let cas_error = store
        .persist_observations(vec![anchored_write(
            sequential_observation(&session_id, 2, "stale expected"),
            Some(second.next_cursor().clone()),
        )])
        .await
        .unwrap_err();
    assert!(matches!(
        cas_error,
        ObservationStoreError::CursorConflict { .. }
    ));

    let collision = store
        .persist_observations(vec![colliding_rewrite(
            &session_id,
            Some(first.next_cursor().clone()),
        )])
        .await
        .unwrap_err();
    assert!(matches!(
        collision,
        ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        }
    ));

    let resume_session = SessionId::new("session.observation-batch.resume-authority").unwrap();
    let resume_write = sequential_writes(&resume_session, 1)
        .pop()
        .expect("resume checkpoint observation");
    let resume = resume_write
        .next_cursor()
        .clone()
        .with_resume_checkpoint(0xfeed_face, 0xcafe_babe);
    let resumed = ObservationWrite::new(
        resume_write.observation().clone(),
        resume_write.expected_cursor().cloned(),
        resume,
    )
    .unwrap();
    let projection = resume_write.projection_generation().clone();
    let resumed =
        AnchoredObservationWrite::new(resumed, resume_write.retrieval_anchor().clone(), projection)
            .unwrap();
    let outcomes = store.persist_observations(vec![resumed]).await.unwrap();
    assert!(matches!(
        outcomes[0].outcome(),
        ObservationPersistOutcome::Committed(_)
    ));
    let cursor = store
        .get_source_cursor(
            resume_write.observation().source(),
            resume_write.observation().scope(),
        )
        .await
        .unwrap()
        .expect("committed cursor");
    assert_eq!(cursor.file_identity(), Some(0xfeed_face));
    assert_eq!(cursor.resume_fingerprint(), Some(0xcafe_babe));
}

#[tokio::test]
async fn persist_observations_recovers_a_peer_commit_as_exact_duplicate() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.observation-batch.peer-commit").unwrap();
    let writes = sequential_writes(&session_id, 1);
    let left_store = store.clone();
    let right_store = store.clone();
    let left_writes = writes.clone();
    let right_writes = writes;
    let (left, right) = tokio::join!(
        left_store.persist_observations(left_writes),
        right_store.persist_observations(right_writes),
    );
    let left = left.expect("concurrent persist must not surface Storage");
    let right = right.expect("concurrent persist must not surface Storage");
    assert_eq!(left.len(), 1);
    assert_eq!(right.len(), 1);
    let persist_class = |outcome: &ObservationPersistOutcome| match outcome {
        ObservationPersistOutcome::Committed(_) => "committed",
        ObservationPersistOutcome::ExactDuplicate(_) => "exact_duplicate",
        ObservationPersistOutcome::CoveredDuplicate(_) => "covered_duplicate",
    };
    let left_class = persist_class(left[0].outcome());
    let right_class = persist_class(right[0].outcome());
    let attached = left_class == right_class;
    let serialized_replay = matches!(
        (left_class, right_class),
        ("committed", "exact_duplicate" | "covered_duplicate")
            | ("exact_duplicate" | "covered_duplicate", "committed")
    );
    assert!(
        attached || serialized_replay,
        "identical peer submits must attach to one writer outcome or serialize as commit+replay, got {left:?} / {right:?}"
    );
}
