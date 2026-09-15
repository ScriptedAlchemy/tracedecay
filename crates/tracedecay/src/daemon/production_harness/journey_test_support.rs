//! Git fixture and MCP tool-payload helpers shared by the
//! production-composition journey tests.

use std::path::Path;

use serde_json::Value;

use tracedecay_mcp::JsonRpcResponse;

use super::ProductionProjectCompositionHarnessV1;
use crate::test_support::git::GIT_FIXTURE_CONFIG;

pub(super) fn git(project: &Path, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(project)
        .args(GIT_FIXTURE_CONFIG)
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

/// The JSON payload a tool answered with, plus whether the tool flagged that
/// answer as a refusal (`isError`).
///
/// A refusal is still an answer: the transport carried a typed payload the
/// caller can act on, which is what separates it from a JSON-RPC error. Only
/// callers that expect a refusal decode through this; [`tool_payload`] treats
/// one as failure.
pub(super) fn tool_answer(response: &JsonRpcResponse) -> (bool, Value) {
    assert!(response.error.is_none(), "tool failed: {response:?}");
    let result = response.result.as_ref().expect("tool result");
    let refused = result["isError"] == true;
    let text = result["content"][0]["text"].as_str().expect("tool text");
    let payload = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tool did not return JSON: {error}; result={result}; text={text}")
    });
    (refused, payload)
}

pub(super) fn tool_payload(response: &JsonRpcResponse) -> Value {
    let (refused, payload) = tool_answer(response);
    assert!(!refused, "tool failed: {payload}");
    payload
}

/// The full payload behind a possibly truncated answer.
///
/// A payload larger than the MCP response cap is carried as a preview plus a
/// local response handle. That is reversible transport framing, not a
/// retrieval outcome — and the preview is cut mid-JSON, so parsing it would
/// silently yield an empty result set. `tracedecay_retrieve` pages the stored
/// response through `offset` / `next_offset` / `has_more`; reassemble it
/// exactly as an agent does before parsing.
pub(super) async fn resolved(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    payload: Value,
) -> Value {
    if payload["truncated"] != serde_json::json!(true) {
        return payload;
    }
    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} truncated its answer without a handle: {payload}"))
        .to_owned();
    let mut content = String::new();
    let mut offset = 0_u64;
    loop {
        let response = harness
            .call_tool(
                project,
                "tracedecay_retrieve",
                serde_json::json!({"handle": handle, "format": "json", "offset": offset}),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{tool} response handle was blocked instead of paging: {error}")
            });
        let retrieved = tool_payload(&response);
        content.push_str(
            retrieved["content"].as_str().unwrap_or_else(|| {
                panic!("{tool} response handle carried no content: {retrieved}")
            }),
        );
        if retrieved["has_more"] != serde_json::json!(true) {
            break;
        }
        let next_offset = retrieved["next_offset"].as_u64().unwrap_or_else(|| {
            panic!("{tool} retrieval reported more pages without a next offset: {retrieved}")
        });
        assert!(
            next_offset > offset,
            "{tool} retrieval did not advance past offset {offset}: {retrieved}"
        );
        offset = next_offset;
    }
    serde_json::from_str(&content).unwrap_or_else(|error| {
        panic!("{tool} response handle content is not JSON: {error}; content={content}")
    })
}
