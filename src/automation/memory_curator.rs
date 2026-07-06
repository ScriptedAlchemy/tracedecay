use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::apply_policy::{MemoryApplyDecision, MemoryApplyPolicy};
use super::artifacts::sha256_json;
use super::backend::{AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse};
use super::config::AutomationConfig;
use super::lifecycle::{failed_backend_fallback_report, AgentTaskRunContext, SchedulerGate};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::dashboard::memory_curate::{
    run_memory_curate, MemoryCurateOptions, CURATION_DEFAULT_MAX_CLUSTERS,
    CURATION_DEFAULT_MIN_CONFIDENCE,
};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCuratorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_max_clusters")]
    pub max_clusters: usize,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

impl Default for MemoryCuratorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            max_clusters: default_max_clusters(),
            min_confidence: default_min_confidence(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCuratorAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

pub async fn run_memory_curator_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
) -> Result<MemoryCuratorAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        cg.store_layout().dashboard_root.clone(),
        cg.store_layout().sessions_db_path.clone(),
        options.run_id.clone(),
        "memory_curator",
        options.trigger,
        config,
        AgentTaskKind::MemoryCurator,
    );
    let max_clusters = options.max_clusters.clamp(1, 50);
    let min_confidence = options.min_confidence.clamp(0.0, 1.0);

    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_run(&run, reason, None).await;
        }
    };

    let review_report = run_memory_curate(
        cg,
        &MemoryCurateOptions {
            apply: false,
            llm: true,
            llm_ops: None,
            max_clusters,
            min_confidence,
        },
    )
    .await?;
    let llm_review =
        review_report
            .get("llm_review")
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: "curation report did not include llm_review".to_string(),
            })?;
    let evidence_hash = Some(sha256_json(&llm_review));
    if llm_review.get("status").and_then(Value::as_str) != Some("needs_llm_review") {
        return skipped_run(&run, "nothing_to_review", evidence_hash).await;
    }

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::MemoryCurator,
        build_memory_curator_prompt(&llm_review),
        evidence_hash.clone(),
        json!({
            "llm_review": llm_review,
            "apply": false,
            "min_confidence": min_confidence,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());

    let response = match backend.run_task(&request) {
        Ok(response) => response,
        Err(err) => {
            let record = finalizer
                .append_backend_fallback_record(evidence_hash, err.to_string())
                .await?;
            return Ok(MemoryCuratorAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
            });
        }
    };
    let proposed_ops = finalizer
        .response_output_json(&response, evidence_hash.clone())
        .await?;

    let dry_run_report = match run_memory_curate(
        cg,
        &MemoryCurateOptions {
            apply: false,
            llm: false,
            llm_ops: Some(proposed_ops.clone()),
            max_clusters,
            min_confidence,
        },
    )
    .await
    {
        Ok(report) => report,
        Err(err) => {
            finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops),
                    err.to_string(),
                )
                .await?;
            return Err(err);
        }
    };

    let accepted_ops = dry_run_report.pointer("/llm_apply/ops").cloned();
    let dry_run_apply_policy = memory_curation_apply_policy(config, accepted_ops.as_ref(), None);
    let should_apply = dry_run_apply_policy
        .get("mutates_store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let validated_report = if should_apply {
        let mut applied_report = match run_memory_curate(
            cg,
            &MemoryCurateOptions {
                apply: true,
                llm: false,
                llm_ops: Some(proposed_ops.clone()),
                max_clusters,
                min_confidence,
            },
        )
        .await
        {
            Ok(report) => report,
            Err(err) => {
                finalizer
                    .append_failed_record(
                        response.model.clone(),
                        evidence_hash,
                        Some(proposed_ops),
                        err.to_string(),
                    )
                    .await?;
                return Err(err);
            }
        };
        let applied_ops = applied_report.pointer("/llm_apply/ops").cloned();
        let applied_count = applied_report
            .pointer("/llm_apply/applied")
            .and_then(value_as_usize)
            .unwrap_or(0);
        let apply_policy =
            memory_curation_apply_policy(config, applied_ops.as_ref(), Some(applied_count));
        annotate_memory_curation_report(&mut applied_report, apply_policy.clone());
        if applied_count > 0 {
            if let Ok(project_db) = cg.open_project_store_db().await {
                crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
                    project_db.conn(),
                    &cg.store_layout().project_root,
                )
                .await;
            }
        }
        applied_report
    } else {
        let mut report = dry_run_report;
        annotate_memory_curation_report(&mut report, dry_run_apply_policy.clone());
        report
    };

    let validation_report = validated_report.get("llm_apply").cloned();
    let applied_ops = validated_report.pointer("/llm_apply/ops").cloned();
    let rejected_ops = validated_report.pointer("/llm_apply/rejected_ops").cloned();
    let accepted_count = validated_report
        .pointer("/llm_apply/ops")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let rejected_count = validated_report
        .pointer("/llm_apply/rejected_ops")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut record = finalizer.success_record(
        &response,
        evidence_hash,
        Some(proposed_ops),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = applied_ops;
    record.rejected_ops = rejected_ops;
    record.validation_report = validation_report;
    let record = finalizer
        .append_success_record(&request, &response, record)
        .await?;

    Ok(MemoryCuratorAutomationRun {
        run_id: run.run_id,
        report: validated_report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

async fn skipped_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<MemoryCuratorAutomationRun> {
    let (report, record) = run.skipped_parts(evidence_hash, reason, None).await?;
    Ok(MemoryCuratorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

fn build_memory_curator_prompt(llm_review: &Value) -> String {
    let messages = llm_review
        .get("messages")
        .cloned()
        .unwrap_or_else(|| json!([]));
    format!(
        "Run TraceDecay memory curation review. Return only the strict JSON object requested by these messages:\n{}",
        serde_json::to_string_pretty(&messages).unwrap_or_else(|_| "[]".to_string())
    )
}

fn memory_curation_apply_policy(
    config: &AutomationConfig,
    accepted_ops: Option<&Value>,
    applied_count: Option<usize>,
) -> Value {
    let ops = accepted_ops
        .and_then(Value::as_array)
        .map_or_else(|| &[] as &[Value], Vec::as_slice);
    let destructive = memory_destructive_op_counts(ops);
    let accepted_count = ops.len();
    let policy = applied_count.map_or_else(
        || MemoryApplyPolicy::curation_ops(config, accepted_count),
        |applied_count| {
            MemoryApplyPolicy::applied_curation_ops(config, accepted_count, applied_count)
        },
    );
    let apply_instructions = match policy.decision() {
        MemoryApplyDecision::AutoApplyAllowed => {
            "Accepted memory curation ops were applied because auto-apply is enabled and dashboard approval is not required."
        }
        MemoryApplyDecision::RequiresDashboardApproval => {
            "Review accepted memory curation ops in the dashboard before applying permanent deletes or merge losers."
        }
        MemoryApplyDecision::DryRunOnly | MemoryApplyDecision::ProposalOnly => {
            "Re-run with an explicit apply policy before mutating the memory store."
        }
        MemoryApplyDecision::NoValidOps | MemoryApplyDecision::NoValidFacts => {
            "No accepted memory curation ops require apply."
        }
    };
    let mut payload = policy.to_json();
    if let Some(object) = payload.as_object_mut() {
        object.insert("dry_run_first".to_string(), json!(true));
        object.insert("accepted_count".to_string(), json!(accepted_count));
        if let Some(applied_count) = applied_count {
            object.insert("applied_count".to_string(), json!(applied_count));
            object.insert(
                "fully_applied".to_string(),
                json!(accepted_count > 0 && applied_count >= accepted_count),
            );
        }
        object.insert(
            "permanent_delete_count".to_string(),
            json!(destructive.permanent_delete_count),
        );
        object.insert(
            "merge_loser_count".to_string(),
            json!(destructive.merge_loser_count),
        );
        object.insert(
            "destructive_target_count".to_string(),
            json!(destructive.permanent_delete_count + destructive.merge_loser_count),
        );
        object.insert("apply_instructions".to_string(), json!(apply_instructions));
    }
    payload
}

fn value_as_usize(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
}

#[derive(Debug, Default)]
struct MemoryDestructiveOpCounts {
    permanent_delete_count: usize,
    merge_loser_count: usize,
}

fn memory_destructive_op_counts(ops: &[Value]) -> MemoryDestructiveOpCounts {
    let mut counts = MemoryDestructiveOpCounts::default();
    for op in ops {
        match op.get("op").and_then(Value::as_str) {
            Some("delete") => counts.permanent_delete_count += 1,
            Some("merge") => {
                counts.merge_loser_count += op
                    .get("loser_ids")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
            }
            _ => {}
        }
    }
    counts
}

fn annotate_memory_curation_report(report: &mut Value, apply_policy: Value) {
    if let Some(object) = report.as_object_mut() {
        object.insert("automation_apply_policy".to_string(), apply_policy.clone());
    }
    if let Some(llm_apply) = report.get_mut("llm_apply").and_then(Value::as_object_mut) {
        llm_apply.insert("apply_policy".to_string(), apply_policy);
    }
}

fn default_max_clusters() -> usize {
    CURATION_DEFAULT_MAX_CLUSTERS
}

fn default_min_confidence() -> f64 {
    CURATION_DEFAULT_MIN_CONFIDENCE
}
