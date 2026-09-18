//! `tracedecay_lcm_doctor` through the production MCP `tools/call` path.
//!
//! The registered test server mounts `UnavailableSessionTemporalRefreshWake`
//! and does not bind a session relation graph. Doctor must name both of those
//! states. It must not invent a current projection, and it must not accept a
//! repair argument.

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    real_mcp_server, setup_empty_project,
};

fn empty_project_doctor_report() -> Value {
    json!({
        "status": "partial",
        "authority_outcome": { "state": "ready" },
        "health": {
            "status": "partial",
            "findings": [
                { "kind": "relation_graph_unavailable", "count": 1 }
            ]
        },
        "projection": {
            "state": "unavailable",
            "reason": "worker_missing",
            "worker": {
                "last_progress_at_unix_micros": null,
                "backlog": 0,
                "blocker": "worker_missing",
                "retry_class": null
            }
        }
    })
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_diagnoses_an_empty_project_store() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let server = real_mcp_server(cg).await;
    let result = handle_real_server_tool_call(&server, "tracedecay_lcm_doctor", json!({})).await;
    let text = extract_real_server_text(&result);
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("lcm doctor must answer JSON: {error}\n{text}"));

    assert_eq!(payload, empty_project_doctor_report());
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_refuses_a_repair_argument_and_keeps_the_same_diagnosis() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let server = real_mcp_server(cg).await;

    let before = handle_real_server_tool_call(&server, "tracedecay_lcm_doctor", json!({})).await;
    let before: Value =
        serde_json::from_str(extract_real_server_text(&before)).expect("doctor diagnosis");
    assert_eq!(before, empty_project_doctor_report());

    let refused = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_lcm_doctor",
        json!({ "apply": true }),
    )
    .await;
    assert_eq!(refused["error"]["code"], -32603);
    assert_eq!(
        refused["error"]["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_lcm_doctor: unknown field `apply`, there are no fields"
    );
    assert_eq!(refused["error"]["data"]["tool"], "tracedecay_lcm_doctor");

    let after = handle_real_server_tool_call(&server, "tracedecay_lcm_doctor", json!({})).await;
    let after: Value =
        serde_json::from_str(extract_real_server_text(&after)).expect("doctor diagnosis");
    assert_eq!(after, empty_project_doctor_report());

    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_rejects_an_unknown_storage_scope() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let server = real_mcp_server(cg).await;
    let refused = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_lcm_doctor",
        json!({ "storage_scope": "hermes_profile" }),
    )
    .await;

    assert_eq!(refused["error"]["code"], -32603);
    assert_eq!(
        refused["error"]["message"],
        "tool execution failed: config error: storage_scope must be one of project, user"
    );
    server.shutdown().await;
}
