use std::path::Path;

use serde_json::Value;

use super::artifact_payloads::{
    ArtifactPayloadContext, ArtifactRefs, codex_handoff_payload, feedback_payload,
    generated_eval_payloads, generated_evals_payload, improvement_gate_payload,
    optimizer_diagnosis_payload, traces_payload, validation_gate_payload,
};
use super::artifact_policy::artifact_policy;
use super::artifact_refs::artifact_ref;
use super::backend::{
    AgentTaskKind, AgentTaskRequest, AgentTaskResponse, prompt_version, task_key,
};
use super::outcomes::load_outcomes_snapshot;
use super::run_ledger::{
    AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    prepare_run_artifact, publish_run_artifact_chain, read_published_artifact_chain,
};
use crate::errors::Result;

pub(crate) use super::artifact_refs::{sha256_bytes, sha256_json};

struct ImprovementArtifactWriter<'a> {
    dashboard_root: &'a Path,
    run_id: &'a str,
    created_at: &'a str,
    artifacts: Vec<AutomationRunArtifact>,
    pending: Vec<(AutomationRunArtifact, Vec<u8>)>,
}

impl<'a> ImprovementArtifactWriter<'a> {
    fn new(dashboard_root: &'a Path, run_id: &'a str, created_at: &'a str) -> Self {
        Self {
            dashboard_root,
            run_id,
            created_at,
            artifacts: Vec::new(),
            pending: Vec::new(),
        }
    }

    async fn write(
        &mut self,
        kind: AutomationRunArtifactKind,
        payload: &Value,
        summary: Option<String>,
    ) -> Result<Value> {
        let (artifact, bytes) =
            prepare_run_artifact(self.run_id, kind, payload, summary, self.created_at)?;
        let artifact_ref = artifact_ref(&artifact);
        self.artifacts.push(artifact.clone());
        self.pending.push((artifact, bytes));
        Ok(artifact_ref)
    }

    async fn finish(self, identity: &Value) -> Result<Vec<AutomationRunArtifact>> {
        publish_run_artifact_chain(self.dashboard_root, self.run_id, self.pending, identity)
            .await?;
        Ok(self.artifacts)
    }
}

pub(crate) async fn write_improvement_artifacts(
    dashboard_root: &Path,
    run_id: &str,
    task: AgentTaskKind,
    request: &AgentTaskRequest,
    response: &AgentTaskResponse,
    record: &AutomationRunLedgerRecord,
) -> Result<Vec<AutomationRunArtifact>> {
    let task_key = task_key(task);
    let created_at = record.completed_at.clone();
    let prompt_version = prompt_version(task);
    let policy = artifact_policy(task);
    // A missing or unreadable outcomes snapshot must never block the run's
    // artifact trail; it only means no post-approval signal is available yet.
    let outcomes = load_outcomes_snapshot(dashboard_root)
        .await
        .unwrap_or_default();
    let publication_identity = serde_json::json!({
        "sha256": sha256_json(&serde_json::json!({
            "task": task_key,
            "prompt_version": prompt_version,
            "policy": {
                "optimizer_action": policy.optimizer_action,
                "next_actions": policy.next_actions(record),
                "handoff_tests": policy.handoff_tests(),
                "eval_replay_commands": policy.eval_replay_commands(),
            },
            "request": request,
            "response": response,
            "record": record,
            "outcomes": outcomes,
        })),
    });
    if let Some(artifacts) =
        read_published_artifact_chain(dashboard_root, run_id, Some(&publication_identity)).await?
    {
        return Ok(artifacts);
    }
    let ctx = ArtifactPayloadContext {
        run_id,
        task,
        task_key,
        prompt_version,
        policy,
        request,
        response,
        record,
        outcomes: &outcomes,
    };
    let mut writer = ImprovementArtifactWriter::new(dashboard_root, run_id, &created_at);

    let trace_ref = writer
        .write(
            AutomationRunArtifactKind::Traces,
            &traces_payload(&ctx),
            Some(format!("{task_key} trace and hash references")),
        )
        .await?;

    let feedback_ref = writer
        .write(
            AutomationRunArtifactKind::Feedback,
            &feedback_payload(&ctx, &trace_ref),
            Some("feedback derived from validation outcomes".to_string()),
        )
        .await?;

    let evals = generated_eval_payloads(&ctx);
    let generated_evals_ref = writer
        .write(
            AutomationRunArtifactKind::GeneratedEvals,
            &generated_evals_payload(&ctx, (&trace_ref, &feedback_ref), &evals),
            Some("evals generated from validation outcomes".to_string()),
        )
        .await?;

    let gate = improvement_gate_payload(&ctx, &evals);
    let validation_gate_ref = writer
        .write(
            AutomationRunArtifactKind::ValidationGate,
            &validation_gate_payload(
                &ctx,
                (&trace_ref, &feedback_ref, &generated_evals_ref),
                &evals,
                &gate,
            ),
            Some(format!(
                "{} accepted, {} rejected",
                record.accepted_count, record.rejected_count
            )),
        )
        .await?;

    let optimizer_diagnosis_ref = writer
        .write(
            AutomationRunArtifactKind::OptimizerDiagnosis,
            &optimizer_diagnosis_payload(
                &ctx,
                (
                    &trace_ref,
                    &feedback_ref,
                    &generated_evals_ref,
                    &validation_gate_ref,
                ),
                &evals,
                &gate,
            ),
            Some("optimizer diagnosis derived from validation outcomes".to_string()),
        )
        .await?;

    let codex_handoff = codex_handoff_payload(
        &ctx,
        &ArtifactRefs {
            trace: trace_ref,
            feedback: feedback_ref,
            generated_evals: generated_evals_ref,
            validation_gate: validation_gate_ref,
            optimizer_diagnosis: optimizer_diagnosis_ref,
        },
        &evals,
        &gate,
    );
    writer
        .write(
            AutomationRunArtifactKind::CodexHandoff,
            &codex_handoff,
            Some(format!("{task_key} review handoff")),
        )
        .await?;

    writer.finish(&publication_identity).await
}
