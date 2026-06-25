use serde_json::{json, Value};

use super::artifact_feedback::{
    validation_feedback_entries, validation_gate_decision, validation_report_hash,
};
use super::artifact_generated_evals::{
    generated_eval_definitions, generated_eval_replay_results, generated_evals_status,
    improvement_gate_decision,
};
use super::artifact_optimizer::{
    codex_handoff_status, is_blocked_improvement_decision, optimizer_blockers,
    optimizer_diagnosis_summary, optimizer_ranked_changes, optimizer_recommendations,
};
use super::artifact_policy::TaskArtifactPolicy;
use super::artifact_refs::{automation_run_artifact_api, automation_run_artifacts_api};
use super::backend::{AgentTaskKind, AgentTaskRequest, AgentTaskResponse};
use super::run_ledger::{AutomationRunArtifactKind, AutomationRunLedgerRecord};
use super::text::truncate_chars_for_prompt;

pub(super) struct ArtifactPayloadContext<'a> {
    pub(super) run_id: &'a str,
    pub(super) task: AgentTaskKind,
    pub(super) task_key: &'a str,
    pub(super) prompt_version: &'a str,
    pub(super) policy: TaskArtifactPolicy,
    pub(super) request: &'a AgentTaskRequest,
    pub(super) response: &'a AgentTaskResponse,
    pub(super) record: &'a AutomationRunLedgerRecord,
}

pub(super) struct GeneratedEvalPayloads {
    pub(super) definitions: Vec<Value>,
    pub(super) count: usize,
    pub(super) runner_status: &'static str,
    pub(super) replay_results: Vec<Value>,
    pub(super) status: &'static str,
    pub(super) validation_decision: &'static str,
}

pub(super) struct ImprovementGatePayload {
    pub(super) decision: &'static str,
    pub(super) blockers: Vec<Value>,
    pub(super) blocked: bool,
}

pub(super) struct ArtifactRefs {
    pub(super) trace: Value,
    pub(super) feedback: Value,
    pub(super) generated_evals: Value,
    pub(super) validation_gate: Value,
    pub(super) optimizer_diagnosis: Value,
}

pub(super) fn traces_payload(ctx: &ArtifactPayloadContext<'_>) -> Value {
    json!({
        "schema_version": 1,
        "run_id": ctx.run_id,
        "task": ctx.task_key,
        "loop_stage": "traces",
        "prompt_version": ctx.prompt_version,
        "response_schema": ctx.request.contract.response_schema,
        "strict_json": ctx.request.contract.strict_json,
        "evidence_hash": ctx.record.evidence_hash,
        "input_hash": ctx.record.input_hash,
        "output_hash": ctx.record.output_hash,
        "context_keys": ctx.request.context.as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
    })
}

pub(super) fn feedback_payload(ctx: &ArtifactPayloadContext<'_>, trace_ref: &Value) -> Value {
    json!({
        "schema_version": 1,
        "run_id": ctx.run_id,
        "task": ctx.task_key,
        "loop_stage": "feedback",
        "status": "derived_from_validation",
        "source": "automation_validation",
        "artifact_refs": [trace_ref.clone()],
        "source_refs": [trace_ref.clone()],
        "summary": {
            "accepted_count": ctx.record.accepted_count,
            "rejected_count": ctx.record.rejected_count,
            "reviewed_count": ctx.record.reviewed_count,
            "skipped_count": ctx.record.skipped_count,
        },
        "human": [],
        "model": validation_feedback_entries(ctx.record),
    })
}

pub(super) fn generated_eval_payloads(ctx: &ArtifactPayloadContext<'_>) -> GeneratedEvalPayloads {
    let definitions = generated_eval_definitions(ctx.record, ctx.task, ctx.policy);
    let count = definitions.len();
    let (runner_status, replay_results) = generated_eval_replay_results(&definitions, ctx.record);
    GeneratedEvalPayloads {
        definitions,
        count,
        runner_status,
        replay_results,
        status: generated_evals_status(count, runner_status),
        validation_decision: validation_gate_decision(ctx.record),
    }
}

pub(super) fn generated_evals_payload(
    ctx: &ArtifactPayloadContext<'_>,
    refs: (&Value, &Value),
    evals: &GeneratedEvalPayloads,
) -> Value {
    let (trace_ref, feedback_ref) = refs;
    json!({
        "schema_version": 1,
        "run_id": ctx.run_id,
        "task": ctx.task_key,
        "loop_stage": "generated_evals",
        "status": "generated_from_validation",
        "format": "tracedecay_automation_eval:v1",
        "generator": "automation_validation:v1",
        "artifact_refs": [
            trace_ref.clone(),
            feedback_ref.clone(),
        ],
        "source_refs": [
            trace_ref.clone(),
            feedback_ref.clone(),
        ],
        "summary": {
            "eval_count": evals.count,
            "accepted_count": ctx.record.accepted_count,
            "rejected_count": ctx.record.rejected_count,
        },
        "runner": {
            "type": "validation_replay",
            "commands": ctx.policy.eval_replay_commands(),
            "artifact_api": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::GeneratedEvals),
            "inputs": {
                "run_id": ctx.run_id,
                "artifact_kind": AutomationRunArtifactKind::GeneratedEvals.as_str(),
                "validation_report_hash": validation_report_hash(ctx.record.validation_report.as_ref()),
                "expected_eval_count": evals.count,
            },
            "checks": [
                "load generated eval artifact from the dashboard artifact API or sidecar path",
                "replay validation definitions against the recorded validation report",
                "preserve expected_outcome for accepted and rejected examples",
            ],
            "status": evals.runner_status,
            "results": evals.replay_results.clone(),
        },
        "promotion": {
            "state": match evals.runner_status {
                "passed" => "validated",
                "failed" => "blocked_failed_replay",
                _ if evals.count == 0 => "blocked_no_examples",
                _ => "candidate",
            },
            "requires_human_review": true,
            "auto_apply": false,
        },
        "eval_definitions": evals.definitions.clone(),
        "result_refs": [{
            "kind": "validation_report",
            "hash": validation_report_hash(ctx.record.validation_report.as_ref()),
            "decision": evals.validation_decision,
        }],
    })
}

pub(super) fn improvement_gate_payload(
    ctx: &ArtifactPayloadContext<'_>,
    evals: &GeneratedEvalPayloads,
) -> ImprovementGatePayload {
    let decision = improvement_gate_decision(ctx.record, evals.count, evals.runner_status);
    ImprovementGatePayload {
        decision,
        blockers: optimizer_blockers(decision, evals.count),
        blocked: is_blocked_improvement_decision(decision),
    }
}

pub(super) fn validation_gate_payload(
    ctx: &ArtifactPayloadContext<'_>,
    refs: (&Value, &Value, &Value),
    evals: &GeneratedEvalPayloads,
    gate: &ImprovementGatePayload,
) -> Value {
    let (trace_ref, feedback_ref, generated_evals_ref) = refs;
    json!({
        "schema_version": 1,
        "run_id": ctx.run_id,
        "task": ctx.task_key,
        "loop_stage": "validation_gate",
        "task_validation": {
            "decision": validation_gate_decision(ctx.record),
            "accepted_count": ctx.record.accepted_count,
            "rejected_count": ctx.record.rejected_count,
            "reviewed_count": ctx.record.reviewed_count,
            "approval_required": ctx.record.accepted_count > 0,
            "report": ctx.record.validation_report,
        },
        "improvement_gate": {
            "decision": gate.decision,
            "feedback_status": "derived_from_validation",
            "generated_evals_status": evals.status,
            "optimizer_status": if gate.blocked {
                "blocked"
            } else if gate.decision == "ready_for_handoff" {
                "ready_for_handoff"
            } else {
                "ready_for_optimizer_review"
            },
            "handoff_status": if gate.blocked {
                "blocked"
            } else if gate.decision == "ready_for_handoff" {
                "ready"
            } else {
                "pending_optimizer_review"
            },
            "criteria": {
                "has_feedback": ctx.record.reviewed_count > 0,
                "has_generated_evals": evals.count > 0,
                "validation_report_hash": validation_report_hash(ctx.record.validation_report.as_ref()),
                "approval_required": ctx.record.accepted_count > 0,
                "auto_apply_allowed": false,
            },
            "source_refs": [
                trace_ref.clone(),
                feedback_ref.clone(),
                generated_evals_ref.clone(),
            ],
            "artifact_refs": [
                trace_ref.clone(),
                feedback_ref.clone(),
                generated_evals_ref.clone(),
            ],
        },
    })
}

pub(super) fn optimizer_diagnosis_payload(
    ctx: &ArtifactPayloadContext<'_>,
    refs: (&Value, &Value, &Value, &Value),
    evals: &GeneratedEvalPayloads,
    gate: &ImprovementGatePayload,
) -> Value {
    let (trace_ref, feedback_ref, generated_evals_ref, validation_gate_ref) = refs;
    json!({
        "schema_version": 1,
        "run_id": ctx.run_id,
        "task": ctx.task_key,
        "loop_stage": "optimizer_diagnosis",
        "status": "generated",
        "summary": optimizer_diagnosis_summary(ctx.record),
        "signals": {
            "validation_decision": evals.validation_decision,
            "accepted_count": ctx.record.accepted_count,
            "rejected_count": ctx.record.rejected_count,
            "reviewed_count": ctx.record.reviewed_count,
            "feedback_status": "derived_from_validation",
            "generated_evals_status": evals.status,
            "validation_gate_decision": gate.decision,
        },
        "recommendations": optimizer_recommendations(ctx.record),
        "ranked_changes": optimizer_ranked_changes(ctx.policy, ctx.record, gate.decision),
        "diagnostic_inputs": [
            trace_ref.clone(),
            feedback_ref.clone(),
            generated_evals_ref.clone(),
            validation_gate_ref.clone(),
        ],
        "artifact_refs": [
            trace_ref.clone(),
            feedback_ref.clone(),
            generated_evals_ref.clone(),
            validation_gate_ref.clone(),
        ],
        "source_refs": [
            feedback_ref.clone(),
            generated_evals_ref.clone(),
            validation_gate_ref.clone(),
        ],
        "blockers": gate.blockers.clone(),
    })
}

pub(super) fn codex_handoff_payload(
    ctx: &ArtifactPayloadContext<'_>,
    refs: &ArtifactRefs,
    evals: &GeneratedEvalPayloads,
    gate: &ImprovementGatePayload,
) -> Value {
    json!({
        "schema_version": 1,
        "run_id": ctx.run_id,
        "task": ctx.task_key,
        "loop_stage": "codex_handoff",
        "status": codex_handoff_status(gate.decision),
        "prompt_version": ctx.prompt_version,
        "backend": ctx.record.backend,
        "host_mode": ctx.record.host_mode,
        "model": ctx.record.model,
        "evidence_hash": ctx.record.evidence_hash,
        "input_hash": ctx.record.input_hash,
        "output_hash": ctx.record.output_hash,
        "request": {
            "evidence_hash": ctx.request.evidence_hash,
            "prompt_preview": truncate_chars_for_prompt(&ctx.request.prompt, 4000),
            "context_hash": ctx.record.input_hash,
        },
        "response": {
            "model": ctx.response.model,
            "input_tokens": ctx.response.input_tokens,
            "output_tokens": ctx.response.output_tokens,
            "output_text_preview": truncate_chars_for_prompt(&ctx.response.output_text, 4000),
            "output_json": ctx.response.output_json,
        },
        "readiness": {
            "validation_gate_decision": gate.decision,
            "eval_count": evals.count,
            "blockers": gate.blockers.clone(),
            "approval_required": ctx.record.accepted_count > 0,
            "auto_apply_allowed": false,
        },
        "source_refs": [
            refs.validation_gate.clone(),
            refs.optimizer_diagnosis.clone(),
        ],
        "machine_summary": {
            "task_key": ctx.task_key,
            "prompt_version": ctx.prompt_version,
            "run_id": ctx.run_id,
            "status": codex_handoff_status(gate.decision),
            "next_stage": match gate.decision {
                "blocked_pending_feedback_or_evals" => "collect_feedback_or_evals",
                "blocked_pending_eval_run" => "run_generated_evals",
                "blocked_failed_eval_replay" => "fix_generated_evals",
                _ => "codex_review",
            },
            "accepted_count": ctx.record.accepted_count,
            "rejected_count": ctx.record.rejected_count,
            "reviewed_count": ctx.record.reviewed_count,
            "artifact_kinds": [
                "traces",
                "feedback",
                "generated_evals",
                "validation_gate",
                "optimizer_diagnosis",
            ],
        },
        "validation_requirements": {
            "must_review_artifact_refs": true,
            "must_run_tests": ctx.policy.handoff_tests(),
            "must_preserve_approval_gate": true,
            "must_not_auto_apply": true,
        },
        "artifact_manifest": {
            "api_list": automation_run_artifacts_api(ctx.run_id),
            "api_payloads": {
                "traces": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::Traces),
                "feedback": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::Feedback),
                "generated_evals": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::GeneratedEvals),
                "validation_gate": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::ValidationGate),
                "optimizer_diagnosis": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::OptimizerDiagnosis),
                "codex_handoff": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::CodexHandoff),
            },
            "refs": [
                refs.trace,
                refs.feedback,
                refs.generated_evals,
                refs.validation_gate,
                refs.optimizer_diagnosis,
            ],
        },
        "eval_replay": {
            "artifact_kind": AutomationRunArtifactKind::GeneratedEvals.as_str(),
            "artifact_api": automation_run_artifact_api(ctx.run_id, AutomationRunArtifactKind::GeneratedEvals),
            "commands": ctx.policy.eval_replay_commands(),
            "requires_human_review": true,
        },
        "next_actions": ctx.policy.next_actions(ctx.record),
        "tests_to_run": ctx.policy.handoff_tests(),
    })
}
