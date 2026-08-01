use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use axum::http::StatusCode;
use serde_json::{Map, Value};
use tempfile::TempDir;
use tokio::sync::RwLock;
use tower::ServiceExt;
use tracedecay_domain::{FactOwnerV1, ProjectId};

use super::lcm_service::{self, SearchPayloadArgs};
use super::*;
use crate::application::configuration::ProductionUserSettingsDaemonClient;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_global_db::{ParseOffset, RegisteredGlobalDb};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_sessions::runtime::lcm::{LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

const PROVIDER: &str = "cursor";
const SESSION_ID: &str = "sess-lcm-fixes";
const NEEDLE: &str = "zebraneedle";
static TEST_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

struct DashboardFixture {
    state: DashboardState,
    sessions: Arc<RegisteredGlobalDb>,
    linked_node_id: String,
    _runtime: RegisteredGlobalDbTestRuntime,
    _tmp: TempDir,
}

impl DashboardFixture {
    async fn open(external_payload: bool, provider_collision: bool) -> Self {
        let tmp = tempfile::tempdir().expect("LCM dashboard fixture");
        let profile_root = tmp.path().join("profile");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let project_id =
            ProjectId::new(format!("project.lcm-dashboard-{nonce}")).expect("project identity");
        let runtime = RegisteredGlobalDbTestRuntime::project(
            &profile_root,
            &project_root,
            project_id.clone(),
        )
        .await
        .expect("registered dashboard LCM test runtime");
        let sessions = runtime
            .project_database_arc()
            .expect("registered project sessions");
        let linked_node_id = seed_lcm_fixture(
            &sessions,
            &project_root,
            external_payload,
            provider_collision,
        )
        .await;
        let memory_path = tmp.path().join("memory.db");
        let memory_authority =
            DatabaseAuthority::acquire_test(&memory_path, "LCM dashboard memory fixture")
                .expect("memory database authority");
        let (memory, _) = Database::publish_test_runtime(
            &memory_path,
            &memory_authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("memory database");
        let memory = Arc::new(memory);
        if external_payload {
            plant_external_needle(sessions.as_ref()).await;
        }
        let memory_path = memory_path.display().to_string();
        let sessions_path = sessions.db_path().display().to_string();
        let resolved_scope =
            crate::scope::resolve_dashboard_scope(&project_root, Some(project_id.as_str()));
        let state = DashboardState {
            project_id: Some(project_id.as_str().to_string()),
            resolved_scope,
            project_graph: None,
            project_graph_resolver: None,
            memory_owner: FactOwnerV1::Project { project_id },
            graph_conn: memory.engine_conn(),
            _database_guards: vec![Arc::clone(&memory)],
            graph_telemetry_handle: memory.storage_telemetry_handle().ok(),
            graph_db_path: memory_path.clone(),
            mem_db: memory,
            mem_db_path: memory_path,
            lcm_db: Some(Arc::clone(&sessions)),
            lcm_db_path: sessions_path,
            lcm_scope: "project_local".to_string(),
            savings_db: None,
            savings_db_path: String::new(),
            project_root: project_root.clone(),
            code_index_freshness_reader: None,
            feedback_status_reader: None,
            storage_mode: "project_local".to_string(),
            store_root: project_root.clone(),
            config_path: project_root.join("config.json"),
            dashboard_root: project_root.join("dashboard"),
            retention_config: crate::config::RetentionConfig::default(),
            user_settings: Arc::new(ProductionUserSettingsDaemonClient),
            curation_activity: Arc::new(RwLock::new(Vec::new())),
            token_counts: Arc::new(token_count::TokenCountCache::new()),
            code_diagnostics_authority: None,
            automation_scheduler_reconciler: None,
            automation_writer: standalone_dashboard_automation_writer(),
            doctor_report_reader: None,
            doctor_remediation_dispatcher: None,
            application_invocation_executor: None,
        };
        Self {
            state,
            sessions,
            linked_node_id,
            _runtime: runtime,
            _tmp: tmp,
        }
    }
}

fn message(
    message_id: &str,
    role: &str,
    ordinal: i64,
    timestamp: i64,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: message_id.to_string(),
        session_id: SESSION_ID.to_string(),
        role: role.to_string(),
        timestamp: Some(timestamp),
        ordinal,
        text: text.to_string(),
        kind: Some("chat".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    }
}

fn summary_draft(
    summary_text: &str,
    expand_hint: Option<&str>,
    metadata_json: Option<&str>,
    source_time_end: i64,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: PROVIDER.to_string(),
        conversation_id: "conv-lcm-fixes".to_string(),
        session_id: SESSION_ID.to_string(),
        depth: 1,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 100,
        summary_token_count: 40,
        source_time_start: Some(1_700_002_000),
        source_time_end: Some(source_time_end),
        expand_hint: expand_hint.map(str::to_string),
        metadata_json: metadata_json.map(str::to_string),
    }
}

async fn seed_lcm_fixture(
    sessions: &RegisteredGlobalDb,
    project_path: &Path,
    external_payload: bool,
    provider_collision: bool,
) -> String {
    let session = SessionRecord {
        provider: PROVIDER.to_string(),
        session_id: SESSION_ID.to_string(),
        project_key: "tracedecay-lcm-fixes".to_string(),
        project_path: project_path.display().to_string(),
        title: Some("LCM fixes session".to_string()),
        started_at: Some(1_700_002_000),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(sessions.upsert_session(&session).await);

    let msg_b = message(
        "msg-b",
        "assistant",
        2,
        1_700_002_000,
        "beta ordering message shared",
    );
    let msg_a = message(
        "msg-a",
        "user",
        1,
        1_700_002_000,
        "alpha ordering message shared",
    );
    let mut msg_c = message(
        "msg-c",
        "assistant",
        3,
        1_700_002_100,
        "gamma vector message shared",
    );
    msg_c.tool_names = Some("tracedecay_search".to_string());
    msg_c.metadata_json = Some("{\"fixture_marker\":\"msg-c-meta\"}".to_string());
    for message in [&msg_b, &msg_a, &msg_c] {
        assert!(
            sessions
                .upsert_transcript_batch(
                    &session,
                    std::slice::from_ref(message),
                    &format!("lcm-dashboard-fixture:{}", message.message_id),
                    ParseOffset::default(),
                )
                .await
        );
    }

    let msg_x = if external_payload {
        let mut text = String::from("externalized tool payload head ");
        text.push_str(&"x".repeat(300_000));
        let mut message = message("msg-x", "tool", 4, 1_700_002_200, &text);
        message.kind = Some("tool_result".to_string());
        message
    } else {
        message(
            "msg-x",
            "assistant",
            4,
            1_700_002_200,
            "delta lightweight fixture message",
        )
    };
    if external_payload {
        sessions
            .lcm_ingest_raw_message(
                sessions
                    .db_path()
                    .parent()
                    .expect("registered session storage root"),
                &msg_x,
            )
            .await
            .expect("externalized message");
        assert!(matches!(
            sessions
                .lcm_load_raw_message(PROVIDER, "msg-x")
                .await
                .expect("externalized raw message")
                .storage_kind,
            LcmStorageKind::External
        ));
    } else {
        assert!(
            sessions
                .upsert_transcript_batch(
                    &session,
                    std::slice::from_ref(&msg_x),
                    "lcm-dashboard-fixture:msg-x",
                    ParseOffset::default(),
                )
                .await
        );
    }

    let store_a = sessions
        .lcm_load_raw_message(PROVIDER, "msg-a")
        .await
        .expect("msg-a raw projection")
        .store_id;
    let store_b = sessions
        .lcm_load_raw_message(PROVIDER, "msg-b")
        .await
        .expect("msg-b raw projection")
        .store_id;
    let store_c = sessions
        .lcm_load_raw_message(PROVIDER, "msg-c")
        .await
        .expect("msg-c raw projection")
        .store_id;
    let linked = insert_summary_node(
        sessions,
        summary_draft(
            "vector projection summary for caching decisions",
            Some("expandhint drilldown"),
            Some("{\"category\":\"general\",\"tags\":[\"vector\"]}"),
            1_700_002_300,
            vec![LcmSourceRef::RawMessage { store_id: store_c }],
        ),
    )
    .await
    .expect("insert linked dashboard summary");
    for (text, time_end, store_id) in [
        ("second summary block two", 1_700_002_400, store_a),
        ("third summary block three", 1_700_002_500, store_b),
    ] {
        insert_summary_node(
            sessions,
            summary_draft(
                text,
                None,
                None,
                time_end,
                vec![LcmSourceRef::RawMessage { store_id }],
            ),
        )
        .await
        .expect("insert dashboard summary");
    }
    if provider_collision {
        let colliding_session = SessionRecord {
            provider: "codex".to_string(),
            session_id: SESSION_ID.to_string(),
            project_key: "provider-collision".to_string(),
            project_path: "/provider-collision".to_string(),
            title: Some("Provider collision".to_string()),
            started_at: Some(1_700_003_000),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        assert!(sessions.upsert_session(&colliding_session).await);
        let mut colliding = message(
            "msg-collision",
            "user",
            1,
            1_700_003_000,
            "provider collision",
        );
        colliding.provider = "codex".to_string();
        assert!(
            sessions
                .upsert_transcript_batch(
                    &colliding_session,
                    std::slice::from_ref(&colliding),
                    "lcm-dashboard-fixture:msg-collision",
                    ParseOffset::default(),
                )
                .await
        );
    }
    linked.node_id
}

async fn insert_summary_node(
    sessions: &RegisteredGlobalDb,
    draft: LcmSummaryNodeDraft,
) -> std::result::Result<
    tracedecay_sessions::runtime::lcm::LcmSummaryNode,
    tracedecay_sessions::runtime::lcm::LcmError,
> {
    let transaction = sessions
        .begin_write_transaction()
        .await
        .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
    let publisher =
        tracedecay_global_db::session_temporal_operations::GlobalDbLcmSummaryPublication::new(
            &transaction,
        );
    let summary =
        tracedecay_sessions::runtime::lcm::dag::insert_summary_node(&publisher, draft).await?;
    transaction.commit().await?;
    Ok(summary)
}

async fn plant_external_needle(db: &RegisteredGlobalDb) {
    let writer = db.writer_connection().expect("registered LCM writer");
    writer
        .execute(
            "UPDATE lcm_raw_messages
             SET snippet_text = ?1, index_text = ?1
             WHERE provider = ?2 AND message_id = 'msg-x'",
            params![format!("{NEEDLE} externalized snippet preview"), PROVIDER],
        )
        .await
        .expect("plant external needle");
}

async fn insert_undated_message(db: &RegisteredGlobalDb) {
    let writer = db.writer_connection().expect("registered LCM writer");
    writer
        .execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             )
             VALUES (?1, 'msg-undated', ?2, 'user', 99, NULL,
                     'undated legacy message', 'hash-undated', 'inline', NULL,
                     'undated legacy message', 'undated legacy message', 0, 0, NULL)",
            params![PROVIDER, SESSION_ID],
        )
        .await
        .expect("insert undated message");
}

async fn corrupt_summary_node_metadata(db: &RegisteredGlobalDb, node_id: &str) {
    let writer = db.writer_connection().expect("registered LCM writer");
    writer
        .execute(
            "UPDATE lcm_summary_nodes
             SET metadata_json = '{not-json'
             WHERE node_id = ?1",
            params![node_id],
        )
        .await
        .expect("corrupt summary metadata");
}

async fn drop_raw_message_fts(db: &RegisteredGlobalDb) {
    let writer = db.writer_connection().expect("registered LCM writer");
    writer
        .execute_batch(
            "DROP TRIGGER IF EXISTS lcm_raw_messages_fts_insert;
             DROP TRIGGER IF EXISTS lcm_raw_messages_fts_delete;
             DROP TRIGGER IF EXISTS lcm_raw_messages_fts_update;
             DROP TABLE IF EXISTS lcm_raw_messages_fts;",
        )
        .await
        .expect("drop raw message FTS");
}

fn value(payload: Map<String, Value>) -> Value {
    Value::Object(payload)
}

fn as_array<'a>(value: &'a Value, path: &[&str]) -> &'a Vec<Value> {
    let mut current = value;
    for key in path {
        current = &current[*key];
    }
    current.as_array().expect("array payload")
}

async fn get_json(state: DashboardState, uri: &str) -> (StatusCode, Value) {
    let response = project_api_router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("dashboard request"),
        )
        .await
        .expect("dashboard response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("dashboard response body");
    let payload = serde_json::from_slice(&body).expect("dashboard JSON response");
    (status, payload)
}

#[tokio::test]
async fn timeline_excludes_undated_rows_and_missing_targets_remain_typed() {
    let fixture = DashboardFixture::open(false, false).await;
    insert_undated_message(fixture.sessions.as_ref()).await;
    let timeline = value(
        lcm_service::timeline_payload(&fixture.state, false, "", 400)
            .await
            .expect("timeline payload"),
    );
    let buckets = as_array(&timeline, &["buckets"]);
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0]["bucket"], "2023-11-14");
    assert_eq!(buckets[0]["count"], 4);
    assert!(buckets.iter().all(|bucket| !bucket["bucket"].is_null()));
    assert_eq!(timeline["undated"]["count"], 1);
    assert!(timeline["undated"]["token_estimate"].as_i64().unwrap_or(0) > 0);

    let scoped = value(
        lcm_service::timeline_payload(&fixture.state, false, SESSION_ID, 400)
            .await
            .expect("scoped timeline"),
    );
    assert_eq!(scoped["undated"]["count"], 1);
    let other = value(
        lcm_service::timeline_payload(&fixture.state, false, "other-session", 400)
            .await
            .expect("other-session timeline"),
    );
    assert_eq!(other["undated"]["count"], 0);
    assert!(as_array(&other, &["buckets"]).is_empty());

    let (status, bad_query) = get_json(
        fixture.state.clone(),
        "/api/plugins/hermes-lcm/search?q=shared&limit=not-a-number",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        bad_query["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("limit")
    );

    let missing_session =
        lcm_service::session_payload(&fixture.state, "missing-session", 10, 0, false)
            .await
            .expect_err("missing session");
    assert_eq!(missing_session.0, StatusCode::NOT_FOUND);
    assert!(
        missing_session.1.0["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("missing-session")
    );
    let missing_node = lcm_service::node_payload(&fixture.state, "missing-node")
        .await
        .expect_err("missing node");
    assert_eq!(missing_node.0, StatusCode::NOT_FOUND);
    assert!(
        missing_node.1.0["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("missing-node")
    );
}

#[tokio::test]
async fn malformed_summary_metadata_surfaces_typed_error() {
    let fixture = DashboardFixture::open(false, false).await;
    corrupt_summary_node_metadata(fixture.sessions.as_ref(), &fixture.linked_node_id).await;
    let error = lcm_service::overview_payload(&fixture.state, "", 20)
        .await
        .expect_err("malformed metadata");
    assert_eq!(error.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error.1.0["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("malformed metadata_json")
    );
}

#[tokio::test]
async fn session_payload_orders_paginates_and_enriches_registered_rows() {
    let fixture = DashboardFixture::open(true, false).await;
    let session = value(
        lcm_service::session_payload(&fixture.state, SESSION_ID, 10, 0, false)
            .await
            .expect("session payload"),
    );
    assert_eq!(session["counts"]["message_count"], 4);
    assert_eq!(session["counts"]["summary_node_count"], 3);
    let messages = as_array(&session, &["messages"]);
    let ids: Vec<&str> = messages
        .iter()
        .map(|row| row["message_id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids, ["msg-a", "msg-b", "msg-c", "msg-x"]);
    let external = messages
        .iter()
        .find(|row| row["message_id"] == "msg-x")
        .expect("external message");
    assert!(
        external["content"]
            .as_str()
            .unwrap_or_default()
            .contains(NEEDLE)
    );
    assert!(external["token_estimate"].as_i64().unwrap_or(0) > 0);
    assert_eq!(external["storage_kind"], "external");
    let enriched = messages
        .iter()
        .find(|row| row["message_id"] == "msg-c")
        .expect("enriched message");
    assert_eq!(enriched["ordinal"], 3);
    assert_eq!(enriched["pinned"], 0);
    assert_eq!(enriched["tool_name"], "tracedecay_search");
    assert!(
        enriched["metadata_json"]
            .as_str()
            .unwrap_or_default()
            .contains("fixture_marker")
    );
    assert!(
        as_array(enriched, &["summary_node_ids"])
            .iter()
            .any(|id| id == fixture.linked_node_id.as_str())
    );

    let first = value(
        lcm_service::session_payload(&fixture.state, SESSION_ID, 2, 0, false)
            .await
            .expect("first page"),
    );
    assert_eq!(as_array(&first, &["summary_nodes"]).len(), 2);
    assert_eq!(first["has_more_summary_nodes"], true);
    assert_eq!(first["has_more"], true);
    let second = value(
        lcm_service::session_payload(&fixture.state, SESSION_ID, 2, 2, false)
            .await
            .expect("second page"),
    );
    assert_eq!(as_array(&second, &["summary_nodes"]).len(), 1);
    assert_eq!(second["has_more_summary_nodes"], false);
    assert_eq!(second["has_more_messages"], false);
    assert_eq!(second["has_more"], false);

    let node = value(
        lcm_service::node_payload(&fixture.state, &fixture.linked_node_id)
            .await
            .expect("node payload"),
    );
    let sources = as_array(&node, &["sources", "messages"]);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["message_id"], "msg-c");
    assert!(
        as_array(&sources[0], &["summary_node_ids"])
            .iter()
            .any(|id| id == fixture.linked_node_id.as_str())
    );
}

#[tokio::test]
async fn provider_ambiguous_session_ids_remain_conflicts() {
    let fixture = DashboardFixture::open(false, true).await;
    let error = lcm_service::session_payload(&fixture.state, SESSION_ID, 10, 0, false)
        .await
        .expect_err("provider ambiguity");
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert!(
        error.1.0["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("ambiguous session id across providers")
    );
}

async fn search(fixture: &DashboardFixture, query: &str, limit: i64, offset: i64) -> Value {
    value(
        lcm_service::search_payload(
            &fixture.state,
            SearchPayloadArgs {
                query,
                limit,
                offset,
                role: "",
                source: "",
                session_id: "",
                since: None,
                until: None,
            },
        )
        .await
        .expect("search payload"),
    )
}

#[tokio::test]
async fn search_uses_externalized_text_and_reports_fts_fallback_truthfully() {
    let fixture = DashboardFixture::open(true, false).await;
    let needle = search(&fixture, NEEDLE, 20, 0).await;
    assert_eq!(needle["engine"], "fts");
    assert_eq!(needle["engine_detail"]["messages"], "fts");
    assert_eq!(needle["engine_detail"]["summary_nodes"], "fts");
    assert_eq!(as_array(&needle, &["matches", "messages"]).len(), 1);
    assert!(
        as_array(&needle, &["matches", "messages"])[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains(NEEDLE)
    );
    assert_eq!(needle["total"]["messages"], 1);

    let overview = value(
        lcm_service::overview_payload(&fixture.state, NEEDLE, 20)
            .await
            .expect("overview search"),
    );
    assert_eq!(as_array(&overview, &["matches", "messages"]).len(), 1);

    let general = search(&fixture, "general", 20, 0).await;
    assert_eq!(general["engine"], "fts");
    assert_eq!(as_array(&general, &["matches", "summary_nodes"]).len(), 0);
    assert_eq!(general["total"]["summary_nodes"], 0);
    for query in ["caching", "expandhint"] {
        assert_eq!(
            as_array(
                &search(&fixture, query, 20, 0).await,
                &["matches", "summary_nodes"]
            )
            .len(),
            1
        );
    }

    let first = search(&fixture, "shared", 1, 0).await;
    let second = search(&fixture, "shared", 1, 1).await;
    assert_eq!(first["limit"], 1);
    assert_eq!(first["offset"], 0);
    assert_eq!(first["total"]["messages"], 3);
    assert_eq!(second["offset"], 1);
    assert_eq!(second["total"]["messages"], 3);
    assert_ne!(
        as_array(&first, &["matches", "messages"])[0]["store_id"],
        as_array(&second, &["matches", "messages"])[0]["store_id"]
    );
    assert!(
        as_array(
            &search(&fixture, "shared", 1, 3).await,
            &["matches", "messages"]
        )
        .is_empty()
    );

    drop_raw_message_fts(fixture.sessions.as_ref()).await;
    let fallback = search(&fixture, "shared", 20, 0).await;
    assert_eq!(fallback["engine_detail"]["messages"], "like");
    assert_eq!(fallback["engine_detail"]["summary_nodes"], "fts");
    assert_eq!(fallback["engine"], "like");
    assert_eq!(as_array(&fallback, &["matches", "messages"]).len(), 3);
    assert_eq!(fallback["total"]["messages"], 3);
    let needle_fallback = search(&fixture, NEEDLE, 20, 0).await;
    assert_eq!(needle_fallback["engine_detail"]["messages"], "like");
    assert!(
        as_array(&needle_fallback, &["matches", "messages"])[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains(NEEDLE)
    );
    assert!(
        as_array(&needle_fallback, &["matches", "messages"])[0]["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains(NEEDLE)
    );
}
