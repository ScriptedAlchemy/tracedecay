use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::ManifestDigest;

use super::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, run_ledger_path,
    validate_run_id_component,
};
use crate::automation::backend::AgentTaskKind;
use crate::automation::backend::task_key as canonical_task_key;
use crate::automation::config_error;
use crate::errors::Result;

const JSON_SCAN_BUFFER_BYTES: usize = 64 * 1024;
const JSON_MAX_DEPTH: usize = 128;
const PROJECTED_TEXT_MAX_BYTES: usize = tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES;
pub(super) const JSON_POINTER_MAX_BYTES: usize = 4 * 1024;
const JSON_POINTER_MAX_SEGMENTS: usize = 64;

const PROJECTION_KEYS: &[&str] = &[
    "schema_version",
    "run_id",
    "status",
    "trigger",
    "task",
    "task_key",
    "error",
    "started_at",
    "completed_at",
    "completed_at_micros",
    "validation_report",
];
const RECORD_KEYS: &[&str] = &[
    "schema_version",
    "run_id",
    "trigger",
    "task",
    "task_key",
    "backend",
    "host_mode",
    "prompt_version",
    "response_schema",
    "strict_json",
    "model",
    "status",
    "evidence_hash",
    "input_hash",
    "output_hash",
    "proposed_ops",
    "applied_ops",
    "rejected_ops",
    "validation_report",
    "reviewed_count",
    "accepted_count",
    "rejected_count",
    "skipped_count",
    "error",
    "error_classification",
    "error_retryable",
    "backend_attempt_count",
    "backend_attempts",
    "fallback_status",
    "report_ref",
    "artifacts",
    "started_at",
    "completed_at",
    "completed_at_micros",
];
const RETRY_ATTEMPT_KEYS: &[&str] = &[
    "attempt",
    "succeeded",
    "failure_classification",
    "backoff_millis",
];
const ARTIFACT_KEYS: &[&str] = &[
    "schema_version",
    "kind",
    "path",
    "sha256",
    "summary",
    "created_at",
];

#[derive(Debug, Clone)]
pub(super) struct RunLedgerRowProjection {
    pub(super) span: Range<u64>,
    pub(super) schema_version: u32,
    pub(super) run_id: String,
    pub(super) status: AutomationRunStatus,
    pub(super) trigger: AutomationTrigger,
    pub(super) task: AgentTaskKind,
    pub(super) task_key: Option<String>,
    /// True when the row's `error` field equals the session-evidence
    /// budget-exhausted label. The projection compares while streaming
    /// instead of capturing the field: failed runs carry arbitrarily long
    /// backend error messages that must not be bounded or allocated here.
    pub(super) session_evidence_budget_exhausted_error: bool,
    pub(super) started_at: String,
    pub(super) completed_at: String,
    pub(super) completed_at_micros: Option<i64>,
    pub(super) validation_report: Option<Range<u64>>,
}

pub(super) struct LogicalRunLifecycle {
    pub(super) newest: RunLedgerRowProjection,
}

pub(super) fn canonical_completion_key(row: &RunLedgerRowProjection) -> Result<(i64, i64, &str)> {
    let (completed_at, completed_at_micros) = super::canonical_completion_parts(
        row.schema_version,
        &row.completed_at,
        row.completed_at_micros,
    )?;
    Ok((completed_at, completed_at_micros, row.run_id.as_str()))
}

pub(super) fn validate_ledger_row_semantics(row: &RunLedgerRowProjection) -> Result<()> {
    super::canonical_started_at_seconds(
        row.schema_version,
        &row.started_at,
        "ledger row started_at",
    )?;
    super::canonical_completion_parts(
        row.schema_version,
        &row.completed_at,
        row.completed_at_micros,
    )
    .map(|_| ())
}

struct LogicalRunAccumulator {
    identity: Option<(AgentTaskKind, String, AutomationTrigger)>,
    status_spans: [Option<Range<u64>>; 5],
    newest: Option<RunLedgerRowProjection>,
    newest_completion: Option<(i64, i64)>,
}

impl Default for LogicalRunAccumulator {
    fn default() -> Self {
        Self {
            identity: None,
            status_spans: std::array::from_fn(|_| None),
            newest: None,
            newest_completion: None,
        }
    }
}

pub(super) fn read_logical_run_lifecycle(
    file: &std::fs::File,
    path: &Path,
    run_id: &str,
) -> Result<Option<LogicalRunLifecycle>> {
    let selected = std::collections::HashSet::from([run_id.to_owned()]);
    read_logical_run_lifecycles(file, path, &selected, true).map(|mut rows| rows.remove(run_id))
}

pub(super) fn read_logical_run_lifecycles(
    file: &std::fs::File,
    path: &Path,
    selected_run_ids: &std::collections::HashSet<String>,
    fail_on_malformed: bool,
) -> Result<std::collections::HashMap<String, LogicalRunLifecycle>> {
    let mut rows = ForwardJsonlScanner::new(file, path)?;
    let mut states = selected_run_ids
        .iter()
        .map(|run_id| (run_id.clone(), LogicalRunAccumulator::default()))
        .collect::<std::collections::HashMap<_, _>>();
    while let Some(line) = rows.next_span()? {
        let row = match scan_jsonl_row(file, path, line) {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(error) if fail_on_malformed => return Err(error),
            Err(_) => continue,
        };
        let Some(state) = states.get_mut(row.run_id.as_str()) else {
            continue;
        };
        let task_key = row
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(row.task));
        if let Some((task, expected_task_key, trigger)) = state.identity.as_ref() {
            if row.task != *task || task_key != expected_task_key || row.trigger != *trigger {
                return Err(config_error(format!(
                    "automation run ledger '{}' mutates immutable identity for run '{}'",
                    path.display(),
                    row.run_id
                )));
            }
        } else {
            state.identity = Some((row.task, task_key.to_owned(), row.trigger));
        }
        let status_index = super::run_status_index(row.status);
        if let Some(canonical_span) = state.status_spans[status_index].as_ref() {
            if !spans_match(file, path, canonical_span, &row.span)? {
                return Err(config_error(format!(
                    "automation run ledger '{}' repeats a conflicting lifecycle state for run '{}'",
                    path.display(),
                    row.run_id
                )));
            }
            continue;
        }
        let previous_status = state
            .newest
            .as_ref()
            .map(|row: &RunLedgerRowProjection| row.status);
        if !super::valid_run_status_transition(previous_status, row.status) {
            return Err(config_error(format!(
                "automation run ledger '{}' contains an invalid lifecycle for run '{}'",
                path.display(),
                row.run_id
            )));
        }
        let completion = canonical_completion_key(&row)?;
        let completion = (completion.0, completion.1);
        if state
            .newest_completion
            .is_some_and(|previous| completion < previous)
        {
            return Err(config_error(format!(
                "automation run ledger '{}' regresses completion time for run '{}'",
                path.display(),
                row.run_id
            )));
        }
        state.status_spans[status_index] = Some(row.span.clone());
        state.newest = Some(row);
        state.newest_completion = Some(completion);
    }
    Ok(states
        .into_iter()
        .filter_map(|(run_id, state)| {
            state
                .newest
                .map(|newest| (run_id, LogicalRunLifecycle { newest }))
        })
        .collect())
}

#[derive(Debug)]
struct ExactRunMatch {
    span: Range<u64>,
    digest: ManifestDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExactRunIdentity {
    pub(super) digest: ManifestDigest,
    pub(super) payload_len: u64,
}

pub async fn find_run_record_exact_bounded(
    dashboard_root: &Path,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let root = dashboard_root.to_path_buf();
    let requested_run_id = run_id.to_owned();
    tokio::task::spawn_blocking(move || {
        find_run_record_exact_bounded_blocking(&root, &requested_run_id)
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join exact automation run ledger read: {error}"
        ))
    })?
}

pub fn find_run_record_exact_bounded_blocking(
    dashboard_root: &Path,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    validate_run_id_component(run_id)?;
    let path = run_ledger_path(dashboard_root);
    let lock = super::exact_publication::acquire_run_ledger_lock(&path)
        .map_err(crate::errors::TraceDecayError::from)?;
    let result = (|| {
        super::exact_publication::ensure_no_exact_append_intent(dashboard_root)
            .map_err(crate::errors::TraceDecayError::from)?;
        read_exact_run_record_bounded(&path, run_id)
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(crate::errors::TraceDecayError::from);
    result.and_then(|record| unlock.map(|()| record))
}

fn read_exact_run_record_bounded(
    path: &Path,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let Some(file) = open_exact_ledger(path)? else {
        return Ok(None);
    };
    let Some(found) = read_exact_run_match(&file, path, run_id)? else {
        return Ok(None);
    };
    let row_len = found
        .span
        .end
        .checked_sub(found.span.start)
        .ok_or_else(|| config_error("automation exact-run row span is invalid"))?;
    let record = {
        let mut handle = &file;
        handle
            .seek(SeekFrom::Start(found.span.start))
            .map_err(|error| ledger_io_error(path, "seek exact row", error))?;
        let mut row = handle.take(row_len);
        let record =
            serde_json::from_reader::<_, AutomationRunLedgerRecord>(&mut row).map_err(|error| {
                config_error(format!(
                    "automation run ledger '{}' exact row is malformed: {error}",
                    path.display()
                ))
            })?;
        if row.limit() != 0 {
            return Err(config_error(
                "automation exact-run decode ended before its scanned row boundary",
            ));
        }
        record
    };
    if record.run_id != run_id {
        return Err(config_error(
            "automation exact-run identity changed between scan and decode",
        ));
    }
    let decoded_digest = digest_span(&file, path, &found.span)?;
    if decoded_digest != found.digest {
        return Err(config_error(
            "automation exact-run bytes changed between scan and decode",
        ));
    }
    Ok(Some(record))
}

#[cfg(test)]
pub(super) fn read_exact_run_digest(path: &Path, run_id: &str) -> Result<Option<ManifestDigest>> {
    let Some(file) = open_exact_ledger(path)? else {
        return Ok(None);
    };
    read_exact_run_match(&file, path, run_id).map(|found| found.map(|found| found.digest))
}

pub(super) fn read_exact_run_identity_from_file(
    file: &std::fs::File,
    path: &Path,
    run_id: &str,
) -> Result<Option<ExactRunIdentity>> {
    read_exact_run_match(file, path, run_id)?
        .map(|found| {
            let payload_len = found
                .span
                .end
                .checked_sub(found.span.start)
                .ok_or_else(|| config_error("automation exact-run row span is invalid"))?;
            Ok(ExactRunIdentity {
                digest: found.digest,
                payload_len,
            })
        })
        .transpose()
}

fn open_exact_ledger(path: &Path) -> Result<Option<std::fs::File>> {
    open_stabilized_run_ledger(path, false)
}

pub(super) fn open_stabilized_run_ledger(
    path: &Path,
    create: bool,
) -> Result<Option<std::fs::File>> {
    let file = super::exact_publication::open_run_ledger_nofollow(path, true, true, false, create)
        .map_err(|error| {
            config_error(format!(
                "failed to open automation run ledger '{}' for durable read: {error}",
                path.display()
            ))
        })?;
    let Some(file) = file else {
        return Ok(None);
    };
    super::sync_run_ledger_file_and_parent(path, &file)?;
    Ok(Some(file))
}

fn read_exact_run_match(
    file: &std::fs::File,
    path: &Path,
    run_id: &str,
) -> Result<Option<ExactRunMatch>> {
    let mut rows = ForwardJsonlScanner::new(file, path)?;
    let mut newest = None;
    let mut identity = None;
    let mut previous_status = None;
    let mut previous_completion = None;
    let mut status_spans: [Option<Range<u64>>; 5] = std::array::from_fn(|_| None);
    while let Some(line) = rows.next_span()? {
        let Some(row) = scan_jsonl_row(file, path, line)? else {
            continue;
        };
        if row.run_id != run_id {
            continue;
        }
        let task_key = row
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(row.task));
        if let Some((task, expected_task_key, trigger)) = identity.as_ref() {
            if row.task != *task || task_key != expected_task_key || row.trigger != *trigger {
                return Err(config_error(format!(
                    "automation run ledger '{}' mutates immutable identity for run '{run_id}'",
                    path.display()
                )));
            }
        } else {
            identity = Some((row.task, task_key.to_owned(), row.trigger));
        }
        let status_index = super::run_status_index(row.status);
        if let Some(canonical_span) = status_spans[status_index].as_ref() {
            let same_state = spans_match(file, path, canonical_span, &row.span)?;
            if !same_state {
                return Err(config_error(format!(
                    "automation run ledger '{}' repeats a conflicting lifecycle state for run '{run_id}'",
                    path.display()
                )));
            }
            continue;
        }
        if !super::valid_run_status_transition(previous_status, row.status) {
            return Err(config_error(format!(
                "automation run ledger '{}' contains an invalid lifecycle for run '{run_id}'",
                path.display()
            )));
        }
        let completion = canonical_completion_key(&row)?;
        let completion = (completion.0, completion.1);
        if previous_completion.is_some_and(|previous| completion < previous) {
            return Err(config_error(format!(
                "automation run ledger '{}' regresses completion time for run '{run_id}'",
                path.display()
            )));
        }
        status_spans[status_index] = Some(row.span.clone());
        previous_status = Some(row.status);
        previous_completion = Some(completion);
        let digest = digest_span(file, path, &row.span)?;
        newest = Some(ExactRunMatch {
            span: row.span,
            digest,
        });
    }
    Ok(newest)
}

pub(super) fn scan_jsonl_row(
    file: &std::fs::File,
    path: &Path,
    line: Range<u64>,
) -> Result<Option<RunLedgerRowProjection>> {
    // A blank/whitespace-only span is not corruption: consecutive newlines
    // (a trailing blank line, an operator edit, or a legacy writer) are
    // benign and must be skipped rather than treated as a fatal error.
    // Callers that scan a spool file expecting exactly one canonical row
    // (e.g. open_bound_spool) still treat `None` as an error themselves.
    let Some(row) = JsonRangeReader::new(file, path, line).parse_ledger_row()? else {
        return Ok(None);
    };
    validate_jsonl_row_schema(file, path, &row.span)?;
    validate_ledger_row_semantics(&row)?;
    Ok(Some(row))
}

pub(super) fn validate_jsonl_row_schema(
    file: &std::fs::File,
    path: &Path,
    span: &Range<u64>,
) -> Result<()> {
    JsonRangeReader::new(file, path, span.clone()).validate_ledger_record()
}

pub(super) fn decode_jsonl_row(
    file: &std::fs::File,
    path: &Path,
    span: &Range<u64>,
) -> Result<AutomationRunLedgerRecord> {
    let row_len = span
        .end
        .checked_sub(span.start)
        .ok_or_else(|| config_error("automation ledger row span is invalid"))?;
    let mut handle = file;
    handle
        .seek(SeekFrom::Start(span.start))
        .map_err(|error| ledger_io_error(path, "seek selected row", error))?;
    let mut row = handle.take(row_len);
    let record =
        serde_json::from_reader::<_, AutomationRunLedgerRecord>(&mut row).map_err(|error| {
            config_error(format!(
                "automation run ledger '{}' selected row is malformed: {error}",
                path.display()
            ))
        })?;
    if row.limit() != 0 {
        return Err(config_error(
            "automation selected-row decode ended before its scanned boundary",
        ));
    }
    Ok(record)
}

pub(super) struct ReverseJsonlScanner<'a> {
    file: &'a std::fs::File,
    path: &'a Path,
    file_len: u64,
    next_end: u64,
    buffer_start: u64,
    buffer_len: usize,
    buffer: [u8; JSON_SCAN_BUFFER_BYTES],
    #[cfg(test)]
    chunks_read: usize,
}

impl<'a> ReverseJsonlScanner<'a> {
    pub(super) fn new(file: &'a std::fs::File, path: &'a Path) -> Result<Self> {
        let file_len = file
            .metadata()
            .map_err(|error| ledger_io_error(path, "inspect for reverse scan", error))?
            .len();
        require_committed_jsonl_eof(file, path, file_len)?;
        Ok(Self {
            file,
            path,
            file_len,
            next_end: file_len,
            buffer_start: 0,
            buffer_len: 0,
            buffer: [0_u8; JSON_SCAN_BUFFER_BYTES],
            #[cfg(test)]
            chunks_read: 0,
        })
    }

    pub(super) fn next_span(&mut self) -> Result<Option<Range<u64>>> {
        self.next_span_with_floor(0)
    }

    pub(super) fn next_span_with_floor(&mut self, floor: u64) -> Result<Option<Range<u64>>> {
        if floor > self.file_len {
            return Err(config_error(
                "automation reverse scan floor exceeds the ledger length",
            ));
        }
        let mut end = self.next_end;
        while end > floor && self.byte_at(end - 1, floor)? == b'\n' {
            end -= 1;
        }
        if end <= floor {
            self.next_end = floor;
            return Ok(None);
        }
        let mut search_end = end;
        loop {
            if search_end <= floor {
                return Err(config_error(format!(
                    "automation run ledger '{}' row crosses its bounded reverse-scan floor",
                    self.path.display()
                )));
            }
            self.ensure_buffered(search_end - 1, floor)?;
            let relative_end = usize::try_from(search_end - self.buffer_start)
                .map_err(|_| config_error("automation reverse scan offset is not representable"))?;
            let relative_floor = usize::try_from(floor.saturating_sub(self.buffer_start))
                .map_err(|_| config_error("automation reverse scan floor is not representable"))?
                .min(relative_end);
            if let Some(newline) = self.buffer[relative_floor..relative_end]
                .iter()
                .rposition(|byte| *byte == b'\n')
            {
                let delimiter = self.buffer_start + (relative_floor + newline) as u64;
                self.next_end = delimiter;
                return Ok(Some(delimiter + 1..end));
            }
            if self.buffer_start <= floor {
                if floor != 0 {
                    return Err(config_error(format!(
                        "automation run ledger '{}' row crosses its bounded reverse-scan floor",
                        self.path.display()
                    )));
                }
                self.next_end = 0;
                return Ok(Some(0..end));
            }
            search_end = self.buffer_start.max(floor);
        }
    }

    fn byte_at(&mut self, offset: u64, floor: u64) -> Result<u8> {
        self.ensure_buffered(offset, floor)?;
        let index = usize::try_from(offset - self.buffer_start)
            .map_err(|_| config_error("automation reverse scan byte is not representable"))?;
        self.buffer
            .get(index)
            .copied()
            .ok_or_else(|| config_error("automation reverse scan byte is outside its buffer"))
    }

    fn ensure_buffered(&mut self, offset: u64, floor: u64) -> Result<()> {
        let buffer_end = self.buffer_start.saturating_add(self.buffer_len as u64);
        if self.buffer_len != 0 && offset >= self.buffer_start && offset < buffer_end {
            return Ok(());
        }
        let chunk_bytes = JSON_SCAN_BUFFER_BYTES as u64;
        let chunk_start = ((offset / chunk_bytes) * chunk_bytes).max(floor);
        let chunk_len = usize::try_from(self.file_len.saturating_sub(chunk_start).min(chunk_bytes))
            .map_err(|_| config_error("automation reverse scan chunk is not representable"))?;
        read_exact_span(
            self.file,
            self.path,
            chunk_start,
            &mut self.buffer[..chunk_len],
        )?;
        self.buffer_start = chunk_start;
        self.buffer_len = chunk_len;
        #[cfg(test)]
        {
            self.chunks_read = self.chunks_read.saturating_add(1);
        }
        Ok(())
    }

    #[cfg(test)]
    fn chunks_read(&self) -> usize {
        self.chunks_read
    }
}

pub(super) fn extract_json_pointer_bounded(
    file: &std::fs::File,
    path: &Path,
    value_span: Range<u64>,
    pointer: &str,
    max_selected_bytes: usize,
) -> Result<Option<Value>> {
    let Some(selected) = locate_json_pointer_span(file, path, value_span, pointer)? else {
        return Ok(None);
    };
    let selected_len = selected
        .end
        .checked_sub(selected.start)
        .ok_or_else(|| config_error("automation cursor value span is invalid"))?;
    if selected_len > max_selected_bytes as u64 {
        return Err(config_error(format!(
            "automation cursor value exceeds its {max_selected_bytes}-byte wire bound"
        )));
    }
    let selected_len = usize::try_from(selected_len)
        .map_err(|_| config_error("automation cursor value length is not representable"))?;
    let mut bytes = vec![0_u8; selected_len];
    read_exact_span(file, path, selected.start, &mut bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| config_error(format!("automation cursor value is malformed: {error}")))
}

pub(super) fn json_pointer_exists(
    file: &std::fs::File,
    path: &Path,
    value_span: Range<u64>,
    pointer: &str,
) -> Result<bool> {
    locate_json_pointer_span(file, path, value_span, pointer).map(|selected| selected.is_some())
}

fn locate_json_pointer_span(
    file: &std::fs::File,
    path: &Path,
    value_span: Range<u64>,
    pointer: &str,
) -> Result<Option<Range<u64>>> {
    let pointer = JsonPointerSpec::parse(pointer)?;
    let mut reader = JsonRangeReader::new(file, path, value_span);
    let selected = reader.locate_pointer(&pointer.tokens, 0, 0)?;
    reader.skip_whitespace()?;
    if !reader.is_eof() {
        return reader.fail("trailing bytes after validation report");
    }
    Ok(selected)
}

pub(super) struct ForwardJsonlScanner<'a> {
    file: &'a std::fs::File,
    path: &'a Path,
    file_len: u64,
    next_start: u64,
    search_offset: u64,
    buffer_start: u64,
    buffer_len: usize,
    buffer: [u8; JSON_SCAN_BUFFER_BYTES],
    #[cfg(test)]
    chunks_read: usize,
}

impl<'a> ForwardJsonlScanner<'a> {
    pub(super) fn new(file: &'a std::fs::File, path: &'a Path) -> Result<Self> {
        let file_len = file
            .metadata()
            .map_err(|error| ledger_io_error(path, "inspect for forward scan", error))?
            .len();
        Self::new_bounded(file, path, file_len)
    }

    pub(super) fn new_bounded(
        file: &'a std::fs::File,
        path: &'a Path,
        file_len: u64,
    ) -> Result<Self> {
        let physical_len = file
            .metadata()
            .map_err(|error| ledger_io_error(path, "inspect for bounded forward scan", error))?
            .len();
        if file_len > physical_len {
            return Err(config_error(
                "automation bounded forward scan exceeds the ledger length",
            ));
        }
        require_committed_jsonl_eof(file, path, file_len)?;
        Ok(Self {
            file,
            path,
            file_len,
            next_start: 0,
            search_offset: 0,
            buffer_start: 0,
            buffer_len: 0,
            buffer: [0_u8; JSON_SCAN_BUFFER_BYTES],
            #[cfg(test)]
            chunks_read: 0,
        })
    }

    pub(super) fn next_span(&mut self) -> Result<Option<Range<u64>>> {
        if self.next_start >= self.file_len {
            return Ok(None);
        }
        let start = self.next_start;
        loop {
            if self.search_offset >= self.file_len {
                self.next_start = self.file_len;
                return Ok(Some(start..self.file_len));
            }
            self.ensure_buffered(self.search_offset)?;
            let relative = usize::try_from(self.search_offset - self.buffer_start)
                .map_err(|_| config_error("automation forward scan offset is not representable"))?;
            if let Some(newline) = self.buffer[relative..self.buffer_len]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                let end = self.search_offset + newline as u64;
                self.next_start = end + 1;
                self.search_offset = self.next_start;
                return Ok(Some(start..end));
            }
            self.search_offset = self.buffer_start + self.buffer_len as u64;
        }
    }

    fn ensure_buffered(&mut self, offset: u64) -> Result<()> {
        let buffer_end = self.buffer_start.saturating_add(self.buffer_len as u64);
        if self.buffer_len != 0 && offset >= self.buffer_start && offset < buffer_end {
            return Ok(());
        }
        let chunk_bytes = JSON_SCAN_BUFFER_BYTES as u64;
        let chunk_start = (offset / chunk_bytes) * chunk_bytes;
        let chunk_len = usize::try_from(self.file_len.saturating_sub(chunk_start).min(chunk_bytes))
            .map_err(|_| config_error("automation forward scan chunk is not representable"))?;
        read_exact_span(
            self.file,
            self.path,
            chunk_start,
            &mut self.buffer[..chunk_len],
        )?;
        self.buffer_start = chunk_start;
        self.buffer_len = chunk_len;
        #[cfg(test)]
        {
            self.chunks_read = self.chunks_read.saturating_add(1);
        }
        Ok(())
    }

    #[cfg(test)]
    fn chunks_read(&self) -> usize {
        self.chunks_read
    }
}

fn require_committed_jsonl_eof(file: &std::fs::File, path: &Path, file_len: u64) -> Result<()> {
    if file_len == 0 {
        return Ok(());
    }
    let mut last = [0_u8; 1];
    read_exact_span(file, path, file_len - 1, &mut last)?;
    if last[0] == b'\n' {
        Ok(())
    } else {
        Err(config_error(format!(
            "automation run ledger '{}' has an incomplete durable tail",
            path.display()
        )))
    }
}

fn digest_span(file: &std::fs::File, path: &Path, span: &Range<u64>) -> Result<ManifestDigest> {
    let mut hasher = Sha256::new();
    let mut offset = span.start;
    let mut buffer = [0_u8; JSON_SCAN_BUFFER_BYTES];
    while offset < span.end {
        let chunk_len = usize::try_from((span.end - offset).min(JSON_SCAN_BUFFER_BYTES as u64))
            .map_err(|_| config_error("automation digest chunk is not representable"))?;
        read_exact_span(file, path, offset, &mut buffer[..chunk_len])?;
        hasher.update(&buffer[..chunk_len]);
        offset += chunk_len as u64;
    }
    digest_from_hasher(hasher)
        .map_err(|error| config_error(format!("invalid automation run digest: {error}")))
}

fn read_exact_span(file: &std::fs::File, path: &Path, offset: u64, bytes: &mut [u8]) -> Result<()> {
    let mut handle = file;
    handle
        .seek(SeekFrom::Start(offset))
        .map_err(|error| ledger_io_error(path, "seek", error))?;
    handle
        .read_exact(bytes)
        .map_err(|error| ledger_io_error(path, "read", error))
}

pub(super) fn span_matches_bytes(
    file: &std::fs::File,
    span: &Range<u64>,
    expected: &[u8],
) -> std::io::Result<bool> {
    let span_len = span.end.checked_sub(span.start).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid JSONL row span")
    })?;
    let expected_len = u64::try_from(expected.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected JSONL row length is not representable",
        )
    })?;
    if span_len != expected_len {
        return Ok(false);
    }
    let mut offset = span.start;
    let mut compared = 0_usize;
    let mut buffer = [0_u8; JSON_SCAN_BUFFER_BYTES];
    while compared < expected.len() {
        let chunk_len = (expected.len() - compared).min(JSON_SCAN_BUFFER_BYTES);
        let mut handle = file;
        handle.seek(SeekFrom::Start(offset))?;
        handle.read_exact(&mut buffer[..chunk_len])?;
        if buffer[..chunk_len] != expected[compared..compared + chunk_len] {
            return Ok(false);
        }
        offset = offset
            .checked_add(u64::try_from(chunk_len).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "JSONL comparison chunk is not representable",
                )
            })?)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "JSONL row offset overflow")
            })?;
        compared += chunk_len;
    }
    Ok(true)
}

pub(super) fn spans_match(
    file: &std::fs::File,
    path: &Path,
    left: &Range<u64>,
    right: &Range<u64>,
) -> Result<bool> {
    let left_len = left
        .end
        .checked_sub(left.start)
        .ok_or_else(|| config_error("invalid left JSONL row span"))?;
    let right_len = right
        .end
        .checked_sub(right.start)
        .ok_or_else(|| config_error("invalid right JSONL row span"))?;
    if left_len != right_len {
        return Ok(false);
    }
    let mut compared = 0_u64;
    let mut left_buffer = [0_u8; JSON_SCAN_BUFFER_BYTES];
    let mut right_buffer = [0_u8; JSON_SCAN_BUFFER_BYTES];
    while compared < left_len {
        let take = usize::try_from((left_len - compared).min(JSON_SCAN_BUFFER_BYTES as u64))
            .map_err(|_| config_error("automation row comparison chunk is not representable"))?;
        read_exact_span(file, path, left.start + compared, &mut left_buffer[..take])?;
        read_exact_span(
            file,
            path,
            right.start + compared,
            &mut right_buffer[..take],
        )?;
        if left_buffer[..take] != right_buffer[..take] {
            return Ok(false);
        }
        compared += take as u64;
    }
    Ok(true)
}

fn ledger_io_error(
    path: &Path,
    operation: &str,
    error: std::io::Error,
) -> crate::errors::TraceDecayError {
    config_error(format!(
        "failed to {operation} automation run ledger '{}': {error}",
        path.display()
    ))
}

fn digest_from_hasher(hasher: Sha256) -> std::result::Result<ManifestDigest, String> {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    ManifestDigest::new(encoded).map_err(|error| error.to_string())
}

struct JsonPointerSpec {
    tokens: Vec<String>,
}

impl JsonPointerSpec {
    fn parse(pointer: &str) -> Result<Self> {
        if pointer.len() > JSON_POINTER_MAX_BYTES {
            return Err(config_error(
                "automation JSON pointer exceeds its byte bound",
            ));
        }
        if pointer.is_empty() {
            return Ok(Self { tokens: Vec::new() });
        }
        if !pointer.starts_with('/') {
            return Err(config_error(
                "automation cursor requires an RFC 6901 JSON pointer",
            ));
        }
        let mut tokens = Vec::new();
        for encoded in pointer.split('/').skip(1) {
            if tokens.len() == JSON_POINTER_MAX_SEGMENTS {
                return Err(config_error(
                    "automation JSON pointer has too many segments",
                ));
            }
            let mut decoded = Vec::with_capacity(encoded.len());
            let bytes = encoded.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                match bytes[index] {
                    b'~' => {
                        let replacement = match bytes.get(index + 1) {
                            Some(b'0') => b'~',
                            Some(b'1') => b'/',
                            _ => {
                                return Err(config_error(
                                    "automation JSON pointer has an invalid escape",
                                ));
                            }
                        };
                        decoded.push(replacement);
                        index += 2;
                    }
                    byte => {
                        decoded.push(byte);
                        index += 1;
                    }
                }
            }
            let decoded = String::from_utf8(decoded)
                .map_err(|_| config_error("automation JSON pointer segment is not UTF-8"))?;
            tokens.push(decoded);
        }
        Ok(Self { tokens })
    }
}

struct JsonRangeReader<'a> {
    file: &'a std::fs::File,
    path: &'a Path,
    end: u64,
    position: u64,
    buffer_start: u64,
    buffer_len: usize,
    buffer: [u8; JSON_SCAN_BUFFER_BYTES],
}

#[derive(Default)]
struct RecordFields {
    schema_version: bool,
    run_id: bool,
    trigger: bool,
    task: bool,
    task_key: bool,
    backend: bool,
    host_mode: bool,
    prompt_version: bool,
    response_schema: bool,
    strict_json: bool,
    model: bool,
    status: bool,
    evidence_hash: bool,
    input_hash: bool,
    output_hash: bool,
    proposed_ops: bool,
    applied_ops: bool,
    rejected_ops: bool,
    validation_report: bool,
    reviewed_count: bool,
    accepted_count: bool,
    rejected_count: bool,
    skipped_count: bool,
    error: bool,
    error_classification: bool,
    error_retryable: bool,
    backend_attempt_count: bool,
    backend_attempts: bool,
    fallback_status: bool,
    report_ref: bool,
    artifacts: bool,
    started_at: bool,
    completed_at: bool,
    completed_at_micros: bool,
}

impl RecordFields {
    fn field_mut(&mut self, key: &str) -> Option<&mut bool> {
        Some(match key {
            "schema_version" => &mut self.schema_version,
            "run_id" => &mut self.run_id,
            "trigger" => &mut self.trigger,
            "task" => &mut self.task,
            "task_key" => &mut self.task_key,
            "backend" => &mut self.backend,
            "host_mode" => &mut self.host_mode,
            "prompt_version" => &mut self.prompt_version,
            "response_schema" => &mut self.response_schema,
            "strict_json" => &mut self.strict_json,
            "model" => &mut self.model,
            "status" => &mut self.status,
            "evidence_hash" => &mut self.evidence_hash,
            "input_hash" => &mut self.input_hash,
            "output_hash" => &mut self.output_hash,
            "proposed_ops" => &mut self.proposed_ops,
            "applied_ops" => &mut self.applied_ops,
            "rejected_ops" => &mut self.rejected_ops,
            "validation_report" => &mut self.validation_report,
            "reviewed_count" => &mut self.reviewed_count,
            "accepted_count" => &mut self.accepted_count,
            "rejected_count" => &mut self.rejected_count,
            "skipped_count" => &mut self.skipped_count,
            "error" => &mut self.error,
            "error_classification" => &mut self.error_classification,
            "error_retryable" => &mut self.error_retryable,
            "backend_attempt_count" => &mut self.backend_attempt_count,
            "backend_attempts" => &mut self.backend_attempts,
            "fallback_status" => &mut self.fallback_status,
            "report_ref" => &mut self.report_ref,
            "artifacts" => &mut self.artifacts,
            "started_at" => &mut self.started_at,
            "completed_at" => &mut self.completed_at,
            "completed_at_micros" => &mut self.completed_at_micros,
            _ => return None,
        })
    }

    fn require(&self) -> Result<()> {
        for (present, name) in [
            (self.schema_version, "schema_version"),
            (self.run_id, "run_id"),
            (self.trigger, "trigger"),
            (self.task, "task"),
            (self.backend, "backend"),
            (self.status, "status"),
            (self.accepted_count, "accepted_count"),
            (self.rejected_count, "rejected_count"),
            (self.started_at, "started_at"),
            (self.completed_at, "completed_at"),
        ] {
            if !present {
                return Err(config_error(format!(
                    "automation ledger record is missing {name}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct RetryAttemptFields {
    attempt: bool,
    succeeded: bool,
    failure_classification: bool,
    backoff_millis: bool,
}

impl RetryAttemptFields {
    fn field_mut(&mut self, key: &str) -> Option<&mut bool> {
        match key {
            "attempt" => Some(&mut self.attempt),
            "succeeded" => Some(&mut self.succeeded),
            "failure_classification" => Some(&mut self.failure_classification),
            "backoff_millis" => Some(&mut self.backoff_millis),
            _ => None,
        }
    }

    fn require(&self) -> Result<()> {
        for (present, name) in [
            (self.attempt, "attempt"),
            (self.succeeded, "succeeded"),
            (self.backoff_millis, "backoff_millis"),
        ] {
            if !present {
                return Err(config_error(format!(
                    "automation retry attempt is missing {name}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct ArtifactFields {
    schema_version: bool,
    kind: bool,
    path: bool,
    sha256: bool,
    summary: bool,
    created_at: bool,
}

fn compare_decoded_bytes(
    expected: &[u8],
    compared: &mut usize,
    matches: &mut bool,
    decoded: &[u8],
) {
    if *matches {
        let end = compared.saturating_add(decoded.len());
        *matches = end <= expected.len() && expected.get(*compared..end) == Some(decoded);
    }
    *compared = compared.saturating_add(decoded.len());
}

impl ArtifactFields {
    fn field_mut(&mut self, key: &str) -> Option<&mut bool> {
        match key {
            "schema_version" => Some(&mut self.schema_version),
            "kind" => Some(&mut self.kind),
            "path" => Some(&mut self.path),
            "sha256" => Some(&mut self.sha256),
            "summary" => Some(&mut self.summary),
            "created_at" => Some(&mut self.created_at),
            _ => None,
        }
    }

    fn require(&self) -> Result<()> {
        for (present, name) in [
            (self.schema_version, "schema_version"),
            (self.kind, "kind"),
            (self.path, "path"),
            (self.sha256, "sha256"),
            (self.created_at, "created_at"),
        ] {
            if !present {
                return Err(config_error(format!(
                    "automation artifact is missing {name}"
                )));
            }
        }
        Ok(())
    }
}

impl<'a> JsonRangeReader<'a> {
    fn new(file: &'a std::fs::File, path: &'a Path, span: Range<u64>) -> Self {
        Self {
            file,
            path,
            end: span.end,
            position: span.start,
            buffer_start: span.start,
            buffer_len: 0,
            buffer: [0_u8; JSON_SCAN_BUFFER_BYTES],
        }
    }

    fn parse_ledger_row(mut self) -> Result<Option<RunLedgerRowProjection>> {
        self.skip_whitespace()?;
        if self.is_eof() {
            return Ok(None);
        }
        let row_start = self.position;
        self.expect_byte(b'{', "automation ledger row must be a JSON object")?;
        let mut schema_version = None;
        let mut run_id = None;
        let mut status = None;
        let mut trigger = None;
        let mut task = None;
        let mut task_key = None;
        let mut task_key_seen = false;
        let mut session_evidence_budget_exhausted_error = false;
        let mut error_seen = false;
        let mut started_at = None;
        let mut completed_at = None;
        let mut completed_at_micros = None;
        let mut completed_at_micros_seen = false;
        let mut validation_report = None;
        let mut validation_report_seen = false;
        self.skip_whitespace()?;
        if self.consume_if(b'}')? {
            return self.fail("automation ledger row is missing required identity fields");
        }
        loop {
            self.skip_whitespace()?;
            let key = self.read_known_key(PROJECTION_KEYS)?.unwrap_or("");
            self.skip_whitespace()?;
            self.expect_byte(b':', "expected ':' after JSON object key")?;
            self.skip_whitespace()?;
            match key {
                "schema_version" => {
                    if schema_version.is_some() {
                        return self.fail("duplicate schema_version field");
                    }
                    schema_version = Some(self.read_u32("automation schema version")?);
                }
                "run_id" => {
                    if run_id.is_some() {
                        return self.fail("duplicate run_id field");
                    }
                    run_id = Some(self.read_string_bounded(
                        PROJECTED_TEXT_MAX_BYTES,
                        "automation run identity",
                    )?);
                }
                "status" => {
                    if status.is_some() {
                        return self.fail("duplicate status field");
                    }
                    let value = self.read_string_bounded(32, "automation run status")?;
                    status = Some(parse_status(&value).ok_or_else(|| {
                        config_error(format!("unknown automation run status '{value}'"))
                    })?);
                }
                "trigger" => {
                    if trigger.is_some() {
                        return self.fail("duplicate trigger field");
                    }
                    let value = self.read_string_bounded(32, "automation run trigger")?;
                    trigger = Some(parse_trigger(&value).ok_or_else(|| {
                        config_error(format!("unknown automation run trigger '{value}'"))
                    })?);
                }
                "task" => {
                    if task.is_some() {
                        return self.fail("duplicate task field");
                    }
                    let value = self.read_string_bounded(32, "automation task kind")?;
                    task = Some(parse_task(&value).ok_or_else(|| {
                        config_error(format!("unknown automation task kind '{value}'"))
                    })?);
                }
                "task_key" => {
                    if task_key_seen {
                        return self.fail("duplicate task_key field");
                    }
                    task_key_seen = true;
                    task_key = self.read_optional_string_bounded(
                        PROJECTED_TEXT_MAX_BYTES,
                        "automation task key",
                    )?;
                }
                "error" => {
                    if error_seen {
                        return self.fail("duplicate error field");
                    }
                    error_seen = true;
                    session_evidence_budget_exhausted_error = if self.peek_byte()? == Some(b'n') {
                        self.skip_literal(b"null")?;
                        false
                    } else {
                        self.read_string_equals(
                            tracedecay_automation::evidence_budget::SESSION_EVIDENCE_BUDGET_EXHAUSTED,
                        )?
                    };
                }
                "started_at" => {
                    if started_at.is_some() {
                        return self.fail("duplicate started_at field");
                    }
                    started_at = Some(self.read_string_bounded(
                        PROJECTED_TEXT_MAX_BYTES,
                        "automation start timestamp",
                    )?);
                }
                "completed_at" => {
                    if completed_at.is_some() {
                        return self.fail("duplicate completed_at field");
                    }
                    completed_at = Some(self.read_string_bounded(
                        PROJECTED_TEXT_MAX_BYTES,
                        "automation completion timestamp",
                    )?);
                }
                "completed_at_micros" => {
                    if completed_at_micros_seen {
                        return self.fail("duplicate completed_at_micros field");
                    }
                    completed_at_micros_seen = true;
                    completed_at_micros =
                        self.read_optional_i64("automation completion timestamp microseconds")?;
                }
                "validation_report" => {
                    if validation_report_seen {
                        return self.fail("duplicate validation_report field");
                    }
                    validation_report_seen = true;
                    let start = self.position;
                    self.skip_value(1)?;
                    validation_report = Some(start..self.position);
                }
                _ => self.skip_value(1)?,
            }
            self.skip_whitespace()?;
            if self.consume_if(b'}')? {
                break;
            }
            self.expect_byte(b',', "expected ',' or '}' in JSON object")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b'}') {
                return self.fail("trailing comma in JSON object");
            }
        }
        let row_end = self.position;
        self.skip_whitespace()?;
        if !self.is_eof() {
            return self.fail("trailing bytes after automation ledger row");
        }
        Ok(Some(RunLedgerRowProjection {
            span: row_start..row_end,
            schema_version: schema_version
                .ok_or_else(|| config_error("automation ledger row is missing schema_version"))?,
            run_id: run_id
                .ok_or_else(|| config_error("automation ledger row is missing run_id"))?,
            status: status
                .ok_or_else(|| config_error("automation ledger row is missing status"))?,
            trigger: trigger
                .ok_or_else(|| config_error("automation ledger row is missing trigger"))?,
            task: task.ok_or_else(|| config_error("automation ledger row is missing task"))?,
            task_key,
            session_evidence_budget_exhausted_error,
            started_at: started_at
                .ok_or_else(|| config_error("automation ledger row is missing started_at"))?,
            completed_at: completed_at
                .ok_or_else(|| config_error("automation ledger row is missing completed_at"))?,
            completed_at_micros,
            validation_report,
        }))
    }

    fn validate_ledger_record(mut self) -> Result<()> {
        self.skip_whitespace()?;
        self.expect_byte(b'{', "automation ledger record must be a JSON object")?;
        let mut fields = RecordFields::default();
        self.validate_object_fields(1, RECORD_KEYS, |reader, key| {
            if let Some(seen) = fields.field_mut(key) {
                if *seen {
                    return reader.fail(&format!("duplicate {key} field"));
                }
                *seen = true;
            }
            match key {
                "schema_version" => reader.validate_u32("automation schema version"),
                "run_id" => reader.validate_string("automation run identity"),
                "trigger" => reader.validate_enum("automation run trigger", parse_trigger),
                "task" => reader.validate_enum("automation task kind", parse_task),
                "task_key" | "host_mode" | "prompt_version" | "model" | "evidence_hash"
                | "input_hash" | "output_hash" | "error" | "fallback_status" => {
                    reader.validate_optional_string(key)
                }
                "backend" | "started_at" | "completed_at" => reader.validate_string(key),
                "response_schema" | "proposed_ops" | "applied_ops" | "rejected_ops"
                | "validation_report" | "report_ref" => reader.validate_optional_json_value(2),
                "strict_json" | "error_retryable" => reader.validate_optional_bool(key),
                "status" => reader.validate_enum("automation run status", parse_status),
                "reviewed_count"
                | "accepted_count"
                | "rejected_count"
                | "skipped_count"
                | "backend_attempt_count" => reader.validate_usize(key),
                "error_classification" => reader.validate_optional_enum(
                    "automation failure classification",
                    parse_failure_class,
                ),
                "backend_attempts" => reader.validate_retry_attempts(2),
                "artifacts" => reader.validate_artifacts(2),
                "completed_at_micros" => reader.validate_optional_i64("completion timestamp"),
                _ => reader.skip_value(2),
            }
        })?;
        fields.require()?;
        self.skip_whitespace()?;
        if !self.is_eof() {
            return self.fail("trailing bytes after automation ledger record");
        }
        Ok(())
    }

    fn validate_object_fields(
        &mut self,
        depth: usize,
        known_keys: &'static [&'static str],
        mut validate: impl FnMut(&mut Self, &str) -> Result<()>,
    ) -> Result<()> {
        self.ensure_container_depth(depth)?;
        self.skip_whitespace()?;
        if self.consume_if(b'}')? {
            return Ok(());
        }
        loop {
            self.skip_whitespace()?;
            let key = self.read_known_key(known_keys)?.unwrap_or("");
            self.skip_whitespace()?;
            self.expect_byte(b':', "expected ':' after JSON object key")?;
            self.skip_whitespace()?;
            validate(self, key)?;
            self.skip_whitespace()?;
            if self.consume_if(b'}')? {
                return Ok(());
            }
            self.expect_byte(b',', "expected ',' or '}' in JSON object")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b'}') {
                return self.fail("trailing comma in JSON object");
            }
        }
    }

    fn validate_retry_attempts(&mut self, depth: usize) -> Result<()> {
        self.validate_array(depth, |reader| reader.validate_retry_attempt(depth + 1))
    }

    fn validate_retry_attempt(&mut self, depth: usize) -> Result<()> {
        self.skip_whitespace()?;
        self.expect_byte(b'{', "automation retry attempt must be a JSON object")?;
        let mut fields = RetryAttemptFields::default();
        self.validate_object_fields(depth, RETRY_ATTEMPT_KEYS, |reader, key| {
            if let Some(seen) = fields.field_mut(key) {
                if *seen {
                    return reader.fail(&format!("duplicate {key} field"));
                }
                *seen = true;
            }
            match key {
                "attempt" => reader.validate_u32("automation retry attempt ordinal"),
                "succeeded" => reader.validate_bool("automation retry attempt success"),
                "failure_classification" => reader.validate_optional_enum(
                    "automation failure classification",
                    parse_failure_class,
                ),
                "backoff_millis" => reader.validate_u64("automation retry backoff"),
                _ => reader.skip_value(depth + 1),
            }
        })?;
        fields.require()
    }

    fn validate_artifacts(&mut self, depth: usize) -> Result<()> {
        self.validate_array(depth, |reader| reader.validate_artifact(depth + 1))
    }

    fn validate_artifact(&mut self, depth: usize) -> Result<()> {
        self.skip_whitespace()?;
        self.expect_byte(b'{', "automation artifact must be a JSON object")?;
        let mut fields = ArtifactFields::default();
        self.validate_object_fields(depth, ARTIFACT_KEYS, |reader, key| {
            if let Some(seen) = fields.field_mut(key) {
                if *seen {
                    return reader.fail(&format!("duplicate {key} field"));
                }
                *seen = true;
            }
            match key {
                "schema_version" => reader.validate_u32("automation artifact schema version"),
                "kind" | "path" | "sha256" | "created_at" => reader.validate_string(key),
                "summary" => reader.validate_optional_string("automation artifact summary"),
                _ => reader.skip_value(depth + 1),
            }
        })?;
        fields.require()
    }

    fn validate_array(
        &mut self,
        depth: usize,
        mut validate: impl FnMut(&mut Self) -> Result<()>,
    ) -> Result<()> {
        self.ensure_container_depth(depth)?;
        self.expect_byte(b'[', "expected JSON array")?;
        self.skip_whitespace()?;
        if self.consume_if(b']')? {
            return Ok(());
        }
        loop {
            validate(self)?;
            self.skip_whitespace()?;
            if self.consume_if(b']')? {
                return Ok(());
            }
            self.expect_byte(b',', "expected ',' or ']' in JSON array")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b']') {
                return self.fail("trailing comma in JSON array");
            }
        }
    }

    fn validate_string(&mut self, label: &str) -> Result<()> {
        match self.peek_byte()? {
            Some(b'"') => self.skip_string(),
            _ => self.fail(&format!("{label} must be a JSON string")),
        }
    }

    fn validate_optional_string(&mut self, label: &str) -> Result<()> {
        self.validate_optional(label, |reader| reader.validate_string(label))
    }

    fn validate_bool(&mut self, label: &str) -> Result<()> {
        match self.peek_byte()? {
            Some(b't') => self.skip_literal(b"true"),
            Some(b'f') => self.skip_literal(b"false"),
            _ => self.fail(&format!("{label} must be a JSON boolean")),
        }
    }

    fn validate_optional_bool(&mut self, label: &str) -> Result<()> {
        self.validate_optional(label, |reader| reader.validate_bool(label))
    }

    fn validate_optional(
        &mut self,
        _label: &str,
        validate: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        if self.peek_byte()? == Some(b'n') {
            self.skip_literal(b"null")
        } else {
            validate(self)
        }
    }

    fn validate_enum<T>(
        &mut self,
        label: &str,
        parse: impl FnOnce(&str) -> Option<T>,
    ) -> Result<()> {
        let value = self.read_string_bounded(64, label)?;
        if parse(&value).is_some() {
            Ok(())
        } else {
            self.fail(&format!("unknown {label} '{value}'"))
        }
    }

    fn validate_optional_enum<T>(
        &mut self,
        label: &str,
        parse: impl FnOnce(&str) -> Option<T>,
    ) -> Result<()> {
        if self.peek_byte()? == Some(b'n') {
            self.skip_literal(b"null")
        } else {
            self.validate_enum(label, parse)
        }
    }

    fn validate_u32(&mut self, label: &str) -> Result<()> {
        self.validate_number_as::<u32>(label)
    }

    fn read_u32(&mut self, label: &str) -> Result<u32> {
        let start = self.position;
        self.skip_number()?;
        let end = self.position;
        let len = end
            .checked_sub(start)
            .ok_or_else(|| config_error(format!("{label} span is invalid")))?;
        let mut handle = self.file;
        handle
            .seek(SeekFrom::Start(start))
            .map_err(|error| ledger_io_error(self.path, "seek projected number", error))?;
        let mut number = handle.take(len);
        let value = serde_json::from_reader::<_, u32>(&mut number)
            .map_err(|error| config_error(format!("{label} is invalid: {error}")))?;
        if number.limit() != 0 {
            return self.fail("projected number decode ended before its scanned boundary");
        }
        Ok(value)
    }

    fn validate_u64(&mut self, label: &str) -> Result<()> {
        self.validate_number_as::<u64>(label)
    }

    fn validate_usize(&mut self, label: &str) -> Result<()> {
        self.validate_number_as::<usize>(label)
    }

    fn validate_optional_i64(&mut self, label: &str) -> Result<()> {
        if self.peek_byte()? == Some(b'n') {
            self.skip_literal(b"null")
        } else {
            self.validate_number_as::<i64>(label)
        }
    }

    fn read_optional_i64(&mut self, label: &str) -> Result<Option<i64>> {
        if self.peek_byte()? == Some(b'n') {
            self.skip_literal(b"null")?;
            return Ok(None);
        }
        let start = self.position;
        self.skip_number()?;
        let end = self.position;
        let len = end
            .checked_sub(start)
            .ok_or_else(|| config_error(format!("{label} span is invalid")))?;
        let mut handle = self.file;
        handle
            .seek(SeekFrom::Start(start))
            .map_err(|error| ledger_io_error(self.path, "seek projected number", error))?;
        let mut number = handle.take(len);
        let value = serde_json::from_reader::<_, i64>(&mut number)
            .map_err(|error| config_error(format!("{label} is invalid: {error}")))?;
        if number.limit() != 0 {
            return self.fail("projected number decode ended before its scanned boundary");
        }
        Ok(Some(value))
    }

    fn validate_number_as<T: DeserializeOwned>(&mut self, label: &str) -> Result<()> {
        let start = self.position;
        self.skip_number()?;
        let end = self.position;
        let len = end
            .checked_sub(start)
            .ok_or_else(|| config_error(format!("{label} span is invalid")))?;
        let mut handle = self.file;
        handle
            .seek(SeekFrom::Start(start))
            .map_err(|error| ledger_io_error(self.path, "seek typed number", error))?;
        let mut number = handle.take(len);
        serde_json::from_reader::<_, T>(&mut number)
            .map_err(|error| config_error(format!("{label} is invalid: {error}")))?;
        if number.limit() != 0 {
            return self.fail("typed number decode ended before its scanned boundary");
        }
        Ok(())
    }

    fn validate_optional_json_value(&mut self, depth: usize) -> Result<()> {
        if self.peek_byte()? == Some(b'n') {
            self.skip_literal(b"null")
        } else {
            self.validate_json_value(depth)
        }
    }

    fn validate_json_value(&mut self, depth: usize) -> Result<()> {
        self.skip_whitespace()?;
        match self.peek_byte()? {
            Some(b'{') => {
                self.ensure_container_depth(depth)?;
                self.expect_byte(b'{', "expected JSON object")?;
                self.skip_whitespace()?;
                if self.consume_if(b'}')? {
                    return Ok(());
                }
                loop {
                    self.skip_string()?;
                    self.skip_whitespace()?;
                    self.expect_byte(b':', "expected ':' after JSON object key")?;
                    self.validate_json_value(depth + 1)?;
                    self.skip_whitespace()?;
                    if self.consume_if(b'}')? {
                        return Ok(());
                    }
                    self.expect_byte(b',', "expected ',' or '}' in JSON object")?;
                    self.skip_whitespace()?;
                    if self.peek_byte()? == Some(b'}') {
                        return self.fail("trailing comma in JSON object");
                    }
                }
            }
            Some(b'[') => {
                self.ensure_container_depth(depth)?;
                self.expect_byte(b'[', "expected JSON array")?;
                self.skip_whitespace()?;
                if self.consume_if(b']')? {
                    return Ok(());
                }
                loop {
                    self.validate_json_value(depth + 1)?;
                    self.skip_whitespace()?;
                    if self.consume_if(b']')? {
                        return Ok(());
                    }
                    self.expect_byte(b',', "expected ',' or ']' in JSON array")?;
                    self.skip_whitespace()?;
                    if self.peek_byte()? == Some(b']') {
                        return self.fail("trailing comma in JSON array");
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.validate_json_number(),
            Some(b'"') => self.skip_string(),
            Some(b't') => self.skip_literal(b"true"),
            Some(b'f') => self.skip_literal(b"false"),
            Some(b'n') => self.skip_literal(b"null"),
            _ => self.fail("expected a JSON value"),
        }
    }

    fn validate_json_number(&mut self) -> Result<()> {
        self.validate_number_as::<serde_json::Number>("automation JSON number")
    }

    fn locate_pointer(
        &mut self,
        tokens: &[String],
        token_index: usize,
        depth: usize,
    ) -> Result<Option<Range<u64>>> {
        self.skip_whitespace()?;
        let start = self.position;
        if token_index == tokens.len() {
            self.skip_value(depth)?;
            return Ok(Some(start..self.position));
        }
        match self.peek_byte()? {
            Some(b'{') => self.locate_in_object(tokens, token_index, depth),
            Some(b'[') => self.locate_in_array(tokens, token_index, depth),
            Some(_) => {
                self.skip_value(depth)?;
                Ok(None)
            }
            None => self.fail("unexpected EOF while locating JSON pointer"),
        }
    }

    fn locate_in_object(
        &mut self,
        tokens: &[String],
        token_index: usize,
        depth: usize,
    ) -> Result<Option<Range<u64>>> {
        self.ensure_container_depth(depth)?;
        self.expect_byte(b'{', "expected JSON object")?;
        self.skip_whitespace()?;
        if self.consume_if(b'}')? {
            return Ok(None);
        }
        let mut found = None;
        loop {
            self.skip_whitespace()?;
            let key_matches = self.read_string_equals(&tokens[token_index])?;
            self.skip_whitespace()?;
            self.expect_byte(b':', "expected ':' after JSON object key")?;
            self.skip_whitespace()?;
            if key_matches {
                found = self.locate_pointer(tokens, token_index + 1, depth + 1)?;
            } else {
                self.skip_value(depth + 1)?;
            }
            self.skip_whitespace()?;
            if self.consume_if(b'}')? {
                return Ok(found);
            }
            self.expect_byte(b',', "expected ',' or '}' in JSON object")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b'}') {
                return self.fail("trailing comma in JSON object");
            }
        }
    }

    fn locate_in_array(
        &mut self,
        tokens: &[String],
        token_index: usize,
        depth: usize,
    ) -> Result<Option<Range<u64>>> {
        self.ensure_container_depth(depth)?;
        let requested_index = parse_array_index(&tokens[token_index]);
        self.expect_byte(b'[', "expected JSON array")?;
        self.skip_whitespace()?;
        if self.consume_if(b']')? {
            return Ok(None);
        }
        let mut current_index = 0_usize;
        let mut found = None;
        loop {
            if requested_index == Some(current_index) {
                found = self.locate_pointer(tokens, token_index + 1, depth + 1)?;
            } else {
                self.skip_value(depth + 1)?;
            }
            self.skip_whitespace()?;
            if self.consume_if(b']')? {
                return Ok(found);
            }
            self.expect_byte(b',', "expected ',' or ']' in JSON array")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b']') {
                return self.fail("trailing comma in JSON array");
            }
            current_index = current_index.saturating_add(1);
        }
    }

    fn skip_value(&mut self, depth: usize) -> Result<()> {
        self.skip_whitespace()?;
        match self.peek_byte()? {
            Some(b'{') => self.skip_object(depth),
            Some(b'[') => self.skip_array(depth),
            Some(b'"') => self.skip_string(),
            Some(b't') => self.skip_literal(b"true"),
            Some(b'f') => self.skip_literal(b"false"),
            Some(b'n') => self.skip_literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.skip_number(),
            Some(_) => self.fail("expected a JSON value"),
            None => self.fail("unexpected EOF while reading JSON value"),
        }
    }

    fn skip_object(&mut self, depth: usize) -> Result<()> {
        self.ensure_container_depth(depth)?;
        self.expect_byte(b'{', "expected JSON object")?;
        self.skip_whitespace()?;
        if self.consume_if(b'}')? {
            return Ok(());
        }
        loop {
            self.skip_whitespace()?;
            self.skip_string()?;
            self.skip_whitespace()?;
            self.expect_byte(b':', "expected ':' after JSON object key")?;
            self.skip_value(depth + 1)?;
            self.skip_whitespace()?;
            if self.consume_if(b'}')? {
                return Ok(());
            }
            self.expect_byte(b',', "expected ',' or '}' in JSON object")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b'}') {
                return self.fail("trailing comma in JSON object");
            }
        }
    }

    fn skip_array(&mut self, depth: usize) -> Result<()> {
        self.ensure_container_depth(depth)?;
        self.expect_byte(b'[', "expected JSON array")?;
        self.skip_whitespace()?;
        if self.consume_if(b']')? {
            return Ok(());
        }
        loop {
            self.skip_value(depth + 1)?;
            self.skip_whitespace()?;
            if self.consume_if(b']')? {
                return Ok(());
            }
            self.expect_byte(b',', "expected ',' or ']' in JSON array")?;
            self.skip_whitespace()?;
            if self.peek_byte()? == Some(b']') {
                return self.fail("trailing comma in JSON array");
            }
        }
    }

    fn ensure_container_depth(&self, depth: usize) -> Result<()> {
        if depth >= JSON_MAX_DEPTH {
            self.fail("JSON nesting exceeds the automation ledger depth bound")
        } else {
            Ok(())
        }
    }

    fn skip_number(&mut self) -> Result<()> {
        if self.consume_if(b'-')? && self.is_eof() {
            return self.fail("incomplete JSON number");
        }
        match self.peek_byte()? {
            Some(b'0') => {
                self.next_byte()?;
                if self.peek_byte()?.is_some_and(|byte| byte.is_ascii_digit()) {
                    return self.fail("leading zero in JSON number");
                }
            }
            Some(b'1'..=b'9') => {
                self.next_byte()?;
                while self.peek_byte()?.is_some_and(|byte| byte.is_ascii_digit()) {
                    self.next_byte()?;
                }
            }
            _ => return self.fail("invalid JSON number"),
        }
        if self.consume_if(b'.')? {
            if !self.peek_byte()?.is_some_and(|byte| byte.is_ascii_digit()) {
                return self.fail("JSON fraction has no digits");
            }
            while self.peek_byte()?.is_some_and(|byte| byte.is_ascii_digit()) {
                self.next_byte()?;
            }
        }
        if self
            .peek_byte()?
            .is_some_and(|byte| matches!(byte, b'e' | b'E'))
        {
            self.next_byte()?;
            if self
                .peek_byte()?
                .is_some_and(|byte| matches!(byte, b'+' | b'-'))
            {
                self.next_byte()?;
            }
            if !self.peek_byte()?.is_some_and(|byte| byte.is_ascii_digit()) {
                return self.fail("JSON exponent has no digits");
            }
            while self.peek_byte()?.is_some_and(|byte| byte.is_ascii_digit()) {
                self.next_byte()?;
            }
        }
        Ok(())
    }

    fn skip_literal(&mut self, literal: &[u8]) -> Result<()> {
        for expected in literal {
            self.expect_byte(*expected, "invalid JSON literal")?;
        }
        Ok(())
    }

    fn read_optional_string_bounded(
        &mut self,
        limit: usize,
        label: &str,
    ) -> Result<Option<String>> {
        if self.peek_byte()? == Some(b'n') {
            self.skip_literal(b"null")?;
            Ok(None)
        } else {
            self.read_string_bounded(limit, label).map(Some)
        }
    }

    fn read_string_bounded(&mut self, limit: usize, label: &str) -> Result<String> {
        self.read_string(Some(limit), label)?
            .ok_or_else(|| config_error(format!("failed to capture {label}")))
    }

    fn skip_string(&mut self) -> Result<()> {
        self.read_string(None, "JSON string").map(|_| ())
    }

    fn read_string_equals(&mut self, expected: &str) -> Result<bool> {
        self.expect_byte(b'"', "expected JSON string")?;
        let expected = expected.as_bytes();
        let mut compared = 0_usize;
        let mut matches = true;
        loop {
            let byte = self
                .next_byte()?
                .ok_or_else(|| config_error("unexpected EOF in JSON string"))?;
            match byte {
                b'"' => return Ok(matches && compared == expected.len()),
                b'\\' => {
                    let escaped = self
                        .next_byte()?
                        .ok_or_else(|| config_error("unexpected EOF in JSON escape"))?;
                    match escaped {
                        b'"' | b'\\' | b'/' => {
                            compare_decoded_bytes(
                                expected,
                                &mut compared,
                                &mut matches,
                                &[escaped],
                            );
                        }
                        b'b' => compare_decoded_bytes(expected, &mut compared, &mut matches, &[8]),
                        b'f' => {
                            compare_decoded_bytes(expected, &mut compared, &mut matches, &[12]);
                        }
                        b'n' => {
                            compare_decoded_bytes(expected, &mut compared, &mut matches, b"\n");
                        }
                        b'r' => {
                            compare_decoded_bytes(expected, &mut compared, &mut matches, b"\r");
                        }
                        b't' => {
                            compare_decoded_bytes(expected, &mut compared, &mut matches, b"\t");
                        }
                        b'u' => {
                            let scalar = self.read_unicode_escape()?;
                            let mut encoded = [0_u8; 4];
                            let encoded = scalar.encode_utf8(&mut encoded);
                            compare_decoded_bytes(
                                expected,
                                &mut compared,
                                &mut matches,
                                encoded.as_bytes(),
                            );
                        }
                        _ => return self.fail("invalid JSON string escape"),
                    }
                }
                0x00..=0x1f => return self.fail("unescaped control byte in JSON string"),
                0x20..=0x7f => {
                    compare_decoded_bytes(expected, &mut compared, &mut matches, &[byte]);
                }
                _ => {
                    let scalar = self.read_utf8_scalar(byte)?;
                    let mut encoded = [0_u8; 4];
                    let encoded = scalar.encode_utf8(&mut encoded);
                    compare_decoded_bytes(
                        expected,
                        &mut compared,
                        &mut matches,
                        encoded.as_bytes(),
                    );
                }
            }
        }
    }

    fn read_known_key(
        &mut self,
        known_keys: &'static [&'static str],
    ) -> Result<Option<&'static str>> {
        const LONGEST_KNOWN_KEY_BYTES: usize = 64;

        self.expect_byte(b'"', "expected JSON string")?;
        let mut decoded = [0_u8; LONGEST_KNOWN_KEY_BYTES];
        let mut decoded_len = 0_usize;
        let mut overflowed = false;
        loop {
            let byte = self
                .next_byte()?
                .ok_or_else(|| config_error("unexpected EOF in JSON string"))?;
            let mut encoded = [0_u8; 4];
            let decoded_bytes: &[u8] = match byte {
                b'"' => {
                    if overflowed {
                        return Ok(None);
                    }
                    let key = std::str::from_utf8(&decoded[..decoded_len])
                        .map_err(|_| config_error("JSON object key is not valid UTF-8"))?;
                    return Ok(known_keys.iter().copied().find(|known| *known == key));
                }
                b'\\' => {
                    let escaped = self
                        .next_byte()?
                        .ok_or_else(|| config_error("unexpected EOF in JSON escape"))?;
                    match escaped {
                        b'"' | b'\\' | b'/' => {
                            encoded[0] = escaped;
                            &encoded[..1]
                        }
                        b'b' => b"\x08",
                        b'f' => b"\x0c",
                        b'n' => b"\n",
                        b'r' => b"\r",
                        b't' => b"\t",
                        b'u' => {
                            let scalar = self.read_unicode_escape()?;
                            scalar.encode_utf8(&mut encoded).as_bytes()
                        }
                        _ => return self.fail("invalid JSON string escape"),
                    }
                }
                0x00..=0x1f => return self.fail("unescaped control byte in JSON string"),
                0x20..=0x7f => {
                    encoded[0] = byte;
                    &encoded[..1]
                }
                _ => {
                    let scalar = self.read_utf8_scalar(byte)?;
                    scalar.encode_utf8(&mut encoded).as_bytes()
                }
            };
            if !overflowed {
                let end = decoded_len.saturating_add(decoded_bytes.len());
                if end > decoded.len() {
                    overflowed = true;
                } else {
                    decoded[decoded_len..end].copy_from_slice(decoded_bytes);
                    decoded_len = end;
                }
            }
        }
    }

    fn read_string(&mut self, limit: Option<usize>, label: &str) -> Result<Option<String>> {
        self.expect_byte(b'"', "expected JSON string")?;
        let mut captured = limit.map(|limit| Vec::with_capacity(limit.min(64)));
        loop {
            let byte = self
                .next_byte()?
                .ok_or_else(|| config_error("unexpected EOF in JSON string"))?;
            match byte {
                b'"' => {
                    return captured
                        .map(|bytes| {
                            String::from_utf8(bytes)
                                .map_err(|_| config_error(format!("{label} is not valid UTF-8")))
                        })
                        .transpose();
                }
                b'\\' => {
                    let escaped = self
                        .next_byte()?
                        .ok_or_else(|| config_error("unexpected EOF in JSON escape"))?;
                    match escaped {
                        b'"' | b'\\' | b'/' => {
                            self.append_captured(&mut captured, limit, &[escaped], label)?;
                        }
                        b'b' => self.append_captured(&mut captured, limit, &[8], label)?,
                        b'f' => self.append_captured(&mut captured, limit, &[12], label)?,
                        b'n' => self.append_captured(&mut captured, limit, b"\n", label)?,
                        b'r' => self.append_captured(&mut captured, limit, b"\r", label)?,
                        b't' => self.append_captured(&mut captured, limit, b"\t", label)?,
                        b'u' => {
                            let scalar = self.read_unicode_escape()?;
                            let mut encoded = [0_u8; 4];
                            let encoded = scalar.encode_utf8(&mut encoded);
                            self.append_captured(&mut captured, limit, encoded.as_bytes(), label)?;
                        }
                        _ => return self.fail("invalid JSON string escape"),
                    }
                }
                0x00..=0x1f => return self.fail("unescaped control byte in JSON string"),
                0x20..=0x7f => {
                    self.append_captured(&mut captured, limit, &[byte], label)?;
                }
                _ => {
                    let scalar = self.read_utf8_scalar(byte)?;
                    let mut encoded = [0_u8; 4];
                    let encoded = scalar.encode_utf8(&mut encoded);
                    self.append_captured(&mut captured, limit, encoded.as_bytes(), label)?;
                }
            }
        }
    }

    fn append_captured(
        &self,
        captured: &mut Option<Vec<u8>>,
        limit: Option<usize>,
        bytes: &[u8],
        label: &str,
    ) -> Result<()> {
        let Some(captured) = captured.as_mut() else {
            return Ok(());
        };
        let limit = limit.unwrap_or(0);
        if captured.len().saturating_add(bytes.len()) > limit {
            return self.fail(&format!("{label} exceeds its {limit}-byte bound"));
        }
        captured.extend_from_slice(bytes);
        Ok(())
    }

    fn read_unicode_escape(&mut self) -> Result<char> {
        let first = self.read_hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            self.expect_byte(b'\\', "high surrogate is missing its low surrogate")?;
            self.expect_byte(b'u', "high surrogate is missing its low surrogate")?;
            let second = self.read_hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return self.fail("invalid low surrogate in JSON string");
            }
            0x1_0000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00))
        } else if (0xdc00..=0xdfff).contains(&first) {
            return self.fail("unexpected low surrogate in JSON string");
        } else {
            first as u32
        };
        char::from_u32(scalar).ok_or_else(|| config_error("invalid Unicode scalar in JSON string"))
    }

    fn read_hex_quad(&mut self) -> Result<u16> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let byte = self
                .next_byte()?
                .ok_or_else(|| config_error("unexpected EOF in Unicode escape"))?;
            let digit = match byte {
                b'0'..=b'9' => (byte - b'0') as u16,
                b'a'..=b'f' => (byte - b'a' + 10) as u16,
                b'A'..=b'F' => (byte - b'A' + 10) as u16,
                _ => return self.fail("invalid hexadecimal digit in Unicode escape"),
            };
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn read_utf8_scalar(&mut self, first: u8) -> Result<char> {
        let (continuations, minimum, mut scalar) = match first {
            0xc2..=0xdf => (1, 0x80, (first & 0x1f) as u32),
            0xe0..=0xef => (2, 0x800, (first & 0x0f) as u32),
            0xf0..=0xf4 => (3, 0x1_0000, (first & 0x07) as u32),
            _ => return self.fail("invalid UTF-8 lead byte in JSON string"),
        };
        for _ in 0..continuations {
            let byte = self
                .next_byte()?
                .ok_or_else(|| config_error("unexpected EOF in UTF-8 sequence"))?;
            if !matches!(byte, 0x80..=0xbf) {
                return self.fail("invalid UTF-8 continuation byte in JSON string");
            }
            scalar = (scalar << 6) | (byte & 0x3f) as u32;
        }
        if scalar < minimum || scalar > 0x10_ffff || (0xd800..=0xdfff).contains(&scalar) {
            return self.fail("invalid UTF-8 scalar in JSON string");
        }
        char::from_u32(scalar).ok_or_else(|| config_error("invalid UTF-8 scalar in JSON string"))
    }

    fn skip_whitespace(&mut self) -> Result<()> {
        while self
            .peek_byte()?
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.next_byte()?;
        }
        Ok(())
    }

    fn expect_byte(&mut self, expected: u8, message: &str) -> Result<()> {
        match self.next_byte()? {
            Some(actual) if actual == expected => Ok(()),
            _ => self.fail(message),
        }
    }

    fn consume_if(&mut self, expected: u8) -> Result<bool> {
        if self.peek_byte()? == Some(expected) {
            self.next_byte()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn is_eof(&self) -> bool {
        self.position >= self.end
    }

    fn peek_byte(&mut self) -> Result<Option<u8>> {
        if self.is_eof() {
            return Ok(None);
        }
        let buffer_end = self.buffer_start.saturating_add(self.buffer_len as u64);
        if self.buffer_len == 0 || self.position < self.buffer_start || self.position >= buffer_end
        {
            self.refill()?;
        }
        let index = usize::try_from(self.position - self.buffer_start)
            .map_err(|_| config_error("automation JSON scan offset is not representable"))?;
        Ok(self.buffer.get(index).copied())
    }

    fn next_byte(&mut self) -> Result<Option<u8>> {
        let byte = self.peek_byte()?;
        if byte.is_some() {
            self.position += 1;
        }
        Ok(byte)
    }

    fn refill(&mut self) -> Result<()> {
        let remaining = self.end.saturating_sub(self.position);
        let take = usize::try_from(remaining.min(JSON_SCAN_BUFFER_BYTES as u64))
            .map_err(|_| config_error("automation JSON scan buffer is not representable"))?;
        let mut handle = self.file;
        handle
            .seek(SeekFrom::Start(self.position))
            .map_err(|error| ledger_io_error(self.path, "seek JSON token", error))?;
        handle
            .read_exact(&mut self.buffer[..take])
            .map_err(|error| ledger_io_error(self.path, "read JSON token", error))?;
        self.buffer_start = self.position;
        self.buffer_len = take;
        Ok(())
    }

    fn fail<T>(&self, message: &str) -> Result<T> {
        Err(config_error(format!(
            "automation run ledger '{}' contains malformed JSON at byte {}: {message}",
            self.path.display(),
            self.position
        )))
    }
}

fn parse_status(value: &str) -> Option<AutomationRunStatus> {
    match value {
        "queued" => Some(AutomationRunStatus::Queued),
        "running" => Some(AutomationRunStatus::Running),
        "succeeded" => Some(AutomationRunStatus::Succeeded),
        "failed" => Some(AutomationRunStatus::Failed),
        "skipped" => Some(AutomationRunStatus::Skipped),
        _ => None,
    }
}

fn parse_trigger(value: &str) -> Option<AutomationTrigger> {
    match value {
        "manual_cli" => Some(AutomationTrigger::ManualCli),
        "manual_mcp" => Some(AutomationTrigger::ManualMcp),
        "dashboard" => Some(AutomationTrigger::Dashboard),
        "application" => Some(AutomationTrigger::Application),
        "scheduler" => Some(AutomationTrigger::Scheduler),
        "host_receipt" => Some(AutomationTrigger::HostReceipt),
        _ => None,
    }
}

fn parse_task(value: &str) -> Option<AgentTaskKind> {
    match value {
        "memory_curator" => Some(AgentTaskKind::MemoryCurator),
        "session_reflector" => Some(AgentTaskKind::SessionReflector),
        "skill_writer" => Some(AgentTaskKind::SkillWriter),
        "combined_review" => Some(AgentTaskKind::CombinedReview),
        "user_job" => Some(AgentTaskKind::UserJob),
        _ => None,
    }
}

fn parse_failure_class(value: &str) -> Option<()> {
    matches!(
        value,
        "retryable"
            | "permanent"
            | "timeout"
            | "unavailable"
            | "denied"
            | "disconnected"
            | "malformed_output"
    )
    .then_some(())
}

fn parse_array_index(value: &str) -> Option<usize> {
    if value.starts_with('+') || (value.starts_with('0') && value.len() != 1) {
        None
    } else {
        value.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_line(run_id: &str, status: &str, completed_at: i64) -> String {
        format!(
            "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
             \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"{status}\",\
             \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"{completed_at}\",\
             \"completed_at\":\"{completed_at}\",\"completed_at_micros\":{}}}",
            completed_at.saturating_mul(1_000_000),
        )
    }

    fn write_ledger(lines: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("ledger");
        (temp, path)
    }

    #[test]
    fn returns_newest_exact_lifecycle_record_across_large_ledger() {
        let mut lines = vec![ledger_line("target", "queued", 1)];
        let padding = "x".repeat(1024);
        for index in 0..9_000 {
            lines.push(format!(
                "{{\"schema_version\":2,\"run_id\":\"unrelated-{index}\",\"trigger\":\"scheduler\",\
                 \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"running\",\
                 \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"2\",\"completed_at\":\"2\",\
                 \"error\":\"{padding}\"}}"
            ));
        }
        lines.push(ledger_line("target", "running", 3));
        let (_temp, path) = write_ledger(&lines);
        assert!(std::fs::metadata(&path).expect("metadata").len() > 8 * 1024 * 1024);

        let record = read_exact_run_record_bounded(&path, "target")
            .expect("bounded read")
            .expect("exact record");

        assert_eq!(record.run_id, "target");
        assert_eq!(record.status, AutomationRunStatus::Running);
        assert!(
            read_exact_run_record_bounded(&path, "missing")
                .expect("bounded read")
                .is_none()
        );
    }

    #[test]
    fn reads_legacy_row_without_fabricating_completion_precision() {
        let line = "{\"schema_version\":1,\"run_id\":\"target\",\"trigger\":\"manual_cli\",\
                    \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"succeeded\",\
                    \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"1970-01-01T00:00:01Z\",\
                    \"completed_at\":\"1970-01-01T00:00:02Z\"}";
        let (_temp, path) = write_ledger(&[line.to_owned()]);

        let record = read_exact_run_record_bounded(&path, "target")
            .expect("bounded read")
            .expect("legacy record");

        assert_eq!(record.completed_at_micros, None);
    }

    #[test]
    fn rejects_malformed_or_ambiguous_rows_without_unrelated_size_poisoning() {
        let malformed = vec![ledger_line("target", "succeeded", 1), "not json".to_owned()];
        let (_temp, malformed_path) = write_ledger(&malformed);
        assert!(read_exact_run_record_bounded(&malformed_path, "target").is_err());

        let oversized = vec![
            ledger_line("target", "succeeded", 1),
            ledger_line("unrelated", "succeeded", 2).replace(
                "\"completed_at\":\"2\"",
                &format!(
                    "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"2\"",
                    "x".repeat(2 * 1024 * 1024)
                ),
            ),
        ];
        let (_temp, oversized_path) = write_ledger(&oversized);
        assert_eq!(
            read_exact_run_record_bounded(&oversized_path, "target")
                .expect("streaming identity scan")
                .expect("target")
                .run_id,
            "target"
        );

        let duplicate_terminals = vec![
            ledger_line("target", "succeeded", 1),
            ledger_line("target", "failed", 2),
        ];
        let (_temp, ambiguous_path) = write_ledger(&duplicate_terminals);
        assert!(read_exact_run_record_bounded(&ambiguous_path, "target").is_err());
    }

    #[test]
    fn exact_lookup_streams_long_keys_inside_canonical_values() {
        let line = ledger_line("target", "succeeded", 1).replace(
            "\"completed_at\":\"1\"",
            &format!(
                "\"validation_report\":{{\"{}\":null}},\"completed_at\":\"1\"",
                "κ".repeat(PROJECTED_TEXT_MAX_BYTES)
            ),
        );
        let (_temp, path) = write_ledger(&[line]);

        assert_eq!(
            read_exact_run_record_bounded(&path, "target")
                .expect("streaming exact lookup")
                .expect("target")
                .run_id,
            "target"
        );
    }

    #[test]
    fn identity_tokens_fail_at_their_bounds_but_unknown_keys_stream() {
        let huge_run = ledger_line(&"x".repeat(PROJECTED_TEXT_MAX_BYTES + 1), "running", 1);
        let (_temp, huge_run_path) = write_ledger(&[huge_run]);
        assert!(read_exact_run_record_bounded(&huge_run_path, "target").is_err());

        let huge_status = ledger_line("other", &"x".repeat(64 * 1024), 1);
        let (_temp, huge_status_path) = write_ledger(&[huge_status]);
        assert!(read_exact_run_record_bounded(&huge_status_path, "target").is_err());

        let huge_key = ledger_line("target", "running", 1).replace(
            "\"completed_at\":\"1\"",
            &format!(
                "\"{}\":null,\"completed_at\":\"1\"",
                "k".repeat(PROJECTED_TEXT_MAX_BYTES + 1)
            ),
        );
        let (_temp, huge_key_path) = write_ledger(&[huge_key]);
        assert_eq!(
            read_exact_run_record_bounded(&huge_key_path, "target")
                .expect("streaming unknown key")
                .expect("target")
                .run_id,
            "target"
        );
    }

    #[test]
    fn escaped_identity_and_crlf_preserve_exact_digest_span() {
        let raw = "  {\"schema_version\":2,\"run_\\u0069d\":\"tar\\u0067et\",\"trigger\":\"scheduler\",\"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"running\",\"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"1\",\"completed_at\":\"1\",\"payload\":\"large\"}  \r\n";
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(&path, raw).expect("ledger");
        let digest = read_exact_run_digest(&path, "target")
            .expect("digest")
            .expect("target digest");
        let file = std::fs::File::open(&path).expect("ledger");
        let identity = read_exact_run_identity_from_file(&file, &path, "target")
            .expect("identity")
            .expect("target identity");
        let expected = digest_from_hasher({
            let mut hasher = Sha256::new();
            hasher.update(raw.trim().as_bytes());
            hasher
        })
        .expect("expected digest");
        assert_eq!(digest, expected);
        assert_eq!(identity.digest, expected);
        assert_eq!(identity.payload_len, raw.trim().len() as u64);
    }

    #[test]
    fn exact_match_stream_validates_large_value_before_full_decode() {
        let malformed = ledger_line("target", "succeeded", 1).replace(
            "\"rejected_count\":0,",
            &format!(
                "\"validation_report\":{{\"payload\":\"{}\"}},\"rejected_count\":false,",
                "x".repeat(2 * 1024 * 1024)
            ),
        );
        let (_temp, path) = write_ledger(&[malformed]);

        assert!(read_exact_run_record_bounded(&path, "target").is_err());
    }

    #[test]
    fn exact_lookup_rejects_valid_json_without_commit_newline() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(&path, ledger_line("target", "succeeded", 1)).expect("ledger");

        let error = read_exact_run_record_bounded(&path, "target")
            .expect_err("unterminated ledger row must not be committed");

        assert!(error.to_string().contains("incomplete durable tail"));
    }

    #[test]
    fn exact_lookup_rejects_unrelated_schema_invalid_row() {
        let unrelated = ledger_line("unrelated", "succeeded", 1)
            .replace("\"accepted_count\":0", "\"accepted_count\":false");
        let (_temp, path) = write_ledger(&[ledger_line("target", "succeeded", 2), unrelated]);

        assert!(read_exact_run_record_bounded(&path, "target").is_err());
    }

    #[test]
    fn exact_lookup_rejects_semantically_invalid_unrelated_rows() {
        let unrelated = ledger_line("unrelated", "succeeded", 1);
        let invalid_rows = [
            unrelated
                .clone()
                .replace("\"schema_version\":2", "\"schema_version\":99"),
            unrelated
                .clone()
                .replace("\"schema_version\":2", "\"schema_version\":1")
                .replace(
                    "\"started_at\":\"1\"",
                    "\"started_at\":\"1970-01-01T00:00:01Z\"",
                ),
            unrelated.clone().replace(
                "\"completed_at\":\"1\"",
                "\"completed_at\":\"9223372036854775807\"",
            ),
            unrelated.replace(
                "\"completed_at_micros\":1000000",
                "\"completed_at_micros\":2000000",
            ),
        ];

        for invalid in invalid_rows {
            let (_temp, path) = write_ledger(&[ledger_line("target", "succeeded", 2), invalid]);
            assert!(read_exact_run_record_bounded(&path, "target").is_err());
        }
    }

    #[test]
    fn exact_lookup_skips_blank_committed_rows() {
        // Renamed from exact_lookup_rejects_empty_committed_rows: a blank
        // committed row is benign (Finding 1's scan_jsonl_row fix) and must
        // be skipped, not treated as ledger corruption.
        let (_temp, path) =
            write_ledger(&[ledger_line("target", "succeeded", 1), "   ".to_owned()]);

        let record = read_exact_run_record_bounded(&path, "target")
            .expect("blank trailing row must not block exact lookup")
            .expect("target row must still be found");
        assert_eq!(record.run_id, "target");
    }

    #[test]
    fn logical_lifecycle_rejects_semantically_invalid_unselected_row() {
        let invalid = ledger_line("unrelated", "succeeded", 2)
            .replace("\"started_at\":\"2\"", "\"started_at\":\"-1\"");
        let (_temp, path) = write_ledger(&[ledger_line("selected", "succeeded", 1), invalid]);
        let file = std::fs::File::open(&path).unwrap();
        let selected = std::collections::HashSet::from(["selected".to_owned()]);

        assert!(read_logical_run_lifecycles(&file, &path, &selected, true).is_err());
    }

    #[test]
    fn exact_lookup_rejects_lifecycle_identity_mutation() {
        let queued = ledger_line("target", "queued", 1);
        let terminal = ledger_line("target", "succeeded", 2)
            .replace("\"task\":\"memory_curator\"", "\"task\":\"skill_writer\"");
        let (_temp, path) = write_ledger(&[queued, terminal]);

        assert!(read_exact_run_record_bounded(&path, "target").is_err());
    }

    #[test]
    fn exact_lookup_ignores_historical_nonadjacent_retry_without_regression() {
        let queued = ledger_line("target", "queued", 1);
        let running = ledger_line("target", "running", 2);
        let (_temp, path) = write_ledger(&[queued.clone(), running, queued]);

        let record = read_exact_run_record_bounded(&path, "target")
            .expect("compatible historical retry")
            .expect("target");

        assert_eq!(record.status, AutomationRunStatus::Running);
    }

    #[test]
    fn exact_lookup_rejects_conflicting_nonadjacent_retry() {
        let queued = ledger_line("target", "queued", 1);
        let running = ledger_line("target", "running", 2);
        let conflicting_queued = ledger_line("target", "queued", 3);
        let (_temp, path) = write_ledger(&[queued, running, conflicting_queued]);

        assert!(read_exact_run_record_bounded(&path, "target").is_err());
    }

    #[test]
    fn exact_lookup_rejects_completion_time_regression() {
        let queued = ledger_line("target", "queued", 300);
        let running = ledger_line("target", "running", 100);
        let (_temp, path) = write_ledger(&[queued, running]);

        assert!(read_exact_run_record_bounded(&path, "target").is_err());
    }

    #[test]
    fn bounded_schema_validator_checks_nested_and_numeric_types() {
        let retry = ledger_line("retry", "failed", 1).replace(
            "\"completed_at\":\"1\"",
            "\"backend_attempts\":[{\"attempt\":1,\"succeeded\":false,\"backoff_millis\":1}],\"completed_at\":\"1\"",
        );
        let artifact = ledger_line("artifact", "succeeded", 2).replace(
            "\"completed_at\":\"2\"",
            "\"artifacts\":[{\"schema_version\":2,\"kind\":\"future\",\"path\":\"a\",\"sha256\":\"b\",\"created_at\":\"2\"}],\"completed_at\":\"2\"",
        );
        let bad_retry = retry.replace("\"backoff_millis\":1", "\"backoff_millis\":-1");
        let bad_artifact = artifact.replace("\"created_at\":\"2\"", "\"created_at\":2");
        for (line, valid) in [
            (retry, true),
            (artifact, true),
            (bad_retry, false),
            (bad_artifact, false),
        ] {
            let (_temp, path) = write_ledger(&[line]);
            let file = std::fs::File::open(&path).expect("ledger");
            let mut rows = ForwardJsonlScanner::new(&file, &path).expect("scanner");
            let span = rows.next_span().expect("span").expect("row");
            assert_eq!(
                validate_jsonl_row_schema(&file, &path, &span).is_ok(),
                valid
            );
        }
    }

    #[test]
    fn reverse_span_scan_reads_each_fixed_chunk_once() {
        let lines = (0..20_000)
            .map(|index| format!("{{\"row\":{index}}}"))
            .collect::<Vec<_>>();
        let (_temp, path) = write_ledger(&lines);
        let file = std::fs::File::open(&path).expect("ledger");
        let file_len = file.metadata().expect("metadata").len();
        let mut scanner = ReverseJsonlScanner::new(&file, &path).expect("scanner");
        let mut rows = 0;
        while scanner.next_span().expect("span").is_some() {
            rows += 1;
        }
        let expected_chunks = usize::try_from(
            file_len.saturating_add(JSON_SCAN_BUFFER_BYTES as u64 - 1)
                / JSON_SCAN_BUFFER_BYTES as u64,
        )
        .expect("chunk count");
        assert_eq!(rows, lines.len());
        assert_eq!(scanner.chunks_read(), expected_chunks);
    }

    #[test]
    fn forward_span_scan_reads_each_fixed_chunk_once() {
        let lines = (0..20_000)
            .map(|index| format!("{{\"row\":{index}}}"))
            .collect::<Vec<_>>();
        let (_temp, path) = write_ledger(&lines);
        let file = std::fs::File::open(&path).expect("ledger");
        let file_len = file.metadata().expect("metadata").len();
        let mut scanner = ForwardJsonlScanner::new(&file, &path).expect("scanner");
        let mut rows = 0;
        while scanner.next_span().expect("span").is_some() {
            rows += 1;
        }
        let expected_chunks = usize::try_from(
            file_len.saturating_add(JSON_SCAN_BUFFER_BYTES as u64 - 1)
                / JSON_SCAN_BUFFER_BYTES as u64,
        )
        .expect("chunk count");
        assert_eq!(rows, lines.len());
        assert_eq!(scanner.chunks_read(), expected_chunks);
    }

    #[test]
    fn ledger_stream_scanners_require_a_commit_newline() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let row = ledger_line("staged", "succeeded", 1);
        std::fs::write(&path, &row).expect("ledger");
        let file = std::fs::File::open(&path).expect("ledger");

        assert!(ForwardJsonlScanner::new(&file, &path).is_err());
        assert!(ReverseJsonlScanner::new(&file, &path).is_err());
        assert!(
            scan_jsonl_row(&file, &path, 0..row.len() as u64)
                .expect("raw staged payload scan")
                .is_some()
        );
    }

    #[test]
    fn stabilized_open_creates_only_when_requested() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);

        assert!(
            open_stabilized_run_ledger(&path, false)
                .expect("noncreating open")
                .is_none()
        );
        assert!(!path.exists());

        let mut file = open_stabilized_run_ledger(&path, true)
            .expect("creating open")
            .expect("created ledger");
        use std::io::Write as _;
        file.write_all(b"\n")
            .expect("write through stabilized handle");
        file.sync_all().expect("sync created ledger");
        assert_eq!(std::fs::read(&path).expect("created ledger bytes"), b"\n");

        use std::io::{Seek as _, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .expect("seek stabilized handle");
        file.write_all(b" ").expect("overwrite without append mode");
        file.sync_all().expect("sync overwritten ledger");
        assert_eq!(
            std::fs::read(&path).expect("overwritten ledger bytes"),
            b" "
        );
    }
}
