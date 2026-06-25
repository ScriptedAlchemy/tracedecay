use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::run_ledger::{AutomationRunArtifact, AutomationRunArtifactKind};

pub(super) fn artifact_ref(artifact: &AutomationRunArtifact) -> Value {
    json!({
        "kind": artifact.kind.clone(),
        "path": artifact.path.clone(),
        "sha256": artifact.sha256.clone(),
        "summary": artifact.summary.clone(),
        "created_at": artifact.created_at.clone(),
    })
}

pub(super) fn automation_run_artifacts_api(run_id: &str) -> String {
    format!("/api/automation/runs/{run_id}/artifacts")
}

pub(super) fn automation_run_artifact_api(run_id: &str, kind: AutomationRunArtifactKind) -> String {
    format!("{}/{}", automation_run_artifacts_api(run_id), kind.as_str())
}

pub(crate) fn sha256_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    sha256_bytes(&bytes)
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}
