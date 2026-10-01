//! The core owner project open publishes before the session stores mount:
//! a call whose tool declares the project session stores answers the typed
//! `mounting` problem, while a graph-only call is answered.

use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::ProjectId;

use super::McpServer;
use tracedecay_mcp::transport::JsonRpcRequest;
use tracedecay_project::project::TraceDecayOpenOptions;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(root)
    .args(args)
    .status()
    .expect("git command should run");
    assert!(status.success(), "git {args:?} failed");
}

/// A server whose project session store has not mounted, registered as its
/// project's graph-tool owner, over a branch one commit ahead of `main`.
async fn core_owner() -> (Arc<McpServer>, TempDir, TempDir) {
    let profile = TempDir::new().expect("isolated profile");
    let dir = TempDir::new().expect("temp project");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join(".gitignore"), ".tracedecay/\n").expect("gitignore");
    std::fs::create_dir_all(dir.path().join("src")).expect("source directory");
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    git(dir.path(), &["checkout", "-q", "-b", "topic"]);
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }\npub fn other() -> u8 { 3 }\n",
    )
    .expect("changed source");
    git(dir.path(), &["commit", "-q", "-am", "change"]);
    let runtime = HostAdmissionTestRuntimeV1::project(
        profile.path(),
        dir.path(),
        ProjectId::new("project.core-owner").expect("typed project identity"),
    )
    .await
    .expect("registered runtime");
    let graph = runtime
        .initialize_project_graph_for_test(
            dir.path(),
            TraceDecayOpenOptions {
                profile_root: Some(profile.path().to_path_buf()),
                global_db_path: None,
            },
        )
        .await
        .expect("daemon-owned project init");
    let mut context = crate::test_support::host_admission::mcp_server_context_for_test(
        Arc::new(runtime),
        graph,
        None,
    )
    .expect("registered MCP server context");
    context.project_session_db = None;
    let server =
        crate::daemon::retained_test_support::mcp_server_with_project_retained_owner_for_test(
            context,
        )
        .await
        .expect("core-owner MCP test server");
    (server, dir, profile)
}

async fn call_tool(server: &McpServer, name: &str, arguments: Value) -> Value {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({"name": name, "arguments": arguments})),
    };
    let response = server
        .handle_request(&request)
        .await
        .expect("tool call should produce a response");
    response
        .result
        .unwrap_or_else(|| panic!("{name} JSON-RPC error: {:?}", response.error))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_store_tool_on_the_core_owner_is_mounting_while_graph_tools_answer() {
    let (server, dir, _profile) = core_owner().await;

    let pr_context = call_tool(
        &server,
        "tracedecay_pr_context",
        json!({"base_ref": "main", "head_ref": "HEAD", "cursor": "continuation"}),
    )
    .await;
    assert_eq!(
        pr_context["structuredContent"]["problem"]["code"],
        json!("application.runtime.mounting"),
        "{pr_context}"
    );

    let active = call_tool(
        &server,
        "tracedecay_active_project",
        json!({"format": "json"}),
    )
    .await;
    let text = active["content"][0]["text"].as_str().expect("tool text");
    let answer: Value = serde_json::from_str(text).expect("active project JSON");
    assert_eq!(
        answer["project_id"],
        json!("project.core-owner"),
        "{answer}"
    );
    assert_eq!(
        answer["project_root"],
        json!(dir.path().canonicalize().unwrap()),
        "{answer}"
    );
}
