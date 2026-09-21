//! Reading an MCP tool answer that outgrew the response frame.
//!
//! A body over the frame cap does not arrive as the JSON a journey reads. MCP
//! stores the original, answers with `{"truncated": true, "handle": …,
//! "preview": …}`, and `preview` is a *string* holding a prefix of that JSON.
//! Every field a predicate looks for — `results`, `code_generation` — is then
//! absent at the top level, and a wait that reads them as missing cannot tell
//! a truncated answer from a generation that is still warming.
//!
//! So read the stored original through `tracedecay_retrieve` the way an agent
//! does, and keep that one authority: the page size that fits the frame is a
//! property of how much ranking provenance a candidate carries, not something
//! a journey should be guessing at.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::JsonRpcResponse;

/// Call one MCP tool on an admitted project and read its JSON answer.
pub async fn tool_json(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    name: &str,
    arguments: Value,
) -> Value {
    let payload = called_tool_payload(harness, project, name, arguments).await;
    resolved_tool_payload(harness, project, payload).await
}

/// Reassemble a payload the response frame replaced with a retrieval handle.
///
/// An untruncated payload is returned as it arrived.
pub async fn resolved_tool_payload(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    payload: Value,
) -> Value {
    if payload.get("truncated") != Some(&json!(true)) {
        return payload;
    }
    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("truncated response omitted its retrieve handle: {payload}"));
    let mut content = String::new();
    let mut offset = 0_u64;
    loop {
        let retrieved = called_tool_payload(
            harness,
            project,
            "tracedecay_retrieve",
            json!({"handle": handle, "format": "json", "offset": offset}),
        )
        .await;
        content.push_str(retrieved["content"].as_str().unwrap_or_else(|| {
            panic!("truncated response handle carried no content page: {retrieved}")
        }));
        if retrieved["has_more"] != json!(true) {
            break;
        }
        let next_offset = retrieved["next_offset"].as_u64().unwrap_or_else(|| {
            panic!("retrieve reported more pages without a next offset: {retrieved}")
        });
        assert!(
            next_offset > offset,
            "retrieve did not advance past offset {offset}: {retrieved}"
        );
        offset = next_offset;
    }
    serde_json::from_str(&content).unwrap_or_else(|error| {
        panic!("truncated response handle did not retrieve JSON: {error}; content={content}")
    })
}

/// One tool call, decoded but not reassembled. `tracedecay_retrieve` pages
/// answer within the frame by construction, so the paging loop above reads
/// them through this rather than through [`tool_json`].
async fn called_tool_payload(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    name: &str,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project, name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    decoded_tool_payload(&response)
}

fn decoded_tool_payload(response: &JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "{response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool effect failed: {result}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool returned invalid JSON: {error}; text={text}"))
}
