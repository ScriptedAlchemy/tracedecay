//! Integration tests for the LCM dashboard API
//! (`/api/plugins/hermes-lcm/*`) against a seeded temp session store served
//! from the profile-sharded project session DB.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use crate::common::{
    GLOBAL_DB_ENV_LOCK as ENV_LOCK, create_runtime, get_json, http_agent, pick_free_port,
    response_to_json, wait_for_dashboard,
};
use crate::dashboard_api_support::{MessageDetails, message};

use crate::runtime::DashboardTestRuntimeV1;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::dashboard;
use tracedecay::sessions::lcm::{
    LcmCleanConfig, LcmError, LcmGcConfig, LcmSourceRef, LcmStatus, LcmSummaryNodeDraft,
};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::TraceDecayOpenOptions;
use tracedecay_domain::ProjectId;

struct Fixture {
    _tmp: TempDir,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    _project_root: std::path::PathBuf,
    session_id: String,
    child_node_id: String,
    parent_node_id: String,
    orphan_path: Option<std::path::PathBuf>,
    seeded_status: Option<LcmStatus>,
    seeded_doctor: Option<Value>,
}

struct PayloadFixtureSeed {
    message_id: &'static str,
    body: String,
    orphan_ref: &'static str,
    orphan_body: &'static str,
    modified: Option<std::time::SystemTime>,
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

async fn seed_lcm_store(
    runtime: &DashboardTestRuntimeV1,
    project: &Path,
    external_message: Option<SessionMessageRecord>,
) -> Result<(String, String, String), LcmError> {
    let session_id = "sess-alpha".to_string();
    let started_at = 1_720_000_000;
    let msg1_at = started_at + 10;
    let msg2_at = started_at + 20;

    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &session(&session_id, project, started_at, "Launch planning session",),
            )
            .await
            .expect("seed session")
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &message(
                    "m-alpha-1",
                    &session_id,
                    "user",
                    1,
                    "Let's plan the launch checklist and rollout.",
                    MessageDetails {
                        timestamp: msg1_at,
                        model: Some("gpt-5.5-high"),
                        metadata_json: Some(r#"{"usage":{"input_tokens":42}}"#),
                    },
                ),
            )
            .await
            .expect("seed first message")
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &message(
                    "m-alpha-2",
                    &session_id,
                    "assistant",
                    2,
                    "Launch summary: ship the rollout plan and verify dashboards.",
                    MessageDetails {
                        timestamp: msg2_at,
                        model: Some("gpt-5.5-high"),
                        metadata_json: Some(r#"{"usage":{"output_tokens":24}}"#),
                    },
                ),
            )
            .await
            .expect("seed second message")
    );

    let msg1_store_id = runtime
        .lcm_load_raw_message_for_test("cursor", "m-alpha-1")
        .await
        .expect("first raw message")
        .store_id;
    let msg2_store_id = runtime
        .lcm_load_raw_message_for_test("cursor", "m-alpha-2")
        .await
        .expect("second raw message")
        .store_id;

    let child = runtime
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
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
            },
        )
        .await?;

    let parent = runtime
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
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
            },
        )
        .await?;

    if let Some(external_message) = external_message {
        runtime
            .lcm_ingest_raw_message_for_test(HostAdmissionScope::Project, &external_message)
            .await?;
    }

    Ok((session_id, child.node_id, parent.node_id))
}

async fn start_fixture(payload_seed: Option<PayloadFixtureSeed>) -> Fixture {
    let tmp = TempDir::new().expect("temp dir");
    let project_root = tmp.path().join("project");
    let profile_root = tmp.path().join(".tracedecay");
    std::fs::create_dir_all(&project_root).expect("project dir");
    std::fs::write(
        project_root.join("lib.rs"),
        "pub fn lcm_fixture() -> u32 { 7 }\n",
    )
    .expect("seed source file");

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: None,
    };
    let project_id = ProjectId::new("dashboard_lcm_fixture").expect("valid project identity");
    let runtime = Arc::new(
        DashboardTestRuntimeV1::project(&profile_root, &project_root, project_id)
            .await
            .expect("registered project session runtime"),
    );
    let cg = runtime
        .initialize_project_graph_for_test(&project_root, open_options)
        .await
        .expect("tracedecay init");
    let external_message = payload_seed.as_ref().map(|seed| {
        let mut external = message(
            seed.message_id,
            "sess-alpha",
            "tool",
            3,
            &seed.body,
            MessageDetails {
                timestamp: 1_720_000_030,
                model: Some("gpt-5.5-high"),
                metadata_json: None,
            },
        );
        external.kind = Some("tool_result".to_string());
        external
    });
    let (session_id, child_node_id, parent_node_id) =
        seed_lcm_store(&runtime, &project_root, external_message)
            .await
            .expect("seed registered LCM store");
    let payload_dir = runtime
        .database_path(HostAdmissionScope::Project)
        .expect("project session database path")
        .parent()
        .expect("project session storage root")
        .join("lcm-payloads");
    let orphan_path = payload_seed.as_ref().map(|seed| {
        std::fs::create_dir_all(&payload_dir).expect("payload dir");
        let path = payload_dir.join(seed.orphan_ref);
        std::fs::write(&path, seed.orphan_body).expect("orphan payload write");
        if let Some(modified) = seed.modified {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(modified))
                .expect("set orphan payload modification time");
        }
        path
    });
    let seeded_status = if payload_seed.is_some() {
        Some(
            runtime
                .lcm_status_deep_for_test("cursor", Some(&session_id))
                .await
                .expect("seeded status"),
        )
    } else {
        None
    };
    let seeded_doctor = if payload_seed.is_some() {
        Some(
            runtime
                .lcm_doctor_for_test(
                    "cursor",
                    Some(&session_id),
                    "diagnose",
                    false,
                    LcmCleanConfig::default(),
                    LcmGcConfig::default(),
                )
                .await
                .expect("seeded doctor"),
        )
    } else {
        None
    };
    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server_runtime = Arc::clone(&runtime);
    let server_graph = Arc::new(cg);
    let server = tokio::spawn(async move {
        let authority = server_runtime
            .dashboard_test_authority()
            .expect("dashboard LCM authority");
        let _ = dashboard::run_until_shutdown_for_tests_with_host_admission(
            server_graph,
            authority,
            dashboard::DashboardTestProjectGraphsV1::default(),
            "127.0.0.1",
            port,
            dashboard::spa_router(),
            std::future::pending(),
        )
        .await;
    });
    wait_for_dashboard(&http_agent(), &base_url).await;

    Fixture {
        _tmp: tmp,
        base_url,
        server,
        _project_root: project_root,
        session_id,
        child_node_id,
        parent_node_id,
        orphan_path,
        seeded_status,
        seeded_doctor,
    }
}

#[test]
fn lcm_overview_and_search_preserve_shapes() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(None).await;
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

        let (status, timeline) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/timeline?limit=1",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(timeline["coverage"]["ordering"], "most_recent");
        assert_eq!(timeline["coverage"]["limit"], 1);
        assert_eq!(timeline["coverage"]["returned_buckets"], 1);
        assert_eq!(timeline["coverage"]["total_dated_buckets"], 1);
        assert_eq!(timeline["coverage"]["truncated"], false);
        assert_eq!(timeline["undated"]["count"], 0);

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
        let fixture = start_fixture(None).await;
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
        let fixture = start_fixture(Some(PayloadFixtureSeed {
            message_id: "payload-tool-1",
            body: format!("dashboard payload secret {}", "X".repeat(300_000)),
            orphan_ref:
                "payload_dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd.payload",
            orphan_body: "dashboard orphan body that must not leak",
            modified: Some(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_719_000_000),
            ),
        }))
        .await;
        let orphan_path = fixture.orphan_path.as_ref().expect("orphan payload path");

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
        let body = format!("cross surface payload secret {}", "Y".repeat(300_000));
        let fixture = start_fixture(Some(PayloadFixtureSeed {
            message_id: "payload-tool-agreement",
            body: body.clone(),
            orphan_ref:
                "payload_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.payload",
            orphan_body: "cross surface orphan body that must not leak",
            modified: None,
        }))
        .await;
        let _orphan_path = fixture.orphan_path.as_ref().expect("orphan payload path");

        let status = fixture.seeded_status.as_ref().expect("seeded status");
        let doctor = fixture.seeded_doctor.as_ref().expect("seeded doctor");
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
            assert_eq!(
                doctor_payloads[doctor_key].as_u64(),
                Some(status_value),
                "doctor payload mismatch for {doctor_key}"
            );
            assert_eq!(
                dashboard_health[dashboard_key].as_u64(),
                Some(status_value),
                "dashboard payload mismatch for {dashboard_key}"
            );
        }

        let dashboard_text = serde_json::to_string(&dashboard).unwrap();
        let doctor_text = serde_json::to_string(&doctor).unwrap();
        assert!(!dashboard_text.contains("cross surface payload secret"));
        assert!(!dashboard_text.contains("cross surface orphan body that must not leak"));
        assert!(!doctor_text.contains("cross surface payload secret"));
        assert!(!doctor_text.contains("cross surface orphan body that must not leak"));
    });
}
