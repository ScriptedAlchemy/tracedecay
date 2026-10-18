//! `tracedecay_hook_runtime` over the production MCP `tools/call` path, the
//! way agent-host hooks call it: the project's owner, or the profile owner for
//! a hook with no project route, answers each action's typed result, and a
//! request outside what hosts send is refused through the owner's problem
//! record instead of being ignored.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tracedecay_runtime_core::tracedecay::current_timestamp;

use crate::support::{
    ProductionCompositionFixture, call_tool_when_ready, production_composition_fixture,
};

const TOOL: &str = "tracedecay_hook_runtime";

async fn call(fixture: &ProductionCompositionFixture, tool: &str, arguments: Value) -> Value {
    call_tool_when_ready(&fixture.harness, &fixture.project_root, tool, arguments).await
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

/// Appends one delivered `search` hint to the served project's hook log, the
/// row a host hook writes when it surfaces a hint.
async fn write_search_hint(fixture: &ProductionCompositionFixture, agent: &str, session_id: &str) {
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
                "agent": agent,
                "event": "hint_emitted",
                "session_id": session_id,
                "category": "search",
                "hint_id": "hint-search-1",
                "ts_unix_ms": (current_timestamp() - 120) * 1000,
            })
        ),
    )
    .expect("write hook analytics row");
}

/// The `search` row of the hints analytics section once background
/// settlement has counted an emitted hint, or the last row read when it never
/// does within the deadline.
async fn settled_search_hints(fixture: &ProductionCompositionFixture) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let analytics = answer_tool(
            fixture,
            "tracedecay_analytics",
            json!({"section": "hints", "format": "json"}),
        )
        .await;
        let search = analytics["hints"]["by_category"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["category"] == "search"))
            .cloned()
            .unwrap_or(Value::Null);
        if search["emitted"]
            .as_u64()
            .is_some_and(|emitted| emitted > 0)
            || Instant::now() >= deadline
        {
            return search;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
            "invalid arguments for tracedecay_hook_runtime: unknown variant `codex_stop`, expected one of `reset_counter`, `hook_v2_admit`, `hook_v2_delivery_receipt`, `hook_v2_feedback_notice_delivery`, `ingest_transcript`, `codex_compact`, `claude_compact`, `cursor_compact`, `hermes_receipt`, `hook_v2_profile_admit`"
        )
    );

    // A hook with no project route selects the profile; the daemon's profile
    // owner answers it even on a project connection.
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
    write_search_hint(&fixture, "cursor", "cursor-session").await;
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
    // A missed live-refresh join is retried after the ingest committed, and
    // the retry answers the same rollout as an exact duplicate. The searched
    // text and the settled hint analytics below prove the first pass either way.
    let committed = ingest["status"] == "committed";
    assert!(
        committed || ingest["status"] == "exact_duplicate",
        "ingest: {ingest}"
    );
    if committed {
        // Admission is the commit; `messages_upserted` counts only the projections
        // this request drained, which the catch-up worker may take first.
        assert_eq!(ingest["observations_committed"], 2, "ingest: {ingest}");
    }
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
    let question = found["results"]
        .as_array()
        .and_then(|results| results.iter().find(|row| row["message"]["role"] == "user"))
        .unwrap_or_else(|| panic!("ingested Cursor question in {found}"));
    assert!(
        question["message"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Where is billing ingestion?")),
        "ingested Cursor question text: {question}"
    );
    assert_eq!(question["message"]["session_id"], "cursor-session");
    assert_eq!(question["message"]["provider"], "cursor");
    if committed {
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
    }

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

/// Codex sends no project-scope transcript ingest; its prompt hook's counter
/// reset is what must carry its delivered hints into analytics.
#[tokio::test]
async fn codex_prompt_counter_reset_settles_emitted_hints() {
    let fixture = production_composition_fixture().await;
    write_search_hint(&fixture, "codex", "codex-session").await;

    assert_eq!(
        answer(&fixture, json!({"action": "reset_counter"})).await,
        json!({"action": "reset_counter", "reset": true})
    );
    assert_eq!(
        settled_search_hints(&fixture).await,
        json!({
            "category": "search",
            "emitted": 1,
            "followed": 0,
            "ignored": 0,
            "suppressed": 0,
        })
    );

    fixture.harness.shutdown().await;
}

/// Cursor reaches the daemon synchronously only at session start, as a
/// session-boundary admission; that admission settles the project's hints.
#[tokio::test]
async fn session_boundary_admission_settles_emitted_hints() {
    let fixture = production_composition_fixture().await;
    write_search_hint(&fixture, "cursor", "cursor-session").await;
    let envelope = tracedecay_hooks::HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: tracedecay_domain::NativeHostIdentityV1::CursorDesktop,
        protected_session_id: [2; 32],
        project_id: [3; 16],
        repository_id: [4; 16],
        worktree_id: [5; 16],
        worktree_epoch: 1,
        binding_token: [6; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: tracedecay_domain::UtcMicros(current_timestamp() * 1_000_000),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::Start,
        },
    };

    // The admission verdict for this unbound envelope is irrelevant here;
    // settlement is keyed to the served project, not to the ledger outcome.
    call(
        &fixture,
        TOOL,
        json!({
            "action": "hook_v2_admit",
            "envelope": envelope,
            "native_session_id": null,
            "native_lifecycle": null,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(
        settled_search_hints(&fixture).await,
        json!({
            "category": "search",
            "emitted": 1,
            "followed": 0,
            "ignored": 0,
            "suppressed": 0,
        })
    );

    fixture.harness.shutdown().await;
}
