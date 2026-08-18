mod retrieval_anchors;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    AnchorDurabilityClass, AnchorSourceGenerationV2, ClaudeByteRangeV1, ClaudeFileGenerationV1,
    ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1, CommitId,
    ComponentVersion, CoverageReportV1, DurableClaudeObservationV1, EvidenceAvailabilityV1,
    EvidenceClass, GenerationBoundRepositoryProvenanceV1, NativeAliasV2,
    ObservationCollisionOutcomeV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadAccessState, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest,
    ProjectionGenerationId, ProviderId, RefId, RepositoryDirtyStateV1, RepositoryEvidenceV1,
    RepositoryId, RepositoryProvenanceV1, RepositoryRemoteIdentityV1, RetentionClass,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, TreeId, UtcMicros, VectorWatermark, WorktreeId,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, NonDurableFrameReason, ObservationCoverageV1, ObservationCursorAdvance,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStatus,
    ObservationReplayRequest, ObservationStore, ObservationStoreError, ObservationWrite,
    SESSION_MESSAGE_PROJECTOR_VERSION, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};
use tracedecay_usecases::host_admission::HostAdmissionScope;

const GENERATION: u64 = 7;

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

fn source() -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new("session.observation-store").unwrap()).unwrap()
}

fn scope() -> ObservationScopeV1 {
    ObservationScopeV1::Profile
}

fn cursor(byte_offset: u64) -> ClaudeSourceCursorV1 {
    cursor_in_generation(GENERATION, byte_offset)
}

fn cursor_in_generation(generation: u64, byte_offset: u64) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        source(),
        scope(),
        ClaudeFileGenerationV1::new(generation).unwrap(),
        byte_offset,
    )
    .unwrap()
}

fn observation(start: u64, end: u64, receipt_id: &str, body: &str) -> DurableClaudeObservationV1 {
    observation_in_generation(GENERATION, start, end, receipt_id, body)
}

fn observation_in_generation(
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    observation_in_scope(generation, start, end, receipt_id, body, scope())
}

fn observation_in_scope(
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    body: &str,
    scope: ObservationScopeV1,
) -> DurableClaudeObservationV1 {
    let payload = json!({
        "kind": "assistant_message",
        "body": body,
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        source(),
        scope,
        ClaudeFileGenerationV1::new(generation).unwrap(),
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();

    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.test").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = ClaudeSourceCursorV1::new(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().position().end(),
    )
    .unwrap();
    anchored_write(ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap())
}

fn anchored_write(write: ObservationWrite) -> AnchoredObservationWrite {
    anchored_write_at(write, UtcMicros(1))
}

fn anchored_write_at(write: ObservationWrite, ingested_at: UtcMicros) -> AnchoredObservationWrite {
    anchored_write_at_with_projection_generation(
        write,
        ingested_at,
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap(),
    )
}

fn anchored_write_at_with_projection_generation(
    write: ObservationWrite,
    ingested_at: UtcMicros,
    projection_generation: ProjectionGenerationId,
) -> AnchoredObservationWrite {
    let authorization = build_observation_resolution_authorization_v1(
        write.observation(),
        "observation-store-test.v1",
    )
    .unwrap();
    let retrieval_anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        ingested_at,
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, retrieval_anchor, projection_generation).unwrap()
}

fn known_repository_provenance_write(write: ObservationWrite) -> AnchoredObservationWrite {
    let observation = write.observation().clone();
    let ObservationScopeV1::Project { project_id } = observation.scope() else {
        panic!("repository provenance requires a project-scoped observation");
    };
    let projection_generation =
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap();
    let authorization = build_observation_resolution_authorization_v1(
        &observation,
        "observation-store-provenance-test.v1",
    )
    .unwrap();
    let observation_anchor = build_observation_retrieval_anchor_v2(
        &observation,
        projection_generation.clone(),
        UtcMicros(7),
        authorization.clone(),
    )
    .unwrap();
    let digest = |byte: char| {
        PrivacyDomainBoundLocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .unwrap()
    };
    let repository_id = RepositoryId::new("repository.provenance-store").unwrap();
    let capture = RepositoryProvenanceV1::new(
        repository_id.clone(),
        Some(project_id.clone()),
        Some(WorktreeId::new("worktree.provenance-store").unwrap()),
        digest('a'),
        RepositoryEvidenceV1::new(
            EvidenceAvailabilityV1::Known(RefId::new("refs/heads/main").unwrap()),
            EvidenceAvailabilityV1::Known(
                CommitId::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            ),
            EvidenceAvailabilityV1::Known(
                TreeId::new("89abcdef0123456789abcdef0123456789abcdef").unwrap(),
            ),
            EvidenceAvailabilityV1::Known(digest('b')),
            RepositoryRemoteIdentityV1::Known(digest('c')),
            EvidenceAvailabilityV1::Known(RepositoryDirtyStateV1::Dirty),
        )
        .unwrap(),
        UtcMicros(7),
    )
    .unwrap();
    let provenance = GenerationBoundRepositoryProvenanceV1::new(
        projection_generation.clone(),
        capture,
        Some(observation.observation_id().clone()),
    )
    .unwrap();
    let repository_anchor = RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::RepositoryCapture {
            repository_id,
            capture_id: provenance.capture_id().clone(),
            receipt: observation.receipt().receipt().clone(),
        },
        owner: observation.scope().clone(),
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(7),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::RepositoryCapture(
            provenance.capture_id().clone(),
        ),
        projection_generation: projection_generation.clone(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![observation.observation_id().clone()],
        source_anchors: vec![],
        authorization,
        payload_access: PayloadAccessState::Eligible,
        retention_class: observation.retention_class().clone(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap();
    AnchoredObservationWrite::new(write, observation_anchor, projection_generation)
        .unwrap()
        .with_repository_provenance_attachment(
            EvidenceAvailabilityV1::Known(provenance),
            Some(repository_anchor),
        )
        .unwrap()
}

fn anchor_with_aliases(
    anchor: &RetrievalAnchorRecordV2,
    aliases: Vec<NativeAliasV2>,
) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: anchor.target().clone(),
        owner: anchor.owner().clone(),
        aliases,
        occurred_at: anchor.occurred_at(),
        ingested_at: anchor.ingested_at(),
        evidence_class: anchor.evidence_class(),
        source_generation: anchor.source_generation().clone(),
        projection_generation: anchor.projection_generation().clone(),
        projection_watermark: anchor.projection_watermark().clone(),
        coverage: anchor.coverage().clone(),
        source_observations: anchor.source_observations().to_vec(),
        source_anchors: anchor.source_anchors().to_vec(),
        authorization: anchor.authorization().clone(),
        payload_access: anchor.payload_access(),
        retention_class: anchor.retention_class().clone(),
        durability: anchor.durability().clone(),
    })
    .unwrap()
}

const CROSS_PROVIDERS: &[&str] = &[
    "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
];

fn native_source() -> ObservationSourceIdentityV1 {
    provider_source("hermes", "session.observation-store-native")
}

fn provider_source(provider: &str, session_id: &str) -> ObservationSourceIdentityV1 {
    ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider).unwrap(),
        SessionId::new(session_id).unwrap(),
    )
    .unwrap()
}

fn native_cursor(generation: u64, position: u64) -> ObservationSourceCursorV1 {
    provider_cursor(
        "hermes",
        "session.observation-store-native",
        generation,
        position,
    )
}

fn provider_cursor(
    provider: &str,
    session_id: &str,
    generation: u64,
    position: u64,
) -> ObservationSourceCursorV1 {
    ObservationSourceCursorV1::for_ordering(
        provider_source(provider, session_id),
        scope(),
        ObservationSourceGenerationV1::new(generation).unwrap(),
        ObservationOrderingDomainV1::SqliteRowId,
        position,
    )
    .unwrap()
}

fn native_observation(
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    native_record_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    provider_observation(ProviderObservationFixture {
        provider: "hermes",
        session_id: "session.observation-store-native",
        generation,
        start,
        end,
        receipt_id,
        native_record_id,
        body,
    })
}

struct ProviderObservationFixture<'a> {
    provider: &'a str,
    session_id: &'a str,
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &'a str,
    native_record_id: &'a str,
    body: &'a str,
}

fn provider_observation(fixture: ProviderObservationFixture<'_>) -> DurableClaudeObservationV1 {
    let ProviderObservationFixture {
        provider,
        session_id,
        generation,
        start,
        end,
        receipt_id,
        native_record_id,
        body,
    } = fixture;
    let payload = json!({
        "kind": "assistant_message",
        "body": body,
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("privacy.observation-record.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        provider_source(provider, session_id),
        scope(),
        ObservationSourceGenerationV1::new(generation).unwrap(),
        ObservationSourceRangeV1::new(start, end).unwrap(),
        ObservationOrderingDomainV1::SqliteRowId,
        ObservationId::new(native_record_id).unwrap(),
    )
    .unwrap();

    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.test").unwrap(),
        payload,
    )
    .unwrap()
}

fn native_write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    provider_write(observation, expected_cursor)
}

fn provider_write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let generation = observation.identity().generation().generation_id();
    let position = observation.identity().position().end();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        ObservationSourceGenerationV1::new(generation).unwrap(),
        ObservationOrderingDomainV1::SqliteRowId,
        position,
    )
    .unwrap();
    anchored_write(ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap())
}

fn provider_malformed_advance(
    provider: &str,
    session_id: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
    start: u64,
    end: u64,
) -> ObservationCursorAdvance {
    ObservationCursorAdvance::for_ordering(
        provider_source(provider, session_id),
        scope(),
        ObservationSourceGenerationV1::new(GENERATION).unwrap(),
        ObservationOrderingDomainV1::SqliteRowId,
        expected_cursor,
        ObservationSourceRangeV1::new(start, end).unwrap(),
        NonDurableFrameReason::MalformedFrame,
    )
    .unwrap()
}

fn cursor_advance(
    expected_cursor: Option<ClaudeSourceCursorV1>,
    start: u64,
    end: u64,
    reason: NonDurableFrameReason,
) -> ObservationCursorAdvance {
    let generation = ClaudeFileGenerationV1::new(GENERATION).unwrap();
    let covered = ClaudeByteRangeV1::new(start, end).unwrap();
    let disposition = match reason {
        NonDurableFrameReason::SanitizerRejected => Some(SanitizerDispositionV1::Rejected),
        NonDurableFrameReason::SanitizerQuarantined => Some(SanitizerDispositionV1::Quarantined),
        _ => None,
    };
    let Some(disposition) = disposition else {
        return ObservationCursorAdvance::new(
            source(),
            scope(),
            generation,
            expected_cursor,
            covered,
            reason,
        )
        .unwrap();
    };
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.cursor.{start}.{end}.{}", reason.as_str()))
                .unwrap(),
            ComponentVersion::new("sanitizer.test.v1").unwrap(),
        )
        .unwrap(),
        disposition,
        SensitivityV1::Sensitive,
        None,
    )
    .unwrap();
    ObservationCursorAdvance::new_with_sanitization_receipt(
        source(),
        scope(),
        generation,
        expected_cursor,
        covered,
        reason,
        receipt,
    )
    .unwrap()
}

#[test]
fn observation_write_requires_exact_source_contiguity() {
    let initial = observation(0, 100, "receipt.cursor-initial", "initial payload");
    assert!(ObservationWrite::new(initial.clone(), None, cursor(100)).is_ok());
    assert!(matches!(
        ObservationWrite::new(initial, None, cursor(99)),
        Err(ObservationStoreError::CursorObservationMismatch)
    ));

    let initial_gap = observation(1, 100, "receipt.cursor-initial-gap", "initial gap");
    assert!(matches!(
        ObservationWrite::new(initial_gap, None, cursor(100)),
        Err(ObservationStoreError::CursorObservationMismatch)
    ));

    let contiguous = observation(100, 200, "receipt.cursor-contiguous", "contiguous");
    assert!(ObservationWrite::new(contiguous.clone(), Some(cursor(100)), cursor(200)).is_ok());
    for non_contiguous in [cursor(99), cursor(101)] {
        assert!(matches!(
            ObservationWrite::new(contiguous.clone(), Some(non_contiguous), cursor(200)),
            Err(ObservationStoreError::CursorObservationMismatch)
        ));
    }

    let replacement_generation = GENERATION + 1;
    let replacement = observation_in_generation(
        replacement_generation,
        0,
        100,
        "receipt.cursor-replacement",
        "replacement",
    );
    assert!(
        ObservationWrite::new(
            replacement,
            Some(cursor(200)),
            cursor_in_generation(replacement_generation, 100),
        )
        .is_ok()
    );

    let replacement_gap = observation_in_generation(
        replacement_generation,
        1,
        100,
        "receipt.cursor-replacement-gap",
        "replacement gap",
    );
    assert!(matches!(
        ObservationWrite::new(
            replacement_gap,
            Some(cursor(200)),
            cursor_in_generation(replacement_generation, 100),
        ),
        Err(ObservationStoreError::CursorObservationMismatch)
    ));
}

fn user_table_counts(database_path: &Path) -> BTreeMap<String, i64> {
    let conn = rusqlite::Connection::open(database_path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name NOT LIKE 'td_runtime_writer_%'
             ORDER BY name",
        )
        .unwrap();
    let tables = statement
        .query_map((), |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(statement);

    let mut counts = BTreeMap::new();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let count = conn
            .query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), (), |row| {
                row.get(0)
            })
            .unwrap();
        counts.insert(table, count);
    }
    counts
}

fn table_deltas(
    before: &BTreeMap<String, i64>,
    after: &BTreeMap<String, i64>,
) -> BTreeMap<String, i64> {
    after
        .iter()
        .filter_map(|(table, after_count)| {
            let delta = after_count - before.get(table).copied().unwrap_or_default();
            (delta != 0).then(|| (table.clone(), delta))
        })
        .collect()
}

#[tokio::test]
async fn persist_commits_receipt_observation_cursor_and_one_projection_queue_row_atomically() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let candidate = observation(0, 100, "receipt.atomic", "first sanitized payload");
    let expected_cursor = cursor(100);
    let before = user_table_counts(&database_path);

    let outcome = store
        .persist_observation(write(candidate.clone(), None))
        .await
        .unwrap();
    let receipt = match outcome {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };

    assert_eq!(receipt.observation(), &candidate);
    assert_eq!(receipt.sanitization_receipt(), candidate.receipt());
    assert_eq!(receipt.committed_cursor(), &expected_cursor);
    assert_eq!(
        receipt.projection_generation().as_str(),
        SESSION_MESSAGE_PROJECTOR_VERSION
    );
    assert_eq!(
        store
            .get_source_cursor(candidate.source(), candidate.scope())
            .await
            .unwrap(),
        Some(expected_cursor.clone())
    );
    let stored = store
        .get_observation(candidate.observation_id())
        .await
        .unwrap()
        .expect("committed observation must be point-readable");
    assert_eq!(stored.sequence(), receipt.sequence());
    assert_eq!(stored.commit_receipt(), &receipt);
    assert_eq!(stored.observation(), &candidate);
    assert_eq!(stored.sanitization_receipt(), candidate.receipt());
    assert_eq!(stored.committed_cursor(), &expected_cursor);
    assert!(matches!(
        stored.repository_provenance_attachment().availability(),
        EvidenceAvailabilityV1::Unavailable
    ));
    assert!(stored.repository_provenance_attachment().anchor().is_none());
    assert_eq!(
        stored.projection_status(),
        ObservationProjectionStatus::Queued
    );

    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    let mut columns = raw_conn.prepare("PRAGMA table_info(observations)").unwrap();
    let column_names = columns
        .query_map((), |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        !column_names.iter().any(|name| name == "idempotency_key"),
        "new schemas must use observation_id as the sole idempotency identity"
    );
    drop(columns);
    assert!(
        raw_conn
            .execute(
                "UPDATE observations SET observation_json = '{}' WHERE observation_id = ?1",
                rusqlite::params![candidate.observation_id().as_str()],
            )
            .is_err(),
        "immutable observations must reject updates"
    );
    assert!(
        raw_conn
            .execute(
                "DELETE FROM observations WHERE observation_id = ?1",
                rusqlite::params![candidate.observation_id().as_str()],
            )
            .is_err(),
        "immutable observations must reject deletes"
    );
    assert!(
        raw_conn
            .execute(
                "UPDATE retrieval_anchors SET projection_generation = 'projection.mutated' \
                 WHERE anchor_id = ?1",
                rusqlite::params![receipt.retrieval_anchor_id().as_str()],
            )
            .is_err(),
        "stable retrieval anchors must reject updates"
    );
    assert!(
        raw_conn
            .execute(
                "DELETE FROM observation_retrieval_anchors WHERE observation_id = ?1",
                rusqlite::params![candidate.observation_id().as_str()],
            )
            .is_err(),
        "observation retrieval anchor bindings must reject deletes"
    );

    let deltas = table_deltas(&before, &user_table_counts(&database_path));
    assert_eq!(
        deltas.len(),
        7,
        "receipt, observation, anchors, provenance, cursor, and queue must commit: {deltas:?}"
    );
    assert!(
        deltas.values().all(|delta| *delta == 1),
        "each authoritative component must be inserted exactly once: {deltas:?}"
    );
    assert_eq!(
        deltas.get("projection_queue"),
        Some(&1),
        "the commit must enqueue exactly one unique projection job"
    );
    assert_eq!(deltas.get("retrieval_anchors"), Some(&1));
    assert_eq!(deltas.get("observation_retrieval_anchors"), Some(&1));
    assert_eq!(deltas.get("observation_repository_provenance"), Some(&1));
}

/// The `idempotency_key` observation shape never shipped in a published
/// release, so admission must refuse it with the typed `ResetRequired` state
/// naming the observation authority — never migrate it in place.
#[tokio::test]
async fn pre_release_idempotency_observation_shape_refuses_admission_with_reset_required() {
    let tmp = TempDir::new().unwrap();
    let bootstrap = profile_runtime(&tmp).await;
    let db_path = bootstrap
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    drop(bootstrap);

    let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
    raw_conn.pragma_update(None, "foreign_keys", false).unwrap();
    raw_conn
        .execute_batch(
            "DROP TABLE observations;
            CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                idempotency_key TEXT NOT NULL UNIQUE,
                payload_digest TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                observation_json TEXT NOT NULL,
                committed_cursor_json TEXT NOT NULL,
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
            );
            INSERT INTO observations
                (observation_id, idempotency_key, payload_digest, receipt_id,
                 observation_json, committed_cursor_json)
            VALUES ('observation.legacy', 'idempotency.legacy', 'digest.legacy',
                    'receipt.legacy', '{}', '{}');",
        )
        .unwrap();
    drop(raw_conn);

    let error = match HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay")).await {
        Ok(_) => panic!("a pre-release observation shape must refuse admission"),
        Err(error) => error,
    };
    let (authority, reason) = error
        .reset_required_context()
        .unwrap_or_else(|| panic!("expected the typed ResetRequired state, got: {error}"));
    assert_eq!(authority, "observations");
    assert!(
        reason.contains("no sanctioned migration"),
        "the refusal must explain that the shape never shipped: {reason}"
    );

    let verify_conn = rusqlite::Connection::open(&db_path).unwrap();
    let legacy_columns_intact = verify_conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_xinfo('observations')
                WHERE name = 'idempotency_key'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap();
    assert!(
        legacy_columns_intact,
        "a refused shape must not be silently migrated"
    );
    let legacy_rows: i64 = verify_conn
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        legacy_rows, 1,
        "refused data must be preserved for recovery"
    );
}

#[tokio::test]
async fn exact_duplicate_returns_original_receipt_without_mutating_cursor_or_store() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let candidate = observation(0, 100, "receipt.duplicate", "stable sanitized payload");

    let first = store
        .persist_observation(write(candidate.clone(), None))
        .await
        .unwrap();
    let original_receipt = match first {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };
    let cursor_before = store
        .get_source_cursor(candidate.source(), candidate.scope())
        .await
        .unwrap();
    let counts_before = user_table_counts(&database_path);

    let duplicate_write =
        ObservationWrite::new(candidate, None, original_receipt.committed_cursor().clone())
            .unwrap();
    let duplicate = store
        .persist_observation(anchored_write_at_with_projection_generation(
            duplicate_write,
            UtcMicros(2),
            ProjectionGenerationId::new("projection.retry.v2").unwrap(),
        ))
        .await
        .unwrap();
    let duplicate_receipt = match duplicate {
        ObservationPersistOutcome::ExactDuplicate(receipt) => receipt,
        other => panic!("exact retry must be reported as a duplicate, got {other:?}"),
    };

    assert_eq!(duplicate_receipt, original_receipt);
    assert_eq!(
        store
            .get_source_cursor(
                original_receipt.observation().source(),
                original_receipt.observation().scope(),
            )
            .await
            .unwrap(),
        cursor_before
    );
    assert_eq!(user_table_counts(&database_path), counts_before);
}

#[tokio::test]
async fn relocated_native_duplicate_advances_coverage_without_reinserting_observation() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let original = native_observation(
        1,
        41,
        42,
        "receipt.native.original",
        "hermes.message.stable",
        "stable payload",
    );
    let original_outcome = store
        .persist_observation(native_write(original.clone(), None))
        .await
        .unwrap();
    let original_receipt = match original_outcome {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first native observation must commit, got {other:?}"),
    };
    let original_cursor = native_cursor(1, 42);
    let counts_after_original = user_table_counts(&database_path);

    let relocated = native_observation(
        2,
        71,
        72,
        "receipt.native.relocated",
        "hermes.message.stable",
        "stable payload",
    );
    assert_eq!(relocated.observation_id(), original.observation_id());
    let relocated_write = native_write(relocated.clone(), Some(original_cursor));
    let relocated_outcome = store
        .persist_observation(relocated_write.clone())
        .await
        .unwrap();
    let coverage_receipt = match relocated_outcome {
        ObservationPersistOutcome::CoveredDuplicate(receipt) => receipt,
        other => panic!("relocated native record must advance coverage, got {other:?}"),
    };
    assert_eq!(coverage_receipt.sequence(), original_receipt.sequence());
    assert_eq!(coverage_receipt.observation(), &original);
    assert_eq!(
        coverage_receipt.retrieval_anchor(),
        original_receipt.retrieval_anchor()
    );
    assert_eq!(coverage_receipt.committed_cursor(), &native_cursor(2, 72));
    assert_eq!(
        store
            .get_source_cursor(&native_source(), &scope())
            .await
            .unwrap(),
        Some(native_cursor(2, 72))
    );
    let deltas = table_deltas(&counts_after_original, &user_table_counts(&database_path));
    assert_eq!(
        deltas,
        BTreeMap::from([
            ("sanitization_receipts".to_owned(), 1),
            ("source_cursor_advances".to_owned(), 1),
        ])
    );

    let stored = store
        .get_observation(original.observation_id())
        .await
        .unwrap()
        .expect("original observation remains authoritative");
    assert_eq!(stored.sequence(), original_receipt.sequence());
    assert_eq!(stored.observation(), &original);
    let replay = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].observation(), &original);

    let conn = rusqlite::Connection::open(&database_path).unwrap();
    let mut statement = conn
        .prepare("SELECT coverage_json, reason, receipt_id FROM source_cursor_advances")
        .unwrap();
    let rows = statement
        .query_map((), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let coverage: ObservationCoverageV1 = serde_json::from_str(&row.0).unwrap();
    assert_eq!(coverage.generation().generation_id(), 2);
    assert_eq!(
        coverage.ordering_domain(),
        ObservationOrderingDomainV1::SqliteRowId
    );
    assert_eq!(
        coverage.range(),
        ObservationSourceRangeV1::new(71, 72).unwrap()
    );
    assert_eq!(row.1, "duplicate_observation");
    assert_eq!(row.2, "receipt.native.relocated");
    drop(statement);
    drop(conn);

    let counts_after_relocation = user_table_counts(&database_path);
    let retry = store.persist_observation(relocated_write).await.unwrap();
    assert!(matches!(
        retry,
        ObservationPersistOutcome::CoveredDuplicate(_)
    ));
    assert_eq!(user_table_counts(&database_path), counts_after_relocation);

    let next = native_observation(
        2,
        72,
        73,
        "receipt.native.next",
        "hermes.message.next",
        "next payload",
    );
    let next_outcome = store
        .persist_observation(native_write(next.clone(), Some(native_cursor(2, 72))))
        .await
        .unwrap();
    assert!(matches!(
        next_outcome,
        ObservationPersistOutcome::Committed(_)
    ));
    assert_eq!(
        store
            .get_source_cursor(&native_source(), &scope())
            .await
            .unwrap(),
        Some(native_cursor(2, 73))
    );
    let replay = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].observation(), &original);
    assert_eq!(replay[1].observation(), &next);
}

#[tokio::test]
async fn cursor_only_progress_persists_non_payload_receipt_and_retries_idempotently() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let before = user_table_counts(&database_path);
    let advance = cursor_advance(None, 0, 10, NonDurableFrameReason::BlankFrame);

    assert_eq!(advance.covered(), ClaudeByteRangeV1::new(0, 10).unwrap());
    assert_eq!(advance.reason(), NonDurableFrameReason::BlankFrame);
    assert_eq!(
        store.advance_source_cursor(advance.clone()).await.unwrap(),
        CursorAdvanceOutcome::Committed
    );
    assert_eq!(
        store.advance_source_cursor(advance).await.unwrap(),
        CursorAdvanceOutcome::ExactDuplicate
    );
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(10))
    );
    assert!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        table_deltas(&before, &user_table_counts(&database_path)),
        BTreeMap::from([
            ("source_cursor_advances".to_owned(), 1),
            ("source_cursors".to_owned(), 1),
        ])
    );

    let conn = rusqlite::Connection::open(&database_path).unwrap();
    let receipt = conn
        .query_row(
            "SELECT coverage_json, reason FROM source_cursor_advances",
            (),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
    let coverage: ObservationCoverageV1 = serde_json::from_str(&receipt.0).unwrap();
    assert_eq!(coverage.range().start(), 0);
    assert_eq!(coverage.range().end(), 10);
    assert_eq!(receipt.1, "blank_frame");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM source_cursor_advances", (), |row| row
            .get::<_, i64>(0),)
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cursor_only_retry_rejects_same_cursor_with_different_reason() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    store
        .advance_source_cursor(cursor_advance(
            None,
            0,
            10,
            NonDurableFrameReason::BlankFrame,
        ))
        .await
        .unwrap();

    assert!(matches!(
        store
            .advance_source_cursor(cursor_advance(
                None,
                0,
                10,
                NonDurableFrameReason::OutOfScope,
            ))
            .await,
        Err(ObservationStoreError::CursorAdvanceCollision)
    ));
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(10))
    );
}

#[tokio::test]
async fn cursor_only_retry_rejects_same_cursor_with_different_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    store
        .advance_source_cursor(cursor_advance(
            None,
            0,
            10,
            NonDurableFrameReason::BlankFrame,
        ))
        .await
        .unwrap();

    assert!(matches!(
        store
            .advance_source_cursor(cursor_advance(
                Some(cursor(5)),
                5,
                10,
                NonDurableFrameReason::BlankFrame,
            ))
            .await,
        Err(ObservationStoreError::CursorAdvanceCollision)
    ));
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(10))
    );
}

#[tokio::test]
async fn contiguous_observation_commit_is_atomic() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    store
        .advance_source_cursor(cursor_advance(
            None,
            0,
            10,
            NonDurableFrameReason::OutOfScope,
        ))
        .await
        .unwrap();
    let candidate = observation(10, 30, "receipt.contiguous", "retained payload");
    let write = write(candidate.clone(), Some(cursor(10)));

    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER fail_contiguous_enqueue
             BEFORE INSERT ON projection_queue BEGIN
                SELECT RAISE(ABORT, 'injected contiguous write failure');
             END;",
        )
        .unwrap();
    assert!(matches!(
        store.persist_observation(write.clone()).await,
        Err(ObservationStoreError::Storage { .. })
    ));
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(10))
    );
    assert!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .is_none()
    );

    raw_conn
        .execute_batch("DROP TRIGGER fail_contiguous_enqueue")
        .unwrap();
    store.persist_observation(write).await.unwrap();
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(30))
    );
    assert!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn cursor_only_progress_rejects_non_contiguous_and_stale_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    store
        .advance_source_cursor(cursor_advance(
            None,
            0,
            10,
            NonDurableFrameReason::SanitizerRejected,
        ))
        .await
        .unwrap();

    let stale = cursor_advance(
        Some(cursor(0)),
        0,
        20,
        NonDurableFrameReason::SanitizerRejected,
    );
    assert!(matches!(
        store.advance_source_cursor(stale).await,
        Err(ObservationStoreError::CursorConflict { expected, actual })
            if expected.as_ref() == &Some(cursor(0)) && actual.as_ref() == &Some(cursor(10))
    ));
    assert!(matches!(
        ObservationCursorAdvance::new(
            source(),
            scope(),
            ClaudeFileGenerationV1::new(GENERATION).unwrap(),
            Some(cursor(10)),
            ClaudeByteRangeV1::new(11, 20).unwrap(),
            NonDurableFrameReason::BlankFrame,
        ),
        Err(ObservationStoreError::CursorCoverageMismatch)
    ));
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(10))
    );
}

#[tokio::test]
async fn sanitizer_cursor_progress_persists_typed_nonpayload_receipt_atomically() {
    let tmp = TempDir::new().unwrap();
    let database_path;
    {
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        database_path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        let advance = cursor_advance(None, 0, 10, NonDurableFrameReason::SanitizerRejected);
        assert_eq!(
            store.advance_source_cursor(advance.clone()).await.unwrap(),
            CursorAdvanceOutcome::Committed
        );
        assert_eq!(
            store.advance_source_cursor(advance).await.unwrap(),
            CursorAdvanceOutcome::ExactDuplicate
        );
    }

    let conn = rusqlite::Connection::open(&database_path).unwrap();
    let row = conn
        .query_row(
            "SELECT advance.reason, advance.receipt_id,
                    receipt.payload_digest, receipt.receipt_json
             FROM source_cursor_advances AS advance
             JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = advance.receipt_id",
            (),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "sanitizer_rejected");
    let receipt_id = row.1;
    assert!(receipt_id.starts_with("receipt.cursor.0.10."));
    assert_eq!(row.2, "");
    let receipt: SanitizationReceiptV1 = serde_json::from_str(&row.3).unwrap();
    assert_eq!(receipt.disposition(), SanitizerDispositionV1::Rejected);
    assert!(receipt.payload().is_none());
}

#[tokio::test]
async fn cursor_only_progress_allows_file_replacement_from_zero_with_exact_cas() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    store
        .advance_source_cursor(cursor_advance(
            None,
            0,
            42,
            NonDurableFrameReason::BlankFrame,
        ))
        .await
        .unwrap();

    let replacement_generation = ClaudeFileGenerationV1::new(GENERATION + 1).unwrap();
    let advance = ObservationCursorAdvance::new(
        source(),
        scope(),
        replacement_generation,
        Some(cursor(42)),
        ClaudeByteRangeV1::new(0, 10).unwrap(),
        NonDurableFrameReason::OutOfScope,
    )
    .unwrap();
    let replacement_cursor =
        ClaudeSourceCursorV1::new(source(), scope(), replacement_generation, 10).unwrap();

    assert_eq!(
        store.advance_source_cursor(advance.clone()).await.unwrap(),
        CursorAdvanceOutcome::Committed
    );
    assert_eq!(
        store.advance_source_cursor(advance).await.unwrap(),
        CursorAdvanceOutcome::ExactDuplicate
    );
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(replacement_cursor)
    );
}

#[test]
fn cursor_only_progress_rejects_file_replacement_after_zero() {
    assert!(matches!(
        ObservationCursorAdvance::new(
            source(),
            scope(),
            ClaudeFileGenerationV1::new(GENERATION + 1).unwrap(),
            Some(cursor(42)),
            ClaudeByteRangeV1::new(1, 10).unwrap(),
            NonDurableFrameReason::OutOfScope,
        ),
        Err(ObservationStoreError::CursorCoverageMismatch)
    ));
}

#[tokio::test]
async fn cursor_only_progress_survives_restart() {
    let tmp = TempDir::new().unwrap();
    {
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        store
            .advance_source_cursor(cursor_advance(
                None,
                0,
                10,
                NonDurableFrameReason::SanitizerQuarantined,
            ))
            .await
            .unwrap();
    }

    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(10))
    );
    store
        .advance_source_cursor(cursor_advance(
            Some(cursor(10)),
            10,
            20,
            NonDurableFrameReason::SanitizerQuarantined,
        ))
        .await
        .unwrap();
    assert_eq!(
        store.get_source_cursor(&source(), &scope()).await.unwrap(),
        Some(cursor(20))
    );
}

#[tokio::test]
async fn identity_collision_is_typed_and_leaves_all_authoritative_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let original = observation(0, 100, "receipt.collision.original", "original payload");
    let colliding = observation(0, 100, "receipt.collision.candidate", "different payload");

    store
        .persist_observation(write(original.clone(), None))
        .await
        .unwrap();
    let stored_before = store
        .get_observation(original.observation_id())
        .await
        .unwrap();
    let cursor_before = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let replay_before = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    let counts_before = user_table_counts(&database_path);

    let error = store
        .persist_observation(write(colliding.clone(), None))
        .await
        .expect_err("same canonical identity with another payload must collide");
    match error {
        ObservationStoreError::ObservationCollision {
            observation_id,
            existing_digest,
            candidate_digest,
            outcome,
        } => {
            assert_eq!(observation_id.as_ref(), original.observation_id());
            assert_eq!(
                existing_digest.as_ref(),
                original.payload_reference().digest()
            );
            assert_eq!(
                candidate_digest.as_ref(),
                colliding.payload_reference().digest()
            );
            assert_eq!(outcome, ObservationCollisionOutcomeV1::IdentityCollision);
        }
        other => panic!("expected typed observation collision, got {other:?}"),
    }

    assert_eq!(
        store
            .get_observation(original.observation_id())
            .await
            .unwrap(),
        stored_before
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        cursor_before
    );
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap(),
        replay_before
    );
    assert_eq!(user_table_counts(&database_path), counts_before);
}

#[tokio::test]
async fn duplicate_identity_with_a_different_receipt_is_rejected_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let original = observation(0, 100, "receipt.retry.original", "stable payload");
    let mismatched_receipt = observation(0, 100, "receipt.retry.changed", "stable payload");

    store
        .persist_observation(write(original.clone(), None))
        .await
        .unwrap();
    let stored_before = store
        .get_observation(original.observation_id())
        .await
        .unwrap();
    let cursor_before = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let counts_before = user_table_counts(&database_path);

    let error = store
        .persist_observation(write(mismatched_receipt, None))
        .await
        .expect_err("an exact payload retry must preserve its receipt identity and policy");
    assert!(matches!(
        error,
        ObservationStoreError::SanitizationReceiptCollision
    ));
    assert_eq!(
        store
            .get_observation(original.observation_id())
            .await
            .unwrap(),
        stored_before
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        cursor_before
    );
    assert_eq!(user_table_counts(&database_path), counts_before);
}

#[tokio::test]
async fn every_observation_statement_failure_rolls_back_the_authoritative_transaction() {
    for (stage, table) in [
        ("receipt", "sanitization_receipts"),
        ("observation", "observations"),
        ("anchor", "retrieval_anchors"),
        ("anchor_binding", "observation_retrieval_anchors"),
        ("repository_provenance", "observation_repository_provenance"),
        ("cursor", "source_cursors"),
        ("enqueue", "projection_queue"),
    ] {
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let database_path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        let candidate = observation(
            0,
            100,
            &format!("receipt.fault.{stage}"),
            "rollback payload",
        );
        let counts_before = user_table_counts(&database_path);

        let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
        raw_conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_observation_{stage}
                 BEFORE INSERT ON {table} BEGIN
                    SELECT RAISE(ABORT, 'injected {stage} statement failure');
                 END;"
            ))
            .unwrap();

        let error = store
            .persist_observation(write(candidate.clone(), None))
            .await
            .expect_err("the injected statement fault must fail persistence");
        assert!(
            matches!(error, ObservationStoreError::Storage { .. }),
            "{stage} fault must surface as a storage error, got {error:?}"
        );
        assert!(
            store
                .get_observation(candidate.observation_id())
                .await
                .unwrap()
                .is_none(),
            "{stage} fault leaked an observation"
        );
        assert_eq!(
            store
                .get_source_cursor(candidate.source(), candidate.scope())
                .await
                .unwrap(),
            None,
            "{stage} fault advanced the cursor"
        );
        assert!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap()
                .is_empty(),
            "{stage} fault leaked replay state"
        );
        assert_eq!(
            user_table_counts(&database_path),
            counts_before,
            "{stage} fault did not roll back every table"
        );
    }
}

#[tokio::test]
async fn concurrent_exact_retry_commits_one_sequence_and_returns_one_duplicate() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store_left = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store_right = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let candidate = observation(0, 100, "receipt.concurrent", "concurrent payload");
    let counts_before = user_table_counts(&database_path);

    let left = store_left.persist_observation(write(candidate.clone(), None));
    let right = store_right.persist_observation(write(candidate.clone(), None));
    let (left, right) = tokio::join!(left, right);
    let outcomes = [left.unwrap(), right.unwrap()];
    let committed = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ObservationPersistOutcome::Committed(receipt) => Some(receipt),
            ObservationPersistOutcome::ExactDuplicate(_)
            | ObservationPersistOutcome::CoveredDuplicate(_) => None,
        })
        .expect("one concurrent writer must commit");
    let duplicate = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ObservationPersistOutcome::ExactDuplicate(receipt) => Some(receipt),
            ObservationPersistOutcome::Committed(_)
            | ObservationPersistOutcome::CoveredDuplicate(_) => None,
        })
        .expect("the other concurrent writer must observe the duplicate");

    assert_eq!(committed, duplicate);
    let replay = store_left
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].sequence(), committed.sequence());
    assert_eq!(replay[0].observation(), &candidate);
    let deltas = table_deltas(&counts_before, &user_table_counts(&database_path));
    assert_eq!(deltas.len(), 7);
    assert!(deltas.values().all(|delta| *delta == 1));
}

#[tokio::test]
async fn stale_exact_cas_cursor_conflict_rolls_back_every_candidate_write() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let first = observation(0, 100, "receipt.cas.first", "first payload");
    let stale_candidate = observation(0, 200, "receipt.cas.stale", "stale payload");

    store
        .persist_observation(write(first.clone(), None))
        .await
        .unwrap();
    let durable_cursor = cursor(100);
    let replay_before = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    let counts_before = user_table_counts(&database_path);

    let error = store
        .persist_observation(write(stale_candidate.clone(), None))
        .await
        .expect_err("a stale exact-CAS owner must lose");
    assert!(matches!(
        error,
        ObservationStoreError::CursorConflict { expected, actual }
            if expected.as_ref().is_none()
                && actual.as_ref().as_ref() == Some(&durable_cursor)
    ));

    assert_eq!(
        store
            .get_source_cursor(first.source(), first.scope())
            .await
            .unwrap(),
        Some(durable_cursor)
    );
    assert!(
        store
            .get_observation(stale_candidate.observation_id())
            .await
            .unwrap()
            .is_none(),
        "the stale observation must roll back"
    );
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap(),
        replay_before
    );
    assert_eq!(user_table_counts(&database_path), counts_before);
}

#[tokio::test]
async fn point_read_and_replay_follow_authoritative_sequence_order() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let observations = [
        observation(0, 10, "receipt.replay.1", "payload one"),
        observation(10, 20, "receipt.replay.2", "payload two"),
        observation(20, 30, "receipt.replay.3", "payload three"),
    ];
    let mut sequences = Vec::new();
    let mut expected_cursor = None;

    for candidate in &observations {
        let outcome = store
            .persist_observation(write(candidate.clone(), expected_cursor.clone()))
            .await
            .unwrap();
        let receipt = match outcome {
            ObservationPersistOutcome::Committed(receipt) => receipt,
            other => panic!("new observation must commit, got {other:?}"),
        };
        sequences.push(receipt.sequence());
        expected_cursor = Some(receipt.committed_cursor().clone());
    }

    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let point_read = store
        .get_observation(observations[1].observation_id())
        .await
        .unwrap()
        .expect("middle observation must be point-readable");
    assert_eq!(point_read.sequence(), sequences[1]);
    assert_eq!(point_read.observation(), &observations[1]);

    let replay = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.len(), observations.len());
    assert_eq!(
        replay
            .iter()
            .map(|stored| stored.sequence())
            .collect::<Vec<_>>(),
        sequences
    );
    assert_eq!(
        replay
            .iter()
            .map(|stored| stored.observation())
            .collect::<Vec<_>>(),
        observations.iter().collect::<Vec<_>>()
    );

    let page = store
        .replay_observations(ObservationReplayRequest::new(sequences[0], 1).unwrap())
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].sequence(), sequences[1]);
    assert_eq!(page[0].observation(), &observations[1]);
}

// These store-contract cases construct provider-tagged durable records directly.
// They do not exercise provider transcript parsers or JSONL framing.
#[tokio::test]
async fn cross_provider_duplicate_conflict_reorder_non_durable_malformed_frame_and_restart_are_idempotent()
 {
    for provider in CROSS_PROVIDERS {
        let session_id = format!("session.cross-store.{provider}");
        let record_id = format!("{provider}.message.cross-store");
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let database_path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        let source = provider_source(provider, &session_id);
        let counts_before_commit = user_table_counts(&database_path);
        let original = provider_observation(ProviderObservationFixture {
            provider,
            session_id: &session_id,
            generation: GENERATION,
            start: 0,
            end: 1,
            receipt_id: &format!("receipt.cross.{provider}.original"),
            native_record_id: &record_id,
            body: "stable cross-provider payload",
        });

        let first = store
            .persist_observation(provider_write(original.clone(), None))
            .await
            .unwrap();
        let original_receipt = match first {
            ObservationPersistOutcome::Committed(receipt) => receipt,
            other => panic!("{provider}: first persist must commit, got {other:?}"),
        };
        let cursor_after_commit = store.get_source_cursor(&source, &scope()).await.unwrap();
        let counts_after_commit = user_table_counts(&database_path);
        assert_eq!(original_receipt.observation(), &original, "{provider}");
        assert_eq!(
            cursor_after_commit.as_ref(),
            Some(original_receipt.committed_cursor()),
            "{provider}"
        );
        let commit_deltas = table_deltas(&counts_before_commit, &counts_after_commit);
        assert_eq!(commit_deltas.len(), 8, "{provider}: {commit_deltas:?}");
        assert!(
            commit_deltas.values().all(|delta| *delta == 1),
            "{provider}: {commit_deltas:?}"
        );
        assert_eq!(
            commit_deltas.get("projection_queue"),
            Some(&1),
            "{provider}"
        );
        let replay_after_commit = store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(replay_after_commit.len(), 1, "{provider}");

        let duplicate = store
            .persist_observation(provider_write(original.clone(), None))
            .await
            .unwrap();
        assert!(
            matches!(duplicate, ObservationPersistOutcome::ExactDuplicate(_)),
            "{provider}: exact retry must be ExactDuplicate, got {duplicate:?}"
        );
        assert_eq!(
            store.get_source_cursor(&source, &scope()).await.unwrap(),
            cursor_after_commit,
            "{provider}"
        );
        assert_eq!(
            user_table_counts(&database_path),
            counts_after_commit,
            "{provider}"
        );

        let colliding = provider_observation(ProviderObservationFixture {
            provider,
            session_id: &session_id,
            generation: GENERATION,
            start: 0,
            end: 1,
            receipt_id: &format!("receipt.cross.{provider}.collision"),
            native_record_id: &record_id,
            body: "conflicting cross-provider payload",
        });
        let collision = store
            .persist_observation(provider_write(colliding, None))
            .await
            .expect_err("{provider}: conflicting identity must fail closed");
        assert!(
            matches!(
                collision,
                ObservationStoreError::ObservationCollision { .. }
            ),
            "{provider}: expected ObservationCollision, got {collision:?}"
        );
        assert_eq!(
            store.get_source_cursor(&source, &scope()).await.unwrap(),
            cursor_after_commit,
            "{provider}"
        );
        assert_eq!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap(),
            replay_after_commit,
            "{provider}"
        );
        assert_eq!(
            user_table_counts(&database_path),
            counts_after_commit,
            "{provider}"
        );

        let reordered = provider_observation(ProviderObservationFixture {
            provider,
            session_id: &session_id,
            generation: GENERATION,
            start: 0,
            end: 2,
            receipt_id: &format!("receipt.cross.{provider}.reorder"),
            native_record_id: &format!("{provider}.message.reordered"),
            body: "reordered payload",
        });
        let reorder_error = store
            .persist_observation(provider_write(reordered.clone(), None))
            .await
            .expect_err("{provider}: stale CAS reorder must roll back");
        assert!(
            matches!(reorder_error, ObservationStoreError::CursorConflict { .. }),
            "{provider}: expected CursorConflict, got {reorder_error:?}"
        );
        assert!(
            store
                .get_observation(reordered.observation_id())
                .await
                .unwrap()
                .is_none(),
            "{provider}: reordered candidate must not persist"
        );
        assert_eq!(
            user_table_counts(&database_path),
            counts_after_commit,
            "{provider}"
        );

        // A complete malformed frame advances non-durable coverage without an observation.
        // This does not model an incomplete JSONL tail.
        let malformed_advance_outcome = store
            .advance_source_cursor(provider_malformed_advance(
                provider,
                &session_id,
                cursor_after_commit.clone(),
                1,
                2,
            ))
            .await
            .unwrap();
        assert_eq!(
            malformed_advance_outcome,
            CursorAdvanceOutcome::Committed,
            "{provider}"
        );
        let cursor_after_malformed_frame =
            Some(provider_cursor(provider, &session_id, GENERATION, 2));
        assert_eq!(
            store.get_source_cursor(&source, &scope()).await.unwrap(),
            cursor_after_malformed_frame,
            "{provider}"
        );
        assert_eq!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap()
                .len(),
            1,
            "{provider}: malformed-frame coverage must not invent observations"
        );
        let malformed_advance_retry = store
            .advance_source_cursor(provider_malformed_advance(
                provider,
                &session_id,
                cursor_after_commit.clone(),
                1,
                2,
            ))
            .await
            .unwrap();
        assert_eq!(
            malformed_advance_retry,
            CursorAdvanceOutcome::ExactDuplicate,
            "{provider}"
        );

        let after_malformed_frame = provider_observation(ProviderObservationFixture {
            provider,
            session_id: &session_id,
            generation: GENERATION,
            start: 2,
            end: 3,
            receipt_id: &format!("receipt.cross.{provider}.after-malformed"),
            native_record_id: &format!("{provider}.message.after-malformed"),
            body: "payload after non-durable malformed frame",
        });
        let after_malformed_frame_outcome = store
            .persist_observation(provider_write(
                after_malformed_frame.clone(),
                cursor_after_malformed_frame.clone(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(
                after_malformed_frame_outcome,
                ObservationPersistOutcome::Committed(_)
            ),
            "{provider}: observation after malformed-frame coverage must commit, got {after_malformed_frame_outcome:?}"
        );
        let observation_id = original.observation_id().clone();
        let after_malformed_frame_id = after_malformed_frame.observation_id().clone();
        let counts_before_restart = user_table_counts(&database_path);
        let replay_before_restart = store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(replay_before_restart.len(), 2, "{provider}");
        drop(runtime);

        // Crash/restart + commit-before-ack: reopen and retry the original write.
        let runtime = profile_runtime(&tmp).await;
        assert_eq!(
            runtime.database_path(HostAdmissionScope::Profile),
            Some(database_path.as_path())
        );
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let restarted_duplicate = store
            .persist_observation(provider_write(original.clone(), None))
            .await
            .unwrap();
        let restarted_receipt = match restarted_duplicate {
            ObservationPersistOutcome::ExactDuplicate(receipt) => receipt,
            other => panic!("{provider}: restart retry must be ExactDuplicate, got {other:?}"),
        };
        assert_eq!(
            restarted_receipt.observation().observation_id(),
            &observation_id,
            "{provider}"
        );
        assert_eq!(
            restarted_receipt.sequence(),
            original_receipt.sequence(),
            "{provider}"
        );
        assert_eq!(
            store
                .get_observation(&observation_id)
                .await
                .unwrap()
                .expect("original survives restart")
                .observation(),
            &original,
            "{provider}"
        );
        assert!(
            store
                .get_observation(&after_malformed_frame_id)
                .await
                .unwrap()
                .is_some(),
            "{provider}"
        );
        assert_eq!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap(),
            replay_before_restart,
            "{provider}"
        );
        assert_eq!(
            user_table_counts(&database_path),
            counts_before_restart,
            "{provider}"
        );
    }
}
