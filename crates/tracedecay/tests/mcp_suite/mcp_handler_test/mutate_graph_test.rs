//! Behavior of `tracedecay_work_mutate_graph` through the real MCP server.
//!
//! Preparation mints the command identity and revision pins. This tool is the
//! only writer: it must commit those pins, replay the same command, and refuse
//! a body that no longer names that authority.

#![cfg(all(feature = "test-transport", unix))]

use std::path::Path;

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources,
};
use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

const CREATED_AT: i64 = 1_700_000_000_000_000;
const TASK_ID: &str = "task.mcp-mutate-graph";
const TASK_TITLE: &str = "Apply one prepared graph mutation";
const INITIATIVE_ID: &str = "initiative.mcp-mutate-graph";
const PLAN_ID: &str = "plan.mcp-mutate-graph";
const MILESTONE_ID: &str = "milestone.mcp-mutate-graph";

fn selection() -> Value {
    json!({ "selection": "profile_owned_no_git" })
}

fn task_item(task_id: &str, title: &str) -> Value {
    json!({
        "input": {
            "task_id": task_id,
            "hierarchy": {
                "initiative_id": INITIATIVE_ID,
                "plan_id": PLAN_ID,
                "milestone_id": MILESTONE_ID
            },
            "title": title,
            "dependencies": [],
            "informational_relations": [],
            "causal_candidates": [],
            "acceptance_criteria": [],
            "effort": 1,
            "scheduled_at": null,
            "deadline": null,
            "created_at": CREATED_AT,
            "updated_at": CREATED_AT
        },
        "accepted_proposal": null,
        "accepted_route": null,
        "execution_admitted_at": null,
        "accepted_attempts": [],
        "accepted_criteria": {},
        "accepted_at": null,
        "archived_at": null,
        "evidence_links": [],
        "handoffs": []
    })
}

fn create_task_change() -> Value {
    json!({
        "change": "create_task",
        "initiative": {
            "id": INITIATIVE_ID,
            "title": "MCP mutate graph initiative",
            "created_at": CREATED_AT
        },
        "plan": {
            "id": PLAN_ID,
            "initiative_id": INITIATIVE_ID,
            "title": "MCP mutate graph plan",
            "created_at": CREATED_AT
        },
        "milestone": {
            "id": MILESTONE_ID,
            "plan_id": PLAN_ID,
            "title": "MCP mutate graph milestone",
            "created_at": CREATED_AT
        },
        "item": task_item(TASK_ID, TASK_TITLE)
    })
}

async fn tool_json(server: &McpServer, tool: &str, arguments: Value) -> (Value, Value) {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    let text = extract_real_server_text(&result);
    let decoded: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {text}"));
    let payload = decoded
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or(decoded);
    (result, payload)
}

async fn prepare(server: &McpServer, change: Value) -> Value {
    let (result, prepared) = tool_json(
        server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection(),
            "change": change,
            "evidence": []
        }),
    )
    .await;
    assert_eq!(result.get("isError"), None, "{prepared}");
    prepared
}

async fn mutate(server: &McpServer, request: Value) -> (Value, Value) {
    tool_json(server, "tracedecay_work_mutate_graph", request).await
}

fn assert_refusal(
    result: &Value,
    envelope: &Value,
    code: &str,
    kind: &str,
    message: &str,
    retry: &str,
    legal_actions: Value,
    owning_layer: &str,
) {
    assert_eq!(result["isError"], true, "{envelope}");
    assert_eq!(envelope["kind"], "problem", "{envelope}");
    let problem = &envelope["value"]["problem"];
    assert_eq!(problem["kind"], kind, "{problem}");
    assert_eq!(problem["code"], code, "{problem}");
    assert_eq!(problem["message"], message, "{problem}");
    assert_eq!(problem["diagnostic"]["code"], code, "{problem}");
    assert_eq!(problem["diagnostic"]["message"], message, "{problem}");
    assert_eq!(problem["retry"], retry, "{problem}");
    assert_eq!(problem["legal_actions"], legal_actions, "{problem}");
    assert_eq!(problem["owning_layer"], owning_layer, "{problem}");
    assert_eq!(problem["committed_receipt"], Value::Null, "{problem}");
}

fn assert_created_task(receipt: &Value, command_id: &Value) {
    assert_eq!(receipt["replayed"], false, "{receipt}");
    assert_eq!(receipt["event"]["command_id"], command_id, "{receipt}");
    assert_eq!(receipt["event"]["sequence"], 1, "{receipt}");
    assert_eq!(
        receipt["event"]["expected_graph_version"],
        Value::Null,
        "{receipt}"
    );
    assert_eq!(receipt["event"]["result_graph_version"], 1, "{receipt}");
    assert_eq!(receipt["event"]["payload"]["kind"], "created", "{receipt}");
    assert_eq!(
        receipt["event"]["payload"]["graph"]["version"], 1,
        "{receipt}"
    );
    assert_eq!(
        receipt["verified_graph_version"]["graph_version"], 1,
        "{receipt}"
    );
    assert_eq!(
        receipt["verified_graph_version"]["event_sequence"], 1,
        "{receipt}"
    );
    assert_eq!(
        receipt["event"]["payload"]["graph"]["initiatives"][0]["id"], INITIATIVE_ID,
        "{receipt}"
    );
    assert_eq!(
        receipt["event"]["payload"]["graph"]["initiatives"][0]["title"],
        "MCP mutate graph initiative",
        "{receipt}"
    );
    assert_eq!(
        receipt["event"]["payload"]["graph"]["plans"][0]["id"], PLAN_ID,
        "{receipt}"
    );
    assert_eq!(
        receipt["event"]["payload"]["graph"]["milestones"][0]["id"], MILESTONE_ID,
        "{receipt}"
    );
    let item = &receipt["event"]["payload"]["graph"]["items"][0]["input"];
    assert_eq!(item["task_id"], TASK_ID, "{receipt}");
    assert_eq!(item["title"], TASK_TITLE, "{receipt}");
    assert_eq!(item["created_at"], CREATED_AT, "{receipt}");
    assert_eq!(item["effort"], 1, "{receipt}");
}

fn write_marker_source(project: &Path) {
    std::fs::write(project.join("marker.rs"), "pub fn marker() -> u8 { 1 }\n")
        .expect("write mutate-graph fixture source");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutate_graph_commits_prepared_task_replays_it_and_refuses_other_bodies() {
    let production = production_composition_fixture_with_sources(write_marker_source).await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");

    let (invalid_result, invalid) = mutate(&server, json!({})).await;
    assert_refusal(
        &invalid_result,
        &invalid,
        "work.invalid_request",
        "invalid_request",
        "The Work application request is invalid",
        "never",
        json!([]),
        "adapter",
    );

    let prepared = prepare(&server, create_task_change()).await;
    assert_eq!(prepared["mutation"], "create_task", "{prepared}");
    let command_id = prepared["request"]["mutation"]["command_id"].clone();
    let (applied_result, applied) = mutate(&server, prepared.clone()).await;
    assert_eq!(applied_result.get("isError"), None, "{applied}");
    assert_created_task(&applied, &command_id);

    let (replay_result, replay) = mutate(&server, prepared.clone()).await;
    assert_eq!(replay_result.get("isError"), None, "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(
        replay["event"]["event_id"], applied["event"]["event_id"],
        "{replay}"
    );
    assert_eq!(replay["event"]["command_id"], command_id, "{replay}");
    assert_eq!(
        replay["event"]["payload"]["graph"]["items"][0]["input"]["task_id"], TASK_ID,
        "{replay}"
    );
    assert_eq!(
        replay["verified_graph_version"], applied["verified_graph_version"],
        "{replay}"
    );

    let mut changed_title = prepared.clone();
    changed_title["request"]["item"]["input"]["title"] = json!("A different prepared task");
    let (conflict_result, conflict) = mutate(&server, changed_title).await;
    assert_refusal(
        &conflict_result,
        &conflict,
        "work.graph_idempotency_conflict",
        "conflict",
        "The Work graph request key was reused with different input",
        "never",
        json!(["correct_request"]),
        "application",
    );

    let mut forged_command = prepared.clone();
    forged_command["request"]["mutation"]["command_id"] = json!("command.mcp-mutate-graph.forged");
    let (forged_result, forged) = mutate(&server, forged_command).await;
    assert_refusal(
        &forged_result,
        &forged,
        "work.graph_version_conflict",
        "stale",
        "The Work graph version does not match the request",
        "after_revalidate",
        json!(["refresh"]),
        "application",
    );

    let follow_up = prepare(
        &server,
        json!({
            "change": "add_task",
            "item": task_item("task.mcp-mutate-graph.second", "Second prepared task")
        }),
    )
    .await;
    assert_eq!(follow_up["mutation"], "add_task", "{follow_up}");
    assert_eq!(
        follow_up["request"]["mutation"]["expected_authority"]["authority"], "verified",
        "{follow_up}"
    );
    assert_eq!(
        follow_up["request"]["mutation"]["expected_authority"]["verified_version"]["graph_version"],
        1,
        "{follow_up}"
    );
    let mut stale = follow_up;
    stale["request"]["mutation"]["expected_authority"]["verified_version"]["graph_version"] =
        json!(2);
    let (stale_result, stale_refusal) = mutate(&server, stale).await;
    assert_refusal(
        &stale_result,
        &stale_refusal,
        "work.graph_version_conflict",
        "stale",
        "The Work graph version does not match the request",
        "after_revalidate",
        json!(["refresh"]),
        "application",
    );
}
