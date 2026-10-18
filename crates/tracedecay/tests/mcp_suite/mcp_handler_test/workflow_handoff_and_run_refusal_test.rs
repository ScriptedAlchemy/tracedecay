//! Observable Workflow handoff and run-control refusals through the
//! production MCP server's `tools/call` path.
//!
//! A handoff is issued only for a declared step of an Active definition
//! version and a run admitted from that version; every other scope is the
//! concealed not-found answer and journals nothing, so the identical request
//! succeeds once the run exists. A replayed redemption, a reused secret, a
//! stale run sequence, and a run started from a rejected version are each
//! their own typed state.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};

use super::workflow_activate_definition_test::{
    KNOWN_OPERATION, assert_refusal, call_tool, definition, discover_live_pins,
    problem_record_with_retry, register,
};
use crate::support::production_composition_fixture;

const ACTIVE_ID: &str = "workflow.mcp-handoff.active";
const REJECTED_ID: &str = "workflow.mcp-handoff.rejected";
const STEP: &str = "prepare";
const RUN_ID: &str = "run.mcp-handoff.admitted";
const LATER_RUN_ID: &str = "run.mcp-handoff.later";
const TASK_ID: &str = "task.mcp-handoff";
const ISSUE_SCHEMA: &str = "schema.workflow.handoff_issue.result";
const ISSUE_BINDING: &str = "binding.http.workflow.handoff_issue";
const REDEEM_SCHEMA: &str = "schema.workflow.handoff_redeem.result";
const REDEEM_BINDING: &str = "binding.http.workflow.handoff_redeem";
const START_SCHEMA: &str = "schema.workflow.start_run.result";
const START_BINDING: &str = "binding.http.workflow.start_run";
const PAUSE_SCHEMA: &str = "schema.workflow.pause_run.result";
const PAUSE_BINDING: &str = "binding.http.workflow.pause_run";

fn success_payload(result: &Value, envelope: &Value) -> Value {
    assert_eq!(result.get("isError"), None, "{envelope}");
    assert_eq!(envelope["kind"], json!("success"), "{envelope}");
    envelope
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("success omitted its payload: {envelope}"))
}

/// The admitted actor and scope every Workflow effect receipt names.
fn effect_authority(envelope: &Value) -> (String, Value) {
    fn find(value: &Value) -> Option<(String, Value)> {
        match value {
            Value::Object(object) => {
                if let (Some(Value::String(actor)), Some(scope @ Value::Object(fields))) =
                    (object.get("actor"), object.get("scope"))
                    && ["project_id", "repository_id", "worktree_id"]
                        .iter()
                        .all(|field| fields.get(*field).is_some_and(Value::is_string))
                {
                    return Some((actor.clone(), scope.clone()));
                }
                object.values().find_map(find)
            }
            Value::Array(values) => values.iter().find_map(find),
            _ => None,
        }
    }
    find(envelope).unwrap_or_else(|| panic!("effect omitted its admitted authority: {envelope}"))
}

struct Handoff<'a> {
    actor: &'a str,
    admitted: &'a Value,
}

impl Handoff<'_> {
    fn scope(&self, definition_id: &str, step_id: &str, run_id: &str, thread_id: &str) -> Value {
        json!({
            "project_id": self.admitted["project_id"],
            "repository_id": self.admitted["repository_id"],
            "worktree_id": self.admitted["worktree_id"],
            "definition_id": definition_id,
            "definition_version": 1,
            "step_id": step_id,
            "task_id": TASK_ID,
            "thread_id": thread_id,
            "run_id": run_id,
            "from_actor_id": self.actor,
            "to_actor_id": self.actor
        })
    }

    fn issue(&self, scope: &Value, secret: &str) -> Value {
        json!({
            "scope": scope,
            "secret": secret,
            "frontier": {
                "task_id": TASK_ID,
                "work_version": 1,
                "attempts": [],
                "unknowns": [],
                "blockers": [],
                "legal_actions": [],
                "lineage": {
                    "issued_at": 1_790_000_000_000_000_i64,
                    "issued_by": self.actor,
                    "prior_frontier_digest": null
                }
            }
        })
    }
}

fn concealed() -> Value {
    problem_record_with_retry(
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        "The requested resource was not found or is not authorized",
        Value::Null,
        "application",
        "never",
        Value::Null,
        json!([]),
    )
}

fn diagnostic_problem(
    kind: &str,
    code: &str,
    message: &str,
    retry: &str,
    detail: Value,
    legal_actions: Value,
) -> Value {
    problem_record_with_retry(
        kind,
        code,
        message,
        json!({ "code": code, "message": message }),
        "application",
        retry,
        detail,
        legal_actions,
    )
}

fn provider() -> Value {
    json!({
        "route": {
            "provider_id": "provider.work.codex-cli",
            "route_id": "route.mcp-handoff.workflow"
        },
        "backend": "codex_cli",
        "model": "mcp-handoff",
        "priority": 1
    })
}

async fn start_run(
    server: &tracedecay::mcp::McpServer,
    definition_id: &str,
    run_id: &str,
) -> (Value, Value) {
    call_tool(
        server,
        "tracedecay_workflow_start_run",
        json!({
            "run_id": run_id,
            "definition_id": definition_id,
            "definition_version": 1,
            "provider": provider(),
            "fan_out": null,
            "command_id": format!("command.{run_id}.start")
        }),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_handoff_and_run_refusals_are_typed_states() {
    let production = production_composition_fixture().await;
    let project_id = production
        .harness
        .project_id(&production.project_root)
        .await
        .expect("registered fixture project");
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    let pins = discover_live_pins(&server, &project_id).await;
    for definition_id in [ACTIVE_ID, REJECTED_ID] {
        register(
            &server,
            &definition(
                definition_id,
                &project_id,
                STEP,
                KNOWN_OPERATION,
                &pins.policy,
                &pins.configuration,
                &pins.catalog,
            ),
        )
        .await;
    }
    let lifecycle = |tool: &'static str, definition_id: &'static str, expected_revision: u64| {
        call_tool(
            &server,
            tool,
            json!({
                "definition_id": definition_id,
                "definition_version": 1,
                "expected_revision": expected_revision
            }),
        )
    };
    let (activated_result, activated) =
        lifecycle("tracedecay_workflow_activate_definition", ACTIVE_ID, 1).await;
    success_payload(&activated_result, &activated);
    let (actor, admitted) = effect_authority(&activated);
    let (rejected_result, rejected) =
        lifecycle("tracedecay_workflow_reject_definition", REJECTED_ID, 1).await;
    success_payload(&rejected_result, &rejected);

    // A run starts only from an active version; a rejected one is a conflict
    // that names its state.
    let (not_active_result, not_active) = start_run(&server, REJECTED_ID, "run.mcp-rejected").await;
    assert_refusal(
        &not_active_result,
        &not_active,
        START_SCHEMA,
        Some(START_BINDING),
        diagnostic_problem(
            "conflict",
            "workflow.definition.not_active",
            "definition_version 1 is rejected; runs start only from an active definition version",
            "never",
            Value::Null,
            json!(["correct_request"]),
        ),
    );
    let (started_result, started) = start_run(&server, ACTIVE_ID, RUN_ID).await;
    let run = success_payload(&started_result, &started);
    let sequence = run["sequence"]
        .as_u64()
        .unwrap_or_else(|| panic!("run omitted its sequence: {run}"));

    // A stale run compare-and-swap names the sequence the caller sent and the
    // one the run holds.
    let stale_sequence = sequence + 5;
    let (stale_result, stale) = call_tool(
        &server,
        "tracedecay_workflow_pause_run",
        json!({
            "run_id": RUN_ID,
            "expected_sequence": stale_sequence,
            "command_id": "command.mcp-handoff.pause.stale"
        }),
    )
    .await;
    let stale_message = format!(
        "expected_sequence {stale_sequence} does not match the current value {sequence}; refresh and resend with expected_sequence {sequence}."
    );
    assert_refusal(
        &stale_result,
        &stale,
        PAUSE_SCHEMA,
        Some(PAUSE_BINDING),
        diagnostic_problem(
            "stale",
            "application.precondition-stale",
            &stale_message,
            "after_revalidate",
            json!({
                "kind": "stale_precondition",
                "field": "expected_sequence",
                "requested": stale_sequence,
                "current": sequence
            }),
            json!(["refresh"]),
        ),
    );

    let handoff = Handoff {
        actor: &actor,
        admitted: &admitted,
    };
    let secret = "s".repeat(48);
    for (label, scope) in [
        (
            "unregistered definition",
            handoff.scope("workflow.mcp-handoff.absent", STEP, RUN_ID, "thread.absent"),
        ),
        (
            "step the version does not declare",
            handoff.scope(ACTIVE_ID, "step.undeclared", RUN_ID, "thread.undeclared"),
        ),
        (
            "definition version that is not active",
            handoff.scope(REJECTED_ID, STEP, RUN_ID, "thread.rejected"),
        ),
        (
            "run of another definition",
            handoff.scope(ACTIVE_ID, STEP, "run.mcp-rejected", "thread.foreign-run"),
        ),
    ] {
        let (result, envelope) = call_tool(
            &server,
            "tracedecay_workflow_handoff_issue",
            handoff.issue(&scope, &secret),
        )
        .await;
        assert_eq!(result["isError"], json!(true), "{label}: {envelope}");
        assert_refusal(&result, &envelope, ISSUE_SCHEMA, None, concealed());
    }

    // A run that does not exist yet is refused without a journal entry: the
    // byte-identical request is granted once the run is admitted.
    let later_scope = handoff.scope(ACTIVE_ID, STEP, LATER_RUN_ID, "thread.later");
    let later_request = handoff.issue(&later_scope, &secret);
    let (before_result, before) = call_tool(
        &server,
        "tracedecay_workflow_handoff_issue",
        later_request.clone(),
    )
    .await;
    assert_refusal(&before_result, &before, ISSUE_SCHEMA, None, concealed());
    let (later_started_result, later_started) = start_run(&server, ACTIVE_ID, LATER_RUN_ID).await;
    success_payload(&later_started_result, &later_started);
    let (granted_result, granted) =
        call_tool(&server, "tracedecay_workflow_handoff_issue", later_request).await;
    let grant = success_payload(&granted_result, &granted);
    assert_eq!(grant["scope"], later_scope, "{granted}");

    // Reusing the secret for another scope collides with the issued grant.
    let (reused_result, reused) = call_tool(
        &server,
        "tracedecay_workflow_handoff_issue",
        handoff.issue(
            &handoff.scope(ACTIVE_ID, STEP, RUN_ID, "thread.reused"),
            &secret,
        ),
    )
    .await;
    assert_refusal(
        &reused_result,
        &reused,
        ISSUE_SCHEMA,
        Some(ISSUE_BINDING),
        diagnostic_problem(
            "conflict",
            "workflow.handoff.token_conflict",
            "another handoff already uses this secret; issue the handoff with a new secret",
            "never",
            Value::Null,
            json!(["correct_request"]),
        ),
    );

    let redeem = json!({ "secret": secret, "expected_scope": later_scope });
    let (redeemed_result, redeemed) = call_tool(
        &server,
        "tracedecay_workflow_handoff_redeem",
        redeem.clone(),
    )
    .await;
    let receipt = success_payload(&redeemed_result, &redeemed);
    assert_eq!(
        receipt["frontier_digest"], grant["frontier_digest"],
        "{redeemed}"
    );
    let (replayed_result, replayed) =
        call_tool(&server, "tracedecay_workflow_handoff_redeem", redeem).await;
    assert_refusal(
        &replayed_result,
        &replayed,
        REDEEM_SCHEMA,
        Some(REDEEM_BINDING),
        diagnostic_problem(
            "conflict",
            "workflow.handoff.replayed",
            "this handoff was already redeemed; ask its issuer for a new handoff",
            "never",
            Value::Null,
            json!(["reauthorize"]),
        ),
    );
}
