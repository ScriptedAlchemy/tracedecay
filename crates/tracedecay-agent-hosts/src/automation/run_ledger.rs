use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use tracedecay_automation::evidence_budget::SESSION_EVIDENCE_BUDGET_EXHAUSTED;

use super::backend::{
    AgentTaskFailureClass, AgentTaskKind, AgentTaskRetryAttempt, task_key as canonical_task_key,
};
use super::config_error;
use crate::errors::{Result, TraceDecayError};

mod cursor;
mod exact_lookup;
mod exact_publication;
mod publication;
mod scheduler_diagnostic;

pub(crate) use cursor::load_latest_task_validation_pointer;
pub use exact_lookup::{find_run_record_exact_bounded, find_run_record_exact_bounded_blocking};
pub use exact_publication::{
    ExactRunPublication, ExactRunPublishOutcome, ExactRunUnboundDiscardOutcome,
    bind_staged_run_record_exact, discard_staged_run_record_exact,
    discard_staged_run_record_exact_blocking, discard_stale_staged_run_record_exact_after_terminal,
    discard_unbound_staged_run_records_if, publish_staged_run_record_exact,
    publish_staged_run_record_exact_blocking, repair_corrupt_run_ledger_append_intent_blocking,
};
pub(crate) use publication::publish_run_artifact_chain;
pub use publication::read_published_artifact_chain;
pub(crate) use scheduler_diagnostic::append_or_reuse_scheduler_diagnostic;

const RUN_LEDGER_FILENAME: &str = "automation_runs.jsonl";
const RUN_ARTIFACTS_DIR: &str = "automation_artifacts";
/// Bounded tail window retained by durable append deduplication. Ledger
/// readers use the fixed-buffer `exact_lookup` scanner instead of allocating
/// this window or the complete append-only ledger.
const RUN_LEDGER_TAIL_CHUNK_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrigger {
    #[default]
    ManualCli,
    ManualMcp,
    Dashboard,
    Application,
    Scheduler,
    HostReceipt,
}

impl AutomationTrigger {
    /// Explicit operator-triggered runs are admitted independently of whether
    /// recurring scheduling is enabled. Backend availability, host mode,
    /// policy, cancellation, and deadline checks still apply.
    pub const fn is_on_demand(self) -> bool {
        matches!(
            self,
            Self::ManualCli | Self::ManualMcp | Self::Dashboard | Self::Application
        )
    }
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

    fn parse(value: &str) -> Option<Self> {
        match value {
            "traces" => Some(Self::Traces),
            "feedback" => Some(Self::Feedback),
            "generated_evals" => Some(Self::GeneratedEvals),
            "validation_gate" => Some(Self::ValidationGate),
            "optimizer_diagnosis" => Some(Self::OptimizerDiagnosis),
            "codex_handoff" => Some(Self::CodexHandoff),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_micros: Option<i64>,
}

impl tracedecay_automation::AutomationRunRecord for AutomationRunLedgerRecord {
    fn accepted_count(&self) -> usize {
        self.accepted_count
    }

    fn validation_report(&self) -> Option<&Value> {
        self.validation_report.as_ref()
    }

    fn applied_ops(&self) -> Option<&Value> {
        self.applied_ops.as_ref()
    }
}

pub fn run_ledger_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(RUN_LEDGER_FILENAME)
}

pub(crate) fn current_timestamp_micros() -> Result<i64> {
    timestamp_micros_at(std::time::SystemTime::now())
}

pub(crate) fn timestamp_micros_at(now: std::time::SystemTime) -> Result<i64> {
    let elapsed = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| config_error(format!("system clock predates the UNIX epoch: {error}")))?;
    i64::try_from(elapsed.as_micros())
        .map_err(|_| config_error("current UNIX timestamp does not fit in signed microseconds"))
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
    find_run_record_exact_bounded(dashboard_root, run_id).await
}

pub async fn append_run_record(
    dashboard_root: &Path,
    record: &AutomationRunLedgerRecord,
) -> Result<()> {
    let path = run_ledger_path(dashboard_root);
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
    use std::io::Write;

    if let Some(parent) = path.parent() {
        crate::storage::PrivateStoreIo::create_dir_all_durable(parent)?;
    }
    crate::storage::retry_transient_file_op(|| {
        let lock = exact_publication::acquire_run_ledger_lock(path)?;
        let write_result: std::io::Result<()> = (|| {
            let dashboard_root = path.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "automation run ledger has no parent directory",
                )
            })?;
            exact_publication::ensure_no_exact_append_intent(dashboard_root)?;
            let mut file =
                exact_publication::open_run_ledger_nofollow(path, true, false, true, true)?
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "automation run ledger disappeared during durable open",
                        )
                    })?;
            ensure_run_ledger_eof_guard(&mut file)?;
            let candidate = serde_json::from_str::<AutomationRunLedgerRecord>(line)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            validate_run_ledger_record_semantics(&candidate).map_err(run_ledger_scan_io_error)?;
            let duplicate = find_existing_ordinary_run(&file, path, &candidate, line.as_bytes())?;
            if duplicate {
                file.sync_all()?;
            } else {
                file.write_all(line.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
            }
            tracedecay_private_fs::framed_log::sync_parent_directory(
                path,
                tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
            )
        })();
        let unlock_result = fs2::FileExt::unlock(&lock);
        write_result?;
        unlock_result?;
        Ok(())
    })
}

fn find_existing_ordinary_run(
    file: &std::fs::File,
    path: &Path,
    candidate: &AutomationRunLedgerRecord,
    candidate_bytes: &[u8],
) -> std::io::Result<bool> {
    let mut rows =
        exact_lookup::ForwardJsonlScanner::new(file, path).map_err(run_ledger_scan_io_error)?;
    let mut duplicate = false;
    let mut newest_status = None;
    let mut newest_completion = None;
    let mut status_spans: [Option<std::ops::Range<u64>>; 5] = std::array::from_fn(|_| None);
    while let Some(span) = rows.next_span().map_err(run_ledger_scan_io_error)? {
        let Some(projection) = exact_lookup::scan_jsonl_row(file, path, span.clone())
            .map_err(run_ledger_scan_io_error)?
        else {
            continue;
        };
        if projection.run_id != candidate.run_id {
            continue;
        }
        let projection_task_key = projection
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(projection.task));
        let candidate_task_key = effective_record_task_key(candidate);
        if projection.task != candidate.task
            || projection_task_key != candidate_task_key
            || projection.trigger != candidate.trigger
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "automation run ledger run '{}' mutates immutable admission identity",
                    candidate.run_id
                ),
            ));
        }
        let matches_candidate = projection.status == candidate.status
            && exact_lookup::span_matches_bytes(file, &projection.span, candidate_bytes)?;
        let status_index = run_status_index(projection.status);
        if let Some(canonical_span) = status_spans[status_index].as_ref() {
            let same_existing_state =
                exact_lookup::spans_match(file, path, canonical_span, &projection.span)
                    .map_err(run_ledger_scan_io_error)?;
            if !same_existing_state {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "automation run ledger run '{}' repeats a conflicting lifecycle state",
                        candidate.run_id
                    ),
                ));
            }
            duplicate |= matches_candidate;
            continue;
        }
        if !valid_run_status_transition(newest_status, projection.status) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "automation run ledger contains lifecycle rows after terminal run '{}'",
                    candidate.run_id
                ),
            ));
        }
        let completion = exact_lookup::canonical_completion_key(&projection)
            .map_err(run_ledger_scan_io_error)?;
        let completion = (completion.0, completion.1);
        if newest_completion.is_some_and(|previous| completion < previous) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "automation run ledger run '{}' regresses its completion timestamp",
                    candidate.run_id
                ),
            ));
        }
        duplicate |= matches_candidate;
        status_spans[status_index] = Some(projection.span);
        newest_status = Some(projection.status);
        newest_completion = Some(completion);
    }
    if duplicate {
        return Ok(true);
    }
    let candidate_completion = canonical_completion_parts(
        candidate.schema_version,
        &candidate.completed_at,
        candidate.completed_at_micros,
    )
    .map_err(run_ledger_scan_io_error)?;
    if newest_completion.is_some_and(|previous| candidate_completion < previous) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "automation run ledger run '{}' regresses its completion timestamp",
                candidate.run_id
            ),
        ));
    }
    let legal = valid_run_status_transition(newest_status, candidate.status);
    if legal {
        Ok(false)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "automation run ledger run '{}' has an invalid lifecycle transition",
                candidate.run_id
            ),
        ))
    }
}

pub(super) fn run_status_index(status: AutomationRunStatus) -> usize {
    match status {
        AutomationRunStatus::Queued => 0,
        AutomationRunStatus::Running => 1,
        AutomationRunStatus::Succeeded => 2,
        AutomationRunStatus::Failed => 3,
        AutomationRunStatus::Skipped => 4,
    }
}

pub(super) fn valid_run_status_transition(
    previous: Option<AutomationRunStatus>,
    next: AutomationRunStatus,
) -> bool {
    match previous {
        None => true,
        Some(AutomationRunStatus::Queued) => {
            next == AutomationRunStatus::Running || next.is_terminal()
        }
        Some(AutomationRunStatus::Running) => next.is_terminal(),
        Some(status) if status.is_terminal() => false,
        Some(_) => false,
    }
}

pub(super) fn canonical_completion_parts(
    schema_version: u32,
    completed_at: &str,
    completed_at_micros: Option<i64>,
) -> Result<(i64, i64)> {
    let (completed_at, canonical_micros) = match schema_version {
        1 => parse_schema_v1_rfc3339_micros(completed_at, "completion timestamp")?,
        2 => {
            let completed_at =
                parse_nonnegative_unix_integer(completed_at, "schema-v2 completion timestamp")?;
            let canonical_micros = completed_at.checked_mul(1_000_000).ok_or_else(|| {
                config_error("automation completion timestamp overflows signed microseconds")
            })?;
            (completed_at, canonical_micros)
        }
        _ => {
            return Err(config_error(format!(
                "automation run ledger schema version {schema_version} is unsupported"
            )));
        }
    };
    let completed_at_micros = completed_at_micros.unwrap_or(canonical_micros);
    if completed_at_micros < 0 {
        return Err(config_error(
            "automation completion timestamp predates the UNIX epoch",
        ));
    }
    let consistent = match schema_version {
        1 => completed_at_micros == canonical_micros,
        2 => completed_at_micros.div_euclid(1_000_000) == completed_at,
        _ => false,
    };
    if !consistent {
        return Err(config_error(
            "automation completion timestamp seconds and microseconds disagree",
        ));
    }
    Ok((completed_at, completed_at_micros))
}

/// Returns one record's exact schema-aware completion instant in Unix
/// microseconds.
pub fn canonical_record_completion_micros(record: &AutomationRunLedgerRecord) -> Result<i64> {
    canonical_completion_parts(
        record.schema_version,
        &record.completed_at,
        record.completed_at_micros,
    )
    .map(|(_, completed_at_micros)| completed_at_micros)
}

pub(super) fn canonical_record_started_at_seconds(
    record: &AutomationRunLedgerRecord,
    label: &str,
) -> Result<i64> {
    canonical_started_at_seconds(record.schema_version, &record.started_at, label)
}

pub(super) fn canonical_started_at_seconds(
    schema_version: u32,
    started_at: &str,
    label: &str,
) -> Result<i64> {
    match schema_version {
        1 => tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp(started_at).ok_or_else(
            || {
                config_error(format!(
                    "automation schema-v1 {label} '{started_at}' is not valid RFC3339"
                ))
            },
        ),
        2 => parse_nonnegative_unix_integer(started_at, label),
        schema_version => Err(config_error(format!(
            "automation run ledger schema version {schema_version} is unsupported"
        ))),
    }
}

pub(super) fn validate_run_ledger_record_semantics(
    record: &AutomationRunLedgerRecord,
) -> Result<()> {
    canonical_record_started_at_seconds(record, "ledger row started_at")?;
    canonical_completion_parts(
        record.schema_version,
        &record.completed_at,
        record.completed_at_micros,
    )
    .map(|_| ())
}

fn parse_schema_v1_rfc3339_micros(value: &str, label: &str) -> Result<(i64, i64)> {
    let seconds =
        tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp(value).ok_or_else(|| {
            config_error(format!(
                "automation schema-v1 {label} '{value}' is not valid RFC3339"
            ))
        })?;
    let fraction_micros = rfc3339_fraction_micros(value, label)?;
    let micros = seconds
        .checked_mul(1_000_000)
        .and_then(|whole| whole.checked_add(fraction_micros))
        .ok_or_else(|| {
            config_error(format!(
                "automation schema-v1 {label} overflows signed microseconds"
            ))
        })?;
    Ok((seconds, micros))
}

fn rfc3339_fraction_micros(value: &str, label: &str) -> Result<i64> {
    let Some(dot) = value.find('.') else {
        return Ok(0);
    };
    let digits = value.as_bytes()[dot + 1..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit());
    let mut micros = 0_i64;
    let mut count = 0_usize;
    for digit in digits {
        count += 1;
        if count <= 6 {
            micros = micros * 10 + i64::from(*digit - b'0');
        } else if *digit != b'0' {
            return Err(config_error(format!(
                "automation schema-v1 {label} has precision finer than exact microseconds"
            )));
        }
    }
    if count == 0 {
        return Err(config_error(format!(
            "automation schema-v1 {label} has an empty fractional component"
        )));
    }
    for _ in count..6 {
        micros *= 10;
    }
    Ok(micros)
}

fn parse_nonnegative_unix_integer(value: &str, label: &str) -> Result<i64> {
    match value.parse::<i64>() {
        Ok(seconds) if seconds >= 0 => Ok(seconds),
        Ok(_) => Err(config_error(format!(
            "automation {label} predates the UNIX epoch"
        ))),
        Err(integer_error) => Err(config_error(format!(
            "automation {label} '{value}' is not nonnegative Unix seconds: {integer_error}"
        ))),
    }
}

/// Selects the latest candidate using the ledger's status-neutral completion
/// order. Conflicting states at the winning canonical identity make the
/// selection invalid; conflicts superseded by a strictly later identity do not
/// affect the selected result. Returning parsed seconds with the record keeps
/// schedule arithmetic on the same validated timestamp that established the
/// winner.
pub(super) type CanonicalCompletionKey<'a> = (i64, i64, &'a str);

pub(super) fn latest_record_by_canonical_completion_key<'a>(
    records: impl IntoIterator<Item = &'a AutomationRunLedgerRecord>,
) -> Result<Option<(&'a AutomationRunLedgerRecord, CanonicalCompletionKey<'a>)>> {
    let mut latest = None;
    for record in records {
        validate_run_id_component(&record.run_id)?;
        let (completed_at, completed_at_micros) = canonical_completion_parts(
            record.schema_version,
            &record.completed_at,
            record.completed_at_micros,
        )?;
        let candidate_key = (completed_at, completed_at_micros, record.run_id.as_str());
        match latest.as_mut() {
            None => latest = Some((record, completed_at, completed_at_micros, false)),
            Some((current, current_completed_at, current_completed_at_micros, conflict)) => {
                let current_key = (
                    *current_completed_at,
                    *current_completed_at_micros,
                    current.run_id.as_str(),
                );
                if candidate_key == current_key && record != *current {
                    *conflict = true;
                } else if candidate_key > current_key {
                    latest = Some((record, completed_at, completed_at_micros, false));
                }
            }
        }
    }
    match latest {
        Some((record, _, _, true)) => Err(config_error(format!(
            "automation history repeats run '{}' with conflicting canonical state",
            record.run_id
        ))),
        Some((record, completed_at, completed_at_micros, false)) => Ok(Some((
            record,
            (completed_at, completed_at_micros, record.run_id.as_str()),
        ))),
        None => Ok(None),
    }
}

pub(super) fn latest_record_by_canonical_completion<'a>(
    records: impl IntoIterator<Item = &'a AutomationRunLedgerRecord>,
) -> Result<Option<(&'a AutomationRunLedgerRecord, i64)>> {
    latest_record_by_canonical_completion_key(records)
        .map(|latest| latest.map(|(record, (completed_at, _, _))| (record, completed_at)))
}

fn run_ledger_scan_io_error(error: TraceDecayError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

pub(super) fn ensure_run_ledger_eof_guard(file: &mut std::fs::File) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "automation run ledger has an incomplete durable tail",
        ))
    }
}

pub(super) fn sync_run_ledger_file_and_parent(path: &Path, file: &std::fs::File) -> Result<()> {
    file.sync_all().map_err(TraceDecayError::from)?;
    tracedecay_private_fs::framed_log::sync_parent_directory(
        path,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )
    .map_err(TraceDecayError::from)
}

/// Loads up to `limit` of the most recently touched runs, newest first.
///
/// A byte-identical retry refreshes a run's physical ordering without
/// regressing the logical lifecycle state returned for that run.
///
/// The ledger is append-only and grows without bound, so rows are located from
/// the tail using fixed-size scan buffers. Full JSON records are decoded only
/// after their bounded identity projection passes the filter and dedup checks.
pub async fn load_run_records(
    dashboard_root: &Path,
    limit: usize,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let root = dashboard_root.to_path_buf();
    let path = run_ledger_path(dashboard_root);
    let read_path = path.clone();
    tokio::task::spawn_blocking(move || {
        with_run_ledger_read_lock(&root, &read_path, || {
            read_run_records_tail(&read_path, limit)
        })
    })
    .await
    .map_err(|e| config_error(format!("failed to join automation run ledger read: {e}")))?
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutomationRunLedgerPageV1 {
    pub records: Vec<AutomationRunLedgerRecord>,
    pub malformed_row_count: usize,
    pub has_more: bool,
}

impl AutomationRunLedgerPageV1 {
    pub fn is_complete(&self) -> bool {
        !self.has_more && self.malformed_row_count == 0
    }
}

/// Fixed-memory scheduler authority for one exact `(task, task_key)` identity.
///
/// Unlike operator pages, this summary ranks effectful rows by their durable
/// completion timestamp across the complete ledger. Logical activity uses the
/// same canonical ordering, so byte-identical retries never move the summary.
#[derive(Debug, Clone, Default)]
pub struct AutomationRunLedgerTaskSummary {
    records: Vec<AutomationRunLedgerRecord>,
    latest_logical_activity: Option<usize>,
    latest_successful: Option<usize>,
    latest_effectful_any_trigger: Option<usize>,
    latest_scheduler_effectful: Option<usize>,
    latest_scheduler_activity: Option<usize>,
    latest_session_evidence_budget_exhausted: Option<usize>,
}

impl AutomationRunLedgerTaskSummary {
    pub fn records(&self) -> &[AutomationRunLedgerRecord] {
        &self.records
    }

    pub fn latest_successful(&self) -> Option<&AutomationRunLedgerRecord> {
        self.latest_successful.map(|index| &self.records[index])
    }

    pub fn latest_logical_activity(&self) -> Option<&AutomationRunLedgerRecord> {
        self.latest_logical_activity
            .map(|index| &self.records[index])
    }

    pub fn latest_effectful_any_trigger(&self) -> Option<&AutomationRunLedgerRecord> {
        self.latest_effectful_any_trigger
            .map(|index| &self.records[index])
    }

    pub fn latest_scheduler_effectful(&self) -> Option<&AutomationRunLedgerRecord> {
        self.latest_scheduler_effectful
            .map(|index| &self.records[index])
    }

    pub fn latest_scheduler_activity(&self) -> Option<&AutomationRunLedgerRecord> {
        self.latest_scheduler_activity
            .map(|index| &self.records[index])
    }

    /// The most recent skip that observed session-evidence budget exhaustion.
    ///
    /// Standing exhaustion is anchored here even after a newer suppression
    /// skip becomes the latest logical activity, so the scheduler backoff can
    /// keep measuring its window from the last real exhausted attempt.
    pub fn latest_session_evidence_budget_exhausted(&self) -> Option<&AutomationRunLedgerRecord> {
        self.latest_session_evidence_budget_exhausted
            .map(|index| &self.records[index])
    }

    pub fn latest_scheduler_effectful_user_job_terminal(
        &self,
    ) -> Option<&AutomationRunLedgerRecord> {
        self.latest_scheduler_effectful()
            .filter(|record| record.task == AgentTaskKind::UserJob)
    }
}

pub async fn load_run_records_page(
    dashboard_root: &Path,
    limit: usize,
) -> Result<AutomationRunLedgerPageV1> {
    let root = dashboard_root.to_path_buf();
    let path = run_ledger_path(dashboard_root);
    tokio::task::spawn_blocking(move || {
        with_run_ledger_read_lock(&root, &path, || read_run_records_tail_page(&path, limit))
    })
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
    validate_requested_task_key(requested_task_key)?;
    let root = dashboard_root.to_path_buf();
    let path = run_ledger_path(dashboard_root);
    let task_key = requested_task_key.to_string();
    tokio::task::spawn_blocking(move || {
        with_run_ledger_read_lock(&root, &path, || {
            read_run_records_tail_with_filter(
                &path,
                limit,
                RUN_LEDGER_TAIL_CHUNK_BYTES,
                &RunRecordFilter::TaskKey(task_key),
            )
        })
    })
    .await
    .map_err(|e| config_error(format!("failed to join automation task ledger read: {e}")))?
}

/// Loads the complete-ledger scheduler summary for one exact task identity.
/// The scan retains at most five row projections and fully decodes only their
/// unique selected records.
///
/// Summaries are memoized per `(ledger path, task, task_key)` and validated by
/// the ledger's length plus a digest of its final bytes, both read while the
/// exclusive ledger lock is held. Scheduler ticks and dashboard status requests
/// therefore rescan the ledger only after it actually changed; see
/// `RUN_LEDGER_SUMMARY_MEMO` for the append-only invariant this relies on.
pub async fn load_run_ledger_task_summary(
    dashboard_root: &Path,
    task: AgentTaskKind,
    requested_task_key: &str,
) -> Result<AutomationRunLedgerTaskSummary> {
    validate_requested_task_key(requested_task_key)?;
    if requested_task_key != canonical_task_key(task)
        && !requested_task_key.starts_with("user_job:")
    {
        return Err(config_error(
            "automation task summary requires an exact canonical task identity",
        ));
    }
    let root = dashboard_root.to_path_buf();
    let path = run_ledger_path(dashboard_root);
    let task_key = requested_task_key.to_owned();
    tokio::task::spawn_blocking(move || {
        with_run_ledger_read_lock(&root, &path, || {
            read_run_ledger_task_summary(&path, task, &task_key)
        })
    })
    .await
    .map_err(|error| config_error(format!("failed to join task ledger summary read: {error}")))?
}

pub async fn load_latest_scheduler_effectful_for_task_key(
    dashboard_root: &Path,
    requested_task_key: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    validate_requested_task_key(requested_task_key)?;
    let task = requested_task_key
        .starts_with("user_job:")
        .then_some(AgentTaskKind::UserJob)
        .ok_or_else(|| {
            config_error("scheduler effectful task-key lookup requires a UserJob task key")
        })?;
    load_run_ledger_task_summary(dashboard_root, task, requested_task_key)
        .await
        .map(|summary| summary.latest_scheduler_effectful().cloned())
}

fn validate_requested_task_key(task_key: &str) -> Result<()> {
    if task_key.len() > tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES {
        Err(config_error(
            "requested automation task key exceeds its byte bound",
        ))
    } else {
        Ok(())
    }
}

fn with_run_ledger_read_lock<T>(
    dashboard_root: &Path,
    path: &Path,
    read: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lock = exact_publication::acquire_run_ledger_lock(path).map_err(TraceDecayError::from)?;
    let result = (|| {
        exact_publication::ensure_no_exact_append_intent(dashboard_root)
            .map_err(TraceDecayError::from)?;
        read()
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(TraceDecayError::from);
    result.and_then(|value| unlock.map(|()| value))
}

enum RunRecordFilter {
    Any,
    TaskKey(String),
}

impl RunRecordFilter {
    fn matches_projection(&self, record: &exact_lookup::RunLedgerRowProjection) -> bool {
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

    fn matches_record(&self, record: &AutomationRunLedgerRecord) -> bool {
        match self {
            Self::Any => true,
            Self::TaskKey(requested) => effective_record_task_key(record) == requested,
        }
    }
}

/// Reads the tail of the ledger and parses the newest `limit` distinct
/// records. Line spans and identity fields use fixed memory; allocation of a
/// complete row is reserved for records that will be returned.
fn read_run_records_tail(path: &Path, limit: usize) -> Result<Vec<AutomationRunLedgerRecord>> {
    read_run_records_tail_with_window(path, limit, RUN_LEDGER_TAIL_CHUNK_BYTES)
}

fn read_run_records_tail_page(path: &Path, limit: usize) -> Result<AutomationRunLedgerPageV1> {
    read_run_records_tail_page_with_window(path, limit, RUN_LEDGER_TAIL_CHUNK_BYTES)
}

fn read_run_records_tail_with_window(
    path: &Path,
    limit: usize,
    initial_window: u64,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    read_run_records_tail_page_with_window(path, limit, initial_window).map(|page| page.records)
}

fn read_run_records_tail_page_with_window(
    path: &Path,
    limit: usize,
    initial_window: u64,
) -> Result<AutomationRunLedgerPageV1> {
    read_run_records_tail_page_with_filter(path, limit, initial_window, &RunRecordFilter::Any)
}

fn read_run_records_tail_with_filter(
    path: &Path,
    limit: usize,
    initial_window: u64,
    filter: &RunRecordFilter,
) -> Result<Vec<AutomationRunLedgerRecord>> {
    read_run_records_tail_page_with_filter(path, limit, initial_window, filter)
        .map(|page| page.records)
}

fn read_run_records_tail_page_with_filter(
    path: &Path,
    limit: usize,
    _initial_window: u64,
    filter: &RunRecordFilter,
) -> Result<AutomationRunLedgerPageV1> {
    let file = match exact_lookup::open_stabilized_run_ledger(path, false)? {
        Some(file) => file,
        None => {
            return Ok(AutomationRunLedgerPageV1 {
                records: Vec::new(),
                malformed_row_count: 0,
                has_more: false,
            });
        }
    };
    exact_lookup::ReverseJsonlScanner::new(&file, path)?;
    if limit == 0 {
        return Ok(AutomationRunLedgerPageV1 {
            records: Vec::new(),
            malformed_row_count: 0,
            has_more: file.metadata().map_err(TraceDecayError::from)?.len() != 0,
        });
    }
    if !matches!(filter, RunRecordFilter::Any) {
        return read_filtered_run_records_two_pass(&file, path, limit, filter);
    }
    read_any_run_records_page(&file, path, limit)
}

/// Maximum distinct `(ledger, task, task_key)` identities retained by the
/// scheduler summary memo. Bounded so unbounded `user_job:` task keys cannot
/// grow the process-wide map without limit.
const RUN_LEDGER_SUMMARY_MEMO_CAPACITY: usize = 64;
/// Tail window hashed as the memo's content witness alongside the file length.
const RUN_LEDGER_SUMMARY_TAIL_DIGEST_BYTES: u64 = 4096;

/// Memo key: canonical ledger path, canonical task-kind name, exact task key.
///
/// `AgentTaskKind` is defined in `tracedecay-automation` and does not derive
/// `Hash`, so the kind is keyed by [`canonical_task_key`], which is injective
/// over the kind enum and therefore carries the same identity.
type RunLedgerSummaryMemoKey = (PathBuf, &'static str, String);

/// One memoized summary plus the ledger fingerprint it was computed from.
#[derive(Clone)]
struct CachedTaskSummary {
    file_len: u64,
    tail_digest: String,
    summary: AutomationRunLedgerTaskSummary,
}

/// Process-wide memo for [`read_run_ledger_task_summary`].
///
/// The scheduler asks for one summary per task on every tick, and the
/// dashboard scheduler-status endpoint asks several times per request. Each
/// answer previously walked the entire append-only ledger twice while holding
/// the exclusive ledger lock, so per-tick cost and lock-hold time grew with
/// ledger size forever. The memo makes a repeated summary of an unchanged
/// ledger O(1).
///
/// Validation is `(file length, sha256 of the final <= 4 KiB)`:
/// * Every production mutation of `automation_runs.jsonl` either appends bytes
///   (`append_jsonl_line_locked`, `exact_publication::publish_under_ledger_lock`
///   writing from `pre_append_eof`, `scheduler_diagnostic::append_or_reuse_blocking`)
///   or truncates (`recover_matching_append_intent`'s `set_len(pre_append_eof)`,
///   `repair_corrupt_append_intent`'s `set_len(clean_eof)`). No path rewrites
///   bytes in place, and the ledger file is never atomically replaced, so a
///   changed ledger always changes its length.
/// * Length alone would still be defeated by a truncate-then-re-append that
///   happened to land on the same total length across two separate locked
///   flows, so the tail digest is compared too: a false hit would require an
///   equal-length ledger whose final 4 KiB are byte-identical yet whose
///   selected rows differ, which append-only framing does not produce.
/// * Both the length and the digest are read from the same open handle while
///   the caller holds the exclusive ledger lock (`with_run_ledger_read_lock`),
///   which also serializes cross-process writers, so there is no TOCTOU window
///   between validating the fingerprint and returning the memoized summary.
static RUN_LEDGER_SUMMARY_MEMO: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<RunLedgerSummaryMemoKey, CachedTaskSummary>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
thread_local! {
    /// Memo hits observed on the calling thread. Thread-local so tests running
    /// in parallel only ever observe their own summary reads.
    static RUN_LEDGER_SUMMARY_MEMO_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn run_ledger_summary_memo() -> std::sync::MutexGuard<
    'static,
    std::collections::HashMap<RunLedgerSummaryMemoKey, CachedTaskSummary>,
> {
    let memo = RUN_LEDGER_SUMMARY_MEMO
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    // The map holds only plain data, so a poisoned lock leaves no broken
    // invariant to recover from.
    match memo.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Memo hits observed by this thread so far; tests compare the value across a
/// pair of reads to assert that an unchanged ledger is answered without a scan.
#[cfg(test)]
fn run_ledger_summary_memo_hits() -> u64 {
    RUN_LEDGER_SUMMARY_MEMO_HITS.with(std::cell::Cell::get)
}

fn run_ledger_summary_memo_key(
    path: &Path,
    task: AgentTaskKind,
    requested_task_key: &str,
) -> RunLedgerSummaryMemoKey {
    let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    (
        canonical_path,
        canonical_task_key(task),
        requested_task_key.to_owned(),
    )
}

/// Hashes the final `min(len, 4 KiB)` bytes of the ledger. Readers always seek
/// before reading, so moving this handle's cursor is safe for later scans.
fn run_ledger_summary_tail_digest(file: &std::fs::File, file_len: u64) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let window = file_len.min(RUN_LEDGER_SUMMARY_TAIL_DIGEST_BYTES);
    let Ok(window_len) = usize::try_from(window) else {
        return Err(config_error(
            "automation run ledger tail window exceeds addressable memory",
        ));
    };
    let mut handle = file;
    handle
        .seek(SeekFrom::Start(file_len.saturating_sub(window)))
        .map_err(TraceDecayError::from)?;
    let mut tail = vec![0_u8; window_len];
    handle
        .read_exact(&mut tail)
        .map_err(TraceDecayError::from)?;
    Ok(super::artifact_refs::sha256_bytes(&tail))
}

fn cached_run_ledger_task_summary(
    key: &RunLedgerSummaryMemoKey,
    file_len: u64,
    tail_digest: &str,
) -> Option<AutomationRunLedgerTaskSummary> {
    let memo = run_ledger_summary_memo();
    let cached = memo.get(key)?;
    (cached.file_len == file_len && cached.tail_digest == tail_digest).then(|| {
        #[cfg(test)]
        RUN_LEDGER_SUMMARY_MEMO_HITS.with(|hits| hits.set(hits.get().saturating_add(1)));
        cached.summary.clone()
    })
}

fn store_run_ledger_task_summary(key: RunLedgerSummaryMemoKey, cached: CachedTaskSummary) {
    let mut memo = run_ledger_summary_memo();
    if memo.len() >= RUN_LEDGER_SUMMARY_MEMO_CAPACITY && !memo.contains_key(&key) {
        // Arbitrary eviction: every entry is equally cheap to recompute.
        if let Some(evicted) = memo.keys().next().cloned() {
            memo.remove(&evicted);
        }
    }
    memo.insert(key, cached);
}

#[derive(Default)]
struct TaskSummarySpans {
    latest_logical_activity: Option<exact_lookup::RunLedgerRowProjection>,
    latest_successful: Option<exact_lookup::RunLedgerRowProjection>,
    latest_effectful_any_trigger: Option<exact_lookup::RunLedgerRowProjection>,
    latest_scheduler_effectful: Option<exact_lookup::RunLedgerRowProjection>,
    latest_scheduler_activity: Option<exact_lookup::RunLedgerRowProjection>,
    latest_session_evidence_budget_exhausted: Option<exact_lookup::RunLedgerRowProjection>,
}

fn is_session_evidence_budget_exhausted_skip(
    status: AutomationRunStatus,
    session_evidence_budget_exhausted_error: bool,
) -> bool {
    status == AutomationRunStatus::Skipped && session_evidence_budget_exhausted_error
}

fn read_run_ledger_task_summary(
    path: &Path,
    task: AgentTaskKind,
    requested_task_key: &str,
) -> Result<AutomationRunLedgerTaskSummary> {
    let Some(file) = exact_lookup::open_stabilized_run_ledger(path, false)? else {
        return Ok(AutomationRunLedgerTaskSummary::default());
    };
    // Answer an unchanged ledger from the memo instead of rescanning it. See
    // `RUN_LEDGER_SUMMARY_MEMO` for why `(len, tail digest)` read under the
    // exclusive ledger lock is a sound witness of unchanged content.
    let file_len = file.metadata().map_err(TraceDecayError::from)?.len();
    let tail_digest = run_ledger_summary_tail_digest(&file, file_len)?;
    let memo_key = run_ledger_summary_memo_key(path, task, requested_task_key);
    if let Some(summary) = cached_run_ledger_task_summary(&memo_key, file_len, &tail_digest) {
        return Ok(summary);
    }
    let mut rows = exact_lookup::ForwardJsonlScanner::new(&file, path)?;
    let mut selected = TaskSummarySpans::default();
    while let Some(line) = rows.next_span()? {
        let Some(projection) = exact_lookup::scan_jsonl_row(&file, path, line)? else {
            continue;
        };
        let effective_task_key = projection
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(projection.task));
        if projection.task != task || effective_task_key != requested_task_key {
            continue;
        }
        exact_lookup::canonical_completion_key(&projection)?;
        select_summary_projection(&mut selected.latest_logical_activity, &projection)?;
        if projection.trigger == AutomationTrigger::Scheduler {
            select_summary_projection(&mut selected.latest_scheduler_activity, &projection)?;
        }
        if projection.status == AutomationRunStatus::Succeeded {
            select_summary_projection(&mut selected.latest_successful, &projection)?;
        }
        if matches!(
            projection.status,
            AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
        ) {
            select_summary_projection(&mut selected.latest_effectful_any_trigger, &projection)?;
            if projection.trigger == AutomationTrigger::Scheduler {
                select_summary_projection(&mut selected.latest_scheduler_effectful, &projection)?;
            }
        }
        if is_session_evidence_budget_exhausted_skip(
            projection.status,
            projection.session_evidence_budget_exhausted_error,
        ) {
            select_summary_projection(
                &mut selected.latest_session_evidence_budget_exhausted,
                &projection,
            )?;
        }
    }
    let selected_run_ids = [
        selected.latest_logical_activity.as_ref(),
        selected.latest_successful.as_ref(),
        selected.latest_effectful_any_trigger.as_ref(),
        selected.latest_scheduler_effectful.as_ref(),
        selected.latest_scheduler_activity.as_ref(),
        selected.latest_session_evidence_budget_exhausted.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|projection| projection.run_id.clone())
    .collect::<std::collections::HashSet<_>>();
    let lifecycles =
        exact_lookup::read_logical_run_lifecycles(&file, path, &selected_run_ids, true)?;
    let summary =
        decode_task_summary(&file, path, task, requested_task_key, selected, &lifecycles)?;
    store_run_ledger_task_summary(
        memo_key,
        CachedTaskSummary {
            file_len,
            tail_digest,
            summary: summary.clone(),
        },
    );
    Ok(summary)
}

fn select_summary_projection(
    slot: &mut Option<exact_lookup::RunLedgerRowProjection>,
    candidate: &exact_lookup::RunLedgerRowProjection,
) -> Result<()> {
    let candidate_key = exact_lookup::canonical_completion_key(candidate)?;
    let replace = slot
        .as_ref()
        .map(|current| exact_lookup::canonical_completion_key(current))
        .transpose()?
        .is_none_or(|current_key| candidate_key > current_key);
    if replace {
        *slot = Some(candidate.clone());
    }
    Ok(())
}

fn decode_task_summary(
    file: &std::fs::File,
    path: &Path,
    task: AgentTaskKind,
    requested_task_key: &str,
    selected: TaskSummarySpans,
    lifecycles: &std::collections::HashMap<String, exact_lookup::LogicalRunLifecycle>,
) -> Result<AutomationRunLedgerTaskSummary> {
    let mut summary = AutomationRunLedgerTaskSummary::default();
    let TaskSummarySpans {
        latest_logical_activity,
        latest_successful,
        latest_effectful_any_trigger,
        latest_scheduler_effectful,
        latest_scheduler_activity,
        latest_session_evidence_budget_exhausted,
    } = selected;
    for (projection, category) in [
        (latest_logical_activity, 0_u8),
        (latest_successful, 1),
        (latest_effectful_any_trigger, 2),
        (latest_scheduler_effectful, 3),
        (latest_scheduler_activity, 4),
        (latest_session_evidence_budget_exhausted, 5),
    ] {
        let Some(selected_projection) = projection else {
            continue;
        };
        let lifecycle = lifecycles
            .get(selected_projection.run_id.as_str())
            .ok_or_else(|| config_error("automation task summary selected run disappeared"))?;
        let projection = &lifecycle.newest;
        if exact_lookup::canonical_completion_key(projection)?
            != exact_lookup::canonical_completion_key(&selected_projection)?
        {
            return Err(config_error(
                "automation task summary lifecycle changed its selected completion order",
            ));
        }
        let projection_task_key = projection
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(projection.task));
        if projection.task != task || projection_task_key != requested_task_key {
            return Err(config_error(
                "automation task summary lifecycle changed its task identity",
            ));
        }
        let category_matches = match category {
            0 => true,
            1 => projection.status == AutomationRunStatus::Succeeded,
            2 => matches!(
                projection.status,
                AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
            ),
            3 => {
                projection.trigger == AutomationTrigger::Scheduler
                    && matches!(
                        projection.status,
                        AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
                    )
            }
            4 => projection.trigger == AutomationTrigger::Scheduler,
            5 => is_session_evidence_budget_exhausted_skip(
                projection.status,
                projection.session_evidence_budget_exhausted_error,
            ),
            _ => false,
        };
        if !category_matches {
            return Err(config_error(
                "automation task summary lifecycle changed its selected category",
            ));
        }
        let index = if let Some(index) = summary
            .records
            .iter()
            .position(|record| record.run_id == projection.run_id)
        {
            index
        } else {
            let record = exact_lookup::decode_jsonl_row(file, path, &projection.span)?;
            require_projection_identity(&record, projection)?;
            if record.task != task || effective_record_task_key(&record) != requested_task_key {
                return Err(config_error(
                    "automation task summary selection changed task identity during decode",
                ));
            }
            summary.records.push(record);
            summary.records.len() - 1
        };
        if category == 1 {
            let record = &summary.records[index];
            canonical_record_started_at_seconds(
                record,
                &format!(
                    "task summary latest successful run '{}' started_at",
                    record.run_id
                ),
            )?;
        }
        match category {
            0 => summary.latest_logical_activity = Some(index),
            1 => summary.latest_successful = Some(index),
            2 => summary.latest_effectful_any_trigger = Some(index),
            3 => summary.latest_scheduler_effectful = Some(index),
            4 => summary.latest_scheduler_activity = Some(index),
            5 => summary.latest_session_evidence_budget_exhausted = Some(index),
            _ => {
                return Err(config_error(
                    "automation task summary selected an unknown category",
                ));
            }
        }
    }
    Ok(summary)
}

fn read_any_run_records_page(
    file: &std::fs::File,
    path: &Path,
    limit: usize,
) -> Result<AutomationRunLedgerPageV1> {
    let mut lines = exact_lookup::ReverseJsonlScanner::new(file, path)?;
    let mut selected_run_order = Vec::new();
    let mut selected_run_ids = std::collections::HashSet::new();
    let mut malformed_row_count: usize = 0;
    let mut has_more = false;
    while let Some(line) = lines.next_span()? {
        let projection = match exact_lookup::scan_jsonl_row(file, path, line) {
            Ok(Some(projection)) => projection,
            Ok(None) => continue,
            Err(error) => {
                malformed_row_count = malformed_row_count.saturating_add(1);
                tracing::warn!(
                    automation_run_ledger = %path.display(),
                    error = %error,
                    "skipping malformed automation run ledger jsonl row"
                );
                continue;
            }
        };
        if selected_run_ids.contains(projection.run_id.as_str()) {
            continue;
        }
        if selected_run_order.len() == limit {
            has_more = true;
            break;
        }
        selected_run_ids.insert(projection.run_id.clone());
        selected_run_order.push(projection.run_id);
    }
    let logical = resolve_selected_logical_records(file, path, &selected_run_ids, false)?;
    let records = selected_run_order
        .into_iter()
        .filter_map(|run_id| logical.get(run_id.as_str()).cloned())
        .collect::<Vec<_>>();
    Ok(AutomationRunLedgerPageV1 {
        records,
        malformed_row_count,
        has_more,
    })
}

struct FilteredRunSelection {
    record: AutomationRunLedgerRecord,
    effective_task_key: String,
}

fn read_filtered_run_records_two_pass(
    file: &std::fs::File,
    path: &Path,
    limit: usize,
    filter: &RunRecordFilter,
) -> Result<AutomationRunLedgerPageV1> {
    let mut selected_run_order = Vec::new();
    let mut selected_run_ids = std::collections::HashSet::new();
    let mut reverse = exact_lookup::ReverseJsonlScanner::new(file, path)?;
    while selected_run_order.len() < limit {
        let Some(line) = reverse.next_span()? else {
            break;
        };
        let Some(projection) = exact_lookup::scan_jsonl_row(file, path, line)? else {
            continue;
        };
        if !filter.matches_projection(&projection)
            || selected_run_ids.contains(projection.run_id.as_str())
        {
            continue;
        }
        selected_run_ids.insert(projection.run_id.clone());
        selected_run_order.push(projection.run_id);
    }

    let logical = resolve_selected_logical_records(file, path, &selected_run_ids, true)?;
    let mut selected = Vec::new();
    for run_id in selected_run_order {
        let record = logical
            .get(run_id.as_str())
            .ok_or_else(|| config_error("automation filtered lifecycle selection disappeared"))?;
        if !filter.matches_record(record) {
            return Err(config_error(
                "automation filtered logical newest row no longer satisfies its filter",
            ));
        }
        selected.push(FilteredRunSelection {
            effective_task_key: effective_record_task_key(record).to_owned(),
            record: record.clone(),
        });
    }

    let selected_indexes = selected
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.record.run_id.as_str(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let mut has_more = false;
    let mut forward = exact_lookup::ForwardJsonlScanner::new(file, path)?;
    while let Some(line) = forward.next_span()? {
        let Some(projection) = exact_lookup::scan_jsonl_row(file, path, line)? else {
            continue;
        };
        let Some(index) = selected_indexes.get(projection.run_id.as_str()).copied() else {
            if !has_more && filter.matches_projection(&projection) {
                has_more = true;
            }
            continue;
        };
        let entry = &selected[index];
        let projection_task_key = projection
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(projection.task));
        if projection.task != entry.record.task
            || projection_task_key != entry.effective_task_key
            || projection.trigger != entry.record.trigger
        {
            return Err(config_error(
                "automation run ledger mutates immutable filtered admission identity",
            ));
        }
    }

    Ok(AutomationRunLedgerPageV1 {
        records: selected.into_iter().map(|entry| entry.record).collect(),
        malformed_row_count: 0,
        has_more,
    })
}

fn resolve_selected_logical_records(
    file: &std::fs::File,
    path: &Path,
    selected_run_ids: &std::collections::HashSet<String>,
    fail_on_malformed: bool,
) -> Result<std::collections::HashMap<String, AutomationRunLedgerRecord>> {
    let mut resolved = std::collections::HashMap::with_capacity(selected_run_ids.len());
    let lifecycles =
        exact_lookup::read_logical_run_lifecycles(file, path, selected_run_ids, fail_on_malformed)?;
    for run_id in selected_run_ids {
        let Some(lifecycle) = lifecycles.get(run_id) else {
            if fail_on_malformed {
                return Err(config_error("automation selected lifecycle disappeared"));
            }
            continue;
        };
        let record = match exact_lookup::decode_jsonl_row(file, path, &lifecycle.newest.span) {
            Ok(record) => record,
            Err(error) if fail_on_malformed => return Err(error),
            Err(_) => continue,
        };
        if let Err(error) = require_projection_identity(&record, &lifecycle.newest) {
            if fail_on_malformed {
                return Err(error);
            }
            continue;
        }
        resolved.insert(run_id.clone(), record);
    }
    Ok(resolved)
}

fn require_projection_identity(
    record: &AutomationRunLedgerRecord,
    projection: &exact_lookup::RunLedgerRowProjection,
) -> Result<()> {
    if record.run_id == projection.run_id
        && record.schema_version == projection.schema_version
        && record.status == projection.status
        && record.trigger == projection.trigger
        && record.task == projection.task
        && record.task_key == projection.task_key
        && (record.error.as_deref() == Some(SESSION_EVIDENCE_BUDGET_EXHAUSTED))
            == projection.session_evidence_budget_exhausted_error
        && record.started_at == projection.started_at
        && record.completed_at == projection.completed_at
        && record.completed_at_micros == projection.completed_at_micros
    {
        Ok(())
    } else {
        Err(config_error(
            "automation run ledger row projection changed during filtered decode",
        ))
    }
}

fn effective_record_task_key(record: &AutomationRunLedgerRecord) -> &str {
    record
        .task_key
        .as_deref()
        .unwrap_or_else(|| canonical_task_key(record.task))
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
        && run_id != "."
        && run_id != ".."
        && run_id.len() <= tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
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

    #[test]
    fn run_id_path_validation_accepts_canonical_dashboard_ids_only_as_normal_components() {
        validate_run_id_component("request.dashboard.http.123.4").unwrap();
        assert!(validate_run_id_component(".").is_err());
        assert!(validate_run_id_component("..").is_err());
        assert!(validate_run_id_component("request/dashboard").is_err());
        assert!(validate_run_id_component("request\\dashboard").is_err());
    }

    /// A minimal valid ledger line for `run_id`, ordered by `completed_at`.
    fn ledger_line(run_id: &str, completed_at: i64) -> String {
        format!(
            "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
             \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"succeeded\",\
             \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"{completed_at}\",\
             \"completed_at\":\"{completed_at}\",\"completed_at_micros\":{}}}",
            completed_at.saturating_mul(1_000_000),
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

    /// A skipped session-reflector scheduler run whose reason sits in the
    /// ledger's `error` field, matching how skip records are minted.
    fn skipped_session_reflector_line(run_id: &str, reason: &str, completed_at: i64) -> String {
        format!(
            "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
             \"task\":\"session_reflector\",\"backend\":\"codex_app_server\",\
             \"status\":\"skipped\",\"accepted_count\":0,\"rejected_count\":0,\
             \"error\":\"{reason}\",\"started_at\":\"{completed_at}\",\
             \"completed_at\":\"{completed_at}\",\"completed_at_micros\":{}}}",
            completed_at.saturating_mul(1_000_000),
        )
    }

    #[test]
    fn task_summary_keeps_the_budget_exhausted_anchor_visible_past_newer_skips() {
        let lines = vec![
            skipped_session_reflector_line("run-budget", SESSION_EVIDENCE_BUDGET_EXHAUSTED, 100),
            skipped_session_reflector_line(
                "run-suppressed",
                "session_evidence_budget_suppressed",
                200,
            ),
        ];
        let (_temp, path) = write_ledger(&lines);

        let summary = read_run_ledger_task_summary(
            &path,
            AgentTaskKind::SessionReflector,
            "session_reflector",
        )
        .unwrap();

        assert_eq!(
            summary.latest_logical_activity().unwrap().run_id,
            "run-suppressed"
        );
        let anchor = summary.latest_session_evidence_budget_exhausted().unwrap();
        assert_eq!(anchor.run_id, "run-budget");
        assert_eq!(
            anchor.error.as_deref(),
            Some(SESSION_EVIDENCE_BUDGET_EXHAUSTED)
        );
        assert!(
            summary
                .records()
                .iter()
                .any(|record| record.run_id == "run-budget"),
            "the anchor record must reach schedule decisions through records()"
        );
    }

    #[test]
    fn task_summary_has_no_budget_anchor_without_an_exhausted_skip() {
        let lines = vec![skipped_session_reflector_line(
            "run-stale",
            "session_evidence_stale",
            100,
        )];
        let (_temp, path) = write_ledger(&lines);

        let summary = read_run_ledger_task_summary(
            &path,
            AgentTaskKind::SessionReflector,
            "session_reflector",
        )
        .unwrap();

        assert!(summary.latest_session_evidence_budget_exhausted().is_none());
    }

    #[test]
    fn automation_policy_reads_canonical_ledger_evidence() {
        let mut record: AutomationRunLedgerRecord =
            serde_json::from_str(&ledger_line("policy-evidence", 1)).unwrap();
        let validation_report = serde_json::json!({"status": "failed_after_partial_effects"});
        let applied_ops = serde_json::json!({
            "deployment": {"status": "partial_failure", "retry_required": true}
        });
        record.accepted_count = 2;
        record.validation_report = Some(validation_report.clone());
        record.applied_ops = Some(applied_ops.clone());

        let next_actions = tracedecay_automation::artifact_policy::artifact_policy(record.task)
            .next_actions(&record);

        assert_eq!(
            tracedecay_automation::AutomationRunRecord::accepted_count(&record),
            2
        );
        assert_eq!(
            tracedecay_automation::AutomationRunRecord::validation_report(&record),
            Some(&validation_report)
        );
        assert_eq!(
            tracedecay_automation::AutomationRunRecord::applied_ops(&record),
            Some(&applied_ops)
        );
        assert_eq!(
            next_actions.first().copied(),
            Some("inspect autonomously applied memory curation outcomes")
        );
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
            lines.push(
                ledger_line(&format!("run-{i:02}"), 3000 + i * 2)
                    .replace("\"succeeded\"", "\"running\""),
            );
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

        let page = read_run_records_tail_page_with_window(&path, 5, 8).unwrap();
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.malformed_row_count, 1);
        assert!(!page.has_more);
        assert!(!page.is_complete());
    }

    #[test]
    fn blank_line_between_rows_is_skipped_across_tail_page_append_and_summary() {
        // A blank line between two committed rows must not be fatal:
        // consecutive newlines (e.g. from an operator edit or a legacy
        // writer) are benign and the pre-rewrite parser explicitly skipped
        // them. Regression coverage for the scan_jsonl_row fix (Finding 1).
        let row1 = ledger_line("run-blank-between-a", 100);
        let row2 = ledger_line("run-blank-between-b", 200);
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        std::fs::write(&path, format!("{row1}\n\n{row2}\n")).unwrap();

        let page = read_run_records_tail_page_with_window(&path, 8, 64).unwrap();
        let ids: Vec<&str> = page.records.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["run-blank-between-b", "run-blank-between-a"]);

        let appended = ledger_line("run-blank-between-c", 300);
        append_jsonl_line_locked(&path, &appended).unwrap();
        let ledger = std::fs::read_to_string(&path).unwrap();
        assert_eq!(ledger.matches("run-blank-between-c").count(), 1);

        let summary =
            read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                .unwrap();
        assert_eq!(
            summary.latest_logical_activity().unwrap().run_id,
            "run-blank-between-c"
        );
    }

    #[test]
    fn trailing_blank_line_is_skipped_across_tail_page_append_and_summary() {
        // A ledger ending in a trailing blank line ("{row}\n\n") must not
        // permanently break every scan. Regression coverage for the
        // scan_jsonl_row fix (Finding 1).
        let row = ledger_line("run-blank-trailing-a", 100);
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        std::fs::write(&path, format!("{row}\n\n")).unwrap();

        let page = read_run_records_tail_page_with_window(&path, 8, 64).unwrap();
        let ids: Vec<&str> = page.records.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["run-blank-trailing-a"]);

        let appended = ledger_line("run-blank-trailing-b", 200);
        append_jsonl_line_locked(&path, &appended).unwrap();
        let ledger = std::fs::read_to_string(&path).unwrap();
        assert_eq!(ledger.matches("run-blank-trailing-b").count(), 1);

        let summary =
            read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                .unwrap();
        assert_eq!(
            summary.latest_logical_activity().unwrap().run_id,
            "run-blank-trailing-b"
        );
    }

    #[test]
    fn whitespace_only_line_is_skipped_by_the_reverse_page_scan() {
        // A pure "\n\n" blank line is absorbed by ReverseJsonlScanner's
        // newline trimming and never reaches scan_jsonl_row, so the two
        // blank-line tests above exercise the Finding-1 fix only through
        // ForwardJsonlScanner (append + summary). A whitespace-only line
        // with non-newline bytes ("   \n") DOES reach scan_jsonl_row from
        // the reverse scanner; it must be skipped as benign, not counted
        // as malformed and not treated as fatal.
        let row1 = ledger_line("run-ws-line-a", 100);
        let row2 = ledger_line("run-ws-line-b", 200);
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        std::fs::write(&path, format!("{row1}\n   \n{row2}\n")).unwrap();

        let page = read_run_records_tail_page_with_window(&path, 8, 64).unwrap();
        let ids: Vec<&str> = page.records.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["run-ws-line-b", "run-ws-line-a"]);
        assert_eq!(page.malformed_row_count, 0);
        assert!(!page.has_more);

        // The filtered (two-pass) reader also traverses the whitespace row
        // through both scanners and must stay fail-open for it.
        let filtered = read_run_records_tail_with_filter(
            &path,
            8,
            64,
            &RunRecordFilter::TaskKey("memory_curator".to_owned()),
        )
        .unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn page_counts_projection_valid_malformed_duplicate_before_limit() {
        let malformed_duplicate = "{\"schema_version\":2,\"run_id\":\"duplicate\",\"trigger\":\"scheduler\",\"task\":\"memory_curator\",\"status\":\"succeeded\"}";
        let lines = vec![
            ledger_line("older", 100),
            malformed_duplicate.to_owned(),
            ledger_line("duplicate", 200),
        ];
        let (_temp, path) = write_ledger(&lines);

        let page = read_run_records_tail_page_with_window(&path, 3, 8).unwrap();

        assert_eq!(page.records.len(), 2);
        assert_eq!(page.malformed_row_count, 1);
        assert!(!page.has_more);
        assert!(!page.is_complete());
    }

    #[test]
    fn page_stream_validates_large_duplicates_before_dedup() {
        let large_valid_duplicate = ledger_line("duplicate", 150).replace(
            "\"completed_at\":\"150\"",
            &format!(
                "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"150\"",
                "x".repeat(2 * 1024 * 1024)
            ),
        );
        let large_malformed_duplicate =
            large_valid_duplicate.replace("\"accepted_count\":0", "\"accepted_count\":false");
        let lines = vec![
            ledger_line("older", 100),
            large_malformed_duplicate,
            ledger_line("duplicate", 200),
        ];
        let (_temp, path) = write_ledger(&lines);

        let page = read_run_records_tail_page_with_window(&path, 3, 8).unwrap();

        assert_eq!(page.records.len(), 2);
        assert_eq!(page.malformed_row_count, 1);
        assert!(!page.has_more);
        assert!(!page.is_complete());
    }

    #[test]
    fn recognized_values_validate_numbers_but_unknown_fields_stream_skip_them() {
        let recognized = ledger_line("recognized", 100).replace(
            "\"completed_at\":\"100\"",
            "\"response_schema\":1e999999,\"completed_at\":\"100\"",
        );
        let unknown = ledger_line("unknown", 200).replace(
            "\"completed_at\":\"200\"",
            "\"future_payload\":1e999999,\"completed_at\":\"200\"",
        );
        let (_temp, path) = write_ledger(&[recognized, unknown]);

        let page = read_run_records_tail_page_with_window(&path, 3, 8).unwrap();

        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].run_id, "unknown");
        assert_eq!(page.malformed_row_count, 1);
        assert!(!page.is_complete());
    }

    #[test]
    fn page_streams_long_keys_inside_canonical_values() {
        let line = ledger_line("long-key", 100).replace(
            "\"completed_at\":\"100\"",
            &format!(
                "\"validation_report\":{{\"{}\":null}},\"completed_at\":\"100\"",
                "κ".repeat(tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES)
            ),
        );
        let (_temp, path) = write_ledger(&[line]);

        let page = read_run_records_tail_page_with_window(&path, 1, 8).unwrap();

        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].run_id, "long-key");
        assert!(page.is_complete());
    }

    #[test]
    fn page_and_filter_reject_valid_json_without_commit_newline() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        std::fs::write(&path, ledger_line("unterminated", 100)).unwrap();

        assert!(read_run_records_tail_page_with_window(&path, 1, 8).is_err());
        assert!(
            read_run_records_tail_with_filter(
                &path,
                1,
                8,
                &RunRecordFilter::TaskKey("memory_curator".to_owned()),
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_newest_matching_run_reserves_identity_before_fallback() {
        let older = ledger_line("same-run", 100).replace(
            "\"task\":\"memory_curator\"",
            "\"task\":\"user_job\",\"task_key\":\"user_job:nightly\"",
        );
        let older = older.replace("\"succeeded\"", "\"running\"");
        let newer = ledger_line("same-run", 200)
            .replace(
                "\"task\":\"memory_curator\"",
                "\"task\":\"user_job\",\"task_key\":\"user_job:nightly\"",
            )
            .replace("\"accepted_count\":0", "\"accepted_count\":false");
        let (_temp, path) = write_ledger(&[older, newer]);

        let task = read_run_records_tail_with_filter(
            &path,
            1,
            8,
            &RunRecordFilter::TaskKey("user_job:nightly".to_owned()),
        );
        let page = read_run_records_tail_page_with_window(&path, 2, 8).unwrap();

        assert!(task.is_err());
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].run_id, "same-run");
        assert_eq!(page.malformed_row_count, 1);
        assert!(!page.is_complete());
    }

    #[test]
    fn filtered_reads_fail_closed_on_any_traversed_malformed_row() {
        let target = ledger_line("target", 100).replace(
            "\"task\":\"memory_curator\"",
            "\"task\":\"user_job\",\"task_key\":\"user_job:nightly\"",
        );
        let (_temp, path) = write_ledger(&[target.clone(), "{\"unrelated\":".to_owned()]);

        assert!(
            read_run_records_tail_with_filter(
                &path,
                1,
                8,
                &RunRecordFilter::TaskKey("user_job:nightly".to_owned()),
            )
            .is_err()
        );
        let unrelated_schema_invalid = ledger_line("unrelated", 200)
            .replace("\"accepted_count\":0", "\"accepted_count\":false");
        let (_temp, path) = write_ledger(&[
            target,
            unrelated_schema_invalid,
            ledger_line("newest-unrelated", 300),
        ]);
        assert!(
            read_run_records_tail_with_filter(
                &path,
                1,
                8,
                &RunRecordFilter::TaskKey("user_job:nightly".to_owned()),
            )
            .is_err()
        );
    }

    #[test]
    fn filtered_verification_streams_older_same_identity_payload() {
        let older = ledger_line("same-run", 100)
            .replace(
                "\"completed_at\":\"100\"",
                &format!(
                    "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"100\"",
                    "x".repeat(2 * 1024 * 1024)
                ),
            )
            .replace("\"succeeded\"", "\"running\"");
        let newest = ledger_line("same-run", 200);
        let (_temp, path) = write_ledger(&[older, newest]);

        let records = read_run_records_tail_with_filter(
            &path,
            1,
            8,
            &RunRecordFilter::TaskKey("memory_curator".to_owned()),
        )
        .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].completed_at, "200");
    }

    #[test]
    fn has_more_requires_an_additional_distinct_qualifying_run() {
        let lines = vec![
            ledger_line("only", 100).replace("\"succeeded\"", "\"running\""),
            ledger_line("only", 200),
        ];
        let (_temp, path) = write_ledger(&lines);

        let page = read_run_records_tail_page_with_window(&path, 1, 8).unwrap();

        assert_eq!(page.records.len(), 1);
        assert!(!page.has_more);
    }

    #[test]
    fn page_and_filter_use_logical_newest_across_historical_retry() {
        let queued = ledger_line("same-run", 100).replace("\"succeeded\"", "\"queued\"");
        let running = ledger_line("same-run", 200).replace("\"succeeded\"", "\"running\"");
        let (_temp, path) = write_ledger(&[queued.clone(), running, queued]);

        let page = read_run_records_tail_page_with_window(&path, 1, 8).unwrap();
        let filtered = read_run_records_tail_with_filter(
            &path,
            1,
            8,
            &RunRecordFilter::TaskKey("memory_curator".to_owned()),
        )
        .unwrap();

        assert_eq!(page.records[0].status, AutomationRunStatus::Running);
        assert!(!page.has_more);
        assert_eq!(filtered[0].status, AutomationRunStatus::Running);
    }

    #[test]
    fn page_and_filter_reject_terminal_lifecycle_regression() {
        let terminal = ledger_line("same-run", 100);
        let running = ledger_line("same-run", 200).replace("\"succeeded\"", "\"running\"");
        let (_temp, path) = write_ledger(&[terminal, running]);

        assert!(read_run_records_tail_page_with_window(&path, 1, 8).is_err());
        assert!(
            read_run_records_tail_with_filter(
                &path,
                1,
                8,
                &RunRecordFilter::TaskKey("memory_curator".to_owned()),
            )
            .is_err()
        );
    }

    #[test]
    fn physical_retry_orders_run_without_regressing_its_logical_state() {
        let queued = ledger_line("run-a", 100).replace("\"succeeded\"", "\"queued\"");
        let running = ledger_line("run-a", 200).replace("\"succeeded\"", "\"running\"");
        let other = ledger_line("run-b", 300);
        let (_temp, path) = write_ledger(&[queued.clone(), running, other, queued]);

        let page = read_run_records_tail_page_with_window(&path, 2, 8).unwrap();

        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records[0].run_id, "run-a");
        assert_eq!(page.records[0].status, AutomationRunStatus::Running);
        assert_eq!(page.records[1].run_id, "run-b");
        assert_eq!(page.records[1].status, AutomationRunStatus::Succeeded);
        assert!(!page.has_more);
    }

    #[test]
    fn task_summary_uses_logical_completion_order_across_large_retry_tail() {
        let queued = ledger_line("run-a", 100).replace("\"succeeded\"", "\"queued\"");
        let running = ledger_line("run-a", 200).replace("\"succeeded\"", "\"running\"");
        let succeeded = ledger_line("run-b", 300);
        let mut lines = vec![queued.clone(), running, succeeded];
        lines.extend(std::iter::repeat_n(queued, 250));
        let (_temp, path) = write_ledger(&lines);

        let summary =
            read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                .unwrap();

        assert_eq!(summary.records().len(), 1);
        assert_eq!(summary.latest_logical_activity().unwrap().run_id, "run-b");
        assert_eq!(summary.latest_successful().unwrap().run_id, "run-b");
        assert_eq!(
            summary.latest_effectful_any_trigger().unwrap().run_id,
            "run-b"
        );
        assert_eq!(
            summary.latest_scheduler_effectful().unwrap().run_id,
            "run-b"
        );
    }

    #[test]
    fn task_summary_exposes_latest_scheduler_activity_without_moving_effectful_anchor() {
        let success = ledger_line("a-success", 100);
        let skipped = ledger_line("z-skip", 100)
            .replace("\"succeeded\"", "\"skipped\"")
            .replace(
                "\"completed_at_micros\":100000000",
                "\"completed_at_micros\":100000900",
            );
        let (_temp, path) = write_ledger(&[success, skipped]);

        let summary =
            read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                .unwrap();

        assert_eq!(
            summary.latest_scheduler_activity().unwrap().run_id,
            "z-skip"
        );
        assert_eq!(
            summary.latest_scheduler_effectful().unwrap().run_id,
            "a-success"
        );
    }

    #[test]
    fn task_summary_and_append_reject_completion_time_regression() {
        let queued = ledger_line("regressed", 300).replace("\"succeeded\"", "\"queued\"");
        let running = ledger_line("regressed", 100).replace("\"succeeded\"", "\"running\"");
        let (_temp, path) = write_ledger(&[queued.clone(), running.clone()]);

        assert!(
            read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                .is_err()
        );
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        append_jsonl_line_locked(&path, &queued).unwrap();
        assert!(append_jsonl_line_locked(&path, &running).is_err());
    }

    /// Reads a summary and reports whether the memo answered it.
    fn summary_with_memo_hit(
        path: &Path,
        task: AgentTaskKind,
        task_key: &str,
    ) -> (AutomationRunLedgerTaskSummary, bool) {
        let before = run_ledger_summary_memo_hits();
        let summary = read_run_ledger_task_summary(path, task, task_key).unwrap();
        (summary, run_ledger_summary_memo_hits() > before)
    }

    #[test]
    fn task_summary_memo_answers_unchanged_ledger_without_rescanning() {
        let (_temp, path) = write_ledger(&[ledger_line("memo-a", 100)]);

        let (first, first_hit) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");
        let (second, second_hit) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");

        assert!(!first_hit, "first read must compute the summary");
        assert!(
            second_hit,
            "unchanged ledger must be answered from the memo"
        );
        assert_eq!(first.records().len(), second.records().len());
        assert_eq!(
            first.latest_successful().unwrap().run_id,
            second.latest_successful().unwrap().run_id
        );
        assert_eq!(second.latest_successful().unwrap().run_id, "memo-a");
    }

    #[test]
    fn task_summary_memo_is_invalidated_by_an_append() {
        let (_temp, path) = write_ledger(&[ledger_line("memo-old", 100)]);

        let (before, _) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");
        assert_eq!(before.latest_successful().unwrap().run_id, "memo-old");
        append_jsonl_line_locked(&path, &ledger_line("memo-new", 200)).unwrap();
        let (after, after_hit) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");

        assert!(!after_hit, "an appended row must invalidate the memo");
        assert_eq!(after.latest_successful().unwrap().run_id, "memo-new");
        assert_eq!(after.latest_logical_activity().unwrap().run_id, "memo-new");
    }

    #[test]
    fn task_summary_memo_is_invalidated_by_a_truncation() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let kept = ledger_line("memo-kept", 100);
        let dropped = ledger_line("memo-dropped", 200);
        std::fs::write(&path, format!("{kept}\n{dropped}\n")).unwrap();

        let (before, _) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");
        assert_eq!(before.latest_successful().unwrap().run_id, "memo-dropped");
        // Recovery truncates the ledger back to a durable prefix.
        std::fs::write(&path, format!("{kept}\n")).unwrap();
        let (after, after_hit) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");

        assert!(!after_hit, "a truncated ledger must invalidate the memo");
        assert_eq!(after.latest_successful().unwrap().run_id, "memo-kept");
    }

    #[test]
    fn task_summary_memo_is_invalidated_by_an_equal_length_rewrite() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let original = ledger_line("memo-rewrite-a", 100);
        let rewritten = ledger_line("memo-rewrite-b", 100);
        assert_eq!(original.len(), rewritten.len());
        std::fs::write(&path, format!("{original}\n")).unwrap();

        let (before, _) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");
        assert_eq!(before.latest_successful().unwrap().run_id, "memo-rewrite-a");
        std::fs::write(&path, format!("{rewritten}\n")).unwrap();
        let (after, after_hit) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");

        assert!(
            !after_hit,
            "an equal-length rewrite must be caught by the tail digest"
        );
        assert_eq!(after.latest_successful().unwrap().run_id, "memo-rewrite-b");
    }

    #[test]
    fn task_summary_memo_keys_distinguish_task_identities() {
        let curator = ledger_line("memo-curator", 100);
        let reflector = ledger_line("memo-reflector", 200)
            .replace("\"memory_curator\"", "\"session_reflector\"");
        let (_temp, path) = write_ledger(&[curator, reflector]);

        let (first, _) =
            summary_with_memo_hit(&path, AgentTaskKind::MemoryCurator, "memory_curator");
        let (second, second_hit) =
            summary_with_memo_hit(&path, AgentTaskKind::SessionReflector, "session_reflector");

        assert!(
            !second_hit,
            "a different task identity must not reuse another memo entry"
        );
        assert_eq!(first.latest_successful().unwrap().run_id, "memo-curator");
        assert_eq!(second.latest_successful().unwrap().run_id, "memo-reflector");
    }

    #[test]
    fn task_summary_rejects_nonnumeric_started_at_for_latest_success() {
        let corrupt = ledger_line("corrupt-started-at", 100)
            .replace("\"started_at\":\"100\"", "\"started_at\":\"corrupt\"");
        let (_temp, path) = write_ledger(&[corrupt]);

        assert!(
            read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                .is_err()
        );
    }

    #[test]
    fn task_summary_rejects_semantically_invalid_unrelated_rows() {
        let valid = ledger_line("selected", 100);
        let unrelated = ledger_line("unrelated", 200)
            .replace("\"task\":\"memory_curator\"", "\"task\":\"skill_writer\"");
        let invalid_rows = [
            unrelated.clone().replace(
                "\"completed_at\":\"200\"",
                "\"completed_at\":\"9223372036854775807\"",
            ),
            unrelated.replace(
                "\"completed_at_micros\":200000000",
                "\"completed_at_micros\":199000000",
            ),
        ];

        for invalid in invalid_rows {
            let (_temp, path) = write_ledger(&[valid.clone(), invalid]);
            assert!(
                read_run_ledger_task_summary(&path, AgentTaskKind::MemoryCurator, "memory_curator")
                    .is_err()
            );
        }
    }

    #[test]
    fn any_page_does_not_decode_the_limit_sentinel_payload() {
        let sentinel = ledger_line("sentinel", 100).replace(
            "\"completed_at\":\"100\"",
            &format!(
                "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"100\"",
                "x".repeat(2 * 1024 * 1024)
            ),
        );
        let selected = ledger_line("selected", 200);
        let (_temp, path) = write_ledger(&[sentinel, selected]);

        let page = read_run_records_tail_page_with_window(&path, 1, 8).unwrap();

        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].run_id, "selected");
        assert!(page.has_more);
    }

    #[test]
    fn zero_limit_reports_nonempty_committed_history_without_scanning_rows() {
        let (_temp, path) = write_ledger(&["not-json".to_owned()]);

        let page = read_run_records_tail_page_with_window(&path, 0, 8).unwrap();

        assert!(page.records.is_empty());
        assert_eq!(page.malformed_row_count, 0);
        assert!(page.has_more);
        assert!(!page.is_complete());
    }

    #[test]
    fn zero_limit_still_rejects_incomplete_durable_tail() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        std::fs::write(&path, ledger_line("unterminated", 100)).unwrap();

        assert!(read_run_records_tail_page_with_window(&path, 0, 8).is_err());
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
        let line = ledger_line("large-unicode", 100).replace(
            "\"completed_at\":\"100\"",
            &format!(
                "\"validation_report\":{{\"payload\":\"{}🧪{}\"}},\"completed_at\":\"100\"",
                "a".repeat(RUN_LEDGER_TAIL_CHUNK_BYTES as usize),
                "b".repeat(1024),
            ),
        );

        append_jsonl_line_locked(&path, &line).unwrap();
        append_jsonl_line_locked(&path, &line).unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert_eq!(contents.lines().next(), Some(line.as_str()));
    }

    #[test]
    fn durable_append_compares_large_newest_row_with_fixed_memory() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let huge = ledger_line("huge", 100).replace(
            "\"completed_at\":\"100\"",
            &format!(
                "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"100\"",
                "x".repeat(2 * 1024 * 1024)
            ),
        );
        let next = ledger_line("next", 200);
        std::fs::write(&path, format!("{huge}\n")).unwrap();

        append_jsonl_line_locked(&path, &huge).unwrap();
        append_jsonl_line_locked(&path, &next).unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        assert_eq!(contents, format!("{huge}\n{next}\n"));
    }

    #[test]
    fn durable_append_retry_deduplicates_behind_intervening_record() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let first = ledger_line("first", 100);
        let second = ledger_line("second", 200);

        append_jsonl_line_locked(&path, &first).unwrap();
        append_jsonl_line_locked(&path, &second).unwrap();
        append_jsonl_line_locked(&path, &first).unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("{first}\n{second}\n")
        );
    }

    #[test]
    fn durable_append_rejects_same_run_with_different_bytes() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let first = ledger_line("same-run", 100);
        let conflict = ledger_line("same-run", 200);

        append_jsonl_line_locked(&path, &first).unwrap();

        assert!(append_jsonl_line_locked(&path, &conflict).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), format!("{first}\n"));
    }

    #[test]
    fn durable_append_rejects_semantically_invalid_candidate_before_write() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let invalid = ledger_line("invalid-candidate", 100)
            .replace("\"started_at\":\"100\"", "\"started_at\":\"-1\"");

        let error = append_jsonl_line_locked(&path, &invalid)
            .expect_err("negative schema-v2 start must not append");

        assert!(error.to_string().contains("predates the UNIX epoch"));
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }

    #[test]
    fn durable_append_allows_forward_lifecycle_and_coalesces_old_retry() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let queued = ledger_line("lifecycle", 100).replace("\"succeeded\"", "\"queued\"");
        let running = ledger_line("lifecycle", 200).replace("\"succeeded\"", "\"running\"");
        let terminal = ledger_line("lifecycle", 300);

        append_jsonl_line_locked(&path, &queued).unwrap();
        append_jsonl_line_locked(&path, &running).unwrap();
        append_jsonl_line_locked(&path, &queued).unwrap();
        append_jsonl_line_locked(&path, &terminal).unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("{queued}\n{running}\n{terminal}\n")
        );
    }

    #[test]
    fn durable_append_accepts_terminal_after_historical_nonadjacent_retry() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(RUN_LEDGER_FILENAME);
        let queued = ledger_line("historical", 100).replace("\"succeeded\"", "\"queued\"");
        let running = ledger_line("historical", 200).replace("\"succeeded\"", "\"running\"");
        let terminal = ledger_line("historical", 300);
        std::fs::write(&path, format!("{queued}\n{running}\n{queued}\n")).unwrap();

        append_jsonl_line_locked(&path, &terminal).unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("{queued}\n{running}\n{queued}\n{terminal}\n")
        );
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

    #[tokio::test]
    async fn scheduler_effectful_anchor_is_not_evicted_by_same_task_non_effect_rows() {
        let anchor = ledger_line("scheduler-anchor", 1).replace(
            "\"task\":\"memory_curator\"",
            "\"task\":\"user_job\",\"task_key\":\"user_job:nightly\"",
        );
        let mut lines = vec![anchor];
        lines.extend((0..300).map(|index| {
            ledger_line(&format!("manual-{index}"), 2 + index)
                .replace("\"trigger\":\"scheduler\"", "\"trigger\":\"dashboard\"")
                .replace(
                    "\"task\":\"memory_curator\"",
                    "\"task\":\"user_job\",\"task_key\":\"user_job:nightly\"",
                )
        }));
        let (temp, _path) = write_ledger(&lines);

        let found = load_latest_scheduler_effectful_for_task_key(temp.path(), "user_job:nightly")
            .await
            .unwrap()
            .expect("full-history summary reaches the scheduler anchor");

        assert_eq!(found.run_id, "scheduler-anchor");
        assert_eq!(found.trigger, AutomationTrigger::Scheduler);
        assert_eq!(found.status, AutomationRunStatus::Succeeded);
    }
}
