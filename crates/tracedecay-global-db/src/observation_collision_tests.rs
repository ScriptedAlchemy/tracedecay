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
//!    (checkpoint advances, queue drains) instead of permanently wedging.
//!    The converged skip must satisfy the skip authority contract
//!    (`schema_contract::invariants`): zero provenance rows for the
//!    observation plus exactly one disposition — never a skip that
//!    contradicts a retained provenance binding.
//!
//! Review-driven contracts pinned alongside:
//! * the refusal terminal survives cursor-advance retention and is bound to
//!   the exact refused candidate digest, so a later canonical payload
//!   revision replay still converges as `CoveredDuplicate`;
//! * coverage is recorded only at the sequential scan frontier — covered
//!   replays and gap-shaped candidates leave every ledger untouched;
//! * only the narrow existing-output collision converges on drain; divergent
//!   workflow/effect state stays a hard error;
//! * no-rework is proven at the domain identity-digest boundary
//!   (`tracedecay_domain::observation::identity_digest_probe`), not just at
//!   the adapter call sites.

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

/// Anchors one observation write exactly as production ingest does.
fn anchored_write_for(
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
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

/// Raw persisted source input for one native transcript record: the exact
/// JSONL line a provider transcript would hold.
fn raw_source_line(
    session_id: &SessionId,
    record_id: &str,
    range: (u64, u64),
    text: &str,
) -> String {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let record = ObservationId::new(record_id).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new(format!("message.{record_id}")).unwrap());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record,
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationSourceRangeV1::new(range.0, range.1).unwrap(),
        ),
    )
    .unwrap();
    serde_json::to_string(&envelope).unwrap()
}

/// Decodes one raw source line into the durable candidate a real catch-up
/// pass would build: parse the envelope, rebind identity to the scan's
/// generation and range, and receipt the sanitized payload.
fn decode_raw_source_record(
    session_id: &SessionId,
    raw_line: &str,
    generation: u64,
    range: (u64, u64),
    receipt_id: &str,
) -> DurableObservationV1 {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id.clone()).unwrap();
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_str(raw_line).unwrap();
    let record = envelope.stable_record_id().clone();
    let payload = serde_json::to_value(&envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(generation).unwrap(),
        ObservationSourceRangeV1::new(range.0, range.1).unwrap(),
        ObservationOrderingDomainV1::SnapshotOrder,
        record,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap()
}

/// One sanitized native transcript record at an explicit source range.
/// Candidates built with the same `record_id` share a canonical observation
/// id regardless of `generation`, range, or payload text — exactly the shape
/// a rewritten source file produces.
fn collision_candidate_at(
    session_id: &SessionId,
    record_id: &str,
    generation: u64,
    range: (u64, u64),
    text: &str,
    receipt_id: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(range.0, range.1).unwrap();
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
    let anchored = anchored_write_for(observation.clone(), expected_cursor);
    (observation, anchored)
}

fn collision_candidate(
    session_id: &SessionId,
    record_id: &str,
    generation: u64,
    text: &str,
    receipt_id: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    collision_candidate_at(
        session_id,
        record_id,
        generation,
        (0, 1),
        text,
        receipt_id,
        expected_cursor,
    )
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
    let identity_digests = tracedecay_domain::observation::identity_digest_probe::count();

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
    // Measured at the domain identity-digest boundary itself (the
    // canonicalize-then-SHA256 chain inside `domain_digest`), not just at the
    // adapter call sites: a stored-row decode would re-derive the identity
    // there and increment this counter.
    assert_eq!(
        tracedecay_domain::observation::identity_digest_probe::count() - identity_digests,
        0,
        "re-admitted terminal collision must not re-derive identity digests at the domain boundary"
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

    // Skip authority contract (schema_contract::invariants): a durable skip
    // is exactly zero provenance rows plus one disposition, with no alias or
    // workflow rows. The contradictory stale binding must be reconciled away,
    // not retained next to the skip, and no partial output rows may leak.
    assert_eq!(
        provenance_rows(&runtime).await,
        Vec::new(),
        "a converged skip must not retain contradictory provenance"
    );
    assert_eq!(
        table_count(&runtime, "observation_projection_aliases").await,
        0
    );
    assert_eq!(table_count(&runtime, "observation_workflow_facts").await, 0);
    assert_eq!(table_count(&runtime, "session_messages").await, 0);
    assert_eq!(table_count(&runtime, "sessions").await, 0);
    // The retained observation row itself stays immutable.
    let stored = store
        .get_observation(observation.observation_id())
        .await
        .unwrap()
        .expect("retained observation row");
    assert_eq!(stored.observation().payload(), observation.payload());

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

/// Rows in the retained admission-refusal authority. Returns an empty list
/// when the authority table does not exist yet, so contract assertions fail
/// cleanly instead of erroring on a missing table.
async fn admission_refusal_rows(runtime: &HostAdmissionTestRuntimeV1) -> Vec<(String, String)> {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut probe = snapshot
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'observation_admission_refusals'",
            (),
        )
        .await
        .expect("probe admission refusal authority");
    if probe
        .next()
        .await
        .expect("read admission refusal authority probe")
        .is_none()
    {
        return Vec::new();
    }
    drop(probe);
    let mut rows = snapshot
        .query(
            "SELECT observation_id, refused_payload_digest
             FROM observation_admission_refusals
             ORDER BY observation_id, refused_payload_digest",
            (),
        )
        .await
        .expect("query admission refusal authority");
    let mut collected = Vec::new();
    while let Some(row) = rows.next().await.expect("read admission refusal rows") {
        collected.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    collected
}

/// Raw `observation_json` column for one retained row, read without decoding
/// so byte-exact immutability can be asserted outside any probe window.
async fn raw_observation_json(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_id: &str,
) -> String {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT observation_json FROM observations WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .expect("query retained observation row");
    rows.next()
        .await
        .expect("read retained observation row")
        .expect("retained observation row")
        .get::<String>(0)
        .expect("decode retained observation column")
}

/// Boundary accounting for one record a catch-up pass decoded and persisted.
struct CatchUpRecordReceipt {
    result: Result<ObservationPersistOutcome, ObservationStoreError>,
    /// Identity digests spent building the candidate from its raw source
    /// line (deserialize + identity derivation) — the trigger's own cost.
    construction_identity_digests: u64,
    /// Identity digests spent inside the store's persist call — terminal-row
    /// rework the store performed on top of the trigger's construction.
    persist_identity_digests: u64,
    /// Adapter-probe deltas across the persist call: stored-observation
    /// reads, collision classifications, payload-revision probes, canonical
    /// command digests.
    persist_probe_deltas: (u64, u64, u64, u64),
}

/// One real catch-up pass over raw persisted source input: read the durable
/// cursor, decode only the records the cursor does not cover, and persist
/// each decoded candidate exactly as ingest would, accounting every identity
/// digest at the domain boundary.
async fn run_catch_up_pass(
    store: &crate::GlobalDbObservationStore,
    session_id: &SessionId,
    generation: u64,
    raw_lines: &[((u64, u64), String)],
    pass_label: &str,
) -> (usize, Vec<CatchUpRecordReceipt>) {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id.clone()).unwrap();
    let scope = ObservationScopeV1::Profile;
    let scan_generation = ObservationSourceGenerationV1::new(generation).unwrap();
    let probe = store.persist_probe();
    let mut decoded = 0;
    let mut receipts = Vec::new();
    for (index, (range, raw_line)) in raw_lines.iter().enumerate() {
        let cursor = store.get_source_cursor(&source, &scope).await.unwrap();
        let covered = cursor.as_ref().is_some_and(|cursor| {
            cursor.generation() == scan_generation
                && cursor.ordering_domain() == ObservationOrderingDomainV1::SnapshotOrder
                && cursor.position() >= range.1
        });
        if covered {
            continue;
        }
        decoded += 1;
        let digests_before_construction =
            tracedecay_domain::observation::identity_digest_probe::count();
        let observation = decode_raw_source_record(
            session_id,
            raw_line,
            generation,
            *range,
            &format!("receipt.catch-up.{pass_label}.{index}"),
        );
        let write = anchored_write_for(observation, cursor);
        let digests_before_persist = tracedecay_domain::observation::identity_digest_probe::count();
        let (reads, classifications, revision_probes, command_digests) = probe.snapshot();
        let result = store.persist_observation(write).await;
        let digests_after_persist = tracedecay_domain::observation::identity_digest_probe::count();
        let (reads_after, classifications_after, revision_probes_after, command_digests_after) =
            probe.snapshot();
        receipts.push(CatchUpRecordReceipt {
            result,
            construction_identity_digests: digests_before_persist - digests_before_construction,
            persist_identity_digests: digests_after_persist - digests_before_persist,
            persist_probe_deltas: (
                reads_after - reads,
                classifications_after - classifications,
                revision_probes_after - revision_probes,
                command_digests_after - command_digests,
            ),
        });
    }
    (decoded, receipts)
}

/// Linux P1-3, covered-replay shape: an identity collision whose range the
/// durable cursor already covers is a replayed verification probe, not the
/// scan frontier. It must stay a typed fail-closed error and leave every
/// coverage ledger untouched — no `admission_refused` advance row, no
/// refusal-authority row, no cursor movement.
#[tokio::test]
async fn covered_replay_collision_leaves_coverage_state_untouched() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.covered-replay").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.covered-replay",
        1,
        "original transcript record",
        "receipt.covered-replay.original",
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

    // Replayed probe: same generation and range the cursor already covers,
    // different payload, no expected cursor (a stale reader's view).
    let (_, covered_write) = collision_candidate(
        &session_id,
        "record.covered-replay",
        1,
        "conflicting replayed payload",
        "receipt.covered-replay.conflicting",
        None,
    );
    let error = store.persist_observation(covered_write).await.unwrap_err();
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

    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        0,
        "a covered replay must not write an admission_refused advance row"
    );
    assert_eq!(admission_refusal_rows(&runtime).await, Vec::new());
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor,
        "a covered replay must not move the cursor"
    );
}

/// Linux P1-3, stale-expected shape: a colliding candidate whose expected
/// cursor does not match the durable one is not the scan frontier. The
/// refusal must stay the typed identity collision — never a cursor conflict,
/// never recorded coverage.
#[tokio::test]
async fn stale_expected_cursor_collision_stays_a_typed_collision() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.stale-expected").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.stale-expected",
        1,
        "original transcript record",
        "receipt.stale-expected.original",
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

    // A fabricated frontier: contiguous with its own claimed position but
    // disagreeing with the durable cursor (durable is at 1, claim is at 5).
    let stale_expected = ObservationSourceCursorV1::for_ordering(
        original.source().clone(),
        original.scope().clone(),
        original.identity().generation(),
        original.identity().ordering_domain(),
        5,
    )
    .unwrap();
    let (_, gap_write) = collision_candidate_at(
        &session_id,
        "record.stale-expected",
        1,
        (5, 6),
        "conflicting gap payload",
        "receipt.stale-expected.conflicting",
        Some(stale_expected),
    );
    let error = store.persist_observation(gap_write).await.unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "a stale-expected collision must stay the typed collision, got {error:?}"
    );

    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        0
    );
    assert_eq!(admission_refusal_rows(&runtime).await, Vec::new());
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor
    );
}

/// Linux P1-3, generation-jump shape: a new-generation candidate that starts
/// mid-file is a gap, not a rescan frontier — recording coverage for it would
/// silently claim bytes no scan ever read. Nothing may be recorded and the
/// cursor must not jump.
#[tokio::test]
async fn generation_jump_collision_records_no_false_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.generation-jump").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.generation-jump",
        1,
        "original transcript record",
        "receipt.generation-jump.original",
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

    // Same record id resurfaces in a NEW generation but mid-file (71..72)
    // with the current durable cursor as the expected view: bytes 0..71 of
    // that generation were never scanned.
    let (_, jump_write) = collision_candidate_at(
        &session_id,
        "record.generation-jump",
        2,
        (71, 72),
        "conflicting jump payload",
        "receipt.generation-jump.conflicting",
        committed_cursor.clone(),
    );
    let error = store.persist_observation(jump_write).await.unwrap_err();
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

    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        0,
        "a generation jump must not record admission_refused coverage over unscanned bytes"
    );
    assert_eq!(admission_refusal_rows(&runtime).await, Vec::new());
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor,
        "a generation jump must not move the cursor over unscanned bytes"
    );
}

/// A codex observation in either its legacy route-context form or its current
/// canonical form. Mirrors the workspace revision-compatibility fixtures: the
/// two forms share one canonical observation id, and moving legacy → current
/// is a recognized canonical payload revision replay.
fn codex_revision_observation(
    session_id: &SessionId,
    generation: u64,
    receipt_id: &str,
    legacy: bool,
    content: &str,
) -> DurableObservationV1 {
    let stable_record_id = ObservationId::new("record.codex.revision").unwrap();
    let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(stable_record_id.clone());
    if legacy {
        relations = relations.with_turn_id(ObservationId::new("route.turn").unwrap());
    }
    let session_fact = CanonicalObservationFactV1::Session {
        project_path: Some(if legacy {
            "/route/project".to_owned()
        } else {
            "/stable/project".to_owned()
        }),
        location_path: Some(if legacy {
            "/route/location".to_owned()
        } else {
            "/stable/project".to_owned()
        }),
        transcript_path: legacy.then(|| "/route/rollout.jsonl".to_owned()),
        title: None,
        started_at: None,
        ended_at: None,
        source: Some("codex_rollout".to_owned()),
        native_source: Some("codex".to_owned()),
        profile: None,
        location_provenance: Some("rollout_context".to_owned()),
    };
    let message_fact = CanonicalObservationFactV1::Message {
        role: CanonicalMessageRoleV1::Assistant,
        content: json!(content),
        model: None,
        timestamp: None,
    };
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let payload = serde_json::to_value(
        CanonicalObservationEnvelopeV1::new(
            ProviderId::new("codex").unwrap(),
            "message",
            stable_record_id.clone(),
            relations,
            vec![session_fact, message_fact],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SqliteRowId, range),
        )
        .unwrap(),
    )
    .unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("codex").unwrap(),
        session_id.clone(),
    )
    .unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(generation).unwrap(),
        range,
        ObservationOrderingDomainV1::SqliteRowId,
        stable_record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap()
}

/// Codex P2: an earlier invalid rewrite records the refusal terminal, and a
/// LATER candidate at the same generation and range that IS a recognized
/// canonical payload revision replay must still converge as
/// `CoveredDuplicate`. The terminal is bound to the exact refused candidate
/// digest, so it can never blanket-reject every differing digest.
#[tokio::test]
async fn canonical_payload_revision_replay_survives_an_earlier_refusal() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.codex.revision-after-refusal").unwrap();

    // Retained legacy-form record.
    let legacy = codex_revision_observation(
        &session_id,
        1,
        "receipt.codex.revision.legacy",
        true,
        "stable authored content",
    );
    assert!(matches!(
        store
            .persist_observation(anchored_write_for(legacy.clone(), None))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(legacy.source(), legacy.scope())
        .await
        .unwrap();

    // Invalid rewrite at the rescan frontier (generation 2, from zero):
    // authored content changed, so it is a true identity collision and
    // records the refusal terminal.
    let corrupted = codex_revision_observation(
        &session_id,
        2,
        "receipt.codex.revision.corrupted",
        false,
        "corrupted rewrite content",
    );
    assert_eq!(corrupted.observation_id(), legacy.observation_id());
    let refusal = store
        .persist_observation(anchored_write_for(corrupted.clone(), committed_cursor))
        .await
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{refusal:?}"
    );
    assert_eq!(admission_refused_advance_count(&runtime, &legacy).await, 1);

    // The recognized revision replay arrives at the SAME generation and range
    // as the refusal: same canonical id, same authored content, only the
    // bounded legacy source-context fields differ from the retained form. Its
    // digest differs from BOTH the retained and the refused digests, so the
    // terminal must not swallow it.
    let revision = codex_revision_observation(
        &session_id,
        2,
        "receipt.codex.revision.current",
        false,
        "stable authored content",
    );
    assert_eq!(revision.observation_id(), legacy.observation_id());
    assert_ne!(
        revision.payload_reference().digest(),
        corrupted.payload_reference().digest()
    );
    let outcome = store
        .persist_observation(anchored_write_for(revision, None))
        .await
        .expect("a recognized revision replay must not be terminally rejected");
    assert!(
        matches!(outcome, ObservationPersistOutcome::CoveredDuplicate(_)),
        "{outcome:?}"
    );
    // The retained row is still the legacy form, untouched.
    let stored = store
        .get_observation(legacy.observation_id())
        .await
        .unwrap()
        .expect("retained observation row");
    assert_eq!(stored.observation().payload(), legacy.payload());
}

/// Linux P1-1 plus the probe-validity blocker, measured at the domain
/// identity-digest boundary and driven from raw persisted source input:
///
/// 1. a real gen-1 catch-up pass ingests the original record from its raw
///    JSONL line;
/// 2. the file is rewritten (generation 2): the real rescan pass decodes the
///    rewritten record, refuses it terminally, and continues past it;
/// 3. later catch-up passes read the durable cursor and reopen ZERO source
///    records — no decode, no identity derivation, no hashing;
/// 4. production cursor-advance retention reclaims the superseded
///    `admission_refused` advance row, and the terminal STILL holds: a stale
///    in-flight re-admission answers from the retained refusal authority with
///    zero decode/derive/hash work;
/// 5. the same holds across a full store restart, and the retained row stays
///    byte-identical throughout.
#[tokio::test]
async fn terminal_refusal_survives_retention_and_catch_up_never_reopens_the_record() {
    use crate::observation::retention::{ObservationRetentionConfig, RetentionMode};

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.terminal-refusal.retention").unwrap();

    // Raw persisted source input.
    let original_lines = vec![(
        (0, 1),
        raw_source_line(
            &session_id,
            "record.retention.0",
            (0, 1),
            "original record zero",
        ),
    )];
    let rewritten_lines = vec![
        (
            (0, 1),
            raw_source_line(
                &session_id,
                "record.retention.0",
                (0, 1),
                "rewritten record zero",
            ),
        ),
        (
            (1, 2),
            raw_source_line(
                &session_id,
                "record.retention.1",
                (1, 2),
                "appended record one",
            ),
        ),
    ];

    // Pass 0: gen-1 ingest of the original file.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 1, &original_lines, "gen1").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0].result,
        Ok(ObservationPersistOutcome::Committed(_))
    ));

    // Pass 1: gen-2 rescan of the rewritten file. Record zero collides and is
    // terminally refused; the scan continues and commits record one.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2").await;
    assert_eq!(decoded, 2);
    assert!(matches!(
        receipts[0].result,
        Err(ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        })
    ));
    assert!(matches!(
        receipts[1].result,
        Ok(ObservationPersistOutcome::Committed(_))
    ));
    let refused = decode_raw_source_record(
        &session_id,
        &rewritten_lines[0].1,
        2,
        (0, 1),
        "receipt.catch-up.gen2.0",
    );
    let retained_row = raw_observation_json(&runtime, refused.observation_id().as_str()).await;
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);

    // Pass 2: a later catch-up pass reopens nothing.
    let digests_before = tracedecay_domain::observation::identity_digest_probe::count();
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2-b").await;
    assert_eq!(
        decoded, 0,
        "catch-up must not reopen covered source records"
    );
    assert_eq!(
        tracedecay_domain::observation::identity_digest_probe::count() - digests_before,
        0,
        "catch-up over covered input must not derive identity digests"
    );

    // Production retention reclaims the superseded admission_refused advance
    // row (the cursor has moved strictly past its coverage).
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let report = database
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");
    assert!(report.applied);
    let legacy_observation = decode_raw_source_record(
        &session_id,
        &original_lines[0].1,
        1,
        (0, 1),
        "receipt.catch-up.gen1.0",
    );
    assert_eq!(
        admission_refused_advance_count(&runtime, &legacy_observation).await,
        0,
        "retention must reclaim the superseded admission_refused advance row"
    );
    // The refusal terminal itself is a retained authority.
    assert_eq!(
        admission_refusal_rows(&runtime).await.len(),
        1,
        "the refusal terminal must survive cursor-advance retention"
    );

    // A stale in-flight re-admission (a temporal trigger re-presenting the
    // refused candidate without a current frontier view) still terminates
    // with zero decode/derive/hash work.
    let stale_replay = anchored_write_for(refused.clone(), None);
    let probe = store.persist_probe();
    let (reads, classifications, revision_probes, command_digests) = probe.snapshot();
    let digests_before = tracedecay_domain::observation::identity_digest_probe::count();
    let error = store.persist_observation(stale_replay).await.unwrap_err();
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
    assert_eq!(
        tracedecay_domain::observation::identity_digest_probe::count() - digests_before,
        0,
        "post-retention re-admission must not derive identity digests at the domain boundary"
    );
    let (reads_after, classifications_after, revision_probes_after, command_digests_after) =
        probe.snapshot();
    assert_eq!(reads_after - reads, 0);
    assert_eq!(classifications_after - classifications, 0);
    assert_eq!(revision_probes_after - revision_probes, 0);
    assert_eq!(command_digests_after - command_digests, 0);

    // Restart: the terminal and coverage are durable, catch-up still reopens
    // nothing, and the retained row is byte-identical.
    drop(store);
    drop(runtime);
    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let reopened_store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let digests_before = tracedecay_domain::observation::identity_digest_probe::count();
    let (decoded, _) =
        run_catch_up_pass(&reopened_store, &session_id, 2, &rewritten_lines, "gen2-c").await;
    assert_eq!(decoded, 0);
    assert_eq!(
        tracedecay_domain::observation::identity_digest_probe::count() - digests_before,
        0,
        "restarted catch-up must not reopen or re-derive the refused record"
    );
    assert_eq!(admission_refusal_rows(&reopened).await.len(), 1);
    assert_eq!(
        raw_observation_json(&reopened, refused.observation_id().as_str()).await,
        retained_row,
        "the retained observation row must stay byte-identical"
    );
}

/// Items 3 and 6 of the owner review, closed together: after production
/// cursor-advance retention has reclaimed the `admission_refused` advance
/// row, a REAL subsequent catch-up/temporal pass — a generation-3 rescan that
/// re-reads the rewritten file from raw persisted source input and rebuilds
/// every candidate through the ingest pipeline, NOT a preconstructed write —
/// re-admits the refused record and must be suppressed by the retained
/// terminal with ZERO store-side decode/canonicalize/SHA work.
///
/// Accounting is per record at the domain identity-digest boundary
/// (`identity_digest_probe` inside `domain_digest`): the trigger pays its own
/// raw-line deserialize + identity derivation (`construction_identity_digests
/// > 0` — this cost is measured, not hidden), and the store's persist call
/// must add exactly zero (`persist_identity_digests == 0`), with zero
/// stored-row reads, collision classifications, revision probes, and command
/// digests. The dispatch counts gate the engine-side work the thread-local
/// domain counter cannot see: the only way the store decodes (and thereby
/// re-derives and re-hashes) the retained row is the stored-observation read
/// dispatch, so zero dispatches means zero engine-side identity digests. The
/// retained row stays byte-identical throughout, and the pass converges so
/// the following pass reopens zero source records.
#[tokio::test]
async fn post_retention_rescan_re_admits_from_raw_source_without_terminal_rework() {
    use crate::observation::retention::{ObservationRetentionConfig, RetentionMode};

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.terminal-refusal.rescan").unwrap();
    let original_lines = vec![(
        (0, 1),
        raw_source_line(
            &session_id,
            "record.rescan.0",
            (0, 1),
            "original record zero",
        ),
    )];
    let rewritten_lines = vec![
        (
            (0, 1),
            raw_source_line(
                &session_id,
                "record.rescan.0",
                (0, 1),
                "rewritten record zero",
            ),
        ),
        (
            (1, 2),
            raw_source_line(
                &session_id,
                "record.rescan.1",
                (1, 2),
                "appended record one",
            ),
        ),
    ];

    // Collide at N: gen-1 ingest, then the gen-2 rescan refuses the rewritten
    // record terminally and commits the appended record, advancing the cursor
    // strictly past the refused coverage.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 1, &original_lines, "gen1").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0].result,
        Ok(ObservationPersistOutcome::Committed(_))
    ));
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2").await;
    assert_eq!(decoded, 2);
    assert!(matches!(
        receipts[0].result,
        Err(ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        })
    ));
    assert!(matches!(
        receipts[1].result,
        Ok(ObservationPersistOutcome::Committed(_))
    ));
    let refused = decode_raw_source_record(
        &session_id,
        &rewritten_lines[0].1,
        2,
        (0, 1),
        "receipt.catch-up.gen2.0",
    );
    let retained_row = raw_observation_json(&runtime, refused.observation_id().as_str()).await;

    // Run production retention: the superseded admission_refused advance row
    // is reclaimed, the refusal terminal survives.
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    database
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");
    assert_eq!(
        admission_refused_advance_count(&runtime, &refused).await,
        0,
        "retention must reclaim the superseded admission_refused advance row"
    );

    // The file changes again: a REAL gen-3 rescan re-reads BOTH raw lines and
    // re-admits the refused record through the ingest pipeline itself.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3").await;
    assert_eq!(
        decoded, 2,
        "a rescan after a real file change re-reads the raw source"
    );
    let refused_readmit = &receipts[0];
    assert!(
        matches!(
            refused_readmit.result,
            Err(ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            })
        ),
        "{:?}",
        refused_readmit.result
    );
    assert!(
        refused_readmit.construction_identity_digests > 0,
        "the trigger's own raw-line decode and identity derivation are real and measured"
    );
    assert_eq!(
        refused_readmit.persist_identity_digests, 0,
        "the store must answer the post-retention raw-source re-admit without \
         decoding, canonicalizing, or hashing the terminal row"
    );
    assert_eq!(
        refused_readmit.persist_probe_deltas,
        (0, 0, 0, 0),
        "the store must not read the stored row, classify, probe revisions, or \
         digest commands for the post-retention raw-source re-admit"
    );
    // The suppression above was answered by the retained refusal terminal:
    // it must have survived cursor-advance retention.
    assert_eq!(
        admission_refusal_rows(&runtime).await.len(),
        1,
        "the refusal terminal must survive cursor-advance retention"
    );
    // The pass still converges: the appended record lands and covers the new
    // generation, so the NEXT pass reopens zero source records.
    assert!(receipts[1].result.is_ok(), "{:?}", receipts[1].result);
    let digests_before = tracedecay_domain::observation::identity_digest_probe::count();
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3-b").await;
    assert_eq!(decoded, 0, "the converged rescan reopens no source records");
    assert_eq!(
        tracedecay_domain::observation::identity_digest_probe::count() - digests_before,
        0
    );

    // Immutable old row: byte-identical after every pass.
    assert_eq!(
        raw_observation_json(&runtime, refused.observation_id().as_str()).await,
        retained_row,
        "the retained observation row must stay byte-identical"
    );
}

/// Linux P1-2: only the narrow existing-output collision converges to a
/// durable skip. Divergent durable workflow-fact state is corrupt authority,
/// not an output collision — it must stay a hard `ProvenanceCollision` error
/// with the queue item retained and the checkpoint unmoved.
#[tokio::test]
async fn drain_keeps_divergent_workflow_fact_state_a_hard_error() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    // A checked-in codex goal record: its canonical projection carries
    // workflow facts alongside the message output.
    let record_id = ObservationId::new("record.goal.divergent").unwrap();
    let encoded = include_str!(
        "../../../tests/fixtures/provider_normalization/codex/thread_goal_updated.expected_envelope.json"
    )
    .replace("$STABLE_RECORD_ID", record_id.as_str());
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_str(&encoded).unwrap();
    let provider = envelope.provider().clone();
    let goal_session = envelope.relations().session_id().clone();
    let range = envelope.evidence().range();
    let ordering_domain = envelope.evidence().ordering_domain();
    let payload = serde_json::to_value(&envelope).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, goal_session).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ordering_domain,
        record_id,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt("receipt.goal.divergent", &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap();
    assert!(matches!(
        store
            .persist_observation(anchored_write_for(observation.clone(), None))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));

    // Divergent durable workflow-fact rows already hold this observation's
    // fact ordinals with different content.
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let anchor_id = tracedecay_domain::derive_exact_observation_anchor_id(
        observation.scope(),
        observation.observation_id(),
    )
    .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    for ordinal in 0..4_i64 {
        transaction
            .execute(
                "INSERT INTO observation_workflow_facts (
                    projector_version, observation_id, fact_ordinal, retrieval_anchor_id,
                    receipt_id, observation_sequence, provider, session_id, semantic_kind,
                    ordering_domain, content_text, output_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'codex', ?6, 'goal', ?7,
                           'divergent seeded goal state',
                           'sha256:0000000000000000000000000000000000000000000000000000000000000000')",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    observation.observation_id().as_str(),
                    ordinal,
                    anchor_id.as_str(),
                    observation.receipt().receipt().receipt_id().as_str(),
                    observation.source().session_id().as_str(),
                    "snapshot_order",
                ],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    let error = store
        .project_observation(observation.observation_id())
        .await
        .expect_err("divergent durable workflow state must stay a hard error");
    assert!(
        matches!(
            error,
            tracedecay_store::ProjectionStoreError::ProvenanceCollision
        ),
        "{error:?}"
    );
    // The queue item is retained and the checkpoint has not moved: corrupt
    // authority is surfaced, never silently skipped past.
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(observation.observation_id())
    );
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
}
