use std::path::Path;

use serde_json::Value;

use super::artifact_payloads::{
    codex_handoff_payload, feedback_payload, generated_eval_payloads, generated_evals_payload,
    improvement_gate_payload, optimizer_diagnosis_payload, traces_payload, validation_gate_payload,
    ArtifactPayloadContext, ArtifactRefs,
};
use super::artifact_policy::artifact_policy;
use super::artifact_refs::artifact_ref;
use super::backend::{
    prompt_version, task_key, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
};
use super::outcomes::load_outcomes_snapshot;
use super::run_ledger::{
    write_run_artifact, AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
};
use crate::errors::Result;
use crate::tracedecay::current_timestamp;

pub(crate) use super::artifact_refs::{sha256_bytes, sha256_json};

struct ImprovementArtifactWriter<'a> {
    dashboard_root: &'a Path,
    run_id: &'a str,
    created_at: &'a str,
    artifacts: Vec<AutomationRunArtifact>,
}

impl<'a> ImprovementArtifactWriter<'a> {
    fn new(dashboard_root: &'a Path, run_id: &'a str, created_at: &'a str) -> Self {
        Self {
            dashboard_root,
            run_id,
            created_at,
            artifacts: Vec::new(),
        }
    }

    async fn write(
        &mut self,
        kind: AutomationRunArtifactKind,
        payload: &Value,
        summary: Option<String>,
    ) -> Result<Value> {
        let artifact = write_run_artifact(
            self.dashboard_root,
            self.run_id,
            kind,
            payload,
            summary,
            self.created_at,
        )
        .await?;
        let artifact_ref = artifact_ref(&artifact);
        self.artifacts.push(artifact);
        Ok(artifact_ref)
    }

    fn finish(self) -> Vec<AutomationRunArtifact> {
        self.artifacts
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
    let created_at = current_timestamp().to_string();
    let task_key = task_key(task);
    let prompt_version = prompt_version(task);
    let policy = artifact_policy(task);
    // A missing or unreadable outcomes snapshot must never block the run's
    // artifact trail; it only means no post-approval signal is available yet.
    let outcomes = load_outcomes_snapshot(dashboard_root)
        .await
        .unwrap_or_default();
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

    Ok(writer.finish())
}
