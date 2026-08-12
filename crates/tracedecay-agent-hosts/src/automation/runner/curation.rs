use serde_json::{Value, json};
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_policy::{
    CurationApplyAuthorityV1, CurationApplyDecisionV1, CurationApplyPolicyInputV1,
    CurationApplySubjectV1, CurationValidationDispositionV1, evaluate_curation_apply,
};

use crate::automation::backend::{
    AgentTaskKind, AgentTaskResponse, agent_task_contract, extract_json_object_prefix,
    prompt_version, task_key,
};
use crate::automation::config::AutomationConfig;
use crate::automation::lifecycle::AgentTaskRunContext;
use crate::automation::run_ledger::{AutomationRunLedgerRecord, AutomationRunStatus};
use crate::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::tracedecay::current_timestamp;

pub(super) fn evaluate_session_curation(
    config: &AutomationConfig,
    authority: &CurationApplyAuthorityV1,
    evidence_hash: Option<&str>,
    accepted_facts: &[Value],
) -> Result<CurationApplyDecisionV1> {
    evaluate_curation_output(
        config,
        authority,
        evidence_hash,
        accepted_facts,
        CurationApplySubjectV1::SessionReflector,
        "session curation",
    )
}

pub(super) fn evaluate_skill_curation(
    config: &AutomationConfig,
    authority: &CurationApplyAuthorityV1,
    evidence_hash: Option<&str>,
    proposals: &[Value],
) -> Result<CurationApplyDecisionV1> {
    evaluate_curation_output(
        config,
        authority,
        evidence_hash,
        proposals,
        CurationApplySubjectV1::SkillWriter,
        "skill curation",
    )
}

fn evaluate_curation_output(
    config: &AutomationConfig,
    authority: &CurationApplyAuthorityV1,
    evidence_hash: Option<&str>,
    output: &[Value],
    subject: CurationApplySubjectV1,
    identity_label: &str,
) -> Result<CurationApplyDecisionV1> {
    let evidence_digest = evidence_hash
        .map(|hash| ManifestDigest::new(hash.to_owned()))
        .transpose()
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid {identity_label} evidence identity: {error}"),
        })?;
    let output_digest = canonical_sha256(&output).map_err(|error| TraceDecayError::Config {
        message: format!("derive {identity_label} output identity: {error}"),
    })?;
    let configuration_digest =
        canonical_sha256(config).map_err(|error| TraceDecayError::Config {
            message: format!("derive {identity_label} configuration identity: {error}"),
        })?;
    evaluate_curation_apply(&CurationApplyPolicyInputV1 {
        authority: authority.clone(),
        subject,
        evidence_digest,
        output_digest,
        validation: if output.is_empty() {
            CurationValidationDispositionV1::NoCandidate
        } else {
            CurationValidationDispositionV1::Accepted
        },
        configuration_digest,
    })
    .map_err(|error| TraceDecayError::Config {
        message: format!("evaluate {identity_label} policy: {error}"),
    })
}

pub(super) fn combined_review_output(
    response: &AgentTaskResponse,
) -> Result<(Value, Vec<Value>, Vec<Value>)> {
    let output = response
        .output_json
        .clone()
        .map_or_else(|| extract_json_object_prefix(&response.output_text), Ok)?;
    let facts = output
        .get("facts")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| TraceDecayError::Config {
            message: "combined review output must include a facts array".to_string(),
        })?;
    let skills = output
        .get("skills")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| TraceDecayError::Config {
            message: "combined review output must include a skills array".to_string(),
        })?;
    Ok((output, facts, skills))
}

pub(super) fn unpersisted_rejected_parts(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    task: AgentTaskKind,
    reason: &str,
    evidence_hash: Option<String>,
    report_task: &'static str,
) -> (Value, AutomationRunLedgerRecord) {
    let completed_at = current_timestamp().to_string();
    let contract = agent_task_contract(task);
    let report = json!({
        "status": "skipped",
        "reason": reason,
        "dry_run": true,
        "task": report_task,
    });
    let record = AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run.run_id.clone(),
        trigger: run.trigger,
        task,
        task_key: Some(task_key(task).to_string()),
        backend: config.backend.as_str().to_string(),
        host_mode: Some(config.host_mode.as_str().to_string()),
        prompt_version: Some(prompt_version(task).to_string()),
        response_schema: Some(contract.response_schema),
        strict_json: Some(contract.strict_json),
        model: None,
        status: AutomationRunStatus::Skipped,
        evidence_hash,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 1,
        error: Some(reason.to_string()),
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status: Some(reason.to_string()),
        report_ref: Some(json!({
            "dashboard_runs": "/api/automation/runs",
            "run_id": run.run_id,
        })),
        artifacts: Vec::new(),
        started_at: run.started_at().to_string(),
        completed_at,
    };
    (report, record)
}
