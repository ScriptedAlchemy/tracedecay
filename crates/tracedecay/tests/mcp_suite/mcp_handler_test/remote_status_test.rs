//! Real `tools/call` coverage for `tracedecay_remote_status`.
//!
//! The production daemon mounts the Remote Brain reader. With no listener and
//! no registered node, that reader is `unconfigured`. A direct server never
//! installs the reader, so the same call is `unavailable`. Neither outcome is
//! an empty success or a semantic tool error.
//!
//! The configured (`observed`) plane is proved at the two seams that can mount
//! one cheaply: `daemon::remote_protocol_tests` provisions a real node and
//! serving listener against the session runtime registry, and
//! `mcp::tools::handlers::info::remote_status_dispatch_tests` drives an
//! observed reader through dispatch. This suite owns the transport boundary
//! those two do not cross.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::common;
use crate::fixture;
use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_server_with_messages, setup_server, successful_tool_text,
};
use crate::support::{TestTempDir, test_temp_dir};

const UNCONFIGURED_JSON: &str = r#"{"kind":"unconfigured"}"#;
const UNAVAILABLE_JSON: &str = r#"{"kind":"unavailable"}"#;
const UNCONFIGURED_MARKDOWN: &str = "**kind:** unconfigured\n";
const UNAVAILABLE_MARKDOWN: &str = "**kind:** unavailable\n";

struct MountedDaemon {
    harness: ProductionProjectCompositionHarnessV1,
    project: PathBuf,
    _isolation: TestTempDir,
}

async fn mount_daemon_without_remote_plane() -> MountedDaemon {
    let isolation = test_temp_dir();
    let project = isolation.path().join("project");
    std::fs::create_dir_all(&project).expect("remote-status project directory");
    fixture::write_indexed_fixture_sources(&project);
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "remote status fixture",
        ],
    ] {
        let status = Command::new(common::git_program())
            .args(args)
            .current_dir(&project)
            .status()
            .expect("git");
        assert!(status.success(), "git must succeed for {project:?}");
    }
    let harness = Box::pin(
        ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
            isolation.path(),
            [project.clone()],
        ),
    )
    .await
    .expect("production composition");
    MountedDaemon {
        harness,
        project,
        _isolation: isolation,
    }
}

fn status_text(result: &Value) -> &str {
    let content = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("remote status returned no content array: {result}"));
    assert_eq!(
        content.len(),
        1,
        "remote status must not attach banners or token footers: {result}"
    );
    assert!(
        result.get("isError").is_none(),
        "a typed remote-status read is not a semantic tool error: {result}"
    );
    content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("remote status text content missing: {result}"))
}

async fn daemon_status(mounted: &MountedDaemon, arguments: Value) -> Value {
    let response = mounted
        .harness
        .call_tool(&mounted.project, "tracedecay_remote_status", arguments)
        .await
        .expect("production tools/call");
    assert!(
        response.error.is_none(),
        "production remote status must succeed: {:?}",
        response.error
    );
    response
        .result
        .unwrap_or_else(|| panic!("production remote status missing result"))
}

#[tokio::test]
async fn production_daemon_reports_unconfigured_remote_plane() {
    let mounted = mount_daemon_without_remote_plane().await;

    let markdown = daemon_status(&mounted, json!({})).await;
    assert_eq!(status_text(&markdown), UNCONFIGURED_MARKDOWN);

    let json_result = daemon_status(&mounted, json!({"format": "json"})).await;
    assert_eq!(status_text(&json_result), UNCONFIGURED_JSON);
}

#[tokio::test]
async fn direct_server_reports_unmounted_remote_authority() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(
                json!(1),
                "tools/call",
                json!({
                    "name": "tracedecay_remote_status",
                    "arguments": {}
                }),
            ),
            jsonrpc_request(
                json!(2),
                "tools/call",
                json!({
                    "name": "tracedecay_remote_status",
                    "arguments": {"format": "json"}
                }),
            ),
        ],
    )
    .await;

    let markdown = response_with_id(&responses, json!(1));
    assert_eq!(
        successful_tool_text(&markdown, "markdown remote status"),
        UNAVAILABLE_MARKDOWN
    );
    let json_response = response_with_id(&responses, json!(2));
    assert_eq!(
        successful_tool_text(&json_response, "json remote status"),
        UNAVAILABLE_JSON
    );
    assert!(
        json_response["result"].get("isError").is_none(),
        "an unmounted remote authority is a typed read, not a tool error: {json_response}"
    );
}
