//! Integration tests for the LCM dashboard API
//! (`/api/plugins/hermes-lcm/*`) against a seeded temp session store served
//! from the profile-sharded project session DB.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use crate::common::{
    create_runtime, get_json, http_agent, message_record_at, pick_free_port, response_to_json,
    wait_for_dashboard, write_empty_global_db_schema, EnvVarGuard, GLOBAL_DB_ENV_LOCK as ENV_LOCK,
};

use serde_json::{json, Value};
use tempfile::TempDir;
use tracedecay::dashboard;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::cursor::project_session_db_path;
use tracedecay::sessions::lcm::{LcmCleanConfig, LcmGcConfig, LcmSourceRef, LcmSummaryNodeDraft};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::TraceDecay;

struct Fixture {
    _tmp: TempDir,
    _env_guards: Vec<EnvVarGuard>,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    session_db_path: std::path::PathBuf,
    _project_root: std::path::PathBuf,
    session_id: String,
    child_node_id: String,
    parent_node_id: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn session(session_id: &str, project: &Path, started_at: i64, title: &str) -> SessionRecord {
    SessionRecord {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        project_key: "lcm-fixture".to_string(),
        project_path: project.display().to_string(),
        title: Some(title.to_string()),
        started_at: Some(started_at),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn message(
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    timestamp: i64,
    text: &str,
    model: Option<&str>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    message_record_at(
        "cursor",
        message_id,
        session_id,
        role,
        ordinal,
        Some(timestamp),
        text,
        "message",
        model,
        None,
        None,
        None,
        metadata_json,
    )
}

async fn lookup_store_id(db_path: &Path, message_id: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path)
        .build()
        .await
        .expect("open raw libsql db");
    let conn = db.connect().expect("connect raw libsql db");
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages WHERE message_id = ?1",
            libsql::params![message_id],
        )
        .await
        .expect("query store id");
    let row = rows
        .next()
        .await
        .expect("read store id row")
        .expect("store id row present");
    row.get(0).expect("store id")
}

async fn seed_lcm_store(db_path: &Path, project: &Path) -> (String, String, String) {
    let gdb = GlobalDb::open_at(db_path).await.expect("open global db");
    let session_id = "sess-alpha".to_string();
    let started_at = 1_720_000_000;
    let msg1_at = started_at + 10;
    let msg2_at = started_at + 20;

    assert!(
        gdb.upsert_session(&session(
            &session_id,
            project,
            started_at,
            "Launch planning session"
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-alpha-1",
            &session_id,
            "user",
            1,
            msg1_at,
            "Let's plan the launch checklist and rollout.",
            Some("gpt-5.5-high"),
            Some(r#"{"usage":{"input_tokens":42}}"#),
        ))
        .await
    );
    assert!(
        gdb.upsert_session_message(&message(
            "m-alpha-2",
            &session_id,
            "assistant",
            2,
            msg2_at,
            "Launch summary: ship the rollout plan and verify dashboards.",
            Some("gpt-5.5-high"),
            Some(r#"{"usage":{"output_tokens":24}}"#),
        ))
        .await
    );

    let msg1_store_id = lookup_store_id(db_path, "m-alpha-1").await;
    let msg2_store_id = lookup_store_id(db_path, "m-alpha-2").await;

    let child = gdb
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "conv-alpha".to_string(),
            session_id: session_id.clone(),
            depth: 0,
            summary_text: "Launch planning discussion and rollout prep.".to_string(),
            source_refs: vec![
                LcmSourceRef::RawMessage {
                    store_id: msg1_store_id,
                },
                LcmSourceRef::RawMessage {
                    store_id: msg2_store_id,
                },
            ],
            source_token_count: 120,
            summary_token_count: 30,
            source_time_start: Some(msg1_at),
            source_time_end: Some(msg2_at),
            expand_hint: Some("launch prep".to_string()),
            metadata_json: Some(
                r#"{"category":"planning","tags":["launch"],"entities":["alpha"]}"#.to_string(),
            ),
        })
        .await
        .expect("insert child summary node");

    let parent = gdb
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "conv-alpha".to_string(),
            session_id: session_id.clone(),
            depth: 1,
            summary_text: "Launch condensed summary node.".to_string(),
            source_refs: vec![LcmSourceRef::SummaryNode {
                node_id: child.node_id.clone(),
            }],
            source_token_count: 30,
            summary_token_count: 10,
            source_time_start: Some(msg1_at),
            source_time_end: Some(msg2_at),
            expand_hint: Some("launch condensed".to_string()),
            metadata_json: Some(r#"{"category":"rollup"}"#.to_string()),
        })
        .await
        .expect("insert parent summary node");

    (session_id, child.node_id, parent.node_id)
}

async fn start_fixture() -> Fixture {
    let tmp = TempDir::new().expect("temp dir");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir");
    std::fs::write(
        project_root.join("lib.rs"),
        "pub fn lcm_fixture() -> u32 { 7 }\n",
    )
    .expect("seed source file");

    let global_db_path = tmp.path().join("global").join("global.db");
    let env_guards = vec![EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &global_db_path)];
    // Pre-create both GlobalDb-schema stores from the cached empty template
    // so seeding and dashboard startup open existing DBs instead of paying a
    // full schema creation each (slow on Windows).
    write_empty_global_db_schema(&global_db_path).await;
    let cg = TraceDecay::init(&project_root)
        .await
        .expect("tracedecay init");
    let session_db_path = project_session_db_path(&project_root);
    write_empty_global_db_schema(&session_db_path).await;
    let (session_id, child_node_id, parent_node_id) =
        seed_lcm_store(&session_db_path, &project_root).await;
    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server = tokio::spawn(async move {
        let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
    });
    wait_for_dashboard(&http_agent(), &base_url).await;

    Fixture {
        _tmp: tmp,
        _env_guards: env_guards,
        base_url,
        server,
        session_db_path,
        _project_root: project_root,
        session_id,
        child_node_id,
        parent_node_id,
    }
}

#[test]
fn lcm_overview_and_search_preserve_shapes() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let agent = http_agent();

        let (status, caps) = get_json(&agent, &format!("{}/api/capabilities", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(caps["features"]["lcm"], true);
        assert_eq!(caps["lcm_scope"], "profile_sharded");

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?limit=5&q=launch",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["exists"], true);
        assert_eq!(overview["storage_scope"], "profile_sharded");
        assert_eq!(overview["overview"]["messages_total"], 2);
        assert_eq!(overview["overview"]["summary_nodes_total"], 2);
        assert_eq!(overview["latest_sessions"][0]["session_id"], fixture.session_id);
        assert!(overview["latest_summary_nodes"].as_array().expect("latest nodes").len() >= 2);
        let message_matches = overview["matches"]["messages"]
            .as_array()
            .expect("message matches");
        assert!(!message_matches.is_empty());
        assert!(message_matches[0]["summary_node_ids"].is_array());
        let node_matches = overview["matches"]["summary_nodes"]
            .as_array()
            .expect("node matches");
        assert!(node_matches
            .iter()
            .any(|row| row["node_id"] == fixture.child_node_id));

        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=launch&role=assistant&source=cursor&session_id={}&since=1719999990&until=1720000100",
                fixture.base_url, fixture.session_id
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["exists"], true);
        assert_ne!(search["engine_detail"]["messages"], "none");
        assert_ne!(search["engine_detail"]["summary_nodes"], "none");
        assert_eq!(search["filters"]["role"], "assistant");
        assert_eq!(search["filters"]["source"], "cursor");
        assert_eq!(search["filters"]["session_id"], fixture.session_id);
        assert!(search["total"]["messages"].as_i64().unwrap_or_default() >= 1);
        assert!(search["matches"]["messages"][0]["summary_node_ids"].is_array());
    });
}

#[test]
fn lcm_session_and_node_routes_expand_sources() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let agent = http_agent();

        let (status, session) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/session/{}?limit=1&order=desc",
                fixture.base_url, fixture.session_id
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(session["counts"]["message_count"], 2);
        assert_eq!(session["counts"]["summary_node_count"], 2);
        assert_eq!(session["order"], "desc");
        assert_eq!(session["has_more_messages"], true);
        assert_eq!(session["has_more_summary_nodes"], true);
        assert!(session["messages"][0]["summary_node_ids"].is_array());

        let (status, parent_node) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/node/{}",
                fixture.base_url, fixture.parent_node_id
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(parent_node["sources"]["type"], "nodes");
        assert_eq!(parent_node["sources"]["ids"][0], fixture.child_node_id);
        assert_eq!(
            parent_node["sources"]["nodes"][0]["node_id"],
            fixture.child_node_id
        );

        let (status, child_node) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/node/{}",
                fixture.base_url, fixture.child_node_id
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(child_node["sources"]["type"], "messages");
        assert_eq!(
            child_node["sources"]["messages"]
                .as_array()
                .expect("source messages")
                .len(),
            2
        );
        assert_eq!(child_node["sources"]["messages"][0]["ordinal"], 1);
        assert_eq!(child_node["sources"]["messages"][1]["ordinal"], 2);
    });
}

fn post_json(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = agent
        .post(url)
        .content_type("application/json")
        .send(body.to_string())
        .expect("POST should succeed");
    response_to_json(response)
}

#[test]
fn lcm_payload_health_and_gc_routes_require_preview_then_apply() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let db = GlobalDb::open_at(&fixture.session_db_path)
            .await
            .expect("session db should reopen");
        let mut external = message(
            "payload-tool-1",
            &fixture.session_id,
            "tool",
            3,
            1_720_000_030,
            &format!("dashboard payload secret {}", "X".repeat(300_000)),
            Some("gpt-5.5-high"),
            None,
        );
        external.kind = Some("tool_result".to_string());
        db.lcm_store(fixture.session_db_path.parent().expect("session db parent"))
            .ingest_raw_message(&external)
            .await
            .expect("payload-backed message should ingest");
        let payload_dir = fixture
            .session_db_path
            .parent()
            .unwrap()
            .join("lcm-payloads");
        std::fs::create_dir_all(&payload_dir).expect("payload dir");
        let orphan_ref =
            "payload_dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd.payload";
        let orphan_path = payload_dir.join(orphan_ref);
        std::fs::write(&orphan_path, "dashboard orphan body that must not leak")
            .expect("orphan payload write");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&orphan_path)
            .and_then(|file| {
                file.set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_719_000_000),
                )
            })
            .expect("backdate orphan payload");

        let agent = http_agent();
        let (status, health) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/payloads/health?provider=cursor&session_id={}",
                fixture.base_url, fixture.session_id
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(health["payload_health"]["status"], "warning");
        assert_eq!(health["payload_health"]["externalized_count"], 1);
        assert_eq!(health["payload_health"]["orphan_file_count"], 1);
        let health_text = serde_json::to_string(&health).unwrap();
        assert!(!health_text.contains("dashboard payload secret"));
        assert!(!health_text.contains("dashboard orphan body that must not leak"));

        let (status, denied) = post_json(
            &agent,
            &format!("{}/api/plugins/hermes-lcm/payloads/gc", fixture.base_url),
            &json!({
                "provider": "cursor",
                "session_id": fixture.session_id,
                "confirm": true
            }),
        );
        assert_eq!(status, 400);
        assert!(orphan_path.exists());
        assert_eq!(denied["status"], "error");

        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/payloads/gc?provider=cursor&session_id={}",
                fixture.base_url, fixture.session_id
            ),
        );
        assert_eq!(status, 200);
        let token = preview["dry_run_token"].as_str().expect("preview token");
        assert_eq!(preview["gc_report"]["orphans"]["count"], 1);
        assert!(orphan_path.exists());

        let (status, applied) = post_json(
            &agent,
            &format!("{}/api/plugins/hermes-lcm/payloads/gc", fixture.base_url),
            &json!({
                "provider": "cursor",
                "session_id": fixture.session_id,
                "confirm": true,
                "dry_run_token": token
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["gc_report"]["orphans"]["count"], 1);
        assert!(!orphan_path.exists());
        let applied_text = serde_json::to_string(&applied).unwrap();
        assert!(!applied_text.contains("dashboard payload secret"));
        assert!(!applied_text.contains("dashboard orphan body that must not leak"));
    });
}

#[test]
fn lcm_payload_health_numbers_agree_across_status_doctor_and_dashboard() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture().await;
        let db = GlobalDb::open_at(&fixture.session_db_path)
            .await
            .expect("session db should reopen");
        let body = format!("cross surface payload secret {}", "Y".repeat(300_000));
        let mut external = message(
            "payload-tool-agreement",
            &fixture.session_id,
            "tool",
            3,
            1_720_000_030,
            &body,
            Some("gpt-5.5-high"),
            None,
        );
        external.kind = Some("tool_result".to_string());
        db.lcm_store(fixture.session_db_path.parent().expect("session db parent"))
            .ingest_raw_message(&external)
            .await
            .expect("payload-backed message should ingest");

        let payload_dir = fixture
            .session_db_path
            .parent()
            .unwrap()
            .join("lcm-payloads");
        std::fs::create_dir_all(&payload_dir).expect("payload dir");
        let orphan_ref =
            "payload_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.payload";
        let orphan_path = payload_dir.join(orphan_ref);
        std::fs::write(&orphan_path, "cross surface orphan body that must not leak")
            .expect("orphan payload write");

        let status = db
            .lcm_status("cursor", Some(&fixture.session_id))
            .await
            .expect("status should load");
        let doctor = db
            .lcm_doctor(
                "cursor",
                Some(&fixture.session_id),
                "diagnose",
                false,
                LcmCleanConfig::default(),
                LcmGcConfig::default(),
            )
            .await
            .expect("doctor should load");
        let (dashboard_status, dashboard) = get_json(
            &http_agent(),
            &format!(
                "{}/api/plugins/hermes-lcm/payloads/health?provider=cursor&session_id={}",
                fixture.base_url, fixture.session_id
            ),
        );
        assert_eq!(dashboard_status, 200);

        let doctor_payloads = &doctor["diagnostics"]["payloads"];
        let dashboard_health = &dashboard["payload_health"];
        for (status_value, doctor_key, dashboard_key) in [
            (
                status.payload.missing_count as u64,
                "missing_files",
                "missing_count",
            ),
            (
                status.payload.orphan_file_count as u64,
                "orphan_files",
                "orphan_file_count",
            ),
            (
                status.payload.unreferenced_count as u64,
                "unreferenced_metadata",
                "unreferenced_count",
            ),
            (status.payload.total_bytes, "total_bytes", "total_bytes"),
            (
                status.payload.referenced_bytes,
                "referenced_bytes",
                "referenced_bytes",
            ),
            (
                status.payload.orphan_file_bytes,
                "orphan_file_bytes",
                "orphan_file_bytes",
            ),
            (
                status.payload.reclaimable_bytes,
                "reclaimable_bytes",
                "reclaimable_bytes",
            ),
            (
                status.payload.reclaimable_bytes_after_grace,
                "reclaimable_bytes_after_grace",
                "reclaimable_bytes_after_grace",
            ),
        ] {
            assert_eq!(doctor_payloads[doctor_key].as_u64(), Some(status_value));
            assert_eq!(dashboard_health[dashboard_key].as_u64(), Some(status_value));
        }

        let dashboard_text = serde_json::to_string(&dashboard).unwrap();
        let doctor_text = serde_json::to_string(&doctor).unwrap();
        assert!(!dashboard_text.contains("cross surface payload secret"));
        assert!(!dashboard_text.contains("cross surface orphan body that must not leak"));
        assert!(!doctor_text.contains("cross surface payload secret"));
        assert!(!doctor_text.contains("cross surface orphan body that must not leak"));
    });
}
