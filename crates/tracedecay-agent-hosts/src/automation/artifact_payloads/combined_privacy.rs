use serde_json::{Value, json};

use super::ArtifactPayloadContext;

pub(super) fn skill_handoff_response(ctx: &ArtifactPayloadContext<'_>) -> Value {
    json!({
        "model": ctx.response.model,
        "input_tokens": ctx.response.input_tokens,
        "output_tokens": ctx.response.output_tokens,
        "output_hash": ctx.record.output_hash,
        "skill_result": {
            "accepted_count": ctx.record.accepted_count,
            "rejected_count": ctx.record.rejected_count,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::artifact_payloads::ArtifactPayloadContext;
    use crate::automation::artifact_policy::artifact_policy;
    use crate::automation::backend::{AgentTaskKind, AgentTaskRequest, AgentTaskResponse};
    use crate::automation::outcomes::AutomationOutcomesSnapshot;
    use crate::automation::run_ledger::{
        AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    };

    #[test]
    fn combined_skill_response_excludes_fact_output_payloads() {
        let secret = "sk-live-combined-fact-artifact";
        let request = AgentTaskRequest::new(
            "run-combined".to_string(),
            AgentTaskKind::CombinedReview,
            secret.to_string(),
            None,
            json!({"facts": [{"content": secret}]}),
        );
        let response = AgentTaskResponse {
            run_id: "run-combined".to_string(),
            task: AgentTaskKind::CombinedReview,
            output_text: secret.to_string(),
            output_json: Some(json!({"facts": [secret]})),
            model: None,
            provider: None,
            input_tokens: None,
            output_tokens: None,
        };
        let record: AutomationRunLedgerRecord = serde_json::from_value(json!({
            "schema_version": 2, "run_id": "run-combined.skill", "trigger": "manual_cli",
            "task": "skill_writer", "backend": "codex_app_server", "status": "succeeded",
            "accepted_count": 0, "rejected_count": 0, "started_at": "0", "completed_at": "1",
            "completed_at_micros": 1_000_000, "output_hash": "sha256:output"
        }))
        .expect("record");
        let outcomes = AutomationOutcomesSnapshot::default();
        let ctx = ArtifactPayloadContext {
            run_id: "run-combined.skill",
            task: AgentTaskKind::SkillWriter,
            task_key: "skill_writer",
            prompt_version: "combined_review:v1",
            policy: artifact_policy(AgentTaskKind::SkillWriter),
            request: &request,
            response: &response,
            record: &record,
            outcomes: &outcomes,
        };

        let serialized = serde_json::to_string(&skill_handoff_response(&ctx)).expect("JSON");
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("sha256:output"));
    }
}
