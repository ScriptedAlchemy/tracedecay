use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationIdV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, ClaudeByteRangeV1,
    ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1,
    ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadDigestV1, PayloadReferenceV1, ProjectionGenerationId,
    ProviderId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
    derive_exact_observation_anchor_id,
};
use tracedecay_store::{
    AnchoredObservationWrite, CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationProjectionStore, ObservationStore, ObservationWrite,
    ProjectionPersistOutcome, ProjectionRebuildOutcome, ProjectionSkipReason, ProjectionStoreError,
    SESSION_MESSAGE_PROJECTOR_VERSION_V4, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use crate::common::isolated_lcm_db_path;

const GENERATION: u64 = 11;

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

fn source(session_id: &str) -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap()
}

fn cursor(session_id: &str, byte_offset: u64) -> ClaudeSourceCursorV1 {
    cursor_in_generation(session_id, GENERATION, byte_offset)
}

fn cursor_in_generation(
    session_id: &str,
    generation: u64,
    byte_offset: u64,
) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        source(session_id),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(generation).unwrap(),
        byte_offset,
    )
    .unwrap()
}

fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.projection-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn observation(
    session_id: &str,
    start: u64,
    end: u64,
    receipt_id: &str,
    payload: Value,
) -> DurableClaudeObservationV1 {
    observation_in_generation(session_id, GENERATION, start, end, receipt_id, payload)
}

fn observation_in_generation(
    session_id: &str,
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    payload: Value,
) -> DurableClaudeObservationV1 {
    DurableClaudeObservationV1::new(
        ClaudeObservationIdentityMaterialV1::new(
            source(session_id),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(generation).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
        )
        .unwrap(),
        receipt(receipt_id, &payload),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn canonical_observation(provider: &str, ordinal: u64) -> DurableObservationV1 {
    canonical_observation_at(
        provider,
        ordinal,
        0,
        1,
        &format!("{provider} convergence canary"),
    )
}

fn canonical_observation_at(
    provider: &str,
    ordinal: u64,
    start: u64,
    end: u64,
    text: &str,
) -> DurableObservationV1 {
    let provider_id = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(format!("session.projection-{provider}")).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider_id.clone(), session_id.clone()).unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(start, end).unwrap();
    let record_id = ObservationId::new(format!("record.projection-{provider}.{ordinal}")).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider_id,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: Some("model.fixture".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();

    DurableObservationV1::new(
        identity,
        receipt(
            &format!("receipt.projection-{provider}.{ordinal}"),
            &payload,
        ),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn canonical_write(observation: DurableObservationV1) -> AnchoredObservationWrite {
    canonical_write_with_cursor(observation, None)
}

fn canonical_write_with_cursor(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    anchored_write(ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap())
}

fn anchored_write(write: ObservationWrite) -> AnchoredObservationWrite {
    let generation = ProjectionGenerationId::new("projection.observation-test.v4").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "projection-test")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = cursor_in_generation(
        observation.source().session_id().as_str(),
        observation.identity().generation().file_id(),
        observation.identity().position().end(),
    );
    anchored_write(ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap())
}

async fn persist(
    store: &GlobalDbObservationStore<'_>,
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> u64 {
    match store
        .persist_observation(write(observation, expected_cursor))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt.sequence(),
        other => panic!("new observation must commit, got {other:?}"),
    }
}

async fn drain_projection_queue(store: &GlobalDbObservationStore<'_>) {
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        store.project_observation(&observation_id).await.unwrap();
    }
}

async fn rebuild_projection_to_completion(
    store: &GlobalDbObservationStore<'_>,
    frontier: u64,
) -> ProjectionRebuildOutcome {
    for _ in 0..32 {
        let outcome = store.rebuild_projection(frontier).await.unwrap();
        if outcome.is_complete() {
            return outcome;
        }
    }
    panic!("projection rebuild did not complete within the bounded test budget");
}

fn conversational_payload(message_id: &str, text: &str) -> Value {
    json!({
        "type": "assistant",
        "uuid": format!("record-{message_id}"),
        "timestamp": "2025-06-15T15:06:40Z",
        "message": {
            "id": message_id,
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "model": "claude-sonnet-4"
        }
    })
}

async fn table_count(tmp: &TempDir, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let quoted = table.replace('"', "\"\"");
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), (), |row| {
        row.get(0)
    })
    .unwrap()
}

async fn checkpoint_database(tmp: &TempDir) {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let checkpoint = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap();
    assert_eq!(checkpoint.0, 0);
    assert_eq!(checkpoint.1, checkpoint.2);
}

struct TestSessionMessageSearchHit {
    message: TestSessionMessage,
}

struct TestSessionMessage {
    message_id: String,
    role: String,
    timestamp: Option<i64>,
    ordinal: i64,
    text: String,
    kind: Option<String>,
    model: Option<String>,
    tool_names: Option<String>,
    source_path: Option<String>,
    source_offset: Option<i64>,
}

async fn search_session_messages(
    tmp: &TempDir,
    query: &str,
    limit: usize,
) -> Vec<TestSessionMessageSearchHit> {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let fts_query = query
        .split_whitespace()
        .filter_map(|word| {
            let sanitized: String = word.chars().filter(|character| *character != '"').collect();
            (!sanitized.is_empty()).then(|| format!("\"{sanitized}\"*"))
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut statement = conn
        .prepare(
            "SELECT message.message_id, message.role, message.timestamp, message.ordinal,
                    message.text, message.kind, message.model, message.tool_names,
                    message.source_path, message.source_offset
             FROM session_messages_fts
             JOIN session_messages AS message ON message.rowid = session_messages_fts.rowid
             JOIN sessions AS session
               ON session.provider = message.provider
              AND session.session_id = message.session_id
             WHERE session_messages_fts MATCH ?1
               AND message.provider = 'claude'
               AND session.project_key = 'user'
             ORDER BY bm25(session_messages_fts)
             LIMIT ?2",
        )
        .unwrap();
    statement
        .query_map(
            rusqlite::params![fts_query, i64::try_from(limit).unwrap()],
            |row| {
                Ok(TestSessionMessageSearchHit {
                    message: TestSessionMessage {
                        message_id: row.get(0)?,
                        role: row.get(1)?,
                        timestamp: row.get(2)?,
                        ordinal: row.get(3)?,
                        text: row.get(4)?,
                        kind: row.get(5)?,
                        model: row.get(6)?,
                        tool_names: row.get(7)?,
                        source_path: row.get(8)?,
                        source_offset: row.get(9)?,
                    },
                })
            },
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

async fn add_other_projector_owner(tmp: &TempDir, observation_id: &CanonicalObservationIdV1) {
    let raw_conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    raw_conn
        .execute(
            "INSERT INTO observation_projection_provenance (
                projector_version, observation_id, receipt_id, output_provider,
                output_message_id, output_digest, message_created
             ) SELECT 'test-projector-v2', observation_id, receipt_id, output_provider,
                      output_message_id, output_digest, 0
               FROM observation_projection_provenance
               WHERE projector_version = ?1 AND observation_id = ?2",
            rusqlite::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id.as_str(),
            ],
        )
        .unwrap();
}

async fn audited_projection_fixture(session_id: &str, message_id: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(
        session_id,
        0,
        100,
        &format!("receipt.{message_id}"),
        conversational_payload(message_id, "audited projection body"),
    );
    persist(&store, candidate, None).await;
    drain_projection_queue(&store).await;
    drop(runtime);

    let audited = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("projected authority must pass its exhaustive audit");
    drop(audited);
    tmp
}

async fn projection_counts(tmp: &TempDir) -> (i64, i64, i64, i64, i64, i64) {
    (
        table_count(tmp, "sessions").await,
        table_count(tmp, "session_messages").await,
        table_count(tmp, "observation_projection_provenance").await,
        table_count(tmp, "observation_projection_checkpoints").await,
        table_count(tmp, "observation_projection_dispositions").await,
        table_count(tmp, "projection_queue").await,
    )
}

async fn projection_provenance_rows(
    tmp: &TempDir,
) -> Vec<(String, String, String, String, String, String, String)> {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT projector_version, observation_id, retrieval_anchor_id, receipt_id,
                    output_provider, output_message_id, output_digest
             FROM observation_projection_provenance
             ORDER BY observation_id",
        )
        .unwrap();
    statement
        .query_map((), |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

async fn projected_message_texts(tmp: &TempDir) -> Vec<String> {
    projected_message_texts_where(tmp, "WHERE provider = 'claude'").await
}

async fn projected_raw_store_ids(tmp: &TempDir) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT message_id, store_id FROM lcm_raw_messages
             WHERE provider = 'claude' ORDER BY message_id",
        )
        .unwrap();
    statement
        .query_map((), |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

async fn all_projected_message_texts(tmp: &TempDir) -> Vec<String> {
    projected_message_texts_where(tmp, "").await
}

async fn projected_message_texts_where(tmp: &TempDir, predicate: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let sql = format!("SELECT text FROM session_messages {predicate} ORDER BY message_id");
    let mut statement = conn.prepare(&sql).unwrap();
    statement
        .query_map((), |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

async fn projection_ownership_rows(tmp: &TempDir) -> Vec<i64> {
    let conn = rusqlite::Connection::open(isolated_lcm_db_path(tmp)).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT message_created
             FROM observation_projection_provenance ORDER BY observation_id",
        )
        .unwrap();
    statement
        .query_map((), |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn projection_output_ids(
    rows: &[(String, String, String, String, String, String, String)],
) -> Vec<String> {
    let mut ids = rows.iter().map(|row| row.5.clone()).collect::<Vec<_>>();
    ids.sort();
    ids
}

mod adoption;
mod failure_audit;
mod message_ids;
mod queue;
mod rebuild;
