use libsql::{Builder, Connection, Value as SqlValue};
use tempfile::tempdir;
use tracedecay_domain::{
    RetrievalAnchorId, RetrievalGrainV1, SessionId, TemporalModeV1, UtcMicros,
};

use super::candidates::*;
use super::cursors::*;
use super::queries::*;
use super::records::*;
use super::rows::*;
use super::*;
use crate::global_db::{GlobalDb, GlobalDbReadSnapshot};
use crate::query::temporal::candidates::CandidateChannel;
use crate::query::temporal::ports::{
    BindingDigest, KernelVersions, PageRequest, TemporalAuthorizedRoot, TemporalExecutionSnapshot,
    TemporalRetrievalScope, TemporalSnapshotRequest, TemporalWatermarks,
};
use crate::query::temporal::ranking::RankingCandidate;
use crate::query::temporal::resolution::ValidatedAuthorization;

const REQUIRED_SCHEMA_INDEXES: &[&str] = &[
    "idx_session_temporal_generations_session_state",
    "idx_session_occurrences_generation_order",
    "idx_session_current_entities_primary_key",
    "idx_session_assertions_subject",
    "idx_session_summary_availability_generation",
    "idx_session_summary_nodes_session_created",
    "session_occurrences_fts",
    "session_summary_nodes_fts",
];
const FOLLOW_UP_SCHEMA_INDEXES: &[&str] = &[
    "session_occurrences(session_id, generation, retrieval_anchor_id, knowledge_at, occurrence_id)",
    "session_assertions(session_id, generation, object_anchor_id, knowledge_at, assertion_id)",
    "session_summary_successors(successor_summary_id, created_at, predecessor_summary_id)",
    "session_occurrences(knowledge_at DESC, session_id, occurrence_id, generation)",
    "session_summary_nodes(created_at DESC, session_id, summary_id)",
];

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn snapshot(generation: u64) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new("session-snapshot").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            TemporalModeV1::Current,
            RetrievalGrainV1::Session,
        )
        .expect("request"),
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn scoped_snapshot(generation: u64, provider: Option<&str>) -> TemporalExecutionSnapshot {
    scoped_snapshot_with_mode(generation, provider, TemporalModeV1::Current)
}

fn scoped_snapshot_with_mode(
    generation: u64,
    provider: Option<&str>,
    mode: TemporalModeV1,
) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new("session-snapshot").expect("session"),
            digest('1'),
            digest('2'),
            digest('3'),
            mode,
            RetrievalGrainV1::Session,
        )
        .expect("request")
        .with_provider_scope(provider.map(str::to_string))
        .expect("provider scope"),
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn root_snapshot_with_mode(
    generation: u64,
    provider: Option<&str>,
    mode: TemporalModeV1,
) -> TemporalExecutionSnapshot {
    let request = TemporalSnapshotRequest::new(
        SessionId::new("session-snapshot").expect("session"),
        digest('1'),
        digest('2'),
        digest('3'),
        mode,
        RetrievalGrainV1::Session,
    )
    .expect("request")
    .with_authorized_root(
        TemporalAuthorizedRoot::profile("profile-1", "store-1", "root-1").expect("profile root"),
    )
    .expect("authorized root")
    .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot)
    .with_provider_scope(provider.map(str::to_string))
    .expect("provider scope");
    TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn record_request() -> PageRequest {
    PageRequest::for_test(32, 64 * 1024, 8 * 1024, 32, 512)
}

fn record_candidate() -> RankingCandidate {
    candidate_for_anchor("anchor-1")
}

fn candidate_for_anchor(anchor_id: &str) -> RankingCandidate {
    RankingCandidate {
        stable_id: "exact:occurrence-1".to_string(),
        anchor_id: RetrievalAnchorId::new(anchor_id).expect("anchor"),
        retriever_record_id: "occurrence-1".to_string(),
        channel: CandidateChannel::ExactMessage,
        raw_score: 1_000,
        knowledge_at_micros: 1,
        logical_message: Some("message-1".to_string()),
        turn: None,
        session: Some("session-snapshot".to_string()),
        source: Some("claude".to_string()),
        evidence_role: Some("user".to_string()),
    }
}

async fn record_kinds(
    db: &GlobalDb,
    snapshot: &TemporalExecutionSnapshot,
    candidate: RankingCandidate,
    request: &PageRequest,
) -> Vec<String> {
    let query = build_record_query(
        snapshot.retrieval_scope(),
        snapshot,
        &[candidate],
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        request.page_item_limit().saturating_add(1),
        request,
    )
    .expect("record query");
    let mut rows = db
        .read_connection()
        .query(&query.sql, query.params)
        .await
        .expect("record rows");
    let mut kinds = Vec::new();
    while let Some(row) = rows.next().await.expect("record row") {
        kinds.push(row.get(3).expect("record kind"));
    }
    kinds
}

async fn insert_generation(db: &GlobalDb, generation: u64) {
    insert_generation_for_session(db, "session-snapshot", generation).await;
}

async fn insert_generation_for_session(db: &GlobalDb, session_id: &str, generation: u64) {
    let frozen = serde_json::json!({
        "active_generation": generation,
        "cursor_key": null,
        "projection_frontier": 0,
        "source_frontier": 0,
        "summary_frontier": 0
    })
    .to_string();
    let generation = i64::try_from(generation).expect("generation");
    // frozen_watermarks_json is immutable after insert; seed it on building.
    db.read_connection()
        .execute(
            "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES (?1, ?2, 'building', ?3, ?2, NULL, NULL, NULL)",
            (session_id, generation, frozen.as_str()),
        )
        .await
        .expect("building generation");
    db.read_connection()
        .execute(
            "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = generation
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
            (session_id, generation),
        )
        .await
        .expect("ready generation");
    db.read_connection()
        .execute(
            "UPDATE session_temporal_generations
                 SET state = 'superseded', completed_at = ?1
                 WHERE session_id = ?2
                   AND generation <> ?1
                   AND state = 'active'",
            (generation, session_id),
        )
        .await
        .expect("supersede prior active generation");
    db.read_connection()
        .execute(
            "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = generation
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
            (session_id, generation),
        )
        .await
        .expect("activate generation");

    let mut rows = db
        .read_connection()
        .query(
            "SELECT frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND generation = ?2
                 LIMIT 1",
            (session_id, generation),
        )
        .await
        .expect("query frozen watermarks");
    let encoded: String = rows
        .next()
        .await
        .expect("row")
        .expect("generation row")
        .get(0)
        .expect("frozen_watermarks_json");
    assert_eq!(
        encoded, frozen,
        "legal building→ready→active transitions must not mutate frozen_watermarks_json"
    );
}

#[test]
fn adapter_contains_only_the_borrowed_global_db_handle() {
    fn assert_exact_fields(adapter: &GlobalDbTemporalReadPort<'_>) {
        let GlobalDbTemporalReadPort { read: _ } = adapter;
    }

    let _ = assert_exact_fields;
    assert_eq!(
        std::mem::size_of::<GlobalDbTemporalReadPort<'static>>(),
        std::mem::size_of::<&'static GlobalDbReadSnapshot>()
    );
}

#[tokio::test]
async fn frozen_generation_survives_rotation_while_a_new_snapshot_observes_drift() {
    let dir = tempdir().expect("temporary directory");
    let db = GlobalDb::try_open_at(&dir.path().join("global.db"))
        .await
        .expect("open database")
        .expect("database");
    insert_generation(&db, 1).await;
    let frozen_snapshot = snapshot(1);
    let frozen_read = db.read_snapshot().await.expect("read snapshot");
    let frozen_adapter = GlobalDbTemporalReadPort::new(&frozen_read);
    frozen_adapter
        .validate_snapshot(&frozen_snapshot)
        .await
        .expect("generation one is frozen active");

    insert_generation(&db, 2).await;

    frozen_adapter
        .validate_snapshot(&frozen_snapshot)
        .await
        .expect("same read snapshot retains generation one");
    let fresh_read = db.read_snapshot().await.expect("fresh read snapshot");
    let fresh_adapter = GlobalDbTemporalReadPort::new(&fresh_read);
    assert!(
        fresh_adapter
            .validate_snapshot(&frozen_snapshot)
            .await
            .is_err()
    );
    fresh_adapter
        .validate_snapshot(&snapshot(2))
        .await
        .expect("new read snapshot sees generation two");
}

#[test]
fn candidate_and_record_cursors_are_stable_and_bounded() {
    let candidate = CandidateCursor {
        clause: 42,
        knowledge_at: 1_234_567,
        session_id: "session-b".to_string(),
        stable_id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_string(),
    };
    let encoded = candidate.encode(256).unwrap();
    assert_eq!(CandidateCursor::decode(Some(&encoded)).unwrap(), candidate);

    let record = RecordCursor {
        candidate: 99_999,
        kind: 4,
        session_id: "session-b".to_string(),
        stable_id: "summary:17".to_string(),
    };
    let encoded = record.encode(256).unwrap();
    assert_eq!(RecordCursor::decode(Some(&encoded)).unwrap(), record);
    assert!(record.encode(8).is_err());
}

#[test]
fn snapshot_uniqueness_probe_reads_at_most_two_rows() {
    let source = include_str!("../retrieval.rs");
    let start = source
        .find("async fn validate_snapshot(")
        .expect("validator");
    let end = source[start..]
        .find("async fn produce_candidates(")
        .map(|offset| start + offset)
        .expect("validator end");
    let validator = &source[start..end];
    assert!(validator.contains("LIMIT 2"));
    assert!(validator.contains("frozen generation is not unique"));
}

#[test]
fn one_hundred_thousand_candidates_are_windowed_before_sql_allocation() {
    let total = 100_000usize;
    let page_items = 37usize;
    let start = 71_111usize;
    let end = bounded_window_end(total, start, page_items.saturating_add(1));
    assert_eq!(end - start, 38);
    assert!(end < total);
}

#[test]
fn mode_sql_is_shaped_without_optional_or_fallback_predicates() {
    let current = RecordModeSql::new(TemporalModeV1::Current, 9);
    assert!(current.occurrence_join.contains("session_current_entities"));
    assert!(!current.occurrence_join.contains(" OR "));

    let as_of = RecordModeSql::new(
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(10),
        },
        9,
    );
    assert!(as_of.occurrence_predicate.contains("o.knowledge_at <= ?9"));
    assert!(as_of.assertion_predicate.contains("a.knowledge_at <= ?9"));

    let evolution = RecordModeSql::new(TemporalModeV1::Evolution, 9);
    assert!(evolution.summary_predicate.contains("availability"));
    let forensic = RecordModeSql::new(TemporalModeV1::Forensic, 9);
    assert_eq!(forensic.summary_predicate, "1 = 1");
}

#[test]
fn candidate_queries_use_keysets_limits_and_mode_indexes() {
    for sql in [
        EXACT_CANDIDATE_QUERY,
        OCCURRENCE_FTS_QUERY,
        TIME_CANDIDATE_QUERY,
        SUMMARY_CANDIDATE_QUERY,
    ] {
        assert!(sql.contains("LIMIT ?"));
        assert!(!sql.to_ascii_uppercase().contains("OFFSET"));
    }
    assert!(TIME_CANDIDATE_QUERY.contains("idx_session_occurrences_generation_order"));
    assert!(OCCURRENCE_FTS_QUERY.contains("session_occurrences_fts MATCH"));
    assert!(SUMMARY_CANDIDATE_QUERY.contains("session_summary_nodes_fts MATCH"));
}

#[test]
fn authorized_root_candidate_queries_use_composite_session_keysets() {
    for sql in [
        ROOT_EXACT_CANDIDATE_QUERY,
        ROOT_OCCURRENCE_FTS_QUERY,
        ROOT_TIME_CANDIDATE_QUERY,
        ROOT_SUMMARY_CANDIDATE_QUERY,
    ] {
        assert!(sql.contains("session_id"));
        assert!(sql.contains("LIMIT ?"));
        assert!(!sql.to_ascii_uppercase().contains("OFFSET"));
    }
    assert!(
        ROOT_EXACT_CANDIDATE_QUERY
            .contains("ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id")
    );
    assert!(
        ROOT_SUMMARY_CANDIDATE_QUERY
            .contains("ORDER BY n.created_at DESC, n.session_id, n.summary_id")
    );
    assert!(ROOT_OCCURRENCE_FTS_QUERY.contains("session_occurrences_fts MATCH"));
    assert!(ROOT_SUMMARY_CANDIDATE_QUERY.contains("session_summary_nodes_fts MATCH"));
    assert_eq!(
        ROOT_OCCURRENCE_FTS_QUERY
            .matches("session_occurrences_fts MATCH")
            .count(),
        1,
        "root-wide FTS must be one calibrated store query, not per-session fan-out"
    );
}

#[test]
fn authorized_root_candidate_queries_bind_durable_anchor_owner_before_materialization() {
    for sql in [
        ROOT_EXACT_CANDIDATE_QUERY,
        ROOT_OCCURRENCE_FTS_QUERY,
        ROOT_TIME_CANDIDATE_QUERY,
        ROOT_SUMMARY_CANDIDATE_QUERY,
    ] {
        assert!(sql.contains("JOIN retrieval_anchors AS authority_anchor"));
        assert!(sql.contains("JOIN sessions AS authority_session"));
        assert!(sql.contains("authority_session.project_key = ?1"));
        assert!(sql.contains("json_extract(authority_anchor.owner_json, '$.kind')"));
    }
}

#[tokio::test]
async fn root_record_authority_binds_the_candidate_source_provider() {
    let dir = tempdir().unwrap();
    let database = Builder::new_local(dir.path().join("root-record-authority.db"))
        .build()
        .await
        .unwrap();
    let conn = database.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL
             );
             INSERT INTO sessions VALUES
                ('provider-good', 'shared-session', 'user'),
                ('provider-bad', 'shared-session', 'different-project');
             INSERT INTO retrieval_anchors VALUES
                ('anchor-1', '{\"kind\":\"profile\"}');
             INSERT INTO session_temporal_generations VALUES
                ('shared-session', 1, 'active');
             INSERT INTO observations VALUES (
                'observation-bad',
                '{\"identity\":{\"source\":{\"provider\":\"provider-bad\"}}}'
             );
             INSERT INTO session_occurrences VALUES (
                'shared-session', 1, 'occurrence-1', 'observation-bad', 'anchor-1'
             );",
    )
    .await
    .unwrap();
    let mut candidate = candidate_for_anchor("anchor-1");
    candidate.session = Some("shared-session".to_string());
    candidate.source = Some("occurrence-1".to_string());
    assert!(
        require_candidate_root_authority(&conn, &candidate, "user", None)
            .await
            .is_err()
    );
}

#[test]
fn provider_scope_is_applied_at_every_candidate_authority_join() {
    for sql in [
        EXACT_CANDIDATE_QUERY,
        OCCURRENCE_FTS_QUERY,
        TIME_CANDIDATE_QUERY,
    ] {
        assert!(sql.contains("JOIN observations AS provider_observation"));
        assert!(sql.contains("$.identity.source.provider"));
        assert!(sql.contains("COALESCE(json_extract"));
        assert!(sql.contains("'claude'"));
    }
    assert!(SUMMARY_CANDIDATE_QUERY.contains("session_summary_sources"));
    assert!(SUMMARY_CANDIDATE_QUERY.contains("JOIN observations AS source_observation"));
    assert!(SUMMARY_CANDIDATE_QUERY.contains("$.identity.source.provider"));
    assert!(SUMMARY_CANDIDATE_QUERY.contains("COALESCE(json_extract"));
    assert!(SUMMARY_CANDIDATE_QUERY.contains("'claude'"));
}

#[test]
fn record_union_filters_provider_and_large_fields_before_materialization() {
    let query = build_record_query(
        &TemporalRetrievalScope::Session(SessionId::new("session-snapshot").expect("session")),
        &scoped_snapshot(1, Some("claude")),
        &[record_candidate()],
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        33,
        &record_request(),
    )
    .expect("record query");
    let records_end = query
        .sql
        .find("SELECT ordinal, kind_rank, stable_id, record_kind")
        .expect("outer records select");
    let records = &query.sql[..records_end];

    assert!(
        records.matches("JOIN observations AS").count() >= 4,
        "occurrence, assertion, copy, and summary-source arms need canonical provider joins"
    );
    assert!(records.matches("$.identity.source.provider").count() >= 5);
    assert!(records.matches("COALESCE(json_extract").count() >= 5);
    assert!(records.matches("'claude'").count() >= 5);
    for field in [
        "evidence_json",
        "proof_json",
        "source_horizon_json",
        "publication_json",
    ] {
        assert!(
            records.contains("length(CAST("),
            "{field} must be byte-bounded in its UNION arm"
        );
        assert!(records.contains(field));
    }
    assert!(records.contains("json_group_array"));
    let source = include_str!("records.rs");
    let builder_start = source.find("fn build_record_query(").expect("builder");
    let builder_end = source[builder_start..]
        .find("struct RecordModeSql")
        .map(|offset| builder_start + offset)
        .expect("builder end");
    let builder = &source[builder_start..builder_end];
    assert!(builder.contains("source_count_cap_param"));
    assert!(builder.contains("source_byte_cap_param"));
    assert!(!builder.contains("LIMIT ?{source_byte_cap_param}"));
}

#[tokio::test]
async fn explain_time_query_uses_generation_order_index() {
    let dir = tempdir().unwrap();
    let database = Builder::new_local(dir.path().join("query-plan.db"))
        .build()
        .await
        .unwrap();
    let conn = database.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );",
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            &format!("EXPLAIN QUERY PLAN {TIME_CANDIDATE_QUERY}"),
            vec![
                SqlValue::Text("session".to_string()),
                SqlValue::Integer(1),
                SqlValue::Null,
                SqlValue::Integer(0),
                SqlValue::Integer(1),
                SqlValue::Integer(i64::MAX),
                SqlValue::Text(String::new()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(1_024),
                SqlValue::Integer(10),
            ],
        )
        .await
        .unwrap();
    let mut details = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        details.push(row.get::<String>(3).unwrap());
    }
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("idx_session_occurrences_generation_order"))
    );
    assert!(details.iter().all(|detail| !detail.contains("SCAN o")));
}

#[tokio::test]
async fn provider_filter_separates_same_session_and_none_reads_all_providers() {
    let dir = tempdir().unwrap();
    let database = Builder::new_local(dir.path().join("provider-scope.db"))
        .build()
        .await
        .unwrap();
    let conn = database.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );
             INSERT INTO observations VALUES
                ('observation-claude', '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'),
                ('observation-codex', '{\"identity\":{\"source\":{\"provider\":\"codex\"}}}');
             INSERT INTO session_occurrences VALUES
                ('shared-session', 1, 'occurrence-claude', 'observation-claude',
                 'anchor-claude', 'message-claude', NULL, 'user', 2),
                ('shared-session', 1, 'occurrence-codex', 'observation-codex',
                 'anchor-codex', 'message-codex', NULL, 'user', 1);",
    )
    .await
    .unwrap();

    async fn occurrence_ids(
        conn: &Connection,
        provider: SqlValue,
    ) -> Result<Vec<String>, libsql::Error> {
        let mut rows = conn
            .query(
                TIME_CANDIDATE_QUERY,
                vec![
                    SqlValue::Text("shared-session".to_string()),
                    SqlValue::Integer(1),
                    provider,
                    SqlValue::Integer(0),
                    SqlValue::Integer(10),
                    SqlValue::Integer(i64::MAX),
                    SqlValue::Text(String::new()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1024),
                    SqlValue::Integer(10),
                ],
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get(0)?);
        }
        Ok(ids)
    }

    assert_eq!(
        occurrence_ids(&conn, SqlValue::Text("claude".to_string()))
            .await
            .unwrap(),
        ["occurrence-claude"]
    );
    assert_eq!(
        occurrence_ids(&conn, SqlValue::Null).await.unwrap(),
        ["occurrence-claude", "occurrence-codex"]
    );
}

#[tokio::test]
async fn root_pagination_restart_provider_filter_and_session_parity_are_stable() {
    let dir = tempdir().unwrap();
    let database = Builder::new_local(dir.path().join("root-pagination.db"))
        .build()
        .await
        .unwrap();
    let conn = database.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_generation_order
                ON session_occurrences(
                    session_id, generation, knowledge_at, occurrence_id
                );
             CREATE INDEX idx_session_occurrences_root_generation_order
                ON session_occurrences(
                    knowledge_at DESC, session_id, occurrence_id, generation
                );
             INSERT INTO session_temporal_generations VALUES
                ('session-a', 1, 'active'),
                ('session-b', 1, 'active'),
                ('session-c', 1, 'active');
             INSERT INTO observations VALUES
                ('observation-claude', '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'),
                ('observation-codex', '{\"identity\":{\"source\":{\"provider\":\"codex\"}}}');
             INSERT INTO retrieval_anchors VALUES
                ('same-anchor', '{\"kind\":\"profile\"}');
             INSERT INTO sessions VALUES
                ('claude', 'session-a', 'user'),
                ('claude', 'session-b', 'user'),
                ('codex', 'session-c', 'user');
             INSERT INTO session_occurrences VALUES
                ('session-a', 1, 'same-id', 'observation-claude',
                 'same-anchor', 'same-message', NULL, 'user', 5),
                ('session-b', 1, 'same-id', 'observation-claude',
                 'same-anchor', 'same-message', NULL, 'user', 5),
                ('session-c', 1, 'same-id', 'observation-codex',
                 'same-anchor', 'same-message', NULL, 'user', 5);",
    )
    .await
    .unwrap();

    async fn root_rows(
        conn: &Connection,
        provider: SqlValue,
        cursor: (i64, &str, &str),
        limit: i64,
    ) -> Vec<(String, String)> {
        let mut rows = conn
            .query(
                ROOT_TIME_CANDIDATE_QUERY,
                vec![
                    SqlValue::Text("user".to_string()),
                    provider,
                    SqlValue::Integer(0),
                    SqlValue::Integer(10),
                    SqlValue::Integer(cursor.0),
                    SqlValue::Text(cursor.1.to_string()),
                    SqlValue::Text(cursor.2.to_string()),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(128),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(1_024),
                    SqlValue::Integer(limit),
                ],
            )
            .await
            .unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            values.push((row.get(5).unwrap(), row.get(0).unwrap()));
        }
        values
    }

    let first = root_rows(&conn, SqlValue::Null, (i64::MAX, "", ""), 1).await;
    assert_eq!(first, [("session-a".to_string(), "same-id".to_string())]);
    let continuation = (5, first[0].0.as_str(), first[0].1.as_str());
    let second = root_rows(&conn, SqlValue::Null, continuation, 1).await;
    let restarted = root_rows(&conn, SqlValue::Null, continuation, 1).await;
    assert_eq!(second, [("session-b".to_string(), "same-id".to_string())]);
    assert_eq!(restarted, second);
    assert_eq!(
        root_rows(
            &conn,
            SqlValue::Text("claude".to_string()),
            (i64::MAX, "", ""),
            10,
        )
        .await,
        [
            ("session-a".to_string(), "same-id".to_string()),
            ("session-b".to_string(), "same-id".to_string()),
        ]
    );

    conn.execute(
        "UPDATE session_temporal_generations
             SET state = 'superseded'
             WHERE session_id <> 'session-a'",
        (),
    )
    .await
    .unwrap();
    let root = root_rows(&conn, SqlValue::Null, (i64::MAX, "", ""), 10).await;
    let mut session_rows = conn
        .query(
            TIME_CANDIDATE_QUERY,
            vec![
                SqlValue::Text("session-a".to_string()),
                SqlValue::Integer(1),
                SqlValue::Null,
                SqlValue::Integer(0),
                SqlValue::Integer(10),
                SqlValue::Integer(i64::MAX),
                SqlValue::Text(String::new()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(1_024),
                SqlValue::Integer(10),
            ],
        )
        .await
        .unwrap();
    let mut session = Vec::new();
    while let Some(row) = session_rows.next().await.unwrap() {
        session.push((row.get(5).unwrap(), row.get(0).unwrap()));
    }
    assert_eq!(
        root, session,
        "single-session root scope must preserve session semantics"
    );
}

#[tokio::test]
async fn root_record_hydration_rejects_cross_session_copy_and_assertion_traps() {
    let dir = tempdir().unwrap();
    let db = GlobalDb::try_open_at(&dir.path().join("root-record-isolation.db"))
        .await
        .expect("open database")
        .expect("database");
    insert_generation_for_session(&db, "session-a", 1).await;
    insert_generation_for_session(&db, "session-b", 1).await;
    let conn = db.read_connection();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
             INSERT INTO sessions (
                provider, session_id, project_key, project_path
             ) VALUES
                ('claude', 'session-a', 'user', '/root-record-test'),
                ('claude', 'session-b', 'user', '/root-record-test');
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-shared', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES
                (
                    'session-a', 1, 'same-id', 'observation-shared', 0,
                    'same-anchor', 'user', 5, '{\"kind\":\"unknown\"}', '{}',
                    'same content', 'same content'
                ),
                (
                    'session-b', 1, 'same-id', 'observation-shared', 0,
                    'same-anchor', 'user', 5, '{\"kind\":\"unknown\"}', '{}',
                    'same content', 'same content'
                ),
                (
                    'session-b', 1, 'source-b', 'observation-shared', 1,
                    'source-anchor-b', 'user', 4, '{\"kind\":\"unknown\"}', '{}',
                    'source', 'source'
                );
             INSERT INTO session_logical_copy_edges (
                session_id, generation, occurrence_id, copied_from_occurrence_id,
                proof_json, knowledge_at, valid_time_json, created_at
             ) VALUES (
                'session-b', 1, 'same-id', 'source-b', '{}', 5,
                '{\"kind\":\"unknown\"}', 5
             );
             INSERT INTO session_assertions (
                session_id, generation, assertion_id, assertion_kind,
                subject_anchor_id, object_anchor_id, knowledge_at,
                valid_time_json, evidence_json
             ) VALUES (
                'session-b', 1, 'assertion-b', 'supports',
                'same-anchor', 'other-anchor', 5, '{\"kind\":\"unknown\"}', '{}'
             );",
    )
    .await
    .unwrap();
    let snapshot = root_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let mut candidate_a = candidate_for_anchor("same-anchor");
    candidate_a.session = Some("session-a".to_string());
    let kinds_a = record_kinds(&db, &snapshot, candidate_a, &record_request()).await;
    assert_eq!(kinds_a, ["occurrence"]);

    let mut candidate_b = candidate_for_anchor("same-anchor");
    candidate_b.session = Some("session-b".to_string());
    let kinds_b = record_kinds(&db, &snapshot, candidate_b, &record_request()).await;
    assert!(kinds_b.contains(&"occurrence".to_string()));
    assert!(kinds_b.contains(&"assertion".to_string()));
    assert!(kinds_b.contains(&"copy".to_string()));
}

#[tokio::test]
async fn oversized_evidence_publication_and_source_json_never_reach_record_rows() {
    let dir = tempdir().unwrap();
    let db = GlobalDb::try_open_at(&dir.path().join("oversized-records.db"))
        .await
        .expect("open database")
        .expect("database");
    let conn = db.read_connection();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-1', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );",
    )
    .await
    .unwrap();
    let oversized_json = serde_json::to_string(&"x".repeat(16 * 1024)).unwrap();
    conn.execute(
        "INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1, 'occurrence-oversized', 'observation-1',
                0, 'anchor-evidence', 'user', 1,
                '{\"kind\":\"unknown\"}', ?1, 'snippet', 'index'
             )",
        [oversized_json.clone()],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-publication', 'session-snapshot', 'anchor-publication',
                'summary', 'summary', '{}', ?1, 1
             )",
        [oversized_json],
    )
    .await
    .unwrap();
    conn.execute_batch(
        "INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-publication', 0, 'anchor', 'source-short', NULL);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-publication', 'available',
                '{}', NULL, 1
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-source', 'session-snapshot', 'anchor-source',
                'summary', 'summary', '{}', NULL, 1
             );",
    )
    .await
    .unwrap();
    let oversized_anchor = format!("source-{}", "y".repeat(512));
    conn.execute(
        "INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-source', 0, 'anchor', ?1, NULL)",
        [oversized_anchor],
    )
    .await
    .unwrap();
    conn.execute_batch(
        "INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-source', 'available',
                '{}', NULL, 1
             );",
    )
    .await
    .unwrap();

    let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let request = PageRequest::for_test(32, 4096, 128, 32, 512);
    assert!(
        !record_kinds(
            &db,
            &snapshot,
            candidate_for_anchor("anchor-evidence"),
            &request,
        )
        .await
        .contains(&"occurrence".to_string())
    );
    for anchor in ["anchor-publication", "anchor-source"] {
        assert!(
            !record_kinds(&db, &snapshot, candidate_for_anchor(anchor), &request)
                .await
                .contains(&"summary".to_string()),
            "oversized summary JSON for {anchor} must be rejected in its UNION arm"
        );
    }
}

#[tokio::test]
async fn summary_source_count_cap_rejects_before_group_array() {
    let dir = tempdir().unwrap();
    let db = GlobalDb::try_open_at(&dir.path().join("source-count-cap.db"))
        .await
        .expect("open database")
        .expect("database");
    let conn = db.read_connection();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-many-sources', 'session-snapshot', 'anchor-many-sources',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-many-sources', 'available',
                '{}', NULL, 1
             );",
    )
    .await
    .unwrap();
    for ordinal in 0..=MAX_SUMMARY_SOURCES_PER_RECORD {
        conn.execute(
            "INSERT INTO session_summary_sources (
                    summary_id, source_ordinal, source_kind,
                    source_anchor_id, source_summary_id
                 ) VALUES ('summary-many-sources', ?1, 'anchor', ?2, NULL)",
            (
                i64::try_from(ordinal).unwrap(),
                format!("source-{ordinal:03}"),
            ),
        )
        .await
        .unwrap();
    }

    let snapshot = scoped_snapshot_with_mode(1, None, TemporalModeV1::Forensic);
    let request = PageRequest::for_test(32, 2 * 1024 * 1024, 1024 * 1024, 32, 512);
    let kinds = record_kinds(
        &db,
        &snapshot,
        candidate_for_anchor("anchor-many-sources"),
        &request,
    )
    .await;
    assert!(
        !kinds.contains(&"summary".to_string()),
        "257 sources must not be truncated into a 256-source summary JSON array"
    );
    let query = build_record_query(
        snapshot.retrieval_scope(),
        &snapshot,
        &[candidate_for_anchor("anchor-many-sources")],
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        33,
        &request,
    )
    .unwrap();
    assert!(query.sql.contains("ss.source_ordinal < ?"));
    assert!(query.sql.contains("LIMIT 257"));
}

#[tokio::test]
async fn provider_specific_summary_requires_retained_provider_evidence() {
    let dir = tempdir().unwrap();
    let db = GlobalDb::try_open_at(&dir.path().join("summary-provider.db"))
        .await
        .expect("open database")
        .expect("database");
    let conn = db.read_connection();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
             INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (
                'observation-claude', 'sha256:fixture', 'receipt-1',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}', '{}'
             );
             INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                valid_time_json, evidence_json, snippet_text, index_text
             ) VALUES (
                'session-snapshot', 1, 'occurrence-claude', 'observation-claude',
                0, 'source-claude', 'user', 1, '{\"kind\":\"unknown\"}',
                '{\"authority\":\"canonical\",\"evidence_class\":\"observed\",
                  \"source_anchor_id\":\"source-claude\",
                  \"sanitization_receipt\":{\"receipt_id\":\"receipt-1\"}}',
                'snippet', 'index'
             );
             INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text, index_text,
                source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-provider', 'session-snapshot', 'anchor-summary-provider',
                'summary', 'summary', '{}', NULL, 1
             );
             INSERT INTO session_summary_sources (
                summary_id, source_ordinal, source_kind, source_anchor_id, source_summary_id
             ) VALUES ('summary-provider', 0, 'anchor', 'source-claude', NULL);
             INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             ) VALUES (
                'session-snapshot', 1, 'summary-provider', 'available',
                '{}', NULL, 1
             );",
    )
    .await
    .unwrap();
    let request = record_request();
    let candidate = || candidate_for_anchor("anchor-summary-provider");

    let claude = record_kinds(
        &db,
        &scoped_snapshot(1, Some("claude")),
        candidate(),
        &request,
    )
    .await;
    assert!(claude.contains(&"summary".to_string()));

    let codex = record_kinds(
        &db,
        &scoped_snapshot(1, Some("codex")),
        candidate(),
        &request,
    )
    .await;
    assert!(!codex.contains(&"summary".to_string()));

    let all = record_kinds(&db, &scoped_snapshot(1, None), candidate(), &request).await;
    assert!(all.contains(&"summary".to_string()));
}

#[tokio::test]
async fn explain_record_query_stays_bounded_after_hundred_thousand_candidates() {
    let total = 100_000usize;
    let start = 71_111usize;
    let end = bounded_window_end(total, start, 38);
    let candidates = (start..end)
        .map(|ordinal| RankingCandidate {
            stable_id: format!("exact:occurrence-{ordinal}"),
            anchor_id: RetrievalAnchorId::new(format!("anchor-{ordinal}")).expect("anchor"),
            retriever_record_id: format!("occurrence-{ordinal}"),
            channel: CandidateChannel::ExactMessage,
            raw_score: 1_000,
            knowledge_at_micros: 1,
            logical_message: None,
            turn: None,
            session: Some("session-snapshot".to_string()),
            source: Some("claude".to_string()),
            evidence_role: Some("user".to_string()),
        })
        .collect::<Vec<_>>();
    let request = PageRequest::for_test(37, 64 * 1024, 8 * 1024, 37, 512);
    let query = build_record_query(
        &TemporalRetrievalScope::Session(SessionId::new("session-snapshot").expect("session")),
        &scoped_snapshot(1, Some("claude")),
        &candidates,
        start,
        &RecordCursor {
            candidate: start,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        38,
        &request,
    )
    .expect("bounded record query");
    assert_eq!(candidates.len(), 38);
    assert!(query.params.len() <= candidates.len() * 3 + 14);

    let dir = tempdir().unwrap();
    let db = GlobalDb::try_open_at(&dir.path().join("record-plan.db"))
        .await
        .expect("open database")
        .expect("database");
    let explain = format!("EXPLAIN QUERY PLAN {}", query.sql);
    let mut rows = db
        .read_connection()
        .query(&explain, query.params)
        .await
        .expect("record query must parse and plan");
    let mut detail_count = 0usize;
    while rows.next().await.expect("plan row").is_some() {
        detail_count += 1;
        assert!(detail_count < 512, "record plan must remain finite");
    }
    assert!(detail_count > 0);
}

#[tokio::test]
async fn explain_root_candidate_query_stays_keyset_bounded_at_hundred_thousand_rows() {
    let dir = tempdir().unwrap();
    let database = Builder::new_local(dir.path().join("root-plan.db"))
        .build()
        .await
        .unwrap();
    let conn = database.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                PRIMARY KEY(session_id, generation)
             );
             CREATE INDEX idx_session_temporal_generations_session_state
                ON session_temporal_generations(session_id, state);
             CREATE TABLE observations (
                observation_id TEXT PRIMARY KEY,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                owner_json TEXT NOT NULL
             );
             CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
             );
             CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                message_id TEXT,
                turn_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
             );
             CREATE INDEX idx_session_occurrences_root_generation_order
                ON session_occurrences(
                    knowledge_at DESC, session_id, occurrence_id, generation
                );
             INSERT INTO session_temporal_generations VALUES ('session-bulk', 1, 'active');
             INSERT INTO observations VALUES (
                'observation-bulk',
                '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}'
             );
             INSERT INTO retrieval_anchors VALUES (
                'anchor-bulk',
                '{\"kind\":\"profile\"}'
             );
             INSERT INTO sessions VALUES ('claude', 'session-bulk', 'user');
             WITH RECURSIVE sequence(value) AS (
                 VALUES(0)
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 99999
             )
             INSERT INTO session_occurrences
             SELECT 'session-bulk', 1, printf('occurrence-%06d', value),
                    'observation-bulk', 'anchor-bulk',
                    NULL, NULL, 'user', value
             FROM sequence;",
    )
    .await
    .unwrap();
    let count: i64 = conn
        .query("SELECT COUNT(*) FROM session_occurrences", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 100_000);

    let mut rows = conn
        .query(
            &format!("EXPLAIN QUERY PLAN {ROOT_TIME_CANDIDATE_QUERY}"),
            vec![
                SqlValue::Text("user".to_string()),
                SqlValue::Null,
                SqlValue::Integer(0),
                SqlValue::Integer(100_001),
                SqlValue::Integer(71_111),
                SqlValue::Text("session-bulk".to_string()),
                SqlValue::Text("occurrence-071111".to_string()),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(128),
                SqlValue::Integer(1_024),
                SqlValue::Integer(1_024),
                SqlValue::Integer(38),
            ],
        )
        .await
        .unwrap();
    let mut details = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        details.push(row.get::<String>(3).unwrap());
    }
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("idx_session_occurrences_root_generation_order"))
    );
    assert!(
        details
            .iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY"))
    );
}

#[test]
fn schema_index_dependencies_are_explicit_and_follow_ups_are_not_hidden() {
    assert!(REQUIRED_SCHEMA_INDEXES.len() >= 8);
    assert_eq!(FOLLOW_UP_SCHEMA_INDEXES.len(), 5);
    assert!(
        FOLLOW_UP_SCHEMA_INDEXES
            .iter()
            .all(|index| index.contains('('))
    );
}

#[test]
fn fts_values_are_bound_as_literal_phrases() {
    assert_eq!(fts_phrase("hello world"), "\"hello world\"");
    assert_eq!(fts_phrase("say \"hello\""), "\"say \"\"hello\"\"\"");
}

#[test]
fn iso_day_bounds_are_micros_and_half_open() {
    let (start, end) = iso_day_bounds("2026-07-18").unwrap();
    assert_eq!(end - start, 86_400_000_000);
    assert!(iso_day_bounds("not-a-date").is_err());
}

#[test]
fn record_query_has_no_offset_or_per_candidate_subqueries() {
    let source = include_str!("records.rs");
    let start = source.find("fn build_record_query(").unwrap();
    let end = source[start..]
        .find("struct RecordModeSql")
        .map(|offset| start + offset)
        .unwrap();
    let builder = &source[start..end];
    assert!(!builder.to_ascii_uppercase().contains(" OFFSET "));
    assert!(!builder.contains("for candidate in candidates {\n        conn.query"));
    assert!(builder.contains("candidate_input(ordinal, session_id, anchor_id)"));
    assert!(builder.contains("ORDER BY ordinal, kind_rank, scope_session, stable_id"));
}

#[test]
fn root_record_query_carries_session_identity_through_hydration() {
    let scope = crate::query::temporal::ports::TemporalRetrievalScope::AllSessionsInAuthorizedRoot;
    let mut candidate = record_candidate();
    candidate.session = Some("session-b".to_string());
    let query = build_record_query(
        &scope,
        &root_snapshot_with_mode(1, None, TemporalModeV1::Current),
        &[candidate],
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        33,
        &record_request(),
    )
    .expect("root record query");
    assert!(
        query
            .sql
            .contains("candidate_input(ordinal, session_id, anchor_id)")
    );
    assert!(query.sql.contains("o.session_id = c.session_id"));
    assert!(query.sql.contains("a.session_id = c.session_id"));
    assert!(query.sql.contains("target.session_id = c.session_id"));
    assert!(query.sql.contains("n.session_id = c.session_id"));
    assert!(
        query
            .sql
            .contains("source_summary.session_id = n.session_id")
    );
    assert!(
        query
            .sql
            .contains("retained_summary.session_id = n.session_id")
    );
    assert!(
        query
            .sql
            .contains("ORDER BY ordinal, kind_rank, scope_session, stable_id")
    );
    let adapter = include_str!("../retrieval.rs");
    assert!(adapter.contains("fn produce_candidate_page_for_scope<'a>("));
    assert!(adapter.contains("fn produce_temporal_record_page_for_scope<'a>("));
}
