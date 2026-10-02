//! `tracedecay_hook_runtime` over the production MCP `tools/call` path, the
//! way agent-host hooks call it: the project's owner, or the profile owner for
//! a hook with no project route, answers each action's typed result, and a
//! request outside what hosts send is refused through the owner's problem
//! record instead of being ignored.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};
use tracedecay_runtime_core::tracedecay::current_timestamp;

use crate::support::{ProductionCompositionFixture, production_composition_fixture};

const TOOL: &str = "tracedecay_hook_runtime";

async fn call(fixture: &ProductionCompositionFixture, tool: &str, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool} answers through its owner, never a JSON-RPC error: {:?}",
        response.error
    );
    response.result.expect("tool result")
}

async fn answer_tool(
    fixture: &ProductionCompositionFixture,
    tool: &str,
    mut arguments: Value,
) -> Value {
    arguments["format"] = json!("json");
    let result = call(fixture, tool, arguments).await;
    assert_eq!(result.get("isError"), None, "{tool} refused: {result}");
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} returned no text: {result}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{tool} JSON: {error}: {text}"))
}

async fn answer(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    answer_tool(fixture, TOOL, arguments).await
}

async fn refusal(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let result = call(fixture, TOOL, arguments).await;
    assert_eq!(result["isError"], true, "hook action must refuse: {result}");
    let problem = &result["structuredContent"]["problem"];
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

    // A hook with no project route selects the profile; the daemon's profile
    // owner answers it even on a project connection.
    assert_eq!(
        refusal(
            &fixture,
            json!({"action": "user_review", "provider": "codex", "session_id": null})
        )
        .await,
        invalid(
            "projectless Hermes review is unavailable: automation requires a pinned project configuration"
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            json!({"action": "ingest_transcript", "provider": "codex", "user_scope": true})
        )
        .await,
        invalid("missing required parameter `session_id`")
    );
    assert_eq!(
        refusal(
            &fixture,
            json!({"action": "hermes_receipt", "event": {"agent": "hermes"}})
        )
        .await,
        invalid("invalid Hermes receipt event: missing field `event`")
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn project_transcript_ingest_settles_emitted_hints_in_the_served_profile() {
    let fixture = production_composition_fixture().await;
    let hint_ts_ms = (current_timestamp() - 120) * 1000;
    let hook_log = fixture
        .harness
        .project_data_root(&fixture.project_root)
        .await
        .expect("project data root")
        .join("hook_analytics.jsonl");
    std::fs::write(
        &hook_log,
        format!(
            "{}\n",
            json!({
                "agent": "cursor",
                "event": "hint_emitted",
                "session_id": "cursor-session",
                "category": "search",
                "hint_id": "hint-search-1",
                "ts_unix_ms": hint_ts_ms,
            })
        ),
    )
    .expect("write hook analytics row");
    let transcript = fixture
        .harness
        .isolation_root()
        .join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Where is billing ingestion?"}]}}
{"role":"assistant","message":{"content":[{"type":"tool_use","name":"tracedecay_context","input":{"task":"billing ingestion"}}]}}
"#,
    )
    .expect("write cursor transcript");

    let ingest = answer(
        &fixture,
        json!({
            "action": "ingest_transcript",
            "provider": "cursor",
            "user_scope": false,
            "event_json": json!({
                "session_id": "cursor-session",
                "transcript_path": transcript,
                "cwd": fixture.project_root,
            })
            .to_string(),
        }),
    )
    .await;
    // Admission is this pass's commit. `messages_upserted` counts only what
    // this pass's own projection drain materialized, and the project catch-up
    // drains the same queue, so the transcript is proven by what reads see.
    assert_eq!(ingest["status"], "committed", "ingest: {ingest}");
    let envelope = answer_tool(
        &fixture,
        "tracedecay_message_search",
        json!({
            "provider": "cursor",
            "query": "billing ingestion",
            "require_fresh": false,
        }),
    )
    .await;
    let found = envelope
        .pointer("/outcome/value/payload")
        .unwrap_or(&envelope);
    assert_eq!(found["status"], "ok", "message search: {found}");
    assert!(
        found["results"].as_array().is_some_and(|results| results
            .iter()
            .any(|row| row["session"]["session_id"] == "cursor-session")),
        "ingested Cursor transcript must be readable: {found}"
    );
    assert_eq!(
        ingest["hint_outcomes"],
        json!({
            "status": "ok",
            "imported_events": 1,
            "import_errors": [],
            "scanned": 1,
            "acted": 1,
            "ignored": 0,
            "unresolved": 0,
            "written": 1,
        })
    );

    let analytics = answer_tool(
        &fixture,
        "tracedecay_analytics",
        json!({"section": "hints", "format": "json"}),
    )
    .await;
    let search = analytics["hints"]["by_category"]
        .as_array()
        .expect("hint categories")
        .iter()
        .find(|row| row["category"] == "search")
        .unwrap_or_else(|| panic!("search hint category in {analytics}"));
    assert_eq!(
        search,
        &json!({
            "category": "search",
            "emitted": 1,
            "followed": 1,
            "ignored": 0,
            "suppressed": 0,
        })
    );

    fixture.harness.shutdown().await;
}
