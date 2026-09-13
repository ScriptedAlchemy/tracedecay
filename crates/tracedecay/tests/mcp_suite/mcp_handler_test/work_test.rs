#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

async fn call(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    let decoded: Value = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"));
    decoded
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or(decoded)
}

fn now_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_micros(),
    )
    .expect("current time fits UtcMicros")
}

/// A fresh provider attempt has no session association yet. The public Work
/// reads must still project it from the authority that committed the attempt.
#[tokio::test]
async fn work_attempt_consumers_read_the_public_start_attempt_effect() {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    let occurred_at = now_micros();
    let selection = json!({ "selection": "profile_owned_no_git" });

    let prepared_create = call(
        &server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "create_task",
                "initiative": {
                    "id": "initiative.mcp-attempt-read",
                    "title": "MCP attempt read initiative",
                    "created_at": occurred_at
                },
                "plan": {
                    "id": "plan.mcp-attempt-read",
                    "initiative_id": "initiative.mcp-attempt-read",
                    "title": "MCP attempt read plan",
                    "created_at": occurred_at
                },
                "milestone": {
                    "id": "milestone.mcp-attempt-read",
                    "plan_id": "plan.mcp-attempt-read",
                    "title": "MCP attempt read milestone",
                    "created_at": occurred_at
                },
                "item": {
                    "input": {
                        "task_id": "task.mcp-attempt-read",
                        "hierarchy": {
                            "initiative_id": "initiative.mcp-attempt-read",
                            "plan_id": "plan.mcp-attempt-read",
                            "milestone_id": "milestone.mcp-attempt-read"
                        },
                        "title": "Read a freshly started attempt",
                        "dependencies": [],
                        "informational_relations": [],
                        "causal_candidates": [],
                        "acceptance_criteria": [],
                        "effort": 1,
                        "scheduled_at": null,
                        "deadline": null,
                        "created_at": occurred_at,
                        "updated_at": occurred_at
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
                }
            },
            "evidence": []
        }),
    )
    .await;
    let created = call(
        &server,
        "tracedecay_work_create",
        prepared_create["request"].clone(),
    )
    .await;
    assert_eq!(created["replayed"], false, "{created}");

    let generated = call(
        &server,
        "tracedecay_work_generate_proposal",
        json!({
            "selection": selection,
            "task_id": "task.mcp-attempt-read",
            "proposal_id": "proposal.mcp-attempt-read",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert!(generated["proposal"].is_object(), "{generated}");
    let initial_version = generated["verified_graph_version"].clone();
    let prepared_accept = call(
        &server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "decide_proposal",
                "proposal": generated["proposal"].clone(),
                "disposition": "accepted"
            },
            "evidence": []
        }),
    )
    .await;
    assert_eq!(
        prepared_accept["mutation"], "decide_proposal",
        "{prepared_accept}"
    );
    let accepted = call(
        &server,
        "tracedecay_work_accept_proposal",
        prepared_accept["request"].clone(),
    )
    .await;
    let accepted_version = accepted["verified_graph_version"].clone();
    assert_eq!(accepted["replayed"], false, "{accepted}");
    let prepared_admit = call(
        &server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "admit_execution",
                "task_id": "task.mcp-attempt-read",
                "based_on_version": accepted_version["graph_version"]
            },
            "evidence": []
        }),
    )
    .await;
    let admitted = call(
        &server,
        "tracedecay_work_admit_execution",
        prepared_admit["request"].clone(),
    )
    .await;
    let admitted_version = admitted["mutation"]["verified_graph_version"].clone();
    assert_eq!(admitted["mutation"]["replayed"], false, "{admitted}");

    let placement_request = json!({
        "task_id": "task.mcp-attempt-read",
        "run_id": "run.mcp-attempt-read",
        "target": {
            "kind": "clean_in_place",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": true
        },
        "occurred_at": now_micros()
    });
    let preflight = call(
        &server,
        "tracedecay_work_placement_preflight",
        placement_request.clone(),
    )
    .await;
    assert_eq!(preflight["blockers"], json!([]), "{preflight}");
    let placed = call(
        &server,
        "tracedecay_work_admit_placement",
        placement_request,
    )
    .await;
    assert_eq!(
        placed["identity"]["run_id"], "run.mcp-attempt-read",
        "{placed}"
    );

    let commit = Command::new(crate::common::git_program())
        .args(["rev-parse", "HEAD"])
        .current_dir(&production.project_root)
        .output()
        .expect("read fixture commit");
    assert!(commit.status.success(), "git rev-parse must succeed");
    let commit = String::from_utf8(commit.stdout)
        .expect("commit is UTF-8")
        .trim()
        .to_owned();
    let attempt_at = now_micros();
    let start_request = json!({
        "task_id": "task.mcp-attempt-read",
        "run_id": "run.mcp-attempt-read",
        "attempt_id": "attempt.mcp-attempt-read",
        "operation": "operation.work.start_attempt",
        "worktree_root": production.project_root,
        "commit": commit,
        "instructions": "Observe the fixture only.",
        "effect_state": "observational",
        "occurred_at": attempt_at,
        "execution_snapshot": admitted["execution_snapshot"].clone()
    });
    let _: tracedecay_contracts::StartWorkAttemptCommand =
        serde_json::from_value(start_request.clone()).expect("valid start-attempt request");
    let mut second_start_request = start_request.clone();
    second_start_request["attempt_id"] = json!("attempt.mcp-attempt-read.second");
    let started = call(&server, "tracedecay_work_start_attempt", start_request).await;
    assert_eq!(
        started["identity"]["attempt_id"], "attempt.mcp-attempt-read",
        "{started}"
    );
    let second_started = call(
        &server,
        "tracedecay_work_start_attempt",
        second_start_request,
    )
    .await;
    assert_eq!(
        second_started["identity"]["attempt_id"], "attempt.mcp-attempt-read.second",
        "{second_started}"
    );

    let attempts = call(
        &server,
        "tracedecay_work_list_attempts",
        json!({ "page_size": 50 }),
    )
    .await;
    assert_eq!(attempts["state"], "listed", "{attempts}");
    let listed_attempt = attempts["attempts"]
        .as_array()
        .and_then(|attempts| {
            attempts
                .iter()
                .find(|attempt| attempt["identity"] == started["identity"])
        })
        .expect("started attempt must be listed");
    assert_eq!(listed_attempt["state"], "failed", "{attempts}");
    assert_eq!(
        listed_attempt["terminal"]["outcome"], "failed",
        "{attempts}"
    );

    let history = call(
        &server,
        "tracedecay_work_execution_history",
        json!({ "page_size": 50 }),
    )
    .await;
    assert_eq!(history["state"], "listed", "{history}");
    assert!(
        history["observed_order"]
            .as_array()
            .is_some_and(|events| events
                .iter()
                .any(|event| event["identity"] == started["identity"])),
        "{history}"
    );

    let topology = call(
        &server,
        "tracedecay_work_topology",
        json!({ "page_size": 50 }),
    )
    .await;
    assert_eq!(topology["state"], "view", "{topology}");
    assert!(
        topology["execution_placement"]["lanes"]
            .as_array()
            .is_some_and(|lanes| lanes.iter().any(|lane| {
                lane["task_id"] == "task.mcp-attempt-read"
                    && lane["run_id"] == "run.mcp-attempt-read"
            })),
        "{topology}"
    );

    let compared = call(
        &server,
        "tracedecay_work_compare_proposal",
        json!({
            "selection": selection,
            "task_id": "task.mcp-attempt-read",
            "old_version": initial_version,
            "new_version": admitted_version,
            "observed_at": now_micros()
        }),
    )
    .await;
    assert_eq!(compared["task_id"], "task.mcp-attempt-read", "{compared}");

    let duplicate = call(
        &server,
        "tracedecay_work_prepare_duplicate_adjudication",
        json!({
            "first_attempt": started["identity"],
            "second_attempt": second_started["identity"],
            "verdict": "not_duplicate",
            "reason": "distinct fixture attempts",
            "quantities": {
                "wall_micros": null,
                "token_count": null,
                "cost_micros": null,
                "test_count": null,
                "effect_count": null,
                "evidence": "owner_receipt",
                "effect_outcome": "not_applicable",
                "coverage": "known"
            }
        }),
    )
    .await;
    assert_eq!(
        duplicate["first_attempt"], started["identity"],
        "{duplicate}"
    );
    assert_eq!(
        duplicate["second_attempt"], second_started["identity"],
        "{duplicate}"
    );
    assert!(
        duplicate["evidence"]["work_generation"].is_string(),
        "{duplicate}"
    );
    assert!(
        duplicate["evidence"]["topology_generation"].is_string(),
        "{duplicate}"
    );
    let adjudicated = call(
        &server,
        "tracedecay_work_adjudicate_duplicate",
        duplicate.clone(),
    )
    .await;
    assert_eq!(
        adjudicated["receipt"]["command"], duplicate,
        "{adjudicated}"
    );

    let experience = call(
        &server,
        "tracedecay_work_experience",
        json!({
            "selection": selection,
            "task_id": "task.mcp-attempt-read",
            "verified_version": admitted_version,
            "expertise_categories": ["testing"],
            "evidence_not_before": occurred_at,
            "observed_at": now_micros(),
            "limit": 10
        }),
    )
    .await;
    assert_ne!(
        experience.pointer("/value/problem/code"),
        Some(&json!("not_found_or_not_authorized")),
        "experience must not require a task-session association: {experience}"
    );

    let placement = call(
        &server,
        "tracedecay_work_placement_status",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read"
        }),
    )
    .await;
    assert_ne!(placement["state"], "absent", "{placement}");

    let cancelled = call(
        &server,
        "tracedecay_work_cancel_attempt",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read",
            "attempt_id": "attempt.mcp-attempt-read",
            "request_id": "cancel.mcp-attempt-read",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert_ne!(
        cancelled.pointer("/value/problem/code"),
        Some(&json!("not_found_or_not_authorized")),
        "the attempt consumer must find the fresh attempt even if its provider already settled: {cancelled}"
    );

    let paused = call(
        &server,
        "tracedecay_work_pause_run",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read",
            "reason": "operator_request",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert_eq!(paused["state"], "paused", "{paused}");
    let resumed = call(
        &server,
        "tracedecay_work_resume_run",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read",
            "reason": "operator_request",
            "expected_authority_version": paused["authority"],
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert_eq!(resumed["state"], "running", "{resumed}");
}
