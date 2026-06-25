use serde_json::{json, Value};

use super::artifact_feedback::validation_feedback_entries;
use super::artifact_policy::TaskArtifactPolicy;
use super::backend::{task_key, AgentTaskKind};
use super::run_ledger::AutomationRunLedgerRecord;

pub(super) fn generated_eval_definitions(
    record: &AutomationRunLedgerRecord,
    task: AgentTaskKind,
    policy: TaskArtifactPolicy,
) -> Vec<Value> {
    let task_key = task_key(task);
    let replay_commands = policy.handoff_tests();
    validation_feedback_entries(record)
        .into_iter()
        .filter_map(|entry| {
            let feedback_id = entry.get("feedback_id")?.as_str()?.to_string();
            let outcome = entry.get("outcome")?.as_str()?.to_string();
            Some(json!({
                "schema_version": 1,
                "eval_id": format!("{task_key}:{feedback_id}"),
                "kind": "automation_validation_regression",
                "source_feedback_ref": feedback_id,
                "source_feedback": {
                    "artifact_kind": "feedback",
                    "feedback_id": feedback_id,
                    "outcome": outcome,
                },
                "expected_outcome": outcome,
                "harness": {
                    "type": "cargo_test_filter",
                    "commands": replay_commands.clone(),
                    "status": "not_run",
                },
                "input": {
                    "task": task_key,
                    "evidence_hash": record.evidence_hash,
                    "input_hash": record.input_hash,
                },
                "fixture": {
                    "candidate": entry.get("item").cloned().unwrap_or(Value::Null),
                    "reason": entry.get("reason").cloned().unwrap_or(Value::Null),
                },
                "expected": {
                    "item": entry.get("item").cloned().unwrap_or(Value::Null),
                    "reason": entry.get("reason").cloned().unwrap_or(Value::Null),
                },
                "assertions": [{
                    "type": "outcome_equals",
                    "expected": outcome,
                }],
            }))
        })
        .collect()
}

pub(super) fn generated_eval_replay_results(
    eval_definitions: &[Value],
    record: &AutomationRunLedgerRecord,
) -> (&'static str, Vec<Value>) {
    if eval_definitions.is_empty() {
        return (
            "not_run",
            vec![json!({
                "check": "has_generated_evals",
                "status": "blocked",
                "reason": "no validation examples were available",
            })],
        );
    }

    let accepted = eval_definitions
        .iter()
        .filter(|definition| {
            definition.get("expected_outcome").and_then(Value::as_str) == Some("accepted")
        })
        .count();
    let rejected = eval_definitions
        .iter()
        .filter(|definition| {
            definition.get("expected_outcome").and_then(Value::as_str) == Some("rejected")
        })
        .count();
    let outcomes_supported = eval_definitions.iter().all(|definition| {
        matches!(
            definition.get("expected_outcome").and_then(Value::as_str),
            Some("accepted" | "rejected")
        )
    });
    let assertions_match = eval_definitions.iter().all(|definition| {
        let expected = definition.get("expected_outcome").and_then(Value::as_str);
        definition
            .get("assertions")
            .and_then(Value::as_array)
            .is_some_and(|assertions| {
                assertions.iter().any(|assertion| {
                    assertion.get("type").and_then(Value::as_str) == Some("outcome_equals")
                        && assertion.get("expected").and_then(Value::as_str) == expected
                })
            })
    });

    let checks = vec![
        json!({
            "check": "accepted_count_matches",
            "expected": record.accepted_count,
            "actual": accepted,
            "status": if accepted == record.accepted_count { "passed" } else { "failed" },
        }),
        json!({
            "check": "rejected_count_matches",
            "expected": record.rejected_count,
            "actual": rejected,
            "status": if rejected == record.rejected_count { "passed" } else { "failed" },
        }),
        json!({
            "check": "outcomes_supported",
            "status": if outcomes_supported { "passed" } else { "failed" },
        }),
        json!({
            "check": "assertions_match_expected_outcome",
            "status": if assertions_match { "passed" } else { "failed" },
        }),
    ];
    let status = if accepted == record.accepted_count
        && rejected == record.rejected_count
        && outcomes_supported
        && assertions_match
    {
        "passed"
    } else {
        "failed"
    };
    (status, checks)
}

pub(super) fn generated_evals_status(eval_count: usize, eval_runner_status: &str) -> &'static str {
    if eval_count == 0 {
        "blocked_no_generated_evals"
    } else if eval_runner_status == "passed" {
        "passed"
    } else if eval_runner_status == "failed" {
        "failed_eval_replay"
    } else {
        "pending_eval_run"
    }
}

pub(super) fn improvement_gate_decision(
    record: &AutomationRunLedgerRecord,
    eval_count: usize,
    eval_runner_status: &str,
) -> &'static str {
    if eval_count == 0 || record.reviewed_count == 0 {
        "blocked_pending_feedback_or_evals"
    } else if eval_runner_status == "failed" {
        "blocked_failed_eval_replay"
    } else if eval_runner_status != "passed" {
        "blocked_pending_eval_run"
    } else if record.rejected_count > 0 {
        "ready_for_optimizer_review"
    } else {
        "ready_for_handoff"
    }
}
