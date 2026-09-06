//! Incremental per-parent-transcript index of Cursor subagent dispatch models.
//!
//! The observation-admission path used to open each candidate parent and
//! materialize every JSONL record as a `serde_json::Value` from byte zero on
//! every subagent batch. This index keeps a verified byte cursor, a
//! native file revision, and a SHA-256 digest of every byte through that
//! cursor.
//!
//! Every cached prefix is content-validated from the one open file handle
//! before serving or scanning only an appended delta. The handle's revision is
//! checked again before parsed models are committed.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde_json::Value;
use tracedecay_store::cursor_dispatch::{
    cursor_dispatch_model, is_subagent_dispatch_tool, record_bytes_may_name_subagent_dispatch,
};

use crate::runtime::source::{
    JsonlFileChangeToken, JsonlNativeFileIdentity, MAX_JSONL_RECORD_BYTES, RawJsonlFrame,
    RawJsonlFrameReader, ResumeDigest, jsonl_file_change_token, jsonl_native_file_identity,
    jsonl_prefix_digest,
};

/// Bound on retained parent-transcript entries.
///
/// One entry is one parent Cursor session transcript. A single operator
/// process sees tens of live sessions, not hundreds; 512 is an order of
/// magnitude above that so a warming sweep across many projects still
/// hits, while a pathological long-lived daemon cannot grow without bound.
/// Eviction only drops memoized (agent → model) maps; the next lookup
/// rescans that parent from byte zero.
const MAX_PARENT_ENTRIES: usize = 512;

const DISPATCH_AGENT_KEYS: &[&str] = &[
    "agent_id",
    "agentId",
    "subagent_id",
    "subagentId",
    "session_id",
    "sessionId",
    "id",
];

/// Bytes and records consumed by one parent-dispatch lookup.
///
/// Production callers feed these into Hotpath gauges. Tests assert scan
/// bounds from the same receipt — there is no test-only production port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchScanReceipt {
    pub bytes_parsed: u64,
    pub records_parsed: u64,
    pub rescanned_from_zero: bool,
}

impl DispatchScanReceipt {
    pub const EMPTY: Self = Self {
        bytes_parsed: 0,
        records_parsed: 0,
        rescanned_from_zero: false,
    };

    fn merge(&mut self, other: Self) {
        self.bytes_parsed = self.bytes_parsed.saturating_add(other.bytes_parsed);
        self.records_parsed = self.records_parsed.saturating_add(other.records_parsed);
        self.rescanned_from_zero |= other.rescanned_from_zero;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ParentFileRevision {
    identity: JsonlNativeFileIdentity,
    len: u64,
    change: JsonlFileChangeToken,
}

struct ParentDispatchEntry {
    revision: ParentFileRevision,
    verified_cursor: u64,
    resume_digest: ResumeDigest,
    models: HashMap<String, String>,
}

enum LookupPlan {
    RefreshObserved {
        model: Option<String>,
    },
    Scan {
        start: u64,
        reset: bool,
        resume_digest: ResumeDigest,
    },
}

struct ScanCommit<'a> {
    parent_path: &'a Path,
    revision: ParentFileRevision,
    start: u64,
    reset: bool,
    resume_digest: ResumeDigest,
    agent_id: &'a str,
}

struct ParentDispatchIndex {
    entries: HashMap<PathBuf, ParentDispatchEntry>,
    lru: VecDeque<PathBuf>,
}

impl ParentDispatchIndex {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn lookup(
        &mut self,
        parent_path: &Path,
        agent_id: &str,
    ) -> (Option<String>, DispatchScanReceipt) {
        let mut file = match File::open(parent_path) {
            Ok(file) => file,
            Err(_) => {
                self.forget(parent_path);
                return (None, DispatchScanReceipt::EMPTY);
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                self.forget(parent_path);
                return (None, DispatchScanReceipt::EMPTY);
            }
        };
        let Some(identity) = jsonl_native_file_identity(&file, &metadata) else {
            self.forget(parent_path);
            return (None, DispatchScanReceipt::EMPTY);
        };
        let revision = ParentFileRevision {
            identity,
            len: metadata.len(),
            change: jsonl_file_change_token(&metadata),
        };
        let plan = match self.plan_lookup(parent_path, &mut file, revision, agent_id) {
            Ok(plan) => plan,
            Err(_) => {
                self.forget(parent_path);
                return (None, DispatchScanReceipt::EMPTY);
            }
        };
        match plan {
            LookupPlan::RefreshObserved { model } => {
                if let Some(entry) = self.entries.get_mut(parent_path) {
                    entry.revision = revision;
                }
                self.touch(parent_path);
                (model, DispatchScanReceipt::EMPTY)
            }
            LookupPlan::Scan {
                start,
                reset,
                resume_digest,
            } => {
                if reset {
                    self.insert_reset(parent_path, revision);
                }
                self.touch(parent_path);
                self.scan_and_commit(
                    file,
                    ScanCommit {
                        parent_path,
                        revision,
                        start,
                        reset,
                        resume_digest,
                        agent_id,
                    },
                )
            }
        }
    }

    fn plan_lookup(
        &self,
        parent_path: &Path,
        file: &mut File,
        revision: ParentFileRevision,
        agent_id: &str,
    ) -> std::io::Result<LookupPlan> {
        let Some(entry) = self.entries.get(parent_path) else {
            return Ok(LookupPlan::Scan {
                start: 0,
                reset: true,
                resume_digest: ResumeDigest::new(),
            });
        };
        if entry.revision.identity != revision.identity || revision.len < entry.verified_cursor {
            return Ok(LookupPlan::Scan {
                start: 0,
                reset: true,
                resume_digest: ResumeDigest::new(),
            });
        }
        let verified_cursor = entry.verified_cursor;
        let expected = entry.resume_digest.witness(verified_cursor);
        let cached = entry.models.get(agent_id).cloned();
        let (resume_digest, _) = jsonl_prefix_digest(file, verified_cursor)?;
        if resume_digest.witness(verified_cursor) == expected {
            // Unverified trailing bytes (a partial frame) are re-read after
            // validating the complete prefix, even when metadata is unchanged.
            if revision.len > verified_cursor {
                Ok(LookupPlan::Scan {
                    start: verified_cursor,
                    reset: false,
                    resume_digest,
                })
            } else {
                Ok(LookupPlan::RefreshObserved { model: cached })
            }
        } else {
            Ok(LookupPlan::Scan {
                start: 0,
                reset: true,
                resume_digest: ResumeDigest::new(),
            })
        }
    }

    fn scan_and_commit(
        &mut self,
        file: File,
        scan: ScanCommit<'_>,
    ) -> (Option<String>, DispatchScanReceipt) {
        let delta = match scan_parent_delta(file, scan.start, scan.resume_digest, scan.agent_id) {
            Ok(delta) => delta,
            Err(_) => {
                self.forget(scan.parent_path);
                return (None, DispatchScanReceipt::EMPTY);
            }
        };
        let final_metadata = match delta.file.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                self.forget(scan.parent_path);
                return (None, DispatchScanReceipt::EMPTY);
            }
        };
        let Some(final_identity) = jsonl_native_file_identity(&delta.file, &final_metadata) else {
            self.forget(scan.parent_path);
            return (None, DispatchScanReceipt::EMPTY);
        };
        let final_revision = ParentFileRevision {
            identity: final_identity,
            len: final_metadata.len(),
            change: jsonl_file_change_token(&final_metadata),
        };
        if final_revision != scan.revision {
            self.forget(scan.parent_path);
            return (None, DispatchScanReceipt::EMPTY);
        }
        let receipt = DispatchScanReceipt {
            bytes_parsed: delta.bytes_parsed,
            records_parsed: delta.records_parsed,
            rescanned_from_zero: scan.reset || scan.start == 0,
        };
        let Some(entry) = self.entries.get_mut(scan.parent_path) else {
            return (delta.transient_model, receipt);
        };
        for (id, model) in delta.models {
            entry.models.entry(id).or_insert(model);
        }
        entry.revision = final_revision;
        entry.verified_cursor = delta.verified_cursor;
        entry.resume_digest = delta.resume_digest;
        let model = entry
            .models
            .get(scan.agent_id)
            .cloned()
            .or(delta.transient_model);
        (model, receipt)
    }

    fn insert_reset(&mut self, path: &Path, revision: ParentFileRevision) {
        let existed = self.entries.contains_key(path);
        self.entries.insert(
            path.to_path_buf(),
            ParentDispatchEntry {
                revision,
                verified_cursor: 0,
                resume_digest: ResumeDigest::new(),
                models: HashMap::new(),
            },
        );
        if !existed {
            self.lru.push_back(path.to_path_buf());
            self.evict_if_needed();
        }
    }

    fn forget(&mut self, path: &Path) {
        self.entries.remove(path);
        if let Some(index) = self.lru.iter().position(|candidate| candidate == path) {
            self.lru.remove(index);
        }
    }

    fn touch(&mut self, path: &Path) {
        if let Some(index) = self.lru.iter().position(|candidate| candidate == path) {
            self.lru.remove(index);
        }
        self.lru.push_back(path.to_path_buf());
    }

    fn evict_if_needed(&mut self) {
        while self.entries.len() > MAX_PARENT_ENTRIES {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

struct ScanDelta {
    file: File,
    models: HashMap<String, String>,
    verified_cursor: u64,
    resume_digest: ResumeDigest,
    bytes_parsed: u64,
    records_parsed: u64,
    transient_model: Option<String>,
}

fn shared_parent_dispatch_index() -> &'static Mutex<ParentDispatchIndex> {
    static INDEX: OnceLock<Mutex<ParentDispatchIndex>> = OnceLock::new();
    INDEX.get_or_init(|| Mutex::new(ParentDispatchIndex::new()))
}

/// Resolve the model a parent Cursor transcript assigned to `agent_id`.
///
/// Tries `{parent_dir}/{parent_session_id}.jsonl` first, then
/// `{parent_dir}.jsonl`.
pub fn parent_dispatch_model_for_subagent(
    path: &Path,
    parent_session_id: &str,
    agent_id: &str,
) -> Option<String> {
    parent_dispatch_model_for_subagent_with_receipt(path, parent_session_id, agent_id).0
}

/// Same two-candidate lookup as [`parent_dispatch_model_for_subagent`],
/// plus the scan receipt for gauges and bound tests.
pub fn parent_dispatch_model_for_subagent_with_receipt(
    path: &Path,
    parent_session_id: &str,
    agent_id: &str,
) -> (Option<String>, DispatchScanReceipt) {
    let Some(parent_dir) = path.parent().and_then(Path::parent) else {
        return (None, DispatchScanReceipt::EMPTY);
    };
    let candidates = [
        parent_dir.join(format!("{parent_session_id}.jsonl")),
        parent_dir.with_extension("jsonl"),
    ];
    let mut receipt = DispatchScanReceipt::EMPTY;
    let mut index = shared_parent_dispatch_index()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    for candidate in &candidates {
        let (model, scan) = index.lookup(candidate, agent_id);
        receipt.merge(scan);
        if let Some(model) = model {
            return (Some(model), receipt);
        }
    }
    (None, receipt)
}

pub(super) fn record_dispatch_scan_gauges(receipt: DispatchScanReceipt) {
    if receipt.bytes_parsed > 0 {
        hotpath::gauge!("sessions.hosts.cursor.dispatch_model_bytes_parsed")
            .inc(receipt.bytes_parsed);
    }
    if receipt.records_parsed > 0 {
        hotpath::gauge!("sessions.hosts.cursor.dispatch_model_records_parsed")
            .inc(receipt.records_parsed);
    }
    if receipt.rescanned_from_zero {
        hotpath::gauge!("sessions.hosts.cursor.dispatch_model_rescan_from_zero").inc(1u64);
    }
}

fn scan_parent_delta(
    file: File,
    start: u64,
    resume_digest: ResumeDigest,
    requested_agent: &str,
) -> std::io::Result<ScanDelta> {
    hotpath::measure_block!("sessions.hosts.cursor.dispatch_model_scan", {
        scan_parent_delta_inner(file, start, resume_digest, requested_agent)
    })
}

fn scan_parent_delta_inner(
    mut file: File,
    start: u64,
    resume_digest: ResumeDigest,
    requested_agent: &str,
) -> std::io::Result<ScanDelta> {
    file.seek(SeekFrom::Start(start))?;
    let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);
    frames.seed_resume_digest(resume_digest.clone());
    let mut models = HashMap::new();
    let mut verified_cursor = start;
    let mut verified_digest = resume_digest;
    let mut bytes_parsed = 0_u64;
    let mut records_parsed = 0_u64;
    let mut transient_model = None;

    loop {
        match frames.next_frame()? {
            RawJsonlFrame::Eof => break,
            RawJsonlFrame::Complete { byte_len } => {
                bytes_parsed = bytes_parsed.saturating_add(byte_len);
                verified_cursor = verified_cursor.saturating_add(byte_len);
                verified_digest = frames.resume_digest();
                let record = frames.record();
                if !record_bytes_may_name_subagent_dispatch(record) {
                    continue;
                }
                records_parsed = records_parsed.saturating_add(1);
                if let Ok(value) = serde_json::from_slice::<Value>(record) {
                    collect_dispatch_models(&value, &mut models);
                }
            }
            RawJsonlFrame::Oversized {
                byte_len,
                terminated: true,
            }
            | RawJsonlFrame::BudgetExhausted { byte_len, .. } => {
                bytes_parsed = bytes_parsed.saturating_add(byte_len);
                verified_cursor = verified_cursor.saturating_add(byte_len);
                verified_digest = frames.resume_digest();
            }
            RawJsonlFrame::Oversized {
                terminated: false, ..
            } => break,
            RawJsonlFrame::Partial { .. } => {
                let record = frames.record();
                if record_bytes_may_name_subagent_dispatch(record) {
                    records_parsed = records_parsed.saturating_add(1);
                    if let Ok(value) = serde_json::from_slice::<Value>(record)
                        && let Some(model) = first_dispatch_model_for_agent(&value, requested_agent)
                    {
                        transient_model = Some(model);
                    }
                }
                break;
            }
        }
    }

    let file = frames.into_inner().into_inner();
    Ok(ScanDelta {
        file,
        models,
        verified_cursor,
        resume_digest: verified_digest,
        bytes_parsed,
        records_parsed,
        transient_model,
    })
}

fn collect_dispatch_models(record: &Value, models: &mut HashMap<String, String>) {
    for item in record_content_items(record) {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !is_subagent_dispatch_tool(name) {
            continue;
        }
        let Some(model) = cursor_dispatch_model(item) else {
            continue;
        };
        for agent_id in dispatch_target_ids(item) {
            models
                .entry(agent_id.to_string())
                .or_insert_with(|| model.clone());
        }
    }
}

fn first_dispatch_model_for_agent(record: &Value, agent_id: &str) -> Option<String> {
    for item in record_content_items(record) {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if is_subagent_dispatch_tool(name)
            && dispatch_targets_agent(item, agent_id)
            && let Some(model) = cursor_dispatch_model(item)
        {
            return Some(model);
        }
    }
    None
}

fn record_content_items(record: &Value) -> &[Value] {
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    content.as_array().map_or(&[], Vec::as_slice)
}

fn dispatch_target_ids(item: &Value) -> impl Iterator<Item = &str> {
    let input = item.get("input").unwrap_or(item);
    DISPATCH_AGENT_KEYS.iter().filter_map(move |key| {
        input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

fn dispatch_targets_agent(item: &Value, agent_id: &str) -> bool {
    dispatch_target_ids(item).any(|id| id == agent_id)
}

/// Pre-change full-file scan, kept as the single-record evaluation oracle
/// for equivalence tests. Production lookups go through the index.
#[cfg(test)]
fn uncached_dispatch_model_for_agent(path: &Path, agent_id: &str) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);
    loop {
        let frame = frames.next_frame().ok()?;
        let record = match frame {
            RawJsonlFrame::Eof => return None,
            RawJsonlFrame::Complete { .. } | RawJsonlFrame::Partial { .. } => {
                let Ok(record) = serde_json::from_slice::<Value>(frames.record()) else {
                    continue;
                };
                record
            }
            RawJsonlFrame::Oversized { .. } | RawJsonlFrame::BudgetExhausted { .. } => {
                continue;
            }
        };
        if frames.record().is_empty() {
            continue;
        }
        if let Some(model) = first_dispatch_model_for_agent(&record, agent_id) {
            return Some(model);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use tempfile::TempDir;

    use super::{
        DispatchScanReceipt, parent_dispatch_model_for_subagent_with_receipt,
        uncached_dispatch_model_for_agent,
    };
    use crate::runtime::source::MAX_JSONL_RECORD_BYTES;

    const TEST_ANCHOR_WINDOW_BYTES: u64 = 4096;
    static FIXTURE_SERIAL: AtomicU64 = AtomicU64::new(0);

    struct Layout {
        _tempdir: TempDir,
        child_path: std::path::PathBuf,
        candidate_one: std::path::PathBuf,
        candidate_two: std::path::PathBuf,
        parent_session_id: String,
    }

    fn unique_session() -> String {
        format!("session-{}", FIXTURE_SERIAL.fetch_add(1, Ordering::Relaxed))
    }

    fn layout() -> Layout {
        let tempdir = TempDir::new().unwrap();
        let parent_session_id = unique_session();
        let parent_dir = tempdir.path().join(&parent_session_id);
        let subagents = parent_dir.join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        let child_path = subagents.join("agent-x.jsonl");
        fs::write(
            &child_path,
            b"{\"role\":\"assistant\",\"message\":{\"content\":\"child\"}}\n",
        )
        .unwrap();
        let candidate_one = parent_dir.join(format!("{parent_session_id}.jsonl"));
        let candidate_two = tempdir.path().join(format!("{parent_session_id}.jsonl"));
        Layout {
            _tempdir: tempdir,
            child_path,
            candidate_one,
            candidate_two,
            parent_session_id,
        }
    }

    fn ordinary_record(text: &str) -> String {
        format!(
            r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn dispatch_record(agent_key: &str, agent_id: &str, model: &str) -> String {
        format!(
            r#"{{"role":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu-1","name":"Task","input":{{"description":"dispatch","prompt":"go","{agent_key}":"{agent_id}","model":"{model}"}}}}]}}}}"#
        )
    }

    fn write_lines(path: &std::path::Path, lines: &[String]) {
        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        fs::write(path, body).unwrap();
    }

    fn lookup(layout: &Layout, agent_id: &str) -> (Option<String>, DispatchScanReceipt) {
        parent_dispatch_model_for_subagent_with_receipt(
            &layout.child_path,
            &layout.parent_session_id,
            agent_id,
        )
    }

    #[test]
    fn unchanged_parent_repeated_misses_parse_bytes_once() {
        let layout = layout();
        let records: Vec<String> = (0..64)
            .map(|i| ordinary_record(&format!("turn-{i}")))
            .collect();
        write_lines(&layout.candidate_two, &records);
        let file_len = fs::metadata(&layout.candidate_two).unwrap().len();

        let (model, first) = lookup(&layout, "missing-agent");
        assert!(model.is_none());
        assert_eq!(first.bytes_parsed, file_len);
        assert_eq!(first.records_parsed, 0);
        assert!(first.rescanned_from_zero);

        for _ in 0..16 {
            let (model, again) = lookup(&layout, "missing-agent");
            assert!(model.is_none());
            assert_eq!(again.bytes_parsed, 0);
            assert_eq!(again.records_parsed, 0);
            assert!(!again.rescanned_from_zero);
        }
    }

    #[test]
    fn prefilter_passing_records_are_json_parsed_once() {
        let layout = layout();
        let records: Vec<String> = (0..8)
            .map(|i| ordinary_record(&format!("mention task in turn-{i}")))
            .collect();
        write_lines(&layout.candidate_two, &records);
        let file_len = fs::metadata(&layout.candidate_two).unwrap().len();

        let (model, first) = lookup(&layout, "missing-agent");
        assert!(model.is_none());
        assert_eq!(first.bytes_parsed, file_len);
        assert_eq!(first.records_parsed, records.len() as u64);

        let (_, again) = lookup(&layout, "missing-agent");
        assert_eq!(again.bytes_parsed, 0);
        assert_eq!(again.records_parsed, 0);
    }

    #[test]
    fn append_only_growth_parses_the_delta_and_resolves_late_dispatch() {
        let layout = layout();
        let records: Vec<String> = (0..16)
            .map(|i| ordinary_record(&format!("turn-{i}")))
            .collect();
        write_lines(&layout.candidate_two, &records);
        let before_len = fs::metadata(&layout.candidate_two).unwrap().len();

        let (model, first) = lookup(&layout, "late-agent");
        assert!(model.is_none());
        assert_eq!(first.bytes_parsed, before_len);

        let late = dispatch_record("agent_id", "late-agent", "late-model");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&layout.candidate_two)
            .unwrap();
        writeln!(file, "{late}").unwrap();
        drop(file);
        let after_len = fs::metadata(&layout.candidate_two).unwrap().len();
        let delta = after_len - before_len;

        let (model, appended) = lookup(&layout, "late-agent");
        assert_eq!(model.as_deref(), Some("late-model"));
        assert_eq!(appended.bytes_parsed, delta);
        assert_eq!(appended.records_parsed, 1);
        assert!(!appended.rescanned_from_zero);
    }

    #[test]
    fn truncation_invalidates_stale_models_and_rescans_from_zero() {
        let layout = layout();
        write_lines(
            &layout.candidate_two,
            &[
                ordinary_record("head"),
                dispatch_record("agent_id", "kept-agent", "old-model"),
                ordinary_record("tail"),
            ],
        );
        assert_eq!(
            lookup(&layout, "kept-agent").0.as_deref(),
            Some("old-model")
        );

        write_lines(
            &layout.candidate_two,
            &[dispatch_record("agent_id", "fresh-agent", "fresh-model")],
        );
        let (stale, stale_receipt) = lookup(&layout, "kept-agent");
        assert!(stale.is_none(), "truncated file must drop the stale model");
        assert!(stale_receipt.rescanned_from_zero);
        assert_eq!(
            lookup(&layout, "fresh-agent").0.as_deref(),
            Some("fresh-model")
        );
    }

    #[test]
    fn inode_replacement_rescans_from_zero_and_drops_stale_models() {
        let layout = layout();
        let old = dispatch_record("agent_id", "old-agent", "old-model");
        let new = dispatch_record("agent_id", "new-agent", "new-model");
        assert_eq!(old.len(), new.len(), "fixture must preserve file length");
        write_lines(&layout.candidate_two, &[old]);
        let original_metadata = fs::metadata(&layout.candidate_two).unwrap();
        let original_len = original_metadata.len();
        let original_mtime = filetime::FileTime::from_last_modification_time(&original_metadata);
        assert_eq!(lookup(&layout, "old-agent").0.as_deref(), Some("old-model"));

        let replacement = layout.candidate_two.with_extension("jsonl.replacement");
        write_lines(&replacement, &[new]);
        assert_eq!(
            fs::metadata(&replacement).unwrap().len(),
            original_len,
            "replacement fixture must preserve file length"
        );
        restore_exact_mtime(&replacement, original_mtime);
        fs::rename(&replacement, &layout.candidate_two).unwrap();

        let (stale, receipt) = lookup(&layout, "old-agent");
        assert!(stale.is_none());
        assert!(receipt.rescanned_from_zero);
        assert_eq!(lookup(&layout, "new-agent").0.as_deref(), Some("new-model"));
    }

    fn rewrite_in_place(path: &std::path::Path, lines: &[String]) {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file.flush().unwrap();
    }

    fn restore_exact_mtime(path: &std::path::Path, original: filetime::FileTime) {
        filetime::set_file_mtime(path, original).unwrap();
    }

    #[test]
    fn same_length_in_place_rewrite_rescans_from_zero() {
        let layout = layout();
        let old = dispatch_record("agent_id", "rewrite-agent", "old-model");
        let new = dispatch_record("agent_id", "rewrite-agent", "new-model");
        assert_eq!(old.len(), new.len(), "fixture must preserve file length");
        write_lines(&layout.candidate_two, &[old]);
        assert_eq!(
            lookup(&layout, "rewrite-agent").0.as_deref(),
            Some("old-model")
        );
        let original_mtime = filetime::FileTime::from_last_modification_time(
            &fs::metadata(&layout.candidate_two).unwrap(),
        );

        rewrite_in_place(&layout.candidate_two, &[new]);
        restore_exact_mtime(&layout.candidate_two, original_mtime);

        let (model, receipt) = lookup(&layout, "rewrite-agent");
        assert_eq!(
            model.as_deref(),
            Some("new-model"),
            "same-length in-place rewrite must drop the stale model"
        );
        assert!(
            receipt.rescanned_from_zero,
            "same-length rewrite must invalidate the verified cursor"
        );
    }

    #[test]
    fn exact_mtime_rewrite_with_unchanged_trailing_anchor_rescans_from_zero() {
        let layout = layout();
        let old = dispatch_record("agent_id", "rewrite-agent", "old-model");
        let new = dispatch_record("agent_id", "rewrite-agent", "new-model");
        let unchanged_tail = ordinary_record(&"x".repeat(TEST_ANCHOR_WINDOW_BYTES as usize));
        assert_eq!(old.len(), new.len(), "fixture must preserve file length");
        assert!(
            unchanged_tail.len() > TEST_ANCHOR_WINDOW_BYTES as usize,
            "fixture tail must contain the complete anchor window"
        );

        write_lines(&layout.candidate_two, &[old, unchanged_tail.clone()]);
        assert_eq!(
            lookup(&layout, "rewrite-agent").0.as_deref(),
            Some("old-model")
        );
        let original_metadata = fs::metadata(&layout.candidate_two).unwrap();
        let original_len = original_metadata.len();
        let original_mtime = filetime::FileTime::from_last_modification_time(&original_metadata);

        rewrite_in_place(&layout.candidate_two, &[new, unchanged_tail]);
        assert_eq!(
            fs::metadata(&layout.candidate_two).unwrap().len(),
            original_len,
            "rewrite fixture must preserve file length"
        );
        restore_exact_mtime(&layout.candidate_two, original_mtime);

        let (model, receipt) = lookup(&layout, "rewrite-agent");
        assert_eq!(model.as_deref(), Some("new-model"));
        assert!(receipt.rescanned_from_zero);
    }

    #[cfg(unix)]
    #[test]
    fn same_length_same_second_rewrite_invalidates_stale_models() {
        let layout = layout();
        let old = dispatch_record("agent_id", "old-agent", "old-model");
        let new = dispatch_record("agent_id", "new-agent", "new-model");
        assert_eq!(old.len(), new.len(), "fixture must preserve file length");

        write_lines(&layout.candidate_two, &[old]);
        filetime::set_file_mtime(
            &layout.candidate_two,
            filetime::FileTime::from_unix_time(1_800_000_000, 100_000_000),
        )
        .unwrap();
        assert_eq!(lookup(&layout, "old-agent").0.as_deref(), Some("old-model"));

        write_lines(&layout.candidate_two, &[new]);
        filetime::set_file_mtime(
            &layout.candidate_two,
            filetime::FileTime::from_unix_time(1_800_000_000, 200_000_000),
        )
        .unwrap();

        let (stale, receipt) = lookup(&layout, "old-agent");
        assert!(
            stale.is_none(),
            "same-length rewrite must drop stale models"
        );
        assert!(receipt.rescanned_from_zero);
        assert_eq!(lookup(&layout, "new-agent").0.as_deref(), Some("new-model"));
    }

    #[test]
    fn longer_in_place_rewrite_of_earlier_dispatch_rescans_from_zero() {
        let layout = layout();
        write_lines(
            &layout.candidate_two,
            &[
                dispatch_record("agent_id", "rewrite-agent", "old-model"),
                ordinary_record("kept-tail"),
            ],
        );
        assert_eq!(
            lookup(&layout, "rewrite-agent").0.as_deref(),
            Some("old-model")
        );

        rewrite_in_place(
            &layout.candidate_two,
            &[
                dispatch_record("agent_id", "rewrite-agent", "new-model"),
                ordinary_record("kept-tail"),
                ordinary_record("compaction-extra"),
            ],
        );

        let (model, receipt) = lookup(&layout, "rewrite-agent");
        assert_eq!(
            model.as_deref(),
            Some("new-model"),
            "longer in-place rewrite must not treat a changed prefix as append-only"
        );
        assert!(
            receipt.rescanned_from_zero,
            "changed prefix must invalidate the verified cursor"
        );
    }

    #[test]
    fn candidate_one_wins_when_both_parents_dispatch() {
        let layout = layout();
        write_lines(
            &layout.candidate_one,
            &[dispatch_record("agent_id", "shared", "from-one")],
        );
        write_lines(
            &layout.candidate_two,
            &[dispatch_record("agent_id", "shared", "from-two")],
        );
        assert_eq!(lookup(&layout, "shared").0.as_deref(), Some("from-one"));
    }

    #[test]
    fn candidate_two_is_used_when_candidate_one_lacks_the_dispatch() {
        let layout = layout();
        write_lines(&layout.candidate_one, &[ordinary_record("no dispatch")]);
        write_lines(
            &layout.candidate_two,
            &[dispatch_record("agent_id", "only-two", "from-two")],
        );
        assert_eq!(lookup(&layout, "only-two").0.as_deref(), Some("from-two"));
    }

    #[test]
    fn index_matches_uncached_scan_across_dispatch_shapes() {
        let layout = layout();
        let corpus = [
            ordinary_record("noise"),
            dispatch_record("agentId", "camel-agent", "camel-model"),
            dispatch_record("subagent_id", "sub-agent", "sub-model"),
            dispatch_record("session_id", "session-agent", "session-model"),
            dispatch_record("agent_id", "first-wins", "first-model"),
            dispatch_record("agent_id", "first-wins", "second-model"),
            r#"{"role":"assistant","content":[{"type":"tool_use","name":"subagent","id":"bare","input":{"agent_id":"top-level","model":"top-model"}}]}"#
                .to_string(),
        ];
        write_lines(&layout.candidate_two, &corpus);

        for agent in [
            "camel-agent",
            "sub-agent",
            "session-agent",
            "first-wins",
            "top-level",
            "absent",
        ] {
            let indexed = lookup(&layout, agent).0;
            let uncached = uncached_dispatch_model_for_agent(&layout.candidate_two, agent);
            assert_eq!(indexed, uncached, "agent {agent}");
        }
        assert_eq!(
            lookup(&layout, "first-wins").0.as_deref(),
            Some("first-model")
        );
    }

    #[test]
    fn oversized_frame_is_skipped_like_the_uncached_scanner() {
        let layout = layout();
        let mut oversized = vec![b'x'; MAX_JSONL_RECORD_BYTES + 1];
        oversized.push(b'\n');
        let dispatch = dispatch_record("agent_id", "after-oversize", "oversize-model");
        let mut body = oversized;
        body.extend_from_slice(dispatch.as_bytes());
        body.push(b'\n');
        fs::write(&layout.candidate_two, body).unwrap();

        let indexed = lookup(&layout, "after-oversize").0;
        let uncached = uncached_dispatch_model_for_agent(&layout.candidate_two, "after-oversize");
        assert_eq!(indexed, uncached);
        assert_eq!(indexed.as_deref(), Some("oversize-model"));
    }

    #[test]
    fn parallel_lookups_single_flight_the_parent_scan() {
        let layout = layout();
        let records: Vec<String> = (0..32)
            .map(|i| ordinary_record(&format!("turn-{i}")))
            .collect();
        write_lines(&layout.candidate_two, &records);
        let file_len = fs::metadata(&layout.candidate_two).unwrap().len();
        let child = layout.child_path.clone();
        let session = layout.parent_session_id.clone();

        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let child = child.clone();
                    let session = session.clone();
                    scope.spawn(move || {
                        parent_dispatch_model_for_subagent_with_receipt(&child, &session, "missing")
                    })
                })
                .collect();
            let receipts: Vec<DispatchScanReceipt> = handles
                .into_iter()
                .map(|handle| handle.join().expect("thread").1)
                .collect();
            let total_bytes: u64 = receipts.iter().map(|receipt| receipt.bytes_parsed).sum();
            assert_eq!(
                total_bytes, file_len,
                "the process mutex single-flights one scan; later waiters report 0"
            );
            assert_eq!(
                receipts
                    .iter()
                    .filter(|receipt| receipt.bytes_parsed > 0)
                    .count(),
                1
            );
        });
    }

    #[test]
    fn missing_parent_is_a_typed_miss() {
        let layout = layout();
        let (model, receipt) = lookup(&layout, "anyone");
        assert!(model.is_none());
        assert_eq!(receipt, DispatchScanReceipt::EMPTY);
    }

    #[test]
    fn partial_trailing_dispatch_is_visible_but_not_cached() {
        let layout = layout();
        write_lines(&layout.candidate_two, &[ordinary_record("complete")]);
        let complete_len = fs::metadata(&layout.candidate_two).unwrap().len();
        let mut file = OpenOptions::new()
            .append(true)
            .open(&layout.candidate_two)
            .unwrap();
        write!(
            file,
            "{}",
            dispatch_record("agent_id", "partial-agent", "partial-model")
        )
        .unwrap();
        drop(file);

        let (model, first) = lookup(&layout, "partial-agent");
        assert_eq!(model.as_deref(), Some("partial-model"));
        assert_eq!(first.bytes_parsed, complete_len);
        assert!(!first.rescanned_from_zero || complete_len > 0);

        let (again, second) = lookup(&layout, "partial-agent");
        assert_eq!(again.as_deref(), Some("partial-model"));
        assert_eq!(
            second.records_parsed, 1,
            "the trailing partial must be re-evaluated until it is a complete frame"
        );
    }

    #[test]
    fn uncached_oracle_reads_the_same_nested_content_as_production() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("parent.jsonl");
        write_lines(
            &path,
            &[json!({
                "role": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "Task",
                        "id": "toolu-nested",
                        "input": {
                            "agentId": "nested",
                            "model_name": "nested-model"
                        }
                    }]
                }
            })
            .to_string()],
        );
        assert_eq!(
            uncached_dispatch_model_for_agent(&path, "nested").as_deref(),
            Some("nested-model")
        );
    }
}
