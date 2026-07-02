use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::backend::{AgentTaskFailureClass, AgentTaskKind};
use crate::errors::{Result, TraceDecayError};

const RUN_LEDGER_FILENAME: &str = "automation_runs.jsonl";
const RUN_ARTIFACTS_DIR: &str = "automation_artifacts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrigger {
    #[default]
    ManualCli,
    Dashboard,
    Scheduler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl AutomationRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Skipped)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunArtifactKind {
    Traces,
    Feedback,
    GeneratedEvals,
    ValidationGate,
    OptimizerDiagnosis,
    CodexHandoff,
}

impl AutomationRunArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Feedback => "feedback",
            Self::GeneratedEvals => "generated_evals",
            Self::ValidationGate => "validation_gate",
            Self::OptimizerDiagnosis => "optimizer_diagnosis",
            Self::CodexHandoff => "codex_handoff",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunArtifact {
    pub schema_version: u32,
    pub kind: String,
    pub path: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunLedgerRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub trigger: AutomationTrigger,
    pub task: AgentTaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_key: Option<String>,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_json: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AutomationRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_ops: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_ops: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_ops: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_report: Option<Value>,
    #[serde(default)]
    pub reviewed_count: usize,
    pub accepted_count: usize,
    pub rejected_count: usize,
    #[serde(default)]
    pub skipped_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_classification: Option<AgentTaskFailureClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_ref: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<AutomationRunArtifact>,
    pub started_at: String,
    pub completed_at: String,
}

pub fn run_ledger_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(RUN_LEDGER_FILENAME)
}

pub fn run_artifact_path(
    dashboard_root: &Path,
    run_id: &str,
    kind: AutomationRunArtifactKind,
) -> Result<PathBuf> {
    validate_run_id_component(run_id)?;
    Ok(dashboard_root
        .join(RUN_ARTIFACTS_DIR)
        .join(run_id)
        .join(format!("{}.json", kind.as_str())))
}

pub async fn write_run_artifact(
    dashboard_root: &Path,
    run_id: &str,
    kind: AutomationRunArtifactKind,
    payload: &Value,
    summary: Option<String>,
    created_at: &str,
) -> Result<AutomationRunArtifact> {
    let path = run_artifact_path(dashboard_root, run_id, kind)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| config_error(format!("failed to create run artifact directory: {e}")))?;
    }
    let bytes = serde_json::to_vec_pretty(payload).map_err(TraceDecayError::from)?;
    tokio::fs::write(&path, &bytes).await.map_err(|e| {
        config_error(format!(
            "failed to write automation run artifact '{}': {e}",
            path.display()
        ))
    })?;

    Ok(AutomationRunArtifact {
        schema_version: 1,
        kind: kind.as_str().to_string(),
        path: artifact_relative_path(run_id, kind),
        sha256: format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
        summary,
        created_at: created_at.to_string(),
    })
}

pub async fn read_run_artifact_payload(
    dashboard_root: &Path,
    run_id: &str,
    artifact: &AutomationRunArtifact,
) -> Result<Value> {
    let path = artifact_path_from_relative(dashboard_root, run_id, &artifact.path)?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        config_error(format!(
            "failed to read automation run artifact '{}': {e}",
            path.display()
        ))
    })?;
    let actual_hash = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    if actual_hash != artifact.sha256 {
        return Err(config_error(format!(
            "automation run artifact '{}' hash mismatch",
            artifact.path
        )));
    }
    serde_json::from_slice(&bytes).map_err(TraceDecayError::from)
}

pub async fn find_run_record(
    dashboard_root: &Path,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let path = run_ledger_path(dashboard_root);
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read automation run ledger '{}': {e}",
                path.display()
            )))
        }
    };
    Ok(contents.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str::<AutomationRunLedgerRecord>(trimmed)
            .ok()
            .filter(|record| record.run_id == run_id)
    }))
}

pub async fn append_run_record(
    dashboard_root: &Path,
    record: &AutomationRunLedgerRecord,
) -> Result<()> {
    let path = run_ledger_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| config_error(format!("failed to create run ledger directory: {e}")))?;
    }
    // The record and its trailing newline must land in a single append.
    // Concurrent runs (e.g. two dashboard automation jobs finishing at the
    // same time) each append to this file; O_APPEND keeps one write atomic,
    // but splitting the line across two writes let them interleave into
    // `{recA}{recB}\n\n`, silently destroying both records at read time
    // (load_run_records skips unparseable lines) and leaving the runs stuck
    // at their previous status forever.
    let mut line = serde_json::to_string(record).map_err(TraceDecayError::from)?;
    line.push('\n');
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .map_err(|e| {
            config_error(format!(
                "failed to open automation run ledger '{}': {e}",
                path.display()
            ))
        })?;
    file.write_all(line.as_bytes()).await.map_err(|e| {
        config_error(format!(
            "failed to write automation run ledger '{}': {e}",
            path.display()
        ))
    })?;
    file.flush().await.map_err(|e| {
        config_error(format!(
            "failed to finish automation run ledger '{}': {e}",
            path.display()
        ))
    })?;
    Ok(())
}

pub async fn load_run_records(
    dashboard_root: &Path,
    limit: usize,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    let path = run_ledger_path(dashboard_root);
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read automation run ledger '{}': {e}",
                path.display()
            )))
        }
    };
    let mut records = Vec::new();
    let mut seen_run_ids = std::collections::BTreeSet::new();
    for line in contents.lines().rev() {
        if records.len() >= limit {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<AutomationRunLedgerRecord>(trimmed) {
            if !seen_run_ids.insert(record.run_id.clone()) {
                continue;
            }
            records.push(record);
        }
    }
    Ok(records)
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}

fn artifact_relative_path(run_id: &str, kind: AutomationRunArtifactKind) -> String {
    format!("{RUN_ARTIFACTS_DIR}/{run_id}/{}.json", kind.as_str())
}

fn artifact_path_from_relative(
    dashboard_root: &Path,
    run_id: &str,
    relative: &str,
) -> Result<PathBuf> {
    validate_run_id_component(run_id)?;
    let path = Path::new(relative);
    let mut components = path.components();
    if components.next()
        != Some(std::path::Component::Normal(std::ffi::OsStr::new(
            RUN_ARTIFACTS_DIR,
        )))
    {
        return Err(config_error(format!(
            "automation run artifact path '{relative}' is outside the artifact directory"
        )));
    }
    if components.next() != Some(std::path::Component::Normal(std::ffi::OsStr::new(run_id))) {
        return Err(config_error(format!(
            "automation run artifact path '{relative}' does not match run '{run_id}'"
        )));
    }
    let mut safe = PathBuf::from(RUN_ARTIFACTS_DIR);
    safe.push(run_id);
    for component in components {
        match component {
            std::path::Component::Normal(part) => safe.push(part),
            _ => {
                return Err(config_error(format!(
                    "automation run artifact path '{relative}' is not safe"
                )))
            }
        }
    }
    Ok(dashboard_root.join(safe))
}

fn validate_run_id_component(run_id: &str) -> Result<()> {
    let valid = !run_id.is_empty()
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(config_error(format!(
            "automation run_id '{run_id}' is not safe for artifact paths"
        )))
    }
}
