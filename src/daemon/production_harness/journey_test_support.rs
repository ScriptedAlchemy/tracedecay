//! Git fixture and MCP tool-payload helpers shared by the
//! production-composition journey tests.

use std::path::Path;

use serde_json::Value;

use crate::mcp::JsonRpcResponse;

pub(super) fn git(project: &Path, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(project)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output")
        .trim()
        .to_owned()
}

pub(super) fn tool_payload(response: &JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool failed: {result}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tool did not return JSON: {error}; result={result}; text={text}")
    })
}
