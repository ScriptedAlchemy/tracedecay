//! Daemon-only `serve --path` behavior for literal unexpanded host templates.

use std::ffi::OsStr;

use serde_json::json;
use tempfile::TempDir;

use crate::common::canonical_existing_path;
use crate::serve_harness::run_serve_runtime;

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
