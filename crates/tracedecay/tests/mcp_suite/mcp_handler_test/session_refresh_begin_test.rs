//! `tracedecay_session_refresh_begin` as a caller issues it: a JSON-RPC
//! `tools/call` on the production MCP server. A fresh profile selector starts
//! one refresh; repeating that selector joins the same operation instead of
//! opening a second one. A selector the request contract rejects is a typed
//! JSON-RPC error, not an empty success.

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture,
};
use serde_json::{Value, json};

const TOOL: &str = "tracedecay_session_refresh_begin";
const SESSION_ID: &str = "session.mcp.refresh-begin";

fn refresh_target() -> Value {
    json!({
        "temporal_mode": { "kind": "current" },
        "grain": "logical_message",
        "frontier": { "observed_through": 0, "committed_through": 0 }
    })
}

fn profile_begin_arguments() -> Value {
    json!({
        "scope": { "kind": "profile" },
        "session": { "id": SESSION_ID },
        "source": { "scope": "codex" },
        "target": refresh_target(),
        "format": "json"
    })
}

fn payload(result: &Value) -> Value {
    serde_json::from_str(extract_real_server_text(result)).expect("begin payload JSON")
}

fn assert_begin_fields(actual: &Value, outcome: &str) {
    assert_eq!(
        json!({
            "outcome": actual["outcome"],
            "scope": actual["scope"],
            "tool": actual["tool"],
            "progress": actual["progress"],
            "receipt": actual["receipt"],
            "error": actual["error"],
        }),
        json!({
            "outcome": outcome,
            "scope": "profile",
            "tool": TOOL,
            "progress": null,
            "receipt": null,
            "error": null,
        }),
        "{actual}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_refresh_begin_starts_then_joins_the_same_profile_operation() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let started_result =
        handle_real_server_tool_call(&server, TOOL, profile_begin_arguments()).await;
    let started = payload(&started_result);
    assert_begin_fields(&started, "started");
    let handle = started["handle"]
        .as_str()
        .expect("started handle")
        .to_owned();
    let digest = handle
        .strip_prefix("srh_")
        .expect("started handle is an opaque srh_ token");
    assert_eq!(digest.len(), 64, "{handle}");
    assert!(
        digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{handle}"
    );
    let operation_id = started["operation_id"]
        .as_str()
        .expect("started operation id")
        .to_owned();
    assert_ne!(handle, operation_id, "{started}");
    let accepted_at = started["accepted_at"]
        .as_i64()
        .expect("started accepted_at");

    let joined_result =
        handle_real_server_tool_call(&server, TOOL, profile_begin_arguments()).await;
    let joined = payload(&joined_result);
    assert_begin_fields(&joined, "joined");
    assert_eq!(joined["handle"], handle, "{joined}");
    assert_eq!(joined["operation_id"], operation_id, "{joined}");
    assert_eq!(joined["accepted_at"], accepted_at, "{joined}");

    let unknown_scope = handle_real_server_tool_call_raw(
        &server,
        TOOL,
        json!({
            "scope": { "kind": "user" },
            "session": { "id": SESSION_ID },
            "source": { "scope": "codex" },
            "target": refresh_target(),
            "format": "json"
        }),
    )
    .await;
    assert_eq!(unknown_scope["error"]["code"], -32603, "{unknown_scope}");
    assert_eq!(
        unknown_scope["error"]["data"]["tool"], TOOL,
        "{unknown_scope}"
    );
    assert_eq!(
        unknown_scope["error"]["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_session_refresh_begin: scope.kind: unknown variant `user`, expected `project` or `profile`",
        "{unknown_scope}"
    );

    let missing_scope =
        handle_real_server_tool_call_raw(&server, TOOL, json!({"format": "json"})).await;
    assert_eq!(missing_scope["error"]["code"], -32603, "{missing_scope}");
    assert_eq!(
        missing_scope["error"]["data"]["tool"], TOOL,
        "{missing_scope}"
    );
    assert_eq!(
        missing_scope["error"]["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_session_refresh_begin: missing field `scope`",
        "{missing_scope}"
    );

    fixture.harness.shutdown().await;
}
