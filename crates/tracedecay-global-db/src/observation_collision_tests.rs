//! Production-shaped regression tests for the two durable Stage0a collision
//! failures observed on 5ddd16271 (ancestral on 2be2b9478 / 0.1.0-beta.34):
//!
//! 1. `observation_identity_collision` — a rewritten native record presents
//!    the same canonical observation id with a different payload digest. The
//!    refusal is deterministic and non-retryable, so it must record durable
//!    terminal coverage in the typed cursor-advance ledger; later catch-up and
//!    temporal triggers must not decode, classify, canonicalize, or hash that
//!    row again, and the retained row must stay byte-identical.
//! 2. projection-drain provenance collision with an existing output — a
//!    queued observation whose drain collides with an already-persisted
//!    provenance row must converge to a durable `output_collision` skip
//!    (checkpoint advances, queue drains) instead of permanently wedging,
//!    while the pre-existing rows stay untouched.

use serde_json::{Value, json};
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
    AnchoredObservationWrite, ObservationCoverageReason, ObservationPersistOutcome,
    ObservationProjectionStore, ObservationStore, ObservationStoreError, ObservationWrite,
    ProjectionPersistOutcome, ProjectionSkipReason, SESSION_MESSAGE_PROJECTOR_VERSION,
};

use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::engine::params;

const COLLISION_PROVIDER: &str = "collision-test";

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.collision-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

/// One sanitized native transcript record. Candidates built with the same
/// `record_id` share a canonical observation id regardless of `generation` or
/// payload text — exactly the shape a rewritten source file produces.
fn collision_candidate(
    session_id: &SessionId,
    record_id: &str,
    generation: u64,
    text: &str,
    receipt_id: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record = ObservationId::new(record_id).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new(format!("message.{record_id}")).unwrap());
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
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(generation).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation.clone(), expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.collision-test.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "collision-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
    (observation, anchored)
}

async fn admission_refused_advance_count(
    runtime: &HostAdmissionTestRuntimeV1,
    observation: &DurableObservationV1,
) -> i64 {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let source_json = serde_json::to_string(observation.source()).unwrap();
    let scope_json = serde_json::to_string(observation.scope()).unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2 AND reason = ?3",
            params![
                source_json,
                scope_json,
                ObservationCoverageReason::AdmissionRefused.as_str()
            ],
        )
        .await
        .expect("query cursor-advance ledger");
    rows.next()
        .await
        .expect("read cursor-advance ledger count")
        .expect("cursor-advance ledger count row")
        .get::<i64>(0)
        .expect("decode cursor-advance ledger count")
}

async fn table_count(runtime: &HostAdmissionTestRuntimeV1, table: &str) -> i64 {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .expect("query table count");
    rows.next()
        .await
        .expect("read table count")
        .expect("table count row")
        .get::<i64>(0)
        .expect("decode table count")
}

type ProvenanceRow = (String, String, i64, String, String, String, String, i64);

async fn provenance_rows(runtime: &HostAdmissionTestRuntimeV1) -> Vec<ProvenanceRow> {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT projector_version, observation_id, output_ordinal, receipt_id,
                    output_provider, output_message_id, output_digest, message_created
             FROM observation_projection_provenance
             ORDER BY projector_version, observation_id, output_ordinal",
            (),
        )
        .await
        .expect("query projection provenance");
    let mut collected = Vec::new();
    while let Some(row) = rows.next().await.expect("read projection provenance") {
        collected.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<i64>(2).unwrap(),
            row.get::<String>(3).unwrap(),
            row.get::<String>(4).unwrap(),
            row.get::<String>(5).unwrap(),
            row.get::<String>(6).unwrap(),
            row.get::<i64>(7).unwrap(),
        ));
    }
    collected
}

/// Stage0a symptom 1, first RED requirement: the first non-retryable identity
/// collision must keep the retained row byte-identical and record durable
/// terminal coverage — the typed source cursor converges past the colliding
/// record and the refusal lands in the `source_cursor_advances` ledger with
/// the typed `admission_refused` reason.
#[tokio::test]
async fn identity_collision_records_durable_admission_refused_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.durable").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.identity-collision",
        1,
        "original transcript record",
        "receipt.identity-collision.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    assert!(committed_cursor.is_some());

    // The source file was rewritten: a new generation re-presents the same
    // native record id with different content.
    let (rewritten, rewritten_write) = collision_candidate(
        &session_id,
        "record.identity-collision",
        2,
        "rewritten transcript record",
        "receipt.identity-collision.rewritten",
        committed_cursor,
    );
    assert_eq!(rewritten.observation_id(), original.observation_id());
    assert_ne!(
        rewritten.payload_reference().digest(),
        original.payload_reference().digest()
    );

    let error = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{error:?}"
    );

    // Immutable old row: the collision must not overwrite or mutate it.
    let stored = store
        .get_observation(original.observation_id())
        .await
        .unwrap()
        .expect("retained observation row");
    assert_eq!(
        stored.observation().payload_reference().digest(),
        original.payload_reference().digest()
    );
    assert_eq!(stored.observation().payload(), original.payload());

    // Durable terminal coverage: the typed cursor converges past the refused
    // record so catch-up never re-reads it...
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(rewritten_write.next_cursor()),
        "identity collision must advance typed source coverage past the refused record"
    );
    // ...and the refusal is durable in the typed cursor-advance ledger.
    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        1,
        "identity collision must record one durable admission_refused advance"
    );
}

/// Stage0a symptom 1, second RED requirement: once the collision is durably
/// terminal, a re-admitted candidate (late catch-up pass or temporal trigger
/// holding a stale cursor) must fail with the same typed error WITHOUT
/// decoding the stored row, re-classifying the collision, probing the payload
/// revision, or computing another canonical digest.
#[tokio::test]
async fn re_admitted_identity_collision_short_circuits_without_decode_or_hash() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.readmitted").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.identity-collision.readmitted",
        1,
        "original transcript record",
        "receipt.identity-collision.readmitted.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let (_, rewritten_write) = collision_candidate(
        &session_id,
        "record.identity-collision.readmitted",
        2,
        "rewritten transcript record",
        "receipt.identity-collision.readmitted.rewritten",
        committed_cursor,
    );
    let first = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            first,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{first:?}"
    );

    let probe = store.persist_probe();
    let (reads, classifications, revision_probes, digests) = probe.snapshot();

    // A later catch-up pass or temporal trigger re-presents the exact same
    // candidate with its now-stale expected cursor.
    let second = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            second,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{second:?}"
    );

    let (reads_after, classifications_after, revision_probes_after, digests_after) =
        probe.snapshot();
    assert_eq!(
        reads_after - reads,
        0,
        "re-admitted terminal collision must not decode the stored observation row again"
    );
    assert_eq!(
        classifications_after - classifications,
        0,
        "re-admitted terminal collision must not re-classify the collision"
    );
    assert_eq!(
        revision_probes_after - revision_probes,
        0,
        "re-admitted terminal collision must not re-probe the payload revision"
    );
    assert_eq!(
        digests_after - digests,
        0,
        "re-admitted terminal collision must not canonicalize or hash again"
    );

    // The terminal coverage stays single-row and the cursor stays put.
    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        1
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(rewritten_write.next_cursor())
    );
}

/// Stage0a symptom 2: a projection drain that collides with an existing
/// provenance row for the same observation (an earlier projection era left a
/// divergent output binding behind) must converge to a durable
/// `output_collision` skip — checkpoint advances, the queue drains, replay is
/// an exact duplicate — while the pre-existing provenance row stays
/// byte-identical and no partial output rows leak.
#[tokio::test]
async fn drain_provenance_collision_with_existing_output_converges_to_durable_skip() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.provenance-collision.drain").unwrap();
    let (observation, write) = collision_candidate(
        &session_id,
        "record.provenance-collision",
        1,
        "provenance drain canary",
        "receipt.provenance-collision",
        None,
    );
    assert!(matches!(
        store.persist_observation(write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));

    // An earlier projection era bound this observation to a different output:
    // the stored provenance disagrees with what the drain now derives.
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let stale_anchor_id = tracedecay_domain::derive_exact_observation_anchor_id(
        observation.scope(),
        observation.observation_id(),
    )
    .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO observation_projection_provenance
                (projector_version, observation_id, output_ordinal, receipt_id,
                 output_provider, output_message_id, output_digest, message_created,
                 retrieval_anchor_id)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, 1, ?7)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                COLLISION_PROVIDER,
                "stale-era-output",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                stale_anchor_id.as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let stale_rows = provenance_rows(&runtime).await;
    assert_eq!(stale_rows.len(), 1);

    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(observation.observation_id())
    );

    let outcome = store
        .project_observation(observation.observation_id())
        .await
        .expect("drain must converge the provenance collision instead of wedging");
    assert!(
        matches!(
            outcome,
            ProjectionPersistOutcome::Skipped {
                reason: ProjectionSkipReason::OutputCollision,
                ..
            }
        ),
        "{outcome:?}"
    );

    // Checkpoint advanced past the collided observation and the queue drained.
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert!(store.next_queued_observation().await.unwrap().is_none());

    // Immutable old rows: the colliding drain must not overwrite or mutate
    // the pre-existing provenance, and no partial output rows may leak.
    assert_eq!(provenance_rows(&runtime).await, stale_rows);
    assert_eq!(table_count(&runtime, "session_messages").await, 0);
    assert_eq!(table_count(&runtime, "sessions").await, 0);

    // The skip is durable: a replay consults the recorded disposition and
    // converges as an exact duplicate.
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str()
            ],
        )
        .await
        .unwrap();
    let reason = rows
        .next()
        .await
        .unwrap()
        .expect("durable projection disposition row")
        .get::<String>(0)
        .unwrap();
    assert_eq!(reason, ProjectionSkipReason::OutputCollision.as_str());
    drop(rows);

    assert!(matches!(
        store
            .project_observation(observation.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
}
