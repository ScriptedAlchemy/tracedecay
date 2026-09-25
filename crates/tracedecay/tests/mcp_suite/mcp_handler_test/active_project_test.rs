//! `tracedecay_active_project` as a caller sees it: one `tools/call` on the
//! production MCP session for a checkout this test created.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::PathBuf;

use crate::common::fixture::git_run as git;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;
use tracedecay_runtime_core::storage::pin_fixture_repository_identity;

use crate::support::{
    TestTempDir, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, test_temp_dir, wait_for_code_index_generation,
};

const PROJECT_ID: &str = "proj_active_proof";
const OPENED_BRANCH: &str = "proof-branch";
const MOVED_BRANCH: &str = "feature-live";
const SCOPE_PREFIX: &str = "src";

struct OpenedCheckout {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: PathBuf,
    profile_root: PathBuf,
    _isolation: TestTempDir,
}

#[tokio::test]
async fn active_project_reports_the_opened_checkout() {
    let opened = open_checkout().await;
    let server = opened
        .harness
        .server(&opened.project_root)
        .expect("mounted production server");
    // `serving_branch` names the branch of the seated, current code index.
    // Open returns before the first generation seats, so the payload read
    // straight after open is a truthful `null` and not the exact answer this
    // test pins. Wait for the seat first.
    wait_for_code_index_generation(&server, "active_project_marker").await;

    let payload = call_json(&server, json!({"format": "json"})).await;
    assert_identity(&opened, &payload);

    let markdown = call_text(&server, json!({"format": "markdown"})).await;
    assert_markdown(&opened, &markdown);

    git(&opened.project_root, &["checkout", "-b", MOVED_BRANCH]);
    let moved = call_json(&server, json!({"format": "json"})).await;
    assert_eq!(moved["project_id"], json!(PROJECT_ID), "{moved}");
    assert_eq!(
        moved["project_root"],
        json!(opened.project_root.display().to_string()),
        "{moved}"
    );
    assert_eq!(
        moved["branch"]["current_branch"],
        json!(MOVED_BRANCH),
        "the same session must name the branch the caller just checked out: {moved}"
    );

    opened.harness.shutdown().await;
}

fn assert_identity(opened: &OpenedCheckout, payload: &Value) {
    let project_root = opened.project_root.display().to_string();
    let data_root = opened.profile_root.join("projects").join(PROJECT_ID);
    let graph_db = data_root.join("tracedecay.db");
    let common_dir = opened
        .project_root
        .join(".git")
        .canonicalize()
        .expect("git common dir");
    let repository_id = format!(
        "repository.daemon.{}",
        sha256_hex(common_dir.display().to_string().as_bytes())
    );

    assert_eq!(payload["project_id"], json!(PROJECT_ID), "{payload}");
    assert_eq!(payload["repository_id"], json!(repository_id), "{payload}");
    assert_eq!(payload["project_root"], json!(project_root), "{payload}");
    assert_eq!(payload["scope_prefix"], json!(SCOPE_PREFIX), "{payload}");
    assert_eq!(
        payload["resolution_source"],
        json!("active_project"),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["class"],
        json!("code_project"),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["mode"],
        json!("profile_sharded"),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["data_root"],
        json!(data_root.display().to_string()),
        "{payload}"
    );
    assert!(payload["storage"].get("config_path").is_none(), "{payload}");
    assert_eq!(
        payload["storage"]["graph_db_path"],
        json!(graph_db.display().to_string()),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["graph_db_exists"],
        json!(true),
        "{payload}"
    );
    let graph_bytes = fs::metadata(&graph_db)
        .unwrap_or_else(|error| panic!("graph db {}: {error}", graph_db.display()))
        .len();
    assert_eq!(
        payload["storage"]["graph_db_size_bytes"],
        json!(graph_bytes),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["sessions_db_path"],
        json!(data_root.join("sessions.db").display().to_string()),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["response_handle_root"],
        json!(data_root.join("response-handles").display().to_string()),
        "{payload}"
    );
    assert_eq!(
        payload["storage"]["lcm_payload_root"],
        json!(data_root.join("lcm-payloads").display().to_string()),
        "{payload}"
    );
    assert!(
        payload["branch"].get("serving_db_path").is_none(),
        "{payload}"
    );
    assert!(
        payload["branch"].get("serving_db_exists").is_none(),
        "{payload}"
    );
    assert_eq!(
        payload["branch"]["current_branch"],
        json!(OPENED_BRANCH),
        "{payload}"
    );
    assert_eq!(
        payload["branch"]["open_active_branch"],
        json!(OPENED_BRANCH),
        "{payload}"
    );
    assert_eq!(
        payload["branch"]["serving_branch"],
        json!(OPENED_BRANCH),
        "{payload}"
    );
    assert_eq!(
        payload["branch"]["branch_drifted"],
        json!(false),
        "{payload}"
    );
    assert_eq!(
        payload["branch"]["branch_resolution"],
        json!("exact"),
        "{payload}"
    );
    assert_eq!(
        payload["branch"]["tracked_branch_count"],
        json!(1),
        "{payload}"
    );
}

fn assert_markdown(opened: &OpenedCheckout, markdown: &str) {
    let project_root = opened.project_root.display().to_string();
    for line in [
        format!("**project_id:** `{PROJECT_ID}`"),
        format!("**project_root:** {project_root}"),
        format!("**scope_prefix:** {SCOPE_PREFIX}"),
        "**resolution_source:** active_project".to_owned(),
        format!("**current_branch:** {OPENED_BRANCH}"),
        "**class:** code_project".to_owned(),
        "**mode:** profile_sharded".to_owned(),
    ] {
        assert!(
            markdown.contains(&line),
            "markdown missing `{line}`:\n{markdown}"
        );
    }
}

async fn call_json(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_active_project", arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_active_project did not return JSON: {error}: {text}")
    })
}

async fn call_text(server: &McpServer, arguments: Value) -> String {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_active_project", arguments).await;
    assert!(response["error"].is_null(), "{response}");
    extract_real_server_text(&response["result"]).to_owned()
}

async fn open_checkout() -> OpenedCheckout {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(project_root.join("src")).expect("project src");
    fs::write(
        project_root.join("src/lib.rs"),
        "pub fn active_project_marker() -> u8 { 7 }\n",
    )
    .expect("fixture source");
    pin_fixture_repository_identity(&project_root, PROJECT_ID).expect("pin repository identity");
    git(&project_root, &["checkout", "-B", OPENED_BRANCH]);
    git(&project_root, &["add", "."]);
    git(
        &project_root,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "pin active project",
        ],
    );

    let project_root = canonical_existing_identity(&project_root).expect("canonical project");
    let isolation_root = isolation
        .path()
        .canonicalize()
        .expect("canonical isolation");
    let harness = ProductionProjectCompositionHarnessV1::open_with_scope_prefix(
        &isolation_root,
        [project_root.clone()],
        SCOPE_PREFIX,
    )
    .await
    .expect("production composition");
    OpenedCheckout {
        harness,
        project_root,
        profile_root: isolation_root.join("profile"),
        _isolation: isolation,
    }
}
