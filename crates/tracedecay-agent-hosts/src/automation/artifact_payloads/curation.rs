use serde_json::{Value, json};
use tracedecay_store::{
    FactCommitReceipt, ProjectMemoryFactCurationOperationEffectV1,
    ProjectMemoryFactCurationReceiptV1,
};

use super::super::artifact_feedback::validation_report_hash;
use super::super::run_ledger::AutomationRunLedgerRecord;

pub(super) fn memory_curation_trace_summary(record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "status": record.status,
        "reviewed_count": record.reviewed_count,
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
        "operation_receipts": receipt_summaries(record.applied_ops.as_ref()),
        "applied_ops_hash": validation_report_hash(record.applied_ops.as_ref()),
        "rejected_ops_hash": validation_report_hash(record.rejected_ops.as_ref()),
        "validation_report_hash": validation_report_hash(record.validation_report.as_ref()),
    })
}

fn receipt_summaries(applied_ops: Option<&Value>) -> Vec<Value> {
    applied_ops
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let receipt = serde_json::from_value::<ProjectMemoryFactCurationReceiptV1>(
                entry.get("receipt")?.clone(),
            )
            .ok()?;
            let status = match entry.get("status").and_then(Value::as_str) {
                Some("applied") => "applied",
                Some("failed_after_partial_effects") => "failed_after_partial_effects",
                _ => "unknown",
            };
            Some(json!({
                "status": status,
                "operation_id": receipt.operation_id(),
                "automation_run_id": receipt.automation_run_id(),
                "input_digest": receipt.input_digest(),
                "accepted_operations": receipt.accepted_operations(),
                "facts_added": receipt.facts_added(),
                "facts_updated": receipt.facts_updated(),
                "facts_merged": receipt.facts_merged(),
                "facts_removed": receipt.facts_removed(),
                "normalized_tags": receipt.normalized_tags(),
                "facts_linked": receipt.facts_linked(),
                "replay_fact_id": receipt.replay_fact_id(),
                "replay_event_id": receipt.replay_event_id(),
                "changed_fact_ids": receipt.changed_facts().iter()
                    .map(|fact| fact.fact_id())
                    .collect::<Vec<_>>(),
                "effects": receipt.operation_effects().iter()
                    .map(effect_summary)
                    .collect::<Vec<_>>(),
            }))
        })
        .collect()
}

fn effect_summary(effect: &ProjectMemoryFactCurationOperationEffectV1) -> Value {
    match effect {
        ProjectMemoryFactCurationOperationEffectV1::Add {
            fact,
            disposition,
            closest_fact,
            similarity_millionths,
            commit,
        } => json!({
            "kind": "add",
            "fact_id": fact.fact_id(),
            "disposition": disposition,
            "closest_fact_id": closest_fact.as_ref().map(|fact| fact.fact_id()),
            "similarity_millionths": similarity_millionths,
            "commit": commit.as_ref().map(commit_summary),
        }),
        ProjectMemoryFactCurationOperationEffectV1::Update {
            fact,
            trust_delta_millionths,
            commit,
        } => json!({
            "kind": "update",
            "fact_id": fact.fact_id(),
            "trust_delta_millionths": trust_delta_millionths,
            "commit": commit_summary(commit),
        }),
        ProjectMemoryFactCurationOperationEffectV1::Merge { outcome } => json!({
            "kind": "merge",
            "operation_id": outcome.operation_id(),
            "winner_fact_id": outcome.winner().fact_id(),
            "content_updated": outcome.content_updated(),
            "deleted_loser_fact_ids": outcome.deleted_losers().iter()
                .map(|fact| fact.fact_id())
                .collect::<Vec<_>>(),
            "commits": outcome.commit_receipts().iter()
                .map(commit_summary)
                .collect::<Vec<_>>(),
        }),
        ProjectMemoryFactCurationOperationEffectV1::Remove {
            target,
            disposition,
            remaining_fact_count,
            commit,
        } => json!({
            "kind": "remove",
            "target_fact_id": target.fact_id(),
            "disposition": disposition,
            "remaining_fact_count": remaining_fact_count,
            "commit": commit.as_ref().map(commit_summary),
        }),
        ProjectMemoryFactCurationOperationEffectV1::NormalizeTags { fact, commit } => json!({
            "kind": "normalize_tags",
            "fact_id": fact.fact_id(),
            "commit": commit_summary(commit),
        }),
        ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
            relation,
            disposition,
            commit,
        } => json!({
            "kind": "link_facts",
            "source_fact_id": relation.source_fact_id(),
            "target_fact_id": relation.target_fact_id(),
            "relation": relation.relation(),
            "evidence_fact_ids": relation.evidence_fact_ids(),
            "confidence": relation.confidence(),
            "disposition": disposition,
            "commit": commit.as_ref().map(commit_summary),
        }),
    }
}

fn commit_summary(receipt: &FactCommitReceipt) -> Value {
    json!({
        "fact_id": receipt.fact_id(),
        "committed_event_ids": receipt.committed_event_ids(),
        "last_event_id": receipt.last_event_id(),
        "active_assertion_id": receipt.active_assertion_id(),
        "committed_state_digest": receipt.committed_state_digest(),
    })
}
