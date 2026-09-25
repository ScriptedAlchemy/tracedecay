//! Daemon-only `serve --path` behavior for literal unexpanded host templates.

use std::ffi::OsStr;

use serde_json::json;
use tempfile::TempDir;

#[cfg(unix)]
use crate::common;
use crate::common::canonical_existing_path;
use crate::serve_harness::run_serve_runtime;
#[cfg(unix)]
use crate::serve_harness::{
    assert_unenrolled_cwd_serve_session, run_serve_requests, unenrolled_cwd_serve_requests,
};

#[tokio::test]
async fn literal_template_without_daemon_fails_closed_before_mcp_handshake() {
    let home = TempDir::new().unwrap();
    let cwd = canonical_existing_path(home.path());

    let output = run_serve_runtime(
        home.path(),
        &cwd,
        Some(OsStr::new("${workspaceFolder}")),
        json!({}),
    );

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "daemon-unreachable serve must not synthesize a local MCP response:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TraceDecay daemon") && stderr.contains("is not available"),
        "expected explicit daemon-unavailable error, got:\n{stderr}"
    );
}

/// The Cursor plugin journey: the host spawns `serve --path ${workspaceFolder}`
/// without expanding the template, from a workspace that is not a TraceDecay
/// project, and sends no initialize roots. `serve` warns and discards the
/// template; the daemon must then complete the handshake and answer each tool
/// call with the typed not-enrolled state instead of closing the connection.
#[cfg(unix)]
#[tokio::test]
async fn literal_template_from_unenrolled_cwd_completes_initialize_and_types_the_refusal() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let output = run_serve_requests(
        home.path(),
        cwd.path(),
        Some(OsStr::new("${workspaceFolder}")),
        &unenrolled_cwd_serve_requests(),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpanded template variable '${workspaceFolder}'"),
        "serve must name the template the host failed to expand:\n{stderr}"
    );
    assert_unenrolled_cwd_serve_session(&output, cwd.path());
}
