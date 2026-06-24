use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use super::backend::AgentTaskKind;
use crate::errors::{Result, TraceDecayError};

const RUN_LEDGER_FILENAME: &str = "automation_runs.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrigger {
    ManualCli,
    Dashboard,
    Scheduler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunLedgerRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub trigger: AutomationTrigger,
    pub task: AgentTaskKind,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AutomationRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_ops: Option<Value>,
    pub accepted_count: usize,
    pub rejected_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: String,
    pub completed_at: String,
}

pub fn run_ledger_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(RUN_LEDGER_FILENAME)
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
    let line = serde_json::to_string(record).map_err(TraceDecayError::from)?;
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
    file.write_all(b"\n").await.map_err(|e| {
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
    for line in contents.lines().rev() {
        if records.len() >= limit {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<AutomationRunLedgerRecord>(trimmed) {
            records.push(record);
        }
    }
    Ok(records)
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}
