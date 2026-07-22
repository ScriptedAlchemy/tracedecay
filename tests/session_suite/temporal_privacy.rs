use std::fs;
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::Digest;
use tempfile::TempDir;
use tracedecay::application::context::{
    CancellationToken, CapabilityDigest, ConfigurationDigest, MonotonicDeadline, PolicyDigest,
    ProfileId, RequestBudgets, RequestContext, RequestId, ResolvedSessionIdentity, SessionRootId,
    SessionStoreId,
};
use tracedecay::application::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalService,
    SessionScopeAuthorizationRequest, SessionScopeAuthorizer, SessionTemporalQuery,
};
use tracedecay::global_db::{GlobalDb, GlobalDbSessionTemporalExecution};
use tracedecay::query::temporal::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay::query::temporal::ranking::DiversityLimits;
use tracedecay::sessions::codex;
use tracedecay::sessions::lcm::types::{
    LcmImmutableSummaryPublication, LcmSummaryPublicationDisposition,
};
use tracedecay::sessions::lcm::{LcmError, LcmSourceRef, LcmSummaryNodeDraft};
use tracedecay::store::{GlobalDbObservationStore, GlobalDbSessionTemporalStore};
use tracedecay_domain::{
    ActorId, CanonicalObservationEnvelopeV1, CanonicalObservationFactV1, CanonicalObservationIdV1,
    DurableObservationV1, MessageOccurrenceIdV1, MessageOccurrenceRecordV1, ObservationId,
    ProjectionOutputOrdinalV1, RetrievalGrainV1, SessionCursorKeyIdV1, SessionCursorVersionV1,
    SessionId, SessionProjectionGenerationV1, SignedCursorKeyRefV1, TemporalModeV1, UtcMicros,
};
use tracedecay_store::{
    ObservationProjectionStore, ObservationReplayRequest, ObservationStore,
    SessionFrozenWatermarksV1, SessionGenerationActivationRequestV1,
    SessionGenerationRebuildDispositionV1, SessionGenerationRebuildRequestV1,
    SessionTemporalCapabilitiesV1, SessionTemporalCapabilityV1,
    SessionTemporalProjectionBatchDispositionV1, SessionTemporalProjectionBatchV1,
    SessionTemporalProjectionStore, SessionTemporalSnapshotV1,
};

use crate::common::{isolated_lcm_db_path, lcm_dag_session, open_lcm_db};

const SESSION_ID: &str = "codex-golden-session";
const SAFE_PHRASE: &str = "The billing pipeline regression is fixed.";
const SAFE_TERM: &str = "billing";
const NATIVE_TOKEN_CANARY: &str = "credential-redacted";
const NATIVE_PATH_CANARY: &str = "/secret/project";
const LEGACY_CANARY: &str = "sk-proj-legacy-unsanitized-canary-1234567890";
const DIGEST: [u8; 32] = [0x5a; 32];
const CURSOR_KEY_ID: &str = "temporal-privacy-key";
const FIXTURE_SOURCE_FRONTIER: u64 = 3;

struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.temporal.privacy").unwrap(),
            1,
            context,
            request,
        )
    }
}

struct DenyAuthorizer;

impl SessionScopeAuthorizer for DenyAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Err(SessionAuthorizationError::Denied)
    }
}

#[derive(Clone, Copy)]
struct Words;

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &str {
        "privacy-words-v1"
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

struct AdmittedCodexFixture {
    message: DurableObservationV1,
    message_anchor: tracedecay_domain::RetrievalAnchorRecordV2,
    temporal_sources: Vec<(
        DurableObservationV1,
        tracedecay_domain::RetrievalAnchorRecordV2,
        usize,
    )>,
}

impl AdmittedCodexFixture {
    fn occurrences(&self) -> Vec<MessageOccurrenceRecordV1> {
        self.temporal_sources
            .iter()
            .flat_map(|(observation, anchor, output_count)| {
                (0..*output_count).map(|output_ordinal| {
                    occurrence(observation, anchor, u32::try_from(output_ordinal).unwrap())
                })
            })
            .collect()
    }

    fn output_manifest(&self) -> Vec<(CanonicalObservationIdV1, usize)> {
        self.temporal_sources
            .iter()
            .map(|(observation, _, output_count)| {
                (observation.observation_id().clone(), *output_count)
            })
            .collect()
    }
}

async fn admit_checked_in_codex_fixture(tmp: &TempDir, db: &GlobalDb) -> AdmittedCodexFixture {
    let session_meta =
        include_str!("../fixtures/provider_normalization/codex/session_meta.input.json");
    let agent_message =
        include_str!("../fixtures/provider_normalization/codex/agent_message.input.json");
    let function_call =
        include_str!("../fixtures/provider_normalization/codex/function_call.input.json");
    assert!(
        function_call.contains(NATIVE_TOKEN_CANARY) && function_call.contains(NATIVE_PATH_CANARY),
        "checked-in provider fixture lost its native privacy canaries"
    );
    assert!(
        agent_message.contains(SAFE_PHRASE),
        "checked-in message fixture lost its searchable acceptance phrase"
    );
    assert!(
        [session_meta, agent_message, function_call]
            .iter()
            .all(|fixture| fixture.ends_with('\n')),
        "checked-in JSONL fixtures must retain their record terminators"
    );

    let transcript_dir = tmp.path().join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&transcript_dir).unwrap();
    let transcript = transcript_dir.join("rollout-2026-01-01T00-00-00-codex-golden-session.jsonl");
    let mut transcript_bytes = Vec::new();
    for fixture in [session_meta, agent_message, function_call] {
        let record: serde_json::Value = serde_json::from_str(fixture).unwrap();
        serde_json::to_writer(&mut transcript_bytes, &record).unwrap();
        transcript_bytes.push(b'\n');
    }
    fs::write(&transcript, transcript_bytes).unwrap();
    let expected_bytes = fs::metadata(&transcript).unwrap().len();
    let progress = codex::try_admit_codex_jsonl_observations_for_profile(
        &transcript,
        db,
        Some(SESSION_ID),
        &[],
        None,
    )
    .await
    .expect("checked-in Codex JSONL must pass production admission");
    assert_eq!(
        progress.bytes_consumed, expected_bytes,
        "production parser did not consume the complete checked-in fixture"
    );
    assert!(
        !progress.source_deferred,
        "binding fixture admission must not be deferred"
    );

    let store = GlobalDbObservationStore::new(db);
    let replay = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(
        replay.len(),
        3,
        "session_meta, agent_message, and function_call must all become durable observations"
    );

    let mut message = None;
    let mut temporal_sources = Vec::new();
    let mut saw_redacted_function_call = false;
    for stored in &replay {
        let observation = stored.observation();
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone())
                .expect("production admission must persist a canonical envelope");
        assert_eq!(envelope.provider().as_str(), "codex");
        assert_eq!(envelope.relations().session_id().as_str(), SESSION_ID);
        assert_no_canary(
            "production-admitted canonical observation",
            &serde_json::to_string(&envelope).unwrap(),
        );
        let projection = store
            .project_observation(observation.observation_id())
            .await
            .expect("binding fixture observation must project");
        assert_no_canary(
            "binding fixture projection receipt",
            &format!("{projection:?}"),
        );
        if let tracedecay_store::ProjectionPersistOutcome::Projected(projected) = &projection {
            temporal_sources.push((
                observation.clone(),
                stored.retrieval_anchor().clone(),
                projected.output_count(),
            ));
        }

        for fact in envelope.facts() {
            match fact {
                CanonicalObservationFactV1::Message {
                    content: serde_json::Value::String(text),
                    ..
                } if text == SAFE_PHRASE => {
                    message = Some((observation.clone(), stored.retrieval_anchor().clone()));
                }
                CanonicalObservationFactV1::ToolInvocation {
                    name, arguments, ..
                } if name == "shell" && arguments.is_null() => {
                    saw_redacted_function_call = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        saw_redacted_function_call,
        "production canonicalization must retain typed shell invocation while dropping arguments"
    );
    let (message, message_anchor) =
        message.expect("production canonicalization must retain the checked-in agent message");
    assert_eq!(
        temporal_sources
            .iter()
            .map(|(_, _, output_count)| output_count)
            .sum::<usize>(),
        2,
        "message and sanitized tool invocation must both reach temporal projection"
    );
    AdmittedCodexFixture {
        message,
        message_anchor,
        temporal_sources,
    }
}

fn canonical_message_id(observation: &DurableObservationV1) -> ObservationId {
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone()).unwrap();
    envelope
        .relations()
        .message_id()
        .expect("message observation must carry its native message identity")
        .clone()
}

fn generation(value: u64) -> SessionProjectionGenerationV1 {
    SessionProjectionGenerationV1::new(value).unwrap()
}

fn cursor_key() -> SignedCursorKeyRefV1 {
    SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new(CURSOR_KEY_ID).unwrap(),
        version: SessionCursorVersionV1::new(1).unwrap(),
    }
}

fn watermarks(active_generation: u64, source_frontier: u64) -> SessionFrozenWatermarksV1 {
    SessionFrozenWatermarksV1::new(
        generation(active_generation),
        source_frontier,
        source_frontier,
        0,
    )
    .with_cursor_key(cursor_key())
}

async fn seed_cursor_key(path: &std::path::Path) {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES (?1, 1, ?2, 1, NULL)",
        libsql::params![CURSOR_KEY_ID, vec![0x44_u8; 32]],
    )
    .await
    .unwrap();
}

fn snapshot(
    session_id: &SessionId,
    active_generation: u64,
    source_frontier: u64,
) -> SessionTemporalSnapshotV1 {
    SessionTemporalSnapshotV1::new(
        session_id.clone(),
        UtcMicros(100),
        watermarks(active_generation, source_frontier),
        SessionTemporalCapabilitiesV1::new([
            SessionTemporalCapabilityV1::FrozenWatermarks,
            SessionTemporalCapabilityV1::GenerationRebuild,
        ]),
    )
}

fn occurrence(
    observation: &DurableObservationV1,
    anchor: &tracedecay_domain::RetrievalAnchorRecordV2,
    output_ordinal: u32,
) -> MessageOccurrenceRecordV1 {
    let output_ordinal = ProjectionOutputOrdinalV1::new(output_ordinal);
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone()).unwrap();
    let relations = envelope.relations();
    let message_id = relations
        .message_id()
        .unwrap_or_else(|| envelope.stable_record_id());
    let role = envelope
        .facts()
        .iter()
        .find_map(|fact| match fact {
            CanonicalObservationFactV1::Message { role, .. } => Some(*role),
            CanonicalObservationFactV1::ToolInvocation { .. } => {
                Some(tracedecay_domain::CanonicalMessageRoleV1::Assistant)
            }
            CanonicalObservationFactV1::ToolResult { .. } => {
                Some(tracedecay_domain::CanonicalMessageRoleV1::Tool)
            }
            _ => None,
        })
        .expect("temporal observation must carry one canonical output fact");
    let grouping = || json!({"kind": "provider_native"});
    let valid_time = anchor.occurred_at().map_or_else(
        || json!({"kind": "unknown"}),
        |interval| json!({"kind": "known", "valid_at": interval.start}),
    );
    serde_json::from_value(json!({
        "occurrence_id": MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            output_ordinal,
        ),
        "source_observation_id": observation.observation_id(),
        "projection_output_ordinal": output_ordinal,
        "retrieval_anchor_id": anchor.anchor_id(),
        "session_id": relations.session_id(),
        "thread_id": relations.thread_id().map(|id| id.as_str()),
        "thread_grouping": relations.thread_id().map(|_| grouping()),
        "turn_id": relations.turn_id().map(|id| id.as_str()),
        "turn_grouping": relations.turn_id().map(|_| grouping()),
        "message_id": message_id,
        "agent_id": relations.agent_id().map(|id| id.as_str()),
        "role": role,
        "knowledge_at": anchor.ingested_at(),
        "valid_time": valid_time,
        "evidence": {
            "authority": "canonical_observation",
            "evidence_class": anchor.evidence_class(),
            "source_anchor_id": anchor.anchor_id(),
            "sanitization_receipt": observation.receipt().receipt(),
        },
    }))
    .unwrap()
}

fn temporal_query(text: &str) -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        SessionId::new(SESSION_ID).unwrap(),
        Some("codex".to_string()),
        text,
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        8,
        DiversityLimits::unbounded(),
        ContextBudget {
            max_bytes: 64_000,
            max_tokens: 16_000,
            estimator_version: Words.version().to_string(),
        },
    )
    .unwrap()
}

fn request_context(policy_digest: [u8; 32], request_id: &str) -> RequestContext {
    RequestContext::new(
        ActorId::new("actor.temporal-privacy").unwrap(),
        RequestId::new(request_id).unwrap(),
        ResolvedSessionIdentity::for_profile(
            ProfileId::new("profile.temporal-privacy").unwrap(),
            SessionStoreId::new("store.profile.temporal-privacy").unwrap(),
            SessionRootId::new("root.temporal-privacy").unwrap(),
        ),
        CapabilityDigest::new(DIGEST),
        PolicyDigest::new(policy_digest),
        ConfigurationDigest::new(DIGEST),
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(30)),
        CancellationToken::new(),
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
    )
}

fn policy_digest(anchor: &tracedecay_domain::RetrievalAnchorRecordV2) -> [u8; 32] {
    let encoded = anchor
        .authorization()
        .access_policy_digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap();
    hex::decode(encoded).unwrap().try_into().unwrap()
}

fn summary_publication(summary_id: &str, store_id: i64) -> LcmImmutableSummaryPublication {
    LcmImmutableSummaryPublication {
        summary_id: summary_id.to_string(),
        predecessor_summary_id: None,
        draft: LcmSummaryNodeDraft {
            provider: "codex".to_string(),
            conversation_id: "conversation.temporal-privacy".to_string(),
            session_id: SESSION_ID.to_string(),
            depth: 0,
            summary_text: format!("immutable summary: {SAFE_PHRASE}"),
            source_refs: vec![LcmSourceRef::RawMessage { store_id }],
            source_token_count: 8,
            summary_token_count: 5,
            source_time_start: Some(1_750_000_000),
            source_time_end: Some(1_750_000_001),
            expand_hint: Some("temporal privacy".to_string()),
            metadata_json: Some(r#"{"route":"temporal-privacy"}"#.to_string()),
        },
    }
}

fn assert_no_canary(label: &str, rendered: &str) {
    for canary in [NATIVE_TOKEN_CANARY, NATIVE_PATH_CANARY, LEGACY_CANARY] {
        assert!(
            !rendered.contains(canary),
            "{label} leaked privacy canary {canary}"
        );
    }
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn assert_dynamic_sinks_are_clean(path: &std::path::Path) {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let mut table_rows = conn
        .query(
            "SELECT name
             FROM sqlite_schema
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND (
                   name GLOB 'session_*'
                   OR name IN ('observations', 'sanitization_receipts')
                   OR lower(name) LIKE '%analytics%'
                   OR lower(name) LIKE '%log%'
                   OR lower(name) LIKE '%error%'
                   OR lower(name) LIKE '%receipt%'
               )
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut tables = Vec::new();
    while let Some(row) = table_rows.next().await.unwrap() {
        tables.push(row.get::<String>(0).unwrap());
    }
    drop(table_rows);

    for table in tables {
        let pragma = format!("PRAGMA table_xinfo({})", quote_identifier(&table));
        let mut column_rows = conn.query(&pragma, ()).await.unwrap();
        let mut columns = Vec::new();
        while let Some(row) = column_rows.next().await.unwrap() {
            let name = row.get::<String>(1).unwrap();
            let declared_type = row.get::<String>(2).unwrap_or_default();
            let hidden = row.get::<i64>(6).unwrap_or_default();
            if hidden == 0 && name != "key_material" && !declared_type.eq_ignore_ascii_case("BLOB")
            {
                columns.push(name);
            }
        }
        drop(column_rows);

        for column in columns {
            let sql = format!(
                "SELECT COUNT(*) FROM {} WHERE
                    instr(CAST({} AS TEXT), ?1) > 0 OR
                    instr(CAST({} AS TEXT), ?2) > 0 OR
                    instr(CAST({} AS TEXT), ?3) > 0",
                quote_identifier(&table),
                quote_identifier(&column),
                quote_identifier(&column),
                quote_identifier(&column),
            );
            let mut rows = conn
                .query(
                    &sql,
                    libsql::params![NATIVE_TOKEN_CANARY, NATIVE_PATH_CANARY, LEGACY_CANARY],
                )
                .await
                .unwrap();
            let count = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            assert_eq!(count, 0, "privacy canary reached {table}.{column}");
        }
    }
}

async fn fts_count(path: &std::path::Path, table: &str, query: &str) -> i64 {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let query = format!("\"{}\"", query.replace('"', "\"\""));
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {} MATCH ?1",
        quote_identifier(table),
        quote_identifier(table),
    );
    let mut rows = conn.query(&sql, [query]).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn set_raw_access_state(path: &std::path::Path, message_id: &ObservationId, state: &str) {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE lcm_raw_messages
         SET metadata_json = ?1
         WHERE provider = 'codex' AND session_id = ?2 AND message_id = ?3",
        libsql::params![
            json!({"payload_access": state}).to_string(),
            SESSION_ID,
            message_id.as_str()
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn sanitized_capture_stays_private_through_temporal_summary_and_context() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    seed_cursor_key(&path).await;
    let fixture = admit_checked_in_codex_fixture(&tmp, &db).await;
    let occurrences = fixture.occurrences();
    let observation = fixture.message;
    let anchor = fixture.message_anchor;
    let message_id = canonical_message_id(&observation);
    let observation_json = serde_json::to_string(&observation).unwrap();
    assert!(observation_json.contains(SAFE_PHRASE));
    assert_no_canary("stored observation JSON", &observation_json);

    let owner_db = libsql::Builder::new_local(&path).build().await.unwrap();
    let owner_conn = owner_db.connect().unwrap();
    owner_conn
        .execute(
            "UPDATE sessions
             SET project_key = 'user', project_path = '/fixture'
             WHERE provider = 'codex' AND session_id = ?1",
            [SESSION_ID],
        )
        .await
        .unwrap();
    drop(owner_conn);
    drop(owner_db);
    let raw = db
        .lcm_load_raw_message("codex", message_id.as_str())
        .await
        .expect("sanitized raw projection");
    assert_no_canary("sanitized raw projection", &format!("{raw:?}"));

    let publication = summary_publication("summary.temporal.privacy", raw.store_id);
    let session_id = SessionId::new(SESSION_ID).unwrap();
    let temporal_store = GlobalDbSessionTemporalStore::new(&db);
    temporal_store
        .begin_session_generation_rebuild(
            SessionGenerationRebuildRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, FIXTURE_SOURCE_FRONTIER),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let projection_receipt = temporal_store
        .persist_session_temporal_projection_batch(
            SessionTemporalProjectionBatchV1::new(
                session_id.clone(),
                generation(2),
                watermarks(1, FIXTURE_SOURCE_FRONTIER),
                occurrences,
                vec![],
                vec![],
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_no_canary(
        "temporal projection receipt",
        &format!("{projection_receipt:?}"),
    );
    let activation = temporal_store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id,
                generation(2),
                snapshot(
                    &SessionId::new(SESSION_ID).unwrap(),
                    1,
                    FIXTURE_SOURCE_FRONTIER,
                ),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_no_canary("generation activation receipt", &format!("{activation:?}"));
    let summary = db
        .lcm_publish_immutable_summary(publication.clone())
        .await
        .unwrap();
    assert_no_canary("summary publication receipt", &format!("{summary:?}"));

    assert_eq!(
        fts_count(&path, "session_occurrences_fts", SAFE_TERM).await,
        2,
        "active and superseded generations must both retain the sanitized occurrence"
    );
    assert_eq!(
        fts_count(&path, "session_occurrences_fts", NATIVE_TOKEN_CANARY).await,
        0
    );
    assert_eq!(
        fts_count(&path, "session_occurrences_fts", NATIVE_PATH_CANARY).await,
        0
    );

    let context = request_context(
        policy_digest(&anchor),
        "request.temporal-privacy.authorized",
    );
    let authorized_service = SessionRetrievalService::new(
        AllowAuthorizer,
        GlobalDbSessionTemporalExecution::new(&db),
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let authorized = authorized_service
        .retrieve(&context, temporal_query(SAFE_TERM))
        .await;
    match &authorized {
        SessionRetrievalOutcome::Complete { items, .. }
        | SessionRetrievalOutcome::Partial { items, .. } => {
            assert_eq!(items[0].ranked.len(), 1);
            assert!(items[0].context.rendered.contains(SAFE_PHRASE));
        }
        SessionRetrievalOutcome::CompleteZero { freshness } => panic!(
            "binding fixture is valid (3 native records admitted, canonical message projected, \
             temporal FTS count=1), but production retrieval returned CompleteZero ({freshness:?})"
        ),
        other => panic!(
            "binding fixture is valid, but downstream production retrieval failed: {other:?}"
        ),
    }
    assert_no_canary("authorized context", &format!("{authorized:?}"));

    let denied_service = SessionRetrievalService::new(
        DenyAuthorizer,
        GlobalDbSessionTemporalExecution::new(&db),
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let denied = denied_service
        .retrieve(
            &request_context(policy_digest(&anchor), "request.temporal-privacy.denied"),
            temporal_query(SAFE_TERM),
        )
        .await;
    assert!(matches!(denied, SessionRetrievalOutcome::Denied));
    assert_no_canary("denied context", &format!("{denied:?}"));

    for state in ["redacted", "deleted"] {
        set_raw_access_state(&path, &message_id, state).await;
        let error = db
            .lcm_publish_immutable_summary(summary_publication(
                &format!("summary.temporal.privacy.{state}"),
                raw.store_id,
            ))
            .await
            .expect_err("ineligible source must fail closed");
        assert!(matches!(
            error,
            LcmError::SummarySourceUnavailable {
                reason: ref actual,
                ..
            } if actual == state
        ));
        assert_no_canary("summary source error", &format!("{error:?}\n{error}"));
    }

    let replayed = db
        .lcm_publish_immutable_summary(publication)
        .await
        .expect("immutable summary replay must use frozen authority");
    assert_eq!(
        replayed.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    assert_no_canary("immutable summary replay", &format!("{replayed:?}"));

    assert_dynamic_sinks_are_clean(&path).await;
}

#[tokio::test]
async fn unsanitized_quarantined_legacy_row_never_migrates_into_temporal_sinks() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&lcm_dag_session("claude", "session.temporal.legacy"))
            .await
    );

    let raw_db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref,
            snippet_text, index_text, metadata_json, legacy_source, legacy_truncated
         ) VALUES (
            'claude', 'message.temporal.legacy', 'session.temporal.legacy',
            'user', 1, 1, ?1, ?2, 'inline', NULL,
            'quarantined legacy record', 'quarantined legacy record',
            '{\"payload_access\":\"quarantined\",\"migration\":\"legacy-unsanitized\"}',
            1, 0
         )",
        libsql::params![
            LEGACY_CANARY,
            hex::encode(sha2::Sha256::digest(LEGACY_CANARY.as_bytes()))
        ],
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages
             WHERE provider = 'claude' AND message_id = 'message.temporal.legacy'",
            (),
        )
        .await
        .unwrap();
    let store_id = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    drop(rows);
    conn.execute(
        "DELETE FROM session_temporal_schema_migrations
         WHERE name = 'session-temporal'",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    drop(db);

    let reopened = GlobalDb::open_at(&path).await.unwrap();
    let error = reopened
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.temporal.legacy".to_string(),
            predecessor_summary_id: None,
            draft: LcmSummaryNodeDraft {
                provider: "claude".to_string(),
                conversation_id: "conversation.temporal.legacy".to_string(),
                session_id: "session.temporal.legacy".to_string(),
                depth: 0,
                summary_text: "must not publish".to_string(),
                source_refs: vec![LcmSourceRef::RawMessage { store_id }],
                source_token_count: 1,
                summary_token_count: 1,
                source_time_start: Some(1),
                source_time_end: Some(1),
                expand_hint: None,
                metadata_json: Some(r#"{"migration":"legacy"}"#.to_string()),
            },
        })
        .await
        .expect_err("quarantined legacy source must not publish");
    assert!(matches!(
        error,
        LcmError::SummarySourceUnavailable {
            reason: ref actual,
            ..
        } if actual == "quarantined"
    ));
    assert_no_canary("legacy migration error", &format!("{error:?}\n{error}"));
    assert_eq!(
        fts_count(&path, "session_occurrences_fts", LEGACY_CANARY).await,
        0
    );
    assert_eq!(
        fts_count(&path, "session_summary_nodes_fts", LEGACY_CANARY).await,
        0
    );
    assert_dynamic_sinks_are_clean(&path).await;
}

#[tokio::test]
async fn sanitized_temporal_state_stays_private_across_reopen_and_rebuild_replay() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let session_id = SessionId::new(SESSION_ID).unwrap();
    let (observation_id, output_manifest) = {
        let db = open_lcm_db(&tmp).await;
        seed_cursor_key(&path).await;
        let fixture = admit_checked_in_codex_fixture(&tmp, &db).await;
        let occurrences = fixture.occurrences();
        let output_manifest = fixture.output_manifest();
        let observation = fixture.message;
        let observation_id = observation.observation_id().clone();
        assert_no_canary("pre-reopen observation", &format!("{observation:?}"));

        let owner_db = libsql::Builder::new_local(&path).build().await.unwrap();
        let owner_conn = owner_db.connect().unwrap();
        owner_conn
            .execute(
                "UPDATE sessions
                 SET project_key = 'user', project_path = '/fixture'
                 WHERE provider = 'codex' AND session_id = ?1",
                [SESSION_ID],
            )
            .await
            .unwrap();
        drop(owner_conn);
        drop(owner_db);

        let temporal_store = GlobalDbSessionTemporalStore::new(&db);
        assert_eq!(
            temporal_store
                .begin_session_generation_rebuild(
                    SessionGenerationRebuildRequestV1::new(
                        session_id.clone(),
                        generation(2),
                        snapshot(&session_id, 1, FIXTURE_SOURCE_FRONTIER),
                    )
                    .unwrap(),
                )
                .await
                .unwrap()
                .disposition(),
            SessionGenerationRebuildDispositionV1::Started
        );
        let projection_receipt = temporal_store
            .persist_session_temporal_projection_batch(
                SessionTemporalProjectionBatchV1::new(
                    session_id.clone(),
                    generation(2),
                    watermarks(1, FIXTURE_SOURCE_FRONTIER),
                    occurrences,
                    vec![],
                    vec![],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            projection_receipt.disposition(),
            SessionTemporalProjectionBatchDispositionV1::Applied
        );
        assert_no_canary(
            "pre-reopen projection receipt",
            &format!("{projection_receipt:?}"),
        );
        (observation_id, output_manifest)
    };

    // Reopen through the production open path used by daemon/process restart.
    let db = GlobalDb::open_at(&path)
        .await
        .expect("reopen after durable projection");
    let temporal_store = GlobalDbSessionTemporalStore::new(&db);
    assert_eq!(
        temporal_store
            .begin_session_generation_rebuild(
                SessionGenerationRebuildRequestV1::new(
                    session_id.clone(),
                    generation(2),
                    snapshot(&session_id, 1, FIXTURE_SOURCE_FRONTIER),
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .disposition(),
        SessionGenerationRebuildDispositionV1::Resumed
    );
    let replay = GlobalDbObservationStore::new(&db)
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    let stored = replay
        .iter()
        .find(|row| row.observation().observation_id() == &observation_id)
        .expect("sanitized observation must survive reopen");
    let occurrences = output_manifest
        .iter()
        .flat_map(|(observation_id, output_count)| {
            let row = replay
                .iter()
                .find(|row| row.observation().observation_id() == observation_id)
                .expect("every projected observation must survive reopen");
            (0..*output_count).map(|output_ordinal| {
                occurrence(
                    row.observation(),
                    row.retrieval_anchor(),
                    u32::try_from(output_ordinal).unwrap(),
                )
            })
        })
        .collect();
    assert_no_canary("post-reopen observation", &format!("{stored:?}"));
    assert_eq!(
        temporal_store
            .persist_session_temporal_projection_batch(
                SessionTemporalProjectionBatchV1::new(
                    session_id.clone(),
                    generation(2),
                    watermarks(1, FIXTURE_SOURCE_FRONTIER),
                    occurrences,
                    vec![],
                    vec![],
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .disposition(),
        SessionTemporalProjectionBatchDispositionV1::ExactReplay
    );
    let activation = temporal_store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id,
                generation(2),
                snapshot(
                    &SessionId::new(SESSION_ID).unwrap(),
                    1,
                    FIXTURE_SOURCE_FRONTIER,
                ),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_no_canary("post-reopen activation", &format!("{activation:?}"));

    assert_eq!(
        fts_count(&path, "session_occurrences_fts", SAFE_TERM).await,
        1
    );
    assert_eq!(
        fts_count(&path, "session_occurrences_fts", NATIVE_TOKEN_CANARY).await,
        0
    );
    assert_eq!(
        fts_count(&path, "session_occurrences_fts", NATIVE_PATH_CANARY).await,
        0
    );
    assert_dynamic_sinks_are_clean(&path).await;
}
