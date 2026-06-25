use serde_json::{json, Value};

use super::artifact_policy::TaskArtifactPolicy;
use super::run_ledger::AutomationRunLedgerRecord;

pub(super) fn optimizer_ranked_changes(
    policy: TaskArtifactPolicy,
    record: &AutomationRunLedgerRecord,
    improvement_decision: &str,
) -> Vec<Value> {
    let priority = if record.rejected_count > 0 {
        "high"
    } else if record.accepted_count > 0 {
        "medium"
    } else {
        "low"
    };
    vec![json!({
        "rank": 1,
        "priority": priority,
        "action": policy.optimizer_action,
        "reason": optimizer_diagnosis_summary(record),
        "validation_required": [
            "inspect generated eval definitions",
            "run listed handoff tests",
            "preserve dashboard approval before mutation",
        ],
        "ready_for_codex_handoff": !is_blocked_improvement_decision(improvement_decision),
    })]
}

pub(super) fn is_blocked_improvement_decision(improvement_decision: &str) -> bool {
    improvement_decision.starts_with("blocked_")
}

pub(super) fn codex_handoff_status(improvement_decision: &str) -> &'static str {
    if is_blocked_improvement_decision(improvement_decision) {
        "blocked"
    } else {
        "ready_for_review"
    }
}

pub(super) fn optimizer_blockers(improvement_decision: &str, eval_count: usize) -> Vec<Value> {
    match improvement_decision {
        "blocked_pending_feedback_or_evals" => vec![json!({
            "id": "pending_feedback_or_evals",
            "reason": if eval_count == 0 {
                "No validation examples were available to generate regression evals."
            } else {
                "Validation did not produce a reviewable improvement signal."
            },
            "required_action": "collect feedback or generated eval examples before optimizer handoff",
        })],
        "blocked_pending_eval_run" => vec![json!({
            "id": "pending_eval_run",
            "reason": "Generated regression evals exist but have not been replayed.",
            "required_action": "run the generated eval replay commands before optimizer or Codex handoff",
        })],
        "blocked_failed_eval_replay" => vec![json!({
            "id": "failed_eval_replay",
            "reason": "Generated regression eval definitions did not match validation results.",
            "required_action": "fix generated eval definitions before optimizer or Codex handoff",
        })],
        _ => Vec::new(),
    }
}

pub(super) fn optimizer_diagnosis_summary(record: &AutomationRunLedgerRecord) -> String {
    format!(
        "Validation accepted {} item(s), rejected {} item(s), and reviewed {} item(s).",
        record.accepted_count, record.rejected_count, record.reviewed_count
    )
}

pub(super) fn optimizer_recommendations(record: &AutomationRunLedgerRecord) -> Vec<Value> {
    if record.rejected_count > 0 {
        vec![json!({
            "id": "review_rejections",
            "priority": "high",
            "action": "Review rejected automation outputs before applying optimizer recommendations.",
            "rationale": "Rejected items indicate the backend proposed at least one invalid change.",
            "evidence_refs": ["feedback:model:rejected", "generated_evals:rejected"],
        })]
    } else if record.accepted_count > 0 {
        vec![json!({
            "id": "review_accepted_changes",
            "priority": "medium",
            "action": "Review accepted automation outputs and keep the generated evals with the run artifact.",
            "rationale": "Accepted items provide regression examples for future backend behavior.",
            "evidence_refs": ["feedback:model:accepted", "generated_evals:accepted"],
        })]
    } else {
        vec![json!({
            "id": "collect_more_evidence",
            "priority": "low",
            "action": "Collect more evidence before tuning the automation task.",
            "rationale": "The validation report produced no accepted or rejected changes.",
            "evidence_refs": ["validation_gate:task_validation"],
        })]
    }
}
