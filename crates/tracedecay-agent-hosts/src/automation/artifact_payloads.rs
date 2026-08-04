use serde_json::{Value, json};

use super::apply_policy::record_has_auto_applied_memory_ops;
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
use super::outcomes::{
    AutomationOutcomesSnapshot, outcome_eval_definitions, outcome_feedback_section,
};
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
    /// Post-approval outcomes of previously applied changes, when a snapshot
    /// has been recorded for this project.
    pub(super) outcomes: &'a AutomationOutcomesSnapshot,
}

pub(super) struct GeneratedEvalPayloads {
    pub(super) definitions: Vec<Value>,
    pub(super) count: usize,
    pub(super) runner_status: &'static str,
    pub(super) replay_results: Vec<Value>,
    pub(super) status: &'static str,
    pub(super) validation_decision: &'static str,
    /// Evals derived from real post-approval outcomes; tracked separately
    /// from the validation-replay definitions so the replay gate semantics
    /// stay unchanged.
    pub(super) outcome_definitions: Vec<Value>,
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
        "evidence_mode": context_evidence_mode(&ctx.request.context),
        "input_hash": ctx.record.input_hash,
        "output_hash": ctx.record.output_hash,
        "context_keys": ctx.request.context.as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
    })
}

/// Extracts the `evidence_mode` label from whichever evidence object the task
/// placed in the request context (e.g. `session_reflection_evidence` or
/// `skill_writer_evidence`), so traces distinguish session-replay-backed runs
/// from grep-only runs. Null for tasks without a mode label.
fn context_evidence_mode(context: &Value) -> Value {
    let modes = context
        .as_object()
        .map(|object| {
            object
                .values()
                .filter_map(|value| value.get("evidence_mode").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if modes.contains(&"session_replay_with_grep") {
        json!("session_replay_with_grep")
    } else if modes.contains(&"grep_only") {
        json!("grep_only")
    } else {
        Value::Null
    }
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
        "applied_change_outcomes": outcome_feedback_section(ctx.task, ctx.outcomes),
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
        outcome_definitions: outcome_eval_definitions(ctx.task, ctx.task_key, ctx.outcomes),
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
            "outcome_eval_count": evals.outcome_definitions.len(),
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
        "outcome_eval_definitions": evals.outcome_definitions.clone(),
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
    let auto_applied = record_has_auto_applied_memory_ops(ctx.task, ctx.record);
    let approval_required =
        automation_record_requires_dashboard_approval(ctx.task, ctx.record, auto_applied);
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
            "approval_required": approval_required,
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
                "approval_required": approval_required,
                "auto_apply_allowed": auto_applied,
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
    let auto_applied = record_has_auto_applied_memory_ops(ctx.task, ctx.record);
    let approval_required =
        automation_record_requires_dashboard_approval(ctx.task, ctx.record, auto_applied);
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
            "approval_required": approval_required,
            "auto_apply_allowed": auto_applied,
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
            "must_preserve_approval_gate": !auto_applied,
            "must_not_auto_apply": !auto_applied,
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
        "next_actions": ctx.policy.next_actions(ctx.record.accepted_count),
        "tests_to_run": ctx.policy.handoff_tests(),
    })
}

fn automation_record_requires_dashboard_approval(
    task: AgentTaskKind,
    record: &AutomationRunLedgerRecord,
    auto_applied: bool,
) -> bool {
    match task {
        AgentTaskKind::MemoryCurator | AgentTaskKind::SessionReflector => false,
        _ => record.accepted_count > 0 && !auto_applied,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::artifact_policy::artifact_policy;
    use super::super::outcomes::{SkillOutcomeRecord, SkillOutcomeVerdict};
    use super::super::run_ledger::{AutomationRunStatus, AutomationTrigger};
    use super::*;

    fn payload_fixture() -> (
        AgentTaskRequest,
        AgentTaskResponse,
        AutomationRunLedgerRecord,
    ) {
        let request = AgentTaskRequest::new(
            "run-outcomes".to_string(),
            AgentTaskKind::SkillWriter,
            "propose skills".to_string(),
            Some("sha256:evidence".to_string()),
            json!({}),
        );
        let response = AgentTaskResponse {
            run_id: "run-outcomes".to_string(),
            task: AgentTaskKind::SkillWriter,
            output_text: "{\"skills\":[]}".to_string(),
            output_json: Some(json!({"skills": []})),
            model: None,
            input_tokens: None,
            output_tokens: None,
        };
        let record = AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: "run-outcomes".to_string(),
            trigger: AutomationTrigger::ManualCli,
            task: AgentTaskKind::SkillWriter,
            task_key: Some("skill_writer".to_string()),
            backend: "codex_app_server".to_string(),
            host_mode: None,
            prompt_version: None,
            response_schema: None,
            strict_json: None,
            model: None,
            status: AutomationRunStatus::Succeeded,
            evidence_hash: Some("sha256:evidence".to_string()),
            input_hash: None,
            output_hash: None,
            proposed_ops: None,
            applied_ops: None,
            rejected_ops: None,
            validation_report: None,
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 0,
            fallback_status: None,
            error: None,
            error_classification: None,
            error_retryable: None,
            report_ref: None,
            artifacts: Vec::new(),
            started_at: "0".to_string(),
            completed_at: "0".to_string(),
        };
        (request, response, record)
    }

    fn outcomes_snapshot() -> AutomationOutcomesSnapshot {
        AutomationOutcomesSnapshot {
            schema_version: 1,
            skills: vec![SkillOutcomeRecord {
                skill_id: "ignored-skill".to_string(),
                title: Some("Ignored skill".to_string()),
                approved_at: 1_000,
                days_since_approval: 30,
                views_since_approval: 2,
                uses_since_approval: 0,
                verdict: SkillOutcomeVerdict::Ignored,
            }],
            facts: Vec::new(),
            skills_refreshed_at: Some(2_000),
            facts_refreshed_at: None,
        }
    }

    #[test]
    fn feedback_payload_includes_applied_change_outcomes() {
        let (request, response, record) = payload_fixture();
        let outcomes = outcomes_snapshot();
        let ctx = ArtifactPayloadContext {
            run_id: "run-outcomes",
            task: AgentTaskKind::SkillWriter,
            task_key: "skill_writer",
            prompt_version: "skill_writer:v1",
            policy: artifact_policy(AgentTaskKind::SkillWriter),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };

        let payload = feedback_payload(&ctx, &json!({"kind": "traces"}));
        let section = payload.get("applied_change_outcomes").unwrap();
        assert_eq!(section.get("status").unwrap(), &json!("available"));
        assert_eq!(
            section.pointer("/skill_verdicts/ignored").unwrap(),
            &json!(1)
        );
        assert_eq!(
            section.pointer("/skills/0/skill_id").unwrap(),
            &json!("ignored-skill")
        );

        let evals = generated_eval_payloads(&ctx);
        assert_eq!(evals.outcome_definitions.len(), 1);
        let generated = generated_evals_payload(
            &ctx,
            (&json!({"kind": "traces"}), &json!({"kind": "feedback"})),
            &evals,
        );
        assert_eq!(
            generated.pointer("/summary/outcome_eval_count").unwrap(),
            &json!(1)
        );
        assert_eq!(
            generated
                .pointer("/outcome_eval_definitions/0/observed_outcome")
                .unwrap(),
            &json!("ignored")
        );
        // The validation-replay definitions must stay outcome-free so the
        // replay gate semantics are unchanged.
        assert_eq!(evals.count, 0);
    }

    #[test]
    fn traces_payload_prefers_replay_backed_evidence_mode_over_key_order() {
        let (mut request, response, record) = payload_fixture();
        request.context = json!({
            "aaa_grep_only_evidence": {
                "evidence_mode": "grep_only",
            },
            "zzz_replay_evidence": {
                "evidence_mode": "session_replay_with_grep",
            },
        });
        let outcomes = AutomationOutcomesSnapshot::default();
        let ctx = ArtifactPayloadContext {
            run_id: "run-outcomes",
            task: AgentTaskKind::CombinedReview,
            task_key: "combined_review",
            prompt_version: "combined_review:v1",
            policy: artifact_policy(AgentTaskKind::CombinedReview),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };

        let payload = traces_payload(&ctx);

        assert_eq!(
            payload.pointer("/evidence_mode").unwrap(),
            &json!("session_replay_with_grep")
        );
    }

    #[test]
    fn empty_outcomes_snapshot_reports_none_recorded() {
        let (request, response, record) = payload_fixture();
        let outcomes = AutomationOutcomesSnapshot::default();
        let ctx = ArtifactPayloadContext {
            run_id: "run-outcomes",
            task: AgentTaskKind::SessionReflector,
            task_key: "session_reflector",
            prompt_version: "session_reflector:v1",
            policy: artifact_policy(AgentTaskKind::SessionReflector),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };

        let payload = feedback_payload(&ctx, &json!({"kind": "traces"}));
        assert_eq!(
            payload.pointer("/applied_change_outcomes/status").unwrap(),
            &json!("no_outcomes_recorded")
        );
        assert!(generated_eval_payloads(&ctx).outcome_definitions.is_empty());
    }

    #[test]
    fn codex_handoff_reports_auto_applied_records_without_approval_gate() {
        let (request, response, mut record) = payload_fixture();
        record.accepted_count = 1;
        record.reviewed_count = 1;
        record.applied_ops = Some(json!(["proposal-1"]));
        record.validation_report = Some(json!({
            "session_fact_apply_policy": {
                "mutates_store": true,
            }
        }));
        let outcomes = AutomationOutcomesSnapshot::default();
        let ctx = ArtifactPayloadContext {
            run_id: "run-outcomes",
            task: AgentTaskKind::SessionReflector,
            task_key: "session_reflector",
            prompt_version: "session_reflector:v1",
            policy: artifact_policy(AgentTaskKind::SessionReflector),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };
        let refs = ArtifactRefs {
            trace: json!({"kind": "traces"}),
            feedback: json!({"kind": "feedback"}),
            generated_evals: json!({"kind": "generated_evals"}),
            validation_gate: json!({"kind": "validation_gate"}),
            optimizer_diagnosis: json!({"kind": "optimizer_diagnosis"}),
        };
        let evals = generated_eval_payloads(&ctx);
        let gate = improvement_gate_payload(&ctx, &evals);
        let validation_payload = validation_gate_payload(
            &ctx,
            (&refs.trace, &refs.feedback, &refs.generated_evals),
            &evals,
            &gate,
        );

        let payload = codex_handoff_payload(&ctx, &refs, &evals, &gate);

        assert_eq!(
            validation_payload
                .pointer("/task_validation/approval_required")
                .unwrap(),
            &json!(false)
        );
        assert_eq!(
            validation_payload
                .pointer("/improvement_gate/criteria/auto_apply_allowed")
                .unwrap(),
            &json!(true)
        );
        assert_eq!(
            payload.pointer("/readiness/approval_required").unwrap(),
            &json!(false)
        );
        assert_eq!(
            payload.pointer("/readiness/auto_apply_allowed").unwrap(),
            &json!(true)
        );
        assert_eq!(
            payload
                .pointer("/validation_requirements/must_not_auto_apply")
                .unwrap(),
            &json!(false)
        );
    }

    #[test]
    fn codex_handoff_reports_partial_memory_apply_without_approval_gate() {
        let (request, response, mut record) = payload_fixture();
        record.accepted_count = 2;
        record.reviewed_count = 2;
        record.applied_ops = Some(json!([
            { "op": "delete", "fact_id": 102 },
            { "op": "delete", "fact_id": 102 }
        ]));
        record.validation_report = Some(json!({
            "applied": 1,
            "results": [
                { "op": "delete", "fact_id": 102, "status": "deleted" },
                { "op": "delete", "fact_id": 102, "status": "error" }
            ],
            "apply_policy": {
                "accepted_count": 2,
                "mutates_store": true,
            }
        }));
        let outcomes = AutomationOutcomesSnapshot::default();
        let ctx = ArtifactPayloadContext {
            run_id: "run-outcomes",
            task: AgentTaskKind::MemoryCurator,
            task_key: "memory_curator",
            prompt_version: "memory_curator:v1",
            policy: artifact_policy(AgentTaskKind::MemoryCurator),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };
        let refs = ArtifactRefs {
            trace: json!({"kind": "traces"}),
            feedback: json!({"kind": "feedback"}),
            generated_evals: json!({"kind": "generated_evals"}),
            validation_gate: json!({"kind": "validation_gate"}),
            optimizer_diagnosis: json!({"kind": "optimizer_diagnosis"}),
        };
        let evals = generated_eval_payloads(&ctx);
        let gate = improvement_gate_payload(&ctx, &evals);
        let validation_payload = validation_gate_payload(
            &ctx,
            (&refs.trace, &refs.feedback, &refs.generated_evals),
            &evals,
            &gate,
        );

        let payload = codex_handoff_payload(&ctx, &refs, &evals, &gate);

        assert_eq!(
            validation_payload
                .pointer("/task_validation/approval_required")
                .unwrap(),
            &json!(false)
        );
        assert_eq!(
            validation_payload
                .pointer("/improvement_gate/criteria/auto_apply_allowed")
                .unwrap(),
            &json!(false)
        );
        assert_eq!(
            payload.pointer("/readiness/approval_required").unwrap(),
            &json!(false)
        );
        assert_eq!(
            payload.pointer("/readiness/auto_apply_allowed").unwrap(),
            &json!(false)
        );
    }

    #[test]
    fn codex_handoff_does_not_treat_skill_writer_applied_ops_as_memory_auto_apply() {
        let (request, response, mut record) = payload_fixture();
        record.accepted_count = 1;
        record.reviewed_count = 1;
        record.applied_ops = Some(json!({
            "created_skills": [],
            "updated_skills": [],
            "staged_consolidations": [],
        }));
        record.validation_report = Some(json!({
            "status": "needs_approval",
            "dry_run": true,
        }));
        let outcomes = AutomationOutcomesSnapshot::default();
        let ctx = ArtifactPayloadContext {
            run_id: "run-outcomes",
            task: AgentTaskKind::SkillWriter,
            task_key: "skill_writer",
            prompt_version: "skill_writer:v1",
            policy: artifact_policy(AgentTaskKind::SkillWriter),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };
        let refs = ArtifactRefs {
            trace: json!({"kind": "traces"}),
            feedback: json!({"kind": "feedback"}),
            generated_evals: json!({"kind": "generated_evals"}),
            validation_gate: json!({"kind": "validation_gate"}),
            optimizer_diagnosis: json!({"kind": "optimizer_diagnosis"}),
        };
        let evals = generated_eval_payloads(&ctx);
        let gate = improvement_gate_payload(&ctx, &evals);

        let payload = codex_handoff_payload(&ctx, &refs, &evals, &gate);

        assert_eq!(
            payload.pointer("/readiness/approval_required").unwrap(),
            &json!(true)
        );
        assert_eq!(
            payload.pointer("/readiness/auto_apply_allowed").unwrap(),
            &json!(false)
        );
        assert_eq!(
            payload
                .pointer("/validation_requirements/must_not_auto_apply")
                .unwrap(),
            &json!(true)
        );
    }
}
