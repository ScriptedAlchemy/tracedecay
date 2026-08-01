use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_application::DirectorySyncPolicy;

use super::backend::{
    AgentTaskFailureClass, AgentTaskKind, AgentTaskRetryAttempt, task_key as canonical_task_key,
};
use super::config_error;
use crate::errors::{Result, TraceDecayError};

const RUN_LEDGER_FILENAME: &str = "automation_runs.jsonl";
const RUN_ARTIFACTS_DIR: &str = "automation_artifacts";
/// Trailing bytes read from the ledger on the first tail pass. Sized to hold
/// several hundred JSONL records so the common scheduler read (`limit == 200`)
/// is satisfied by one bounded read even as the append-only ledger grows into
/// tens of thousands of lines. The window doubles on demand when a pass has
/// not yet gathered `limit` distinct records.
const RUN_LEDGER_TAIL_CHUNK_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrigger {
    #[default]
    ManualCli,
    Dashboard,
    Scheduler,
    HostReceipt,
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
    Manifest,
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
            Self::Manifest => "manifest",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "traces" => Some(Self::Traces),
            "feedback" => Some(Self::Feedback),
            "generated_evals" => Some(Self::GeneratedEvals),
            "validation_gate" => Some(Self::ValidationGate),
            "optimizer_diagnosis" => Some(Self::OptimizerDiagnosis),
            "codex_handoff" => Some(Self::CodexHandoff),
            "manifest" => Some(Self::Manifest),
            _ => None,
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
    #[serde(default)]
    pub backend_attempt_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backend_attempts: Vec<AgentTaskRetryAttempt>,
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
    let (artifact, bytes) = prepare_run_artifact(run_id, kind, payload, summary, created_at)?;
    let path = run_artifact_path(dashboard_root, run_id, kind)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| config_error(format!("failed to create run artifact directory: {e}")))?;
    }
    tokio::fs::write(&path, &bytes).await.map_err(|e| {
        config_error(format!(
            "failed to write automation run artifact '{}': {e}",
            path.display()
        ))
    })?;

    Ok(artifact)
}

pub(crate) fn prepare_run_artifact(
    run_id: &str,
    kind: AutomationRunArtifactKind,
    payload: &Value,
    summary: Option<String>,
    created_at: &str,
) -> Result<(AutomationRunArtifact, Vec<u8>)> {
    validate_run_id_component(run_id)?;
    let bytes = serde_json::to_vec_pretty(payload).map_err(TraceDecayError::from)?;
    let artifact = AutomationRunArtifact {
        schema_version: 1,
        kind: kind.as_str().to_string(),
        path: artifact_relative_path(run_id, kind),
        sha256: super::artifact_refs::sha256_bytes(&bytes),
        summary,
        created_at: created_at.to_string(),
    };
    Ok((artifact, bytes))
}

pub async fn read_published_artifact_manifest(
    dashboard_root: &Path,
    run_id: &str,
    expected_identity: Option<&Value>,
) -> Result<Option<Vec<AutomationRunArtifact>>> {
    let manifest_path =
        run_artifact_path(dashboard_root, run_id, AutomationRunArtifactKind::Manifest)?;
    crate::storage::reject_symlink_components(&manifest_path, "automation artifact manifest")
        .map_err(TraceDecayError::from)?;
    let bytes = match tokio::fs::read(&manifest_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "failed to read artifact manifest '{}': {error}",
                manifest_path.display()
            )));
        }
    };
    let payload: Value = serde_json::from_slice(&bytes).map_err(TraceDecayError::from)?;
    if payload.get("run_id").and_then(Value::as_str) != Some(run_id) {
        return Err(config_error(
            "artifact manifest run_id does not match its path",
        ));
    }
    if let Some(expected_identity) = expected_identity
        && payload.get("identity") != Some(expected_identity)
    {
        return Err(config_error(
            "published artifact manifest does not match the current run identity",
        ));
    }
    let mut artifacts = serde_json::from_value::<Vec<AutomationRunArtifact>>(
        payload
            .get("artifacts")
            .cloned()
            .ok_or_else(|| config_error("artifact manifest is missing artifacts"))?,
    )
    .map_err(TraceDecayError::from)?;
    let expected = [
        AutomationRunArtifactKind::Traces,
        AutomationRunArtifactKind::Feedback,
        AutomationRunArtifactKind::GeneratedEvals,
        AutomationRunArtifactKind::ValidationGate,
        AutomationRunArtifactKind::OptimizerDiagnosis,
        AutomationRunArtifactKind::CodexHandoff,
    ];
    let mut seen = std::collections::BTreeSet::new();
    for artifact in &artifacts {
        let kind = AutomationRunArtifactKind::parse(&artifact.kind)
            .filter(|kind| *kind != AutomationRunArtifactKind::Manifest)
            .ok_or_else(|| {
                config_error(format!("invalid manifested artifact '{}'", artifact.kind))
            })?;
        if !seen.insert(kind.as_str()) || artifact.path != artifact_relative_path(run_id, kind) {
            return Err(config_error(format!(
                "artifact manifest contains a duplicate or non-canonical '{}' entry",
                artifact.kind
            )));
        }
        read_run_artifact_payload(dashboard_root, run_id, artifact).await?;
    }
    if artifacts.len() != expected.len()
        || expected.iter().any(|kind| !seen.contains(kind.as_str()))
    {
        return Err(config_error(
            "artifact manifest does not contain the complete chain",
        ));
    }
    let created_at = artifacts
        .first()
        .map(|artifact| artifact.created_at.clone())
        .unwrap_or_default();
    artifacts.push(AutomationRunArtifact {
        schema_version: 1,
        kind: AutomationRunArtifactKind::Manifest.as_str().to_string(),
        path: artifact_relative_path(run_id, AutomationRunArtifactKind::Manifest),
        sha256: super::artifact_refs::sha256_bytes(&bytes),
        summary: Some("canonical atomic artifact manifest".to_string()),
        created_at,
    });
    let dashboard_root = dashboard_root.to_path_buf();
    let run_directory = manifest_path
        .parent()
        .ok_or_else(|| config_error("artifact manifest is missing its run directory"))?
        .to_path_buf();
    tokio::task::spawn_blocking(move || {
        sync_directory(&run_directory)?;
        let artifacts_root = run_directory
            .parent()
            .ok_or_else(|| config_error("artifact run is missing its root"))?;
        sync_directory(artifacts_root)?;
        sync_directory(&dashboard_root)
    })
    .await
    .map_err(|error| {
        config_error(format!("failed to join artifact durability fence: {error}"))
    })??;
    Ok(Some(artifacts))
}

pub(crate) async fn publish_run_artifact_chain(
    dashboard_root: &Path,
    run_id: &str,
    artifacts: Vec<(AutomationRunArtifact, Vec<u8>)>,
) -> Result<()> {
    validate_run_id_component(run_id)?;
    let dashboard_root = dashboard_root.to_path_buf();
    let run_id = run_id.to_string();
    tokio::task::spawn_blocking(move || {
        publish_run_artifact_chain_blocking(&dashboard_root, &run_id, &artifacts)
    })
    .await
    .map_err(|error| config_error(format!("failed to join artifact publication: {error}")))?
}

fn publish_run_artifact_chain_blocking(
    dashboard_root: &Path,
    run_id: &str,
    artifacts: &[(AutomationRunArtifact, Vec<u8>)],
) -> Result<()> {
    publish_run_artifact_chain_blocking_with_fault(dashboard_root, run_id, artifacts, &mut |_| {
        Ok(())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactPublishBoundary {
    BeforeArtifact(usize),
    AfterArtifactSync(usize),
    BeforeStageSync,
    BeforeRename,
    AfterRename,
}

fn publish_run_artifact_chain_blocking_with_fault(
    dashboard_root: &Path,
    run_id: &str,
    artifacts: &[(AutomationRunArtifact, Vec<u8>)],
    fault: &mut impl FnMut(ArtifactPublishBoundary) -> Result<()>,
) -> Result<()> {
    use std::io::Write;

    let artifacts_root = dashboard_root.join(RUN_ARTIFACTS_DIR);
    std::fs::create_dir_all(dashboard_root).map_err(|error| {
        config_error(format!(
            "failed to create dashboard root '{}': {error}",
            dashboard_root.display()
        ))
    })?;
    crate::storage::reject_symlink_components(&artifacts_root, "automation artifact")
        .map_err(TraceDecayError::from)?;
    std::fs::create_dir_all(&artifacts_root).map_err(|error| {
        config_error(format!(
            "failed to create artifact root '{}': {error}",
            artifacts_root.display()
        ))
    })?;
    sync_directory(dashboard_root)?;
    let mut kinds = std::collections::BTreeSet::new();
    for (artifact, bytes) in artifacts {
        let kind = AutomationRunArtifactKind::parse(&artifact.kind)
            .ok_or_else(|| config_error(format!("unknown artifact kind '{}'", artifact.kind)))?;
        if !kinds.insert(kind.as_str()) {
            return Err(config_error(format!(
                "artifact chain contains duplicate kind '{}'",
                artifact.kind
            )));
        }
        if artifact.path != artifact_relative_path(run_id, kind) {
            return Err(config_error(format!(
                "artifact '{}' does not use its canonical path",
                artifact.kind
            )));
        }
        if artifact.sha256 != super::artifact_refs::sha256_bytes(bytes) {
            return Err(config_error(format!(
                "artifact '{}' metadata hash does not match its bytes",
                artifact.kind
            )));
        }
    }
    let final_directory = artifacts_root.join(run_id);
    crate::storage::reject_symlink_components(&final_directory, "automation artifact run")
        .map_err(TraceDecayError::from)?;
    let chain_matches = |directory: &Path| -> Result<bool> {
        for (artifact, expected) in artifacts {
            let path = artifact_path_from_relative(dashboard_root, run_id, &artifact.path)?;
            let filename = path
                .file_name()
                .ok_or_else(|| config_error("artifact path is missing a filename"))?;
            match std::fs::read(directory.join(filename)) {
                Ok(actual) if actual.as_slice() == expected.as_slice() => {}
                Ok(_) => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(config_error(format!(
                        "failed to verify artifact chain '{}': {error}",
                        directory.display()
                    )));
                }
            }
        }
        Ok(true)
    };
    if final_directory.exists() {
        return if chain_matches(&final_directory)? {
            sync_directory(&final_directory)?;
            sync_directory(&artifacts_root)?;
            sync_directory(dashboard_root)?;
            Ok(())
        } else {
            Err(config_error(format!(
                "artifact chain '{}' already exists with different content",
                final_directory.display()
            )))
        };
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stage_directory =
        artifacts_root.join(format!(".stage-{run_id}-{}-{nonce}", std::process::id()));
    let mut publish = || -> Result<()> {
        std::fs::create_dir(&stage_directory).map_err(|error| {
            config_error(format!(
                "failed to create artifact stage '{}': {error}",
                stage_directory.display()
            ))
        })?;
        for (index, (artifact, bytes)) in artifacts.iter().enumerate() {
            fault(ArtifactPublishBoundary::BeforeArtifact(index))?;
            let destination = artifact_path_from_relative(dashboard_root, run_id, &artifact.path)?;
            let filename = destination
                .file_name()
                .ok_or_else(|| config_error("artifact path is missing a filename"))?;
            let staged = stage_directory.join(filename);
            let mut file = std::fs::File::create(&staged).map_err(|error| {
                config_error(format!(
                    "failed to create staged artifact '{}': {error}",
                    staged.display()
                ))
            })?;
            file.write_all(bytes).map_err(|error| {
                config_error(format!(
                    "failed to write staged artifact '{}': {error}",
                    staged.display()
                ))
            })?;
            file.sync_all().map_err(|error| {
                config_error(format!(
                    "failed to sync staged artifact '{}': {error}",
                    staged.display()
                ))
            })?;
            fault(ArtifactPublishBoundary::AfterArtifactSync(index))?;
        }
        fault(ArtifactPublishBoundary::BeforeStageSync)?;
        sync_directory(&stage_directory)?;
        fault(ArtifactPublishBoundary::BeforeRename)?;
        std::fs::rename(&stage_directory, &final_directory).map_err(|error| {
            config_error(format!(
                "failed to publish artifact chain '{}': {error}",
                final_directory.display()
            ))
        })?;
        fault(ArtifactPublishBoundary::AfterRename)?;
        sync_directory(&artifacts_root)
    };
    if let Err(error) = publish() {
        let _ = std::fs::remove_dir_all(&stage_directory);
        return Err(error);
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    tracedecay_application::sync_directory(path, DirectorySyncPolicy::Strict).map_err(|error| {
        config_error(format!(
            "failed to sync artifact directory '{}': {error}",
            path.display()
        ))
    })
}

pub async fn read_run_artifact_payload(
    dashboard_root: &Path,
    run_id: &str,
    artifact: &AutomationRunArtifact,
) -> Result<Value> {
    let kind = AutomationRunArtifactKind::parse(&artifact.kind)
        .ok_or_else(|| config_error(format!("unknown artifact kind '{}'", artifact.kind)))?;
    if artifact.path != artifact_relative_path(run_id, kind) {
        return Err(config_error(format!(
            "artifact '{}' does not use its canonical path",
            artifact.kind
        )));
    }
    let path = artifact_path_from_relative(dashboard_root, run_id, &artifact.path)?;
    crate::storage::reject_symlink_components(&path, "automation artifact")
        .map_err(TraceDecayError::from)?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        config_error(format!(
            "failed to read automation run artifact '{}': {e}",
            path.display()
        ))
    })?;
    let actual_hash = super::artifact_refs::sha256_bytes(&bytes);
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
            )));
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
    let line = serde_json::to_string(record).map_err(TraceDecayError::from)?;
    let write_path = path.clone();
    tokio::task::spawn_blocking(move || append_jsonl_line_locked(&write_path, &line))
        .await
        .map_err(|e| config_error(format!("failed to join automation run ledger write: {e}")))?
        .map_err(|e| {
            config_error(format!(
                "failed to write automation run ledger '{}': {e}",
                path.display()
            ))
        })?;
    Ok(())
}

fn append_jsonl_line_locked(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::storage::retry_transient_file_op(|| {
        let lock_path = crate::storage::append_lock_path(path);
        let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path)?;
        let write_result: std::io::Result<()> = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(path)?;
            let file_len = file.metadata()?.len();
            let mut requested = RUN_LEDGER_TAIL_CHUNK_BYTES.max(1);
            let duplicate = loop {
                let window = requested.min(file_len);
                let start = file_len.saturating_sub(window);
                file.seek(SeekFrom::Start(start))?;
                let mut tail = vec![0u8; window as usize];
                file.read_exact(&mut tail)?;
                let complete = if start == 0 {
                    tail.as_slice()
                } else {
                    tail.iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(&[][..], |newline| &tail[newline + 1..])
                };
                if let Some(newest) = complete
                    .split(|byte| *byte == b'\n')
                    .rev()
                    .find(|candidate| !candidate.is_empty())
                {
                    break newest == line.as_bytes();
                }
                if start == 0 {
                    break false;
                }
                requested = requested.saturating_mul(2);
            };
            if duplicate {
                file.sync_all()?;
            } else {
                file.write_all(format!("{line}\n").as_bytes())?;
                file.sync_all()?;
            }
            #[cfg(unix)]
            if let Some(parent) = path.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        let unlock_result = fs2::FileExt::unlock(&lock);
        write_result?;
        unlock_result?;
        Ok(())
    })
}

/// Loads up to `limit` of the newest ledger records, deduplicated by
/// `run_id` (keeping the latest lifecycle row for each run), newest first.
///
/// The ledger is append-only and grows without bound, so this reads only the
/// tail of the file rather than the whole thing: a bounded window is read
/// backwards from the end and doubled on demand until it yields `limit`
/// distinct records or reaches the start of the file. The per-tick scheduler
/// gate calls this every few seconds, so the cost is kept proportional to
/// `limit` instead of the ledger length.
pub async fn load_run_records(
    dashboard_root: &Path,
    limit: usize,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let path = run_ledger_path(dashboard_root);
    let read_path = path.clone();
    tokio::task::spawn_blocking(move || read_run_records_tail(&read_path, limit))
        .await
        .map_err(|e| config_error(format!("failed to join automation run ledger read: {e}")))?
}

pub async fn load_run_records_for_task_key(
    dashboard_root: &Path,
    requested_task_key: &str,
    limit: usize,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let path = run_ledger_path(dashboard_root);
    let task_key = requested_task_key.to_string();
    tokio::task::spawn_blocking(move || {
        read_run_records_tail_with_filter(
            &path,
            limit,
            RUN_LEDGER_TAIL_CHUNK_BYTES,
            &RunRecordFilter::TaskKey(task_key),
        )
    })
    .await
    .map_err(|e| config_error(format!("failed to join automation task ledger read: {e}")))?
}

enum RunRecordFilter {
    Any,
    TaskKey(String),
}

impl RunRecordFilter {
    fn matches(&self, record: &AutomationRunLedgerRecord) -> bool {
        match self {
            Self::Any => true,
            Self::TaskKey(requested) => {
                record
                    .task_key
                    .as_deref()
                    .unwrap_or_else(|| canonical_task_key(record.task))
                    == requested
            }
        }
    }
}

/// Reads the tail of the ledger and parses the newest `limit` distinct
/// records. Only complete lines are parsed: when the read window does not
/// begin at the start of the file, the (possibly truncated) leading line is
/// dropped so a chunk boundary never masquerades as a malformed row.
fn read_run_records_tail(path: &Path, limit: usize) -> Result<Vec<AutomationRunLedgerRecord>> {
    read_run_records_tail_with_window(path, limit, RUN_LEDGER_TAIL_CHUNK_BYTES)
}

fn read_run_records_tail_with_window(
    path: &Path,
    limit: usize,
    initial_window: u64,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    read_run_records_tail_with_filter(path, limit, initial_window, &RunRecordFilter::Any)
}

fn read_run_records_tail_with_filter(
    path: &Path,
    limit: usize,
    initial_window: u64,
    filter: &RunRecordFilter,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(config_error(format!(
                "failed to read automation run ledger '{}': {e}",
                path.display()
            )));
        }
    };
    let file_len = file
        .metadata()
        .map_err(|e| {
            config_error(format!(
                "failed to inspect automation run ledger '{}': {e}",
                path.display()
            ))
        })?
        .len();
    if file_len == 0 {
        return Ok(Vec::new());
    }

    let mut requested = initial_window.max(1);
    loop {
        let window = requested.min(file_len);
        let start = file_len - window;
        let reached_start = start == 0;
        file.seek(SeekFrom::Start(start)).map_err(|e| {
            config_error(format!(
                "failed to seek automation run ledger '{}': {e}",
                path.display()
            ))
        })?;
        let mut buf = vec![0u8; window as usize];
        file.read_exact(&mut buf).map_err(|e| {
            config_error(format!(
                "failed to read automation run ledger '{}': {e}",
                path.display()
            ))
        })?;

        // Drop a partial leading line unless the window covers the whole file,
        // so we never parse a byte-truncated JSON row as malformed.
        let slice: &[u8] = if reached_start {
            &buf
        } else {
            match buf.iter().position(|&byte| byte == b'\n') {
                Some(newline) => &buf[newline + 1..],
                // No line boundary inside the window: grow and retry.
                None => &[],
            }
        };
        let text = String::from_utf8_lossy(slice);
        let records = parse_run_records_newest_first(&text, limit, path, filter);
        if records.len() >= limit || reached_start {
            return Ok(records);
        }
        requested = requested.saturating_mul(2);
    }
}

/// Parses `text` (a suffix of the ledger containing only complete lines) into
/// up to `limit` distinct records, newest first, deduplicated by `run_id`.
fn parse_run_records_newest_first(
    text: &str,
    limit: usize,
    path: &Path,
    filter: &RunRecordFilter,
) -> Vec<AutomationRunLedgerRecord> {
    let mut records = Vec::new();
    let mut seen_run_ids = std::collections::BTreeSet::new();
    for line in text.lines().rev() {
        if records.len() >= limit {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<AutomationRunLedgerRecord>(trimmed) {
            Ok(record) => {
                if !filter.matches(&record) {
                    continue;
                }
                if !seen_run_ids.insert(record.run_id.clone()) {
                    continue;
                }
                records.push(record);
            }
            Err(err) => {
                tracing::warn!(
                    automation_run_ledger = %path.display(),
                    error = %err,
                    "skipping malformed automation run ledger jsonl row"
                );
            }
        }
    }
    records
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
                )));
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A minimal valid ledger line for `run_id`, ordered by `completed_at`.
    fn ledger_line(run_id: &str, completed_at: i64) -> String {
        format!(
            "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
             \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"succeeded\",\
             \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"{completed_at}\",\
             \"completed_at\":\"{completed_at}\"}}"
        )
    }

    fn write_ledger(lines: &[String]) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(&path, body).unwrap();
        (temp, path)
    }

    #[test]
    fn tail_read_returns_newest_limit_in_order() {
        let lines: Vec<String> = (0..10)
            .map(|i| ledger_line(&format!("run-{i}"), 1000 + i))
            .collect();
        let (_temp, path) = write_ledger(&lines);

        let records = read_run_records_tail_with_window(&path, 3, 64).unwrap();
        let ids: Vec<&str> = records.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["run-9", "run-8", "run-7"]);
    }

    #[test]
    fn tail_read_grows_window_until_limit_satisfied() {
        // Many records, a deliberately tiny initial window that holds far
        // fewer than the requested limit: the grow loop must widen until the
        // limit is met without ever mis-parsing a chunk-boundary line.
        let lines: Vec<String> = (0..50)
            .map(|i| ledger_line(&format!("run-{i:03}"), 2000 + i))
            .collect();
        let (_temp, path) = write_ledger(&lines);

        let records = read_run_records_tail_with_window(&path, 40, 32).unwrap();
        assert_eq!(records.len(), 40);
        assert_eq!(records[0].run_id, "run-049");
        assert_eq!(records[39].run_id, "run-010");
    }

    #[test]
    fn tail_read_dedups_by_run_id_keeping_newest_and_grows_for_distinct_count() {
        // Each run has two lifecycle rows sharing a run_id; dedup keeps the
        // newest, so satisfying `limit` distinct runs forces the window to
        // grow past `limit` raw lines.
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(ledger_line(&format!("run-{i:02}"), 3000 + i * 2));
            lines.push(ledger_line(&format!("run-{i:02}"), 3000 + i * 2 + 1));
        }
        let (_temp, path) = write_ledger(&lines);

        let records = read_run_records_tail_with_window(&path, 5, 40).unwrap();
        let ids: Vec<&str> = records.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["run-19", "run-18", "run-17", "run-16", "run-15"]);
    }

    #[test]
    fn tail_read_skips_malformed_lines_without_truncation_false_positives() {
        let lines = vec![
            ledger_line("older", 100),
            "not json".to_string(),
            ledger_line("newest", 200),
        ];
        let (_temp, path) = write_ledger(&lines);

        let records = read_run_records_tail_with_window(&path, 1, 8).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "newest");
    }

    #[test]
    fn tail_read_handles_missing_and_empty_ledger() {
        let temp = tempfile::TempDir::new().unwrap();
        let missing = temp.path().join(RUN_LEDGER_FILENAME);
        assert!(
            read_run_records_tail_with_window(&missing, 10, 64)
                .unwrap()
                .is_empty()
        );

        std::fs::write(&missing, b"").unwrap();
        assert!(
            read_run_records_tail_with_window(&missing, 10, 64)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn durable_append_deduplicates_large_unicode_terminal_rows() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let line = format!(
            "{{\"run_id\":\"large-unicode\",\"payload\":\"{}🧪{}\"}}",
            "a".repeat(RUN_LEDGER_TAIL_CHUNK_BYTES as usize),
            "b".repeat(1024),
        );

        append_jsonl_line_locked(&path, &line).unwrap();
        append_jsonl_line_locked(&path, &line).unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert_eq!(contents.lines().next(), Some(line.as_str()));
    }

    #[tokio::test]
    async fn task_key_read_finds_record_behind_more_than_two_hundred_unrelated_runs() {
        let mut lines = vec![ledger_line("skill-target", 1).replace(
            "\"task\":\"memory_curator\"",
            "\"task\":\"skill_writer\",\"task_key\":\"skill_writer\"",
        )];
        lines.extend((0..250).map(|index| ledger_line(&format!("unrelated-{index}"), 2 + index)));
        let (temp, _path) = write_ledger(&lines);

        let records = load_run_records_for_task_key(temp.path(), "skill_writer", 1)
            .await
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "skill-target");
    }

    fn test_artifact_chain(run_id: &str) -> Vec<(AutomationRunArtifact, Vec<u8>)> {
        [
            (
                AutomationRunArtifactKind::Traces,
                serde_json::json!({"trace": 1}),
            ),
            (
                AutomationRunArtifactKind::Feedback,
                serde_json::json!({"feedback": 2}),
            ),
            (
                AutomationRunArtifactKind::Manifest,
                serde_json::json!({"manifest": 3}),
            ),
        ]
        .into_iter()
        .map(|(kind, payload)| prepare_run_artifact(run_id, kind, &payload, None, "1").unwrap())
        .collect()
    }

    #[test]
    fn artifact_publication_faults_never_expose_a_partial_final_chain() {
        let boundaries = [
            ArtifactPublishBoundary::BeforeArtifact(0),
            ArtifactPublishBoundary::BeforeArtifact(1),
            ArtifactPublishBoundary::BeforeArtifact(2),
            ArtifactPublishBoundary::AfterArtifactSync(0),
            ArtifactPublishBoundary::AfterArtifactSync(1),
            ArtifactPublishBoundary::AfterArtifactSync(2),
            ArtifactPublishBoundary::BeforeStageSync,
            ArtifactPublishBoundary::BeforeRename,
        ];
        for (index, boundary) in boundaries.into_iter().enumerate() {
            let temp = tempfile::TempDir::new().unwrap();
            let run_id = format!("fault-{index}");
            let artifacts = test_artifact_chain(&run_id);
            let error = publish_run_artifact_chain_blocking_with_fault(
                temp.path(),
                &run_id,
                &artifacts,
                &mut |current| {
                    if current == boundary {
                        Err(config_error("injected artifact publication fault"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("injected"));
            assert!(!temp.path().join(RUN_ARTIFACTS_DIR).join(&run_id).exists());
            assert!(!run_ledger_path(temp.path()).exists());
        }
    }

    #[test]
    fn post_rename_fault_leaves_a_complete_idempotent_chain_without_a_receipt() {
        let temp = tempfile::TempDir::new().unwrap();
        let run_id = "post-rename-fault";
        let artifacts = test_artifact_chain(run_id);

        publish_run_artifact_chain_blocking_with_fault(
            temp.path(),
            run_id,
            &artifacts,
            &mut |boundary| {
                if boundary == ArtifactPublishBoundary::AfterRename {
                    Err(config_error("injected post-rename fault"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

        let final_directory = temp.path().join(RUN_ARTIFACTS_DIR).join(run_id);
        assert!(final_directory.is_dir());
        assert_eq!(
            std::fs::read_dir(&final_directory).unwrap().count(),
            artifacts.len()
        );
        assert!(!run_ledger_path(temp.path()).exists());
        publish_run_artifact_chain_blocking(temp.path(), run_id, &artifacts).unwrap();
    }

    #[test]
    fn artifact_publication_rejects_non_canonical_metadata_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut artifacts = test_artifact_chain("mismatched-path");
        artifacts[0].0.path = "automation_artifacts/mismatched-path/nested/traces.json".to_string();

        let error = publish_run_artifact_chain_blocking(temp.path(), "mismatched-path", &artifacts)
            .unwrap_err();

        assert!(error.to_string().contains("canonical path"));
        assert!(
            !temp
                .path()
                .join(RUN_ARTIFACTS_DIR)
                .join("mismatched-path")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_publication_rejects_symlinked_run_directories() {
        let temp = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let artifacts_root = temp.path().join(RUN_ARTIFACTS_DIR);
        std::fs::create_dir_all(&artifacts_root).unwrap();
        std::os::unix::fs::symlink(outside.path(), artifacts_root.join("symlink-run")).unwrap();
        let artifacts = test_artifact_chain("symlink-run");

        let error = publish_run_artifact_chain_blocking(temp.path(), "symlink-run", &artifacts)
            .unwrap_err();

        assert!(error.to_string().contains("must not contain symlinks"));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
