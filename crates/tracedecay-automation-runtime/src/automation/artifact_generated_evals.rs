use serde_json::{Value, json};

use super::artifact_feedback::validation_feedback_entries;
use super::backend::{AgentTaskKind, task_key};
use super::run_ledger::AutomationRunLedgerRecord;
use tracedecay_automation::artifact_policy::TaskArtifactPolicy;

/// No behavioral validator or harness ever executes generated examples, so
/// independent evaluation is never reported as anything but not run.
pub(super) const INDEPENDENT_EVALUATION_STATUS: &str = "not_run";

pub(super) const INDEPENDENT_EVALUATION_REASON: &str =
    "no behavioral validator or harness executed these examples";

pub(super) fn generated_eval_definitions(
    record: &AutomationRunLedgerRecord,
    task: AgentTaskKind,
    policy: TaskArtifactPolicy,
) -> Vec<Value> {
    let task_key = task_key(task);
    let harness_command = policy.handoff_test();
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
                    "type": "bazel_test_filter",
                    "commands": [harness_command],
                    "status": INDEPENDENT_EVALUATION_STATUS,
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

/// Checks that the generated examples agree with the counts and labels the run
/// recorded. This is record integrity, not behavioral evaluation: no validator,
/// model, or harness executes the examples.
pub(super) fn recorded_validation_integrity(
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

pub(super) fn generated_evals_status(eval_count: usize) -> &'static str {
    if eval_count == 0 {
        "blocked_no_generated_evals"
    } else {
        INDEPENDENT_EVALUATION_STATUS
    }
}

pub(super) fn improvement_gate_decision(
    record: &AutomationRunLedgerRecord,
    eval_count: usize,
    record_integrity_status: &str,
) -> &'static str {
    if eval_count == 0 || record.reviewed_count == 0 {
        "blocked_pending_feedback_or_evals"
    } else if record_integrity_status != "passed" {
        "blocked_failed_record_integrity"
    } else if record.rejected_count > 0 {
        "ready_for_optimizer_review"
    } else {
        "ready_for_handoff"
    }
}
