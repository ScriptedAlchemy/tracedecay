//! `tracedecay_hook_runtime` over the production MCP `tools/call` path, the
//! way agent-host hooks call it: the project's owner answers each action's
//! typed result, and a request outside what hosts send is refused through the
//! owner's problem record instead of being ignored.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};

use crate::support::{ProductionCompositionFixture, production_composition_fixture};

const TOOL: &str = "tracedecay_hook_runtime";

async fn call(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, TOOL, arguments)
        .await
        .unwrap_or_else(|error| panic!("hook runtime production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "hook runtime answers through its owner, never a JSON-RPC error: {:?}",
        response.error
    );
    response.result.expect("hook runtime result")
}

async fn answer(fixture: &ProductionCompositionFixture, mut arguments: Value) -> Value {
    arguments["format"] = json!("json");
    let result = call(fixture, arguments).await;
    assert_eq!(result.get("isError"), None, "hook action refused: {result}");
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("hook runtime returned no text: {result}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("hook runtime JSON: {error}: {text}"))
}

async fn refusal(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let result = call(fixture, arguments).await;
    assert_eq!(result["isError"], true, "hook action must refuse: {result}");
    let problem = &result["problem"];
    json!({
        "kind": problem["kind"],
        "code": problem["code"],
        "message": problem["message"],
    })
}

fn invalid(detail: &str) -> Value {
    json!({
        "kind": "invalid_request",
        "code": "application.surface.invalid_request",
        "message": detail,
    })
}

#[tokio::test]
async fn hook_runtime_answers_typed_results_and_refuses_what_no_host_sends() {
    let fixture = production_composition_fixture().await;

    assert_eq!(
        answer(&fixture, json!({"action": "reset_counter"})).await,
        json!({"action": "reset_counter", "reset": true})
    );
    assert_eq!(
        answer(
            &fixture,
            json!({"action": "claude_compact", "event_json": "{}", "user_scope": false})
        )
        .await,
        json!({
            "action": "claude_compact",
            "status": "unavailable",
            "reason": "claude_postcompact_provenance_unavailable",
            "summary_nodes_created": 0,
            "summary_node_ids": [],
        })
    );

    assert_eq!(
        refusal(
            &fixture,
            json!({"action": "reset_counter", "project_root": "/elsewhere"})
        )
        .await,
        invalid(
            "invalid arguments for tracedecay_hook_runtime: unknown field `project_root`, there are no fields"
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            json!({
                "action": "ingest_transcript",
                "provider": "cursor",
                "user_scope": false,
                "event_json": "{}",
                "timeout_budget_ms": 250,
            })
        )
        .await,
        invalid(
            "invalid arguments for tracedecay_hook_runtime: unknown field `timeout_budget_ms`, expected one of `provider`, `user_scope`, `session_id`, `event_json`, `messages`, `max_new_bytes`"
        )
    );
    assert_eq!(
        refusal(&fixture, json!({"action": "codex_stop", "session_id": "s"})).await,
        invalid(
            "invalid arguments for tracedecay_hook_runtime: unknown variant `codex_stop`, expected one of `reset_counter`, `hook_v2_admit`, `hook_v2_delivery_receipt`, `hook_v2_feedback_notice_delivery`, `opencode_lsp_updated`, `ingest_transcript`, `codex_compact`, `claude_compact`, `cursor_compact`, `user_review`, `hermes_receipt`, `hook_v2_profile_admit`"
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            json!({"action": "ingest_transcript", "provider": "codex", "user_scope": true})
        )
        .await,
        invalid("user transcript ingest requires projectless daemon routing")
    );
    assert_eq!(
        refusal(
            &fixture,
            json!({"action": "hermes_receipt", "event": {"agent": "hermes"}})
        )
        .await,
        invalid("hook action `hermes_receipt` requires projectless daemon routing")
    );

    fixture.harness.shutdown().await;
}
