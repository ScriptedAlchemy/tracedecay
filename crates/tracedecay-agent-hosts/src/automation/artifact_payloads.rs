use serde_json::{Value, json};

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
    /// Post-activation outcomes of previously applied changes, when a snapshot
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
    /// Evals derived from real post-activation outcomes; tracked separately
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
        "automatic_application": generated_eval_application_effect(ctx.record, evals),
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
    let automatic_application = automatic_application_effect(ctx.record);
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
            "report": ctx.record.validation_report,
            "automatic_application": automatic_application.clone(),
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
                "ready_for_optimizer"
            },
            "criteria": {
                "has_feedback": ctx.record.reviewed_count > 0,
                "has_generated_evals": evals.count > 0,
                "validation_report_hash": validation_report_hash(ctx.record.validation_report.as_ref()),
                "automatic_application": automatic_application,
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
    let automatic_application = automatic_application_effect(ctx.record);
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
            "automatic_application": automatic_application.clone(),
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
                _ => "monitor_applied_outcomes",
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
            "must_run_tests": ctx.policy.handoff_tests(),
            "automatic_application": true,
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
            "application": automatic_application,
        },
        "next_actions": ctx.policy.next_actions(ctx.record),
        "tests_to_run": ctx.policy.handoff_tests(),
    })
}

fn automatic_application_effect(record: &AutomationRunLedgerRecord) -> Value {
    let deployment = record
        .applied_ops
        .as_ref()
        .and_then(|operations| operations.get("deployment"))
        .cloned();
    let deployment_retry = deployment
        .as_ref()
        .and_then(|receipt| receipt.get("retry_required"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let recorded_status = record
        .validation_report
        .as_ref()
        .and_then(|report| report.get("status"))
        .and_then(Value::as_str);
    let status = match recorded_status {
        Some("no_candidate") => "no_candidate",
        Some("quarantined") => "quarantined",
        Some("failed_after_partial_effects") => "partial",
        Some("applied") if deployment_retry => "partial",
        Some("applied") => "applied",
        _ if deployment_retry || record.error_retryable == Some(true) => "retry",
        _ if record.error.is_some() => "quarantined",
        _ if record.accepted_count > 0 && record.applied_ops.is_some() => "applied",
        _ => "unknown",
    };
    json!({
        "status": status,
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
        "retry_required": deployment_retry || record.error_retryable == Some(true),
        "deployment_receipt": deployment,
    })
}

fn generated_eval_application_effect(
    record: &AutomationRunLedgerRecord,
    evals: &GeneratedEvalPayloads,
) -> Value {
    match evals.runner_status {
        "passed" => automatic_application_effect(record),
        "failed" => json!({
            "status": "quarantined",
            "reason": "validation_replay_failed",
            "retry_required": false,
            "deployment_receipt": Value::Null,
        }),
        _ if evals.count == 0 => json!({
            "status": "no_candidate",
            "reason": "no_validation_examples",
            "retry_required": false,
            "deployment_receipt": Value::Null,
        }),
        _ => json!({
            "status": "retry",
            "reason": "validation_replay_incomplete",
            "retry_required": true,
            "deployment_receipt": Value::Null,
        }),
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
            provider: None,
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
            backend_attempt_count: 0,
            backend_attempts: Vec::new(),
            report_ref: None,
            artifacts: Vec::new(),
            started_at: "0".to_string(),
            completed_at: "0".to_string(),
        };
        (request, response, record)
    }

    fn outcomes_snapshot() -> AutomationOutcomesSnapshot {
        AutomationOutcomesSnapshot {
            schema_version: 2,
            skills: vec![SkillOutcomeRecord {
                skill_id: "ignored-skill".to_string(),
                title: Some("Ignored skill".to_string()),
                activated_at: 1_000,
                days_since_activation: 30,
                views_since_activation: 2,
                uses_since_activation: 0,
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
    fn codex_handoff_reports_applied_effect_with_deployment_receipt() {
        let (request, response, mut record) = payload_fixture();
        record.accepted_count = 1;
        record.reviewed_count = 1;
        record.applied_ops = Some(json!({
            "created_skills": [{ "skill_id": "outcome-skill" }],
            "deployment": {
                "status": "complete",
                "exports": [],
                "materialization_scopes": [],
                "errors": [],
                "reason": null,
                "retry_required": false,
            },
        }));
        record.validation_report = Some(json!({
            "status": "applied",
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
                .pointer("/task_validation/automatic_application/status")
                .unwrap(),
            &json!("applied")
        );
        assert_eq!(
            payload
                .pointer("/readiness/automatic_application/deployment_receipt/status")
                .unwrap(),
            &json!("complete")
        );
        assert_eq!(
            payload
                .pointer("/validation_requirements/automatic_application")
                .unwrap(),
            &json!(true)
        );
    }

    #[test]
    fn automatic_application_effect_reports_no_candidate_quarantine_and_partial_retry() {
        let (_, _, mut no_candidate) = payload_fixture();
        no_candidate.validation_report = Some(json!({"status": "no_candidate"}));
        assert_eq!(
            automatic_application_effect(&no_candidate).pointer("/status"),
            Some(&json!("no_candidate"))
        );

        let (_, _, mut quarantined) = payload_fixture();
        quarantined.validation_report = Some(json!({"status": "quarantined"}));
        assert_eq!(
            automatic_application_effect(&quarantined).pointer("/status"),
            Some(&json!("quarantined"))
        );

        let (_, _, mut partial) = payload_fixture();
        partial.accepted_count = 1;
        partial.rejected_count = 1;
        partial.error_retryable = Some(true);
        partial.applied_ops = Some(json!({
            "deployment": {
                "status": "partial_failure",
                "retry_required": true,
            },
        }));
        partial.validation_report = Some(json!({"status": "failed_after_partial_effects"}));
        let effect = automatic_application_effect(&partial);
        assert_eq!(effect.pointer("/status"), Some(&json!("partial")));
        assert_eq!(effect.pointer("/retry_required"), Some(&json!(true)));
    }
}
