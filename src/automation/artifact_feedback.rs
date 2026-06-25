use serde_json::{json, Value};

use super::artifact_refs::sha256_bytes;
use super::run_ledger::AutomationRunLedgerRecord;

pub(super) fn validation_feedback_entries(record: &AutomationRunLedgerRecord) -> Vec<Value> {
    let mut entries = Vec::new();
    for (outcome, items) in [
        ("accepted", accepted_feedback_items(record)),
        ("rejected", artifact_items(record.rejected_ops.as_ref())),
    ] {
        for (index, item) in items.into_iter().enumerate() {
            entries.push(json!({
                "feedback_id": format!("{outcome}:{index}"),
                "outcome": outcome,
                "source": "automation_validation",
                "reason": validation_item_reason(&item),
                "item": item,
            }));
        }
    }
    entries
}

fn accepted_feedback_items(record: &AutomationRunLedgerRecord) -> Vec<Value> {
    let applied = artifact_items(record.applied_ops.as_ref());
    if !applied.is_empty() {
        return applied;
    }
    artifact_items(
        record
            .validation_report
            .as_ref()
            .and_then(|report| report.pointer("/pending_proposals/accepted_facts")),
    )
}

fn artifact_items(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(map)) => {
            let nested = map
                .iter()
                .filter_map(|(key, value)| match value {
                    Value::Array(items) => Some(
                        items
                            .iter()
                            .map(|item| json!({ "field": key, "value": item }))
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>();
            if nested.is_empty() {
                vec![Value::Object(map.clone())]
            } else {
                nested
            }
        }
        Some(value) if !value.is_null() => vec![value.clone()],
        _ => Vec::new(),
    }
}

fn validation_item_reason(item: &Value) -> Value {
    for key in ["rejected_reason", "validation_reason", "reason", "error"] {
        if let Some(reason) = item.get(key).cloned() {
            return reason;
        }
        if let Some(reason) = item.get("value").and_then(|value| value.get(key)).cloned() {
            return reason;
        }
    }
    Value::Null
}

pub(super) fn validation_report_hash(report: Option<&Value>) -> Value {
    let Some(report) = report else {
        return Value::Null;
    };
    match serde_json::to_vec(report) {
        Ok(bytes) => Value::String(sha256_bytes(&bytes)),
        Err(_) => Value::Null,
    }
}

pub(super) fn validation_gate_decision(record: &AutomationRunLedgerRecord) -> &'static str {
    if record.accepted_count == 0 && record.rejected_count == 0 {
        "no_valid_changes"
    } else if record.rejected_count == 0 {
        "passed"
    } else {
        "passed_with_rejections"
    }
}
