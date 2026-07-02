//! Degraded serving: `serve` must not exit when startup project resolution
//! fails, because MCP hosts (Cursor especially) never retry a failed server
//! spawn — one startup exit turns every later tool call in the session into
//! "Timed out waiting for connection" until the user toggles the server or
//! reloads the window. Instead, serve completes the handshake, answers tool
//! calls with an actionable error, and recovers in-session once resolution
//! starts succeeding.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use crate::common::canonical_existing_path;
use crate::serve_harness::{
    canonical_path_string, degraded_tool_error_text, init_project_direct, init_project_under,
    init_project_with_file, json_rpc_response, register_global_project, run_serve_runtime,
    ServeStdioSession,
};

/// Asserts that a recovered `tracedecay_runtime` response is a real (non
/// degraded) result serving `expected_project`.
fn assert_recovered_runtime_response(response: &serde_json::Value, expected_project: &Path) {
    assert!(
        response.get("error").is_none() && response["result"]["isError"] != json!(true),
        "post-recovery tool call should be served by the recovered server:\n{response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("recovered runtime tool should return text content");
    let runtime: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        canonical_path_string(Path::new(
            runtime["database"]["project_root"]
                .as_str()
                .expect("runtime should include database.project_root")
        )),
        canonical_path_string(expected_project),
        "recovered server must serve the expected project"
    );
}

/// A dead MCP process permanently kills the client scope, so an explicit
/// uninitialized `--path` must NOT exit: serve completes the handshake and
/// answers tool calls with an actionable error naming the explicit path —
/// and still never silently serves the registered global-fallback project.
#[tokio::test]
async fn explicit_uninitialized_path_serves_degraded_error_instead_of_global_fallback() {
    let home = TempDir::new().unwrap();
    let explicit = TempDir::new().unwrap();
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), active.path()).await;

    let output = run_serve_runtime(
        home.path(),
        explicit.path(),
        Some(explicit.path().as_os_str()),
        json!({}),
    );

    assert!(
        output.status.success(),
        "degraded serve should stay alive until stdin closes\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let initialize = json_rpc_response(&output.stdout, 1);
    assert_eq!(
        initialize["result"]["protocolVersion"],
        json!("2024-11-05"),
        "degraded serve must complete the MCP handshake:\n{initialize}"
    );
    let tool_call = json_rpc_response(&output.stdout, 2);
    let text = degraded_tool_error_text(&tool_call);
    let explicit_display = explicit.path().display().to_string();
    for needle in [
        explicit_display.as_str(),
        "tracedecay init",
        "tracedecay tool",
        "Cursor Settings → MCP",
    ] {
        assert!(
            text.contains(needle),
            "degraded tool error should mention '{needle}':\n{text}"
        );
    }
    assert!(
        !text.contains(&active.path().display().to_string()),
        "degraded serve must not leak or serve the global-fallback project:\n{text}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&explicit_display) && stderr.contains("degraded MCP mode"),
        "stderr should name the explicit path and the degraded mode marker\nstderr:\n{stderr}"
    );
}

/// Ambiguous global fallback is a recoverable config problem: serve must not
/// pick an arbitrary project, but it must not exit either — it stays alive in
/// degraded mode and reports the ambiguity from tool calls.
#[tokio::test]
async fn same_depth_descendant_global_fallback_is_ambiguous_and_stays_alive() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let alpha = init_project_under(
        home.path(),
        cwd.path(),
        "alpha",
        "pub fn alpha_marker() {}\n",
    )
    .await;
    let beta =
        init_project_under(home.path(), cwd.path(), "beta", "pub fn beta_marker() {}\n").await;
    register_global_project(home.path(), &alpha).await;
    register_global_project(home.path(), &beta).await;

    let output = run_serve_runtime(home.path(), cwd.path(), None, json!({}));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ambiguous global fallback should serve degraded, not exit\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    let tool_call = json_rpc_response(&output.stdout, 2);
    let text = degraded_tool_error_text(&tool_call);
    assert!(
        text.contains("Multiple tracedecay projects found"),
        "tool error should explain the ambiguity:\n{text}"
    );
    assert!(
        stderr.contains("Multiple tracedecay projects found"),
        "stderr should explain the ambiguity:\n{stderr}"
    );
    assert!(
        !stderr.contains("no projects registered in the global database"),
        "stderr should not contradict ambiguity with a no-projects error:\n{stderr}"
    );
}

/// Degraded serve must recover in-session: once `tracedecay init` makes the
/// explicit `--path` resolve, the very next tool call is served for real —
/// no server toggle or window reload (which Cursor would otherwise require).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_path_recovers_after_project_init() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn degraded_recovery_marker() {}\n",
    )
    .unwrap();

    let mut session = ServeStdioSession::spawn(
        home.path(),
        project.path(),
        Some(project.path().as_os_str()),
    );
    session.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
    let initialize = session.response_with_id(1);
    assert_eq!(initialize["result"]["protocolVersion"], json!("2024-11-05"));

    session.send(&ServeStdioSession::runtime_call(2));
    degraded_tool_error_text(&session.response_with_id(2));

    // The user fixes the project mid-session.
    init_project_direct(home.path(), project.path()).await;

    session.send(&ServeStdioSession::runtime_call(3));
    assert_recovered_runtime_response(&session.response_with_id(3), project.path());

    assert!(
        session.close_and_wait().success(),
        "recovered serve should exit cleanly"
    );
}

/// The PRIMARY Cursor failure scenario: spawned from `$HOME` with a literal
/// unexpanded `${workspaceFolder}` (discarded as a template, cwd resolves
/// nothing, empty registry) → degraded. After the user initializes and
/// registers a project, the retry must re-run the full startup resolution
/// ladder — reaching the global registry, not just the dead cwd path — and
/// serve the project on the next tool call.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn literal_template_from_home_recovers_after_project_registration() {
    let home = TempDir::new().unwrap();
    let home_cwd = canonical_existing_path(home.path());

    let mut session = ServeStdioSession::spawn(
        home.path(),
        &home_cwd,
        Some(OsStr::new("${workspaceFolder}")),
    );
    session.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
    let initialize = session.response_with_id(1);
    assert_eq!(initialize["result"]["protocolVersion"], json!("2024-11-05"));

    session.send(&ServeStdioSession::runtime_call(2));
    let text = degraded_tool_error_text(&session.response_with_id(2));
    assert!(
        text.contains("tracedecay init"),
        "degraded error should point at tracedecay init:\n{text}"
    );

    // The user initializes a project elsewhere; init registers it in the
    // global registry (mirrored explicitly for the fixture-based init).
    let project =
        init_project_with_file(home.path(), "pub fn home_scope_recovery_marker() {}\n").await;
    register_global_project(home.path(), project.path()).await;

    session.send(&ServeStdioSession::runtime_call(3));
    assert_recovered_runtime_response(&session.response_with_id(3), project.path());

    assert!(
        session.close_and_wait().success(),
        "recovered serve should exit cleanly"
    );
}

/// Requests pipelined behind the recovery-triggering tools/call must survive
/// the degraded→recovered transport handoff: both arrive in one stdin chunk,
/// so the second sits in the transport's read buffer while the first
/// triggers recovery — a raw handoff (fresh transport) would drop it and the
/// client would hang forever.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipelined_requests_survive_recovery_handoff() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn pipelined_recovery_marker() {}\n",
    )
    .unwrap();

    let mut session = ServeStdioSession::spawn(
        home.path(),
        project.path(),
        Some(project.path().as_os_str()),
    );
    session.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }));
    session.response_with_id(1);
    session.send(&ServeStdioSession::runtime_call(2));
    degraded_tool_error_text(&session.response_with_id(2));

    init_project_direct(home.path(), project.path()).await;

    // One write, two requests: id 3 triggers recovery, id 4 is pipelined
    // behind it in the same chunk.
    session.send_raw(&format!(
        "{}\n{}\n",
        ServeStdioSession::runtime_call(3),
        ServeStdioSession::runtime_call(4)
    ));
    assert_recovered_runtime_response(&session.response_with_id(3), project.path());
    assert_recovered_runtime_response(&session.response_with_id(4), project.path());

    assert!(
        session.close_and_wait().success(),
        "recovered serve should exit cleanly"
    );
}
