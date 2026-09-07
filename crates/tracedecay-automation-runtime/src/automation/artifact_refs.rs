use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;

use super::config_error;
use super::run_ledger::{AutomationRunArtifact, AutomationRunArtifactKind};
use crate::errors::Result;

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

pub(crate) fn sha256_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        config_error(format!(
            "failed to serialize automation value for sha256 digest: {error}"
        ))
    })?;
    Ok(sha256_bytes(&bytes))
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    encode_tagged_lowercase_hex("sha256:", &Sha256::digest(bytes))
}
