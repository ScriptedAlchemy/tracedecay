use std::path::{Path, PathBuf};

use serde_json::Value;

use tracedecay_automation::evidence_budget::SESSION_EVIDENCE_BUDGET_EXHAUSTED;
pub use tracedecay_contracts::automation::{
    AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger,
};

use super::config_error;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::AutomationRunTaskSummaryV1;

mod publication;

pub use super::exact_run::{
    ExactRunPublication, ExactRunPublishOutcome, ExactRunUnboundDiscardOutcome,
};
pub(crate) use publication::publish_run_artifact_chain;
pub use publication::read_published_artifact_chain;

const RUN_LEDGER_AUTHORITY: &str = "automation run ledger";
const RUN_ARTIFACTS_DIR: &str = "automation_artifacts";

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

#[tracing::instrument(name = "automation.run_artifact.write", level = "trace", skip_all)]
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
        let directory = parent.to_path_buf();
        crate::automation::blocking_fs(move || std::fs::create_dir_all(directory))
            .await
            .map_err(|e| TraceDecayError::File {
                message: format!("failed to create run artifact directory: {e}"),
                path: parent.display().to_string(),
            })?;
    }
    let write_path = path.clone();
    crate::automation::blocking_fs(move || std::fs::write(write_path, bytes))
        .await
        .map_err(|e| TraceDecayError::File {
            message: format!("failed to write automation run artifact: {e}"),
            path: path.display().to_string(),
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

#[tracing::instrument(name = "automation.run_artifact.read", level = "trace", skip_all)]
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
    tracedecay_runtime_core::storage::reject_symlink_components(&path, "automation artifact")
        .map_err(TraceDecayError::from)?;
    let read_path = path.clone();
    let bytes = crate::automation::blocking_fs(move || std::fs::read(read_path))
        .await
        .map_err(|e| TraceDecayError::File {
            message: format!("failed to read automation run artifact: {e}"),
            path: path.display().to_string(),
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

pub(super) fn canonical_completion_parts(
    schema_version: u32,
    completed_at: &str,
    completed_at_micros: Option<i64>,
) -> Result<(i64, i64)> {
    require_supported_schema(schema_version)?;
    let completed_at = parse_nonnegative_unix_integer(completed_at, "completion timestamp")?;
    let canonical_micros = completed_at.checked_mul(1_000_000).ok_or_else(|| {
        config_error("automation completion timestamp overflows signed microseconds")
    })?;
    let completed_at_micros = completed_at_micros.unwrap_or(canonical_micros);
    if completed_at_micros < 0 {
        return Err(config_error(
            "automation completion timestamp predates the UNIX epoch",
        ));
    }
    if completed_at_micros.div_euclid(1_000_000) != completed_at {
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

/// Validated start instant in Unix seconds. Callers that window the ledger,
/// including analytics, must use this instead of parsing `started_at`.
pub fn canonical_record_started_at_seconds(
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
    require_supported_schema(schema_version)?;
    parse_nonnegative_unix_integer(started_at, label)
}

/// A row below schema v2 requires a ledger reset; a newer row does not,
/// because a downgraded binary must not reset history a newer one wrote.
fn require_supported_schema(schema_version: u32) -> Result<()> {
    match schema_version {
        2 => Ok(()),
        0..2 => Err(TraceDecayError::reset_required(
            RUN_LEDGER_AUTHORITY,
            format!("a row has retired schema version {schema_version}"),
        )),
        _ => Err(config_error(format!(
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

/// One bounded newest-first page of run history.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationRunLedgerPageV1 {
    pub records: Vec<AutomationRunLedgerRecord>,
    pub has_more: bool,
}

impl AutomationRunLedgerPageV1 {
    /// Whether the page holds every published run.
    pub fn is_complete(&self) -> bool {
        !self.has_more
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
    /// Collects the six store selections, keeping each selected run once.
    pub(crate) fn from_selections(selections: AutomationRunTaskSummaryV1) -> Self {
        let mut summary = Self::default();
        let AutomationRunTaskSummaryV1 {
            latest_logical_activity,
            latest_successful,
            latest_effectful_any_trigger,
            latest_scheduler_effectful,
            latest_scheduler_activity,
            latest_session_evidence_budget_exhausted,
        } = selections;
        summary.latest_logical_activity = summary.select(latest_logical_activity);
        summary.latest_successful = summary.select(latest_successful);
        summary.latest_effectful_any_trigger = summary.select(latest_effectful_any_trigger);
        summary.latest_scheduler_effectful = summary.select(latest_scheduler_effectful);
        summary.latest_scheduler_activity = summary.select(latest_scheduler_activity);
        summary.latest_session_evidence_budget_exhausted =
            summary.select(latest_session_evidence_budget_exhausted);
        summary
    }

    fn select(&mut self, record: Option<AutomationRunLedgerRecord>) -> Option<usize> {
        let record = record?;
        if let Some(index) = self
            .records
            .iter()
            .position(|selected| selected.run_id == record.run_id)
        {
            return Some(index);
        }
        self.records.push(record);
        Some(self.records.len() - 1)
    }

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
}

pub(super) fn is_session_evidence_budget_exhausted_reason(reason: Option<&str>) -> bool {
    reason == Some(SESSION_EVIDENCE_BUDGET_EXHAUSTED)
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

pub(super) fn validate_run_id_component(run_id: &str) -> Result<()> {
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
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn run_id_path_validation_accepts_canonical_dashboard_ids_only_as_normal_components() {
        validate_run_id_component("request.dashboard.http.123.4").unwrap();
        assert_eq!(
            validate_run_id_component(".").unwrap_err().to_string(),
            "config error: automation run_id '.' is not safe for artifact paths"
        );
        assert_eq!(
            validate_run_id_component("..").unwrap_err().to_string(),
            "config error: automation run_id '..' is not safe for artifact paths"
        );
        assert_eq!(
            validate_run_id_component("request/dashboard")
                .unwrap_err()
                .to_string(),
            "config error: automation run_id 'request/dashboard' is not safe for artifact paths"
        );
        assert_eq!(
            validate_run_id_component("request\\dashboard")
                .unwrap_err()
                .to_string(),
            "config error: automation run_id 'request\\dashboard' is not safe for artifact paths"
        );
    }
}
