//! Writer-transaction counters for observation admission batching.
//!
//! These tests count durable product transactions (`RuntimeTransactionScopeV1`
//! rows in the rusqlite-runtime idempotency ledger), not elapsed time.

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationCollisionOutcomeV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, FOREGROUND_BATCH_MAX_OPERATIONS, ObservationPersistOutcome,
    ObservationStore, ObservationStoreError, ObservationWrite,
};

use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

const BATCH_PROVIDER: &str = "observation-batch-test";
const BATCH_SIZE: usize = 8;
const ABOVE_WRITER_COALESCING_LIMIT: usize = FOREGROUND_BATCH_MAX_OPERATIONS as usize + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WriterTxnCensus {
    operations: i64,
    scopes: i64,
}

async fn writer_txn_census(runtime: &HostAdmissionTestRuntimeV1) -> WriterTxnCensus {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*), COUNT(DISTINCT transaction_scope_json)
             FROM td_runtime_writer_idempotency_v1",
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
        scopes: row.get::<i64>(1).expect("distinct transaction scopes"),
    }
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
        fixture_receipt(&format!("receipt.batch.{ordinal}"), &payload),
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
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

fn sequential_writes(session_id: &SessionId, count: usize) -> Vec<AnchoredObservationWrite> {
    let mut writes = Vec::with_capacity(count);
    let mut expected = None;
    for ordinal in 0..u64::try_from(count).expect("batch fits u64") {
        let observation = sequential_observation(session_id, ordinal, &format!("frame {ordinal}"));
        let write = anchored_write(observation, expected);
        expected = Some(write.next_cursor().clone());
        writes.push(write);
    }
    writes
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
async fn empty_observation_batch_returns_no_outcomes_and_opens_no_writer_txn() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let before = writer_txn_census(&runtime).await;
    let outcomes = store.persist_observations(Vec::new()).await.unwrap();
    assert!(outcomes.is_empty());
    assert_eq!(writer_txn_census(&runtime).await, before);
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
    assert_eq!(
        after.scopes - before.scopes,
        BATCH_SIZE as i64,
        "one persist_observation still opens one RuntimeTransactionScopeV1"
    );
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
    let session_id = SessionId::new("session.observation-batch.one-txn").unwrap();
    let writes = sequential_writes(&session_id, ABOVE_WRITER_COALESCING_LIMIT);
    let before = writer_txn_census(&runtime).await;
    let outcomes = store.persist_observations(writes).await.unwrap();
    assert_eq!(outcomes.len(), ABOVE_WRITER_COALESCING_LIMIT);
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, ObservationPersistOutcome::Committed(_)))
    );
    let after = writer_txn_census(&runtime).await;
    assert_eq!(
        after.operations - before.operations,
        1,
        "the bounded batch must be one admitted writer operation"
    );
    assert_eq!(
        after.scopes - before.scopes,
        1,
        "the bounded batch must share one RuntimeTransactionScopeV1"
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
    let session_id = SessionId::new("session.observation-batch.atomic-preflight").unwrap();
    let writes = sequential_writes(&session_id, 2);
    let first = writes[0].clone();
    let second = writes[1].clone();
    let invalid_second = anchored_write(
        second.observation().clone(),
        Some(second.next_cursor().clone()),
    );

    let error = store
        .persist_observations(vec![first.clone(), invalid_second])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ObservationStoreError::CursorConflict { .. }
    ));
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
        stale.as_slice(),
        [ObservationPersistOutcome::ExactDuplicate(_)]
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

    let resume = second
        .next_cursor()
        .clone()
        .with_resume_checkpoint(0xfeed_face, 0xcafe_babe);
    let resumed = ObservationWrite::new(
        second.observation().clone(),
        second.expected_cursor().cloned(),
        resume,
    )
    .unwrap();
    let projection = second.projection_generation().clone();
    let resumed =
        AnchoredObservationWrite::new(resumed, second.retrieval_anchor().clone(), projection)
            .unwrap();
    let outcomes = store.persist_observations(vec![resumed]).await.unwrap();
    assert!(matches!(
        outcomes.as_slice(),
        [ObservationPersistOutcome::Committed(_)]
    ));
    let cursor = store
        .get_source_cursor(second.observation().source(), second.observation().scope())
        .await
        .unwrap()
        .expect("committed cursor");
    assert_eq!(cursor.file_identity(), Some(0xfeed_face));
    assert_eq!(cursor.resume_fingerprint(), Some(0xcafe_babe));
}
