//! Shared session-ingest abstractions and provider-neutral transcript helpers.
//!
//! These types and helpers sit below any particular session source adapter:
//! file-backed [`crate::sessions::source`] drivers and the Hermes `SQLite` sweep
//! both depend on them so they do not need to import from each other.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::SessionMessageRecord;

/// Generic per-transcript backlog threshold for warning that automatic
/// session transcript catch-up may not drain recall transcripts quickly enough.
pub const SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES: u64 = 2 * 1024 * 1024;

/// Counters returned by an ingestion pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
}

impl TranscriptIngestStats {
    /// Accumulate another pass's counters into this one.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            sessions_upserted: self
                .sessions_upserted
                .saturating_add(other.sessions_upserted),
            messages_upserted: self
                .messages_upserted
                .saturating_add(other.messages_upserted),
        }
    }
}

/// The incremental position persisted between ingestion runs.
///
/// `position` is interpreted per cursor kind: a byte offset (`ByteOffset`), a
/// stable 64-bit content hash prefix (`ContentHash`), or a last-seen `rowid`
/// (`RowCursor`). `mtime` is the file modification time in epoch seconds, used
/// to detect rewrites and to skip unchanged files cheaply.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoredCursor {
    pub position: u64,
    pub mtime: u64,
    pub file_id: u64,
}

/// Mapped rows read past the stored cursor, plus the advanced cursor.
pub struct NewRows<T> {
    pub items: Vec<T>,
    pub new_cursor: StoredCursor,
}

/// **`RowCursor`** reader for SQLite-backed transcript stores (Zed, Copilot CLI
/// `session-store.db`).
///
/// Selects rows whose rowid is greater than `prev.position` (the last-seen
/// rowid), ordered ascending, mapping each through `map_row` *during* iteration
/// (libsql rows must not outlive the cursor) and advancing the stored cursor to
/// the maximum rowid seen. `select_sql` must select the rowid as its first
/// column and accept a single `?` bound to the previous rowid, e.g.
/// `"SELECT rowid, role, text FROM turns WHERE rowid > ? ORDER BY rowid"`.
/// Fail-open: any query error yields `None`; `map_row` returning `None` skips
/// that row while still advancing the cursor.
pub async fn read_new_rows<T>(
    conn: &libsql::Connection,
    select_sql: &str,
    prev: StoredCursor,
    mut map_row: impl FnMut(i64, &libsql::Row) -> Option<T>,
) -> Option<NewRows<T>> {
    let mut result_rows = match conn
        .query(select_sql, libsql::params![prev.position as i64])
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(
                select_sql,
                previous_rowid = prev.position,
                error = %error,
                "skipping transcript row source query"
            );
            return None;
        }
    };

    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    while let Ok(Some(row)) = result_rows.next().await {
        let Ok(rowid) = row.get::<i64>(0) else {
            tracing::debug!(
                select_sql,
                "skipping transcript row without rowid in column 0"
            );
            continue;
        };
        if rowid as u64 > max_rowid {
            max_rowid = rowid as u64;
        }
        if let Some(item) = map_row(rowid, &row) {
            items.push(item);
        }
    }

    Some(NewRows {
        items,
        new_cursor: StoredCursor {
            position: max_rowid,
            // Row stores have no single file mtime; the rowid alone is the
            // monotonic cursor, so mtime is left as a sentinel.
            mtime: 0,
            file_id: 0,
        },
    })
}

/// Compare two paths for equality, canonicalizing when possible so that
/// symlinks/`..`/trailing differences do not cause false mismatches. Falls back
/// to a literal comparison when canonicalization fails (e.g. a path that no
/// longer exists).
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => normalized_paths_equal(&a, &b),
        _ => normalized_paths_equal(a, b),
    }
}

pub fn path_belongs_to_project(path: &Path, project_root: &Path) -> bool {
    ProjectRootMatcher::new(project_root).contains(path)
}

/// A project root with its git worktree/common-dir resolutions computed once,
/// so repeated membership tests (e.g. one per discovered workflow run) do not
/// re-run `git_worktree_root`/`git_common_dir` on the fixed project side. A
/// single [`ProjectRootMatcher::contains`] call is exactly equivalent to
/// [`path_belongs_to_project`], which is a thin wrapper over it.
pub struct ProjectRootMatcher {
    root: PathBuf,
    worktree: Option<PathBuf>,
    common_dir: Option<PathBuf>,
}

impl ProjectRootMatcher {
    /// Resolve the fixed project-side git identity once.
    pub fn new(project_root: &Path) -> Self {
        Self {
            root: project_root.to_path_buf(),
            worktree: tracedecay_runtime_core::worktree::git_worktree_root(project_root),
            common_dir: tracedecay_runtime_core::worktree::git_common_dir(project_root),
        }
    }

    /// True when `path` belongs to this project: it is the root, shares the
    /// project's git worktree or common dir, or discovers back to the root.
    /// Only the varying `path` side is git-resolved here.
    pub fn contains(&self, path: &Path) -> bool {
        if paths_equal(path, &self.root) {
            return true;
        }

        if let (Some(path_worktree), Some(project_worktree)) = (
            tracedecay_runtime_core::worktree::git_worktree_root(path).as_ref(),
            self.worktree.as_ref(),
        ) {
            if paths_equal(path_worktree, project_worktree) {
                return true;
            }
            return tracedecay_runtime_core::worktree::git_common_dir(path)
                .as_ref()
                .zip(self.common_dir.as_ref())
                .is_some_and(|(path_common, project_common)| {
                    paths_equal(path_common, project_common)
                });
        }

        tracedecay_runtime_core::config::discover_project_root(path)
            .as_ref()
            .is_some_and(|discovered| paths_equal(discovered, &self.root))
    }
}

#[cfg(windows)]
fn normalized_paths_equal(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        let path = path.to_string_lossy().replace('/', "\\");
        path.strip_prefix(r"\\?\")
            .unwrap_or(&path)
            .to_ascii_lowercase()
    }

    normalize(a) == normalize(b)
}

#[cfg(not(windows))]
fn normalized_paths_equal(a: &Path, b: &Path) -> bool {
    a == b
}

/// Collapse internal whitespace/newlines to single spaces and clip to at most
/// `max` characters, appending a single-character `…` when truncation occurred.
/// Shared by the workflow surfaces (run/agent summaries, result summaries,
/// unfinished-run evidence) so a multi-line blob never smears a table, bullet,
/// or stored column.
pub fn one_line_truncated(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(max).collect();
    format!("{truncated}…")
}

/// Clip `text` to at most `max_bytes` on a UTF-8 boundary, appending a single
/// `…` only when truncation occurred. Unlike [`one_line_truncated`] this keeps
/// internal newlines, so multi-line derived-row previews retain their structure.
pub fn preview_truncated(text: &str, max_bytes: usize) -> String {
    let prefix = tracedecay_runtime_core::text::utf8_prefix_at_or_before(text, max_bytes);
    if prefix.len() == text.len() {
        prefix.to_string()
    } else {
        format!("{prefix}…")
    }
}

/// Collapse whitespace and clip to a short preview suitable for a session title.
pub fn preview_title(text: &str) -> String {
    const MAX_TITLE_CHARS: usize = 80;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        collapsed
    } else {
        collapsed.chars().take(MAX_TITLE_CHARS).collect()
    }
}

/// Return the storage representation used by LCM raw ingest for provider
/// transcript content. This intentionally matches the active-message path:
/// strings stay strings, structured content is compact JSON.
pub fn message_storage_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

/// Return lossless storage text plus tool names discovered in either structured
/// content blocks or a sibling `tool_calls` field.
pub fn content_storage_text_and_tools(
    content: &Value,
    tool_calls: Option<&Value>,
) -> (String, Vec<String>) {
    let mut tools = Vec::new();
    collect_tool_names(content, &mut tools);
    if let Some(tool_calls) = tool_calls {
        collect_tool_names(tool_calls, &mut tools);
    }
    tools.sort();
    tools.dedup();
    (message_storage_text(content), tools)
}

pub fn append_tool_calls_metadata(map: &mut serde_json::Map<String, Value>, message: &Value) {
    if let Some(tool_calls) = message.get("tool_calls") {
        map.insert("tool_calls".to_string(), tool_calls.clone());
    }
}

/// Byte length of `serde_json::to_string(value)`, or 0 when `value` is absent.
fn json_byte_len(value: Option<&Value>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    let mut sink = ByteCountSink::default();
    if serde_json::to_writer(&mut sink, value).is_ok() {
        sink.count
    } else {
        0
    }
}

/// `io::Write` sink that counts bytes without retaining them, so JSON byte
/// lengths can be measured without allocating an intermediate `String`.
#[derive(Default)]
struct ByteCountSink {
    count: u64,
}

impl io::Write for ByteCountSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.count += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Records bounded per-call tool metadata (byte counts and identifiers only,
/// never content) for `tool_use`/`tool_result` blocks found in `content`.
/// Inserts the `tool_events` key only when at least one entry was collected.
pub fn append_tool_event_metadata(map: &mut serde_json::Map<String, Value>, content: &Value) {
    let Some(items) = content.as_array() else {
        return;
    };
    let mut events = Vec::new();
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        match item_type {
            "tool_use" => {
                let mut event = serde_json::Map::new();
                event.insert("type".to_string(), Value::String("tool_use".to_string()));
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    event.insert("tool_name".to_string(), Value::String(name.to_string()));
                }
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    event.insert("call_id".to_string(), Value::String(id.to_string()));
                }
                event.insert(
                    "input_bytes".to_string(),
                    Value::from(json_byte_len(item.get("input"))),
                );
                events.push(Value::Object(event));
            }
            "tool_result" => {
                let mut event = serde_json::Map::new();
                event.insert("type".to_string(), Value::String("tool_result".to_string()));
                if let Some(id) = item.get("tool_use_id").and_then(Value::as_str) {
                    event.insert("call_id".to_string(), Value::String(id.to_string()));
                }
                event.insert(
                    "output_bytes".to_string(),
                    Value::from(json_byte_len(item.get("content"))),
                );
                events.push(Value::Object(event));
            }
            _ => {}
        }
    }
    if !events.is_empty() {
        map.insert("tool_events".to_string(), Value::Array(events));
    }
}

#[derive(Clone, Copy)]
pub struct TranscriptLocation<'a> {
    pub cwd: Option<&'a Path>,
    pub provenance: &'a str,
}

impl<'a> TranscriptLocation<'a> {
    pub fn new(cwd: Option<&'a Path>, provenance: &'a str) -> Self {
        Self { cwd, provenance }
    }
}

#[derive(Clone, Copy)]
pub struct TranscriptLocationMetadataKeys {
    pub cwd: &'static str,
    pub worktree: &'static str,
    pub provenance: &'static str,
}

impl TranscriptLocationMetadataKeys {
    pub const fn new(cwd: &'static str, worktree: &'static str, provenance: &'static str) -> Self {
        Self {
            cwd,
            worktree,
            provenance,
        }
    }
}

pub fn append_location_metadata(
    map: &mut serde_json::Map<String, Value>,
    keys: TranscriptLocationMetadataKeys,
    location: TranscriptLocation<'_>,
) {
    let Some(cwd) = location.cwd else {
        return;
    };
    map.insert(
        keys.cwd.to_string(),
        Value::String(cwd.to_string_lossy().to_string()),
    );
    if let Some(worktree) = tracedecay_runtime_core::worktree::git_worktree_root(cwd) {
        map.insert(
            keys.worktree.to_string(),
            Value::String(worktree.to_string_lossy().to_string()),
        );
    }
    map.insert(
        keys.provenance.to_string(),
        Value::String(location.provenance.to_string()),
    );
}

/// Token-usage counter keys recognized by the savings dashboard
/// (`dashboard/savings_api.rs` `MESSAGE_TOKENS_CTE`): both the Anthropic
/// (`input_tokens`/`output_tokens`/`cache_*`) and `OpenAI`
/// (`prompt_tokens`/`completion_tokens`) shapes, plus total/reasoning counters
/// for reference.
const USAGE_COUNTER_KEYS: [&str; 9] = [
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "total_tokens",
    "reasoning_tokens",
    "reasoning_output_tokens",
];

/// Extracts a `usage` counters object from a transcript record/message,
/// keeping only recognized numeric token counters (so arbitrarily large or
/// provider-private payloads never bloat `metadata_json`). Returns `None`
/// when the value has no `usage` object or it carries no recognized counters.
pub fn usage_counters_from(value: &Value) -> Option<Value> {
    let usage = value.get("usage")?.as_object()?;
    let mut counters = serde_json::Map::new();
    for key in USAGE_COUNTER_KEYS {
        if let Some(count) = usage.get(key).and_then(Value::as_i64) {
            counters.insert(key.to_string(), Value::from(count));
        }
    }
    if !counters.contains_key("cache_read_input_tokens") {
        if let Some(count) = usage.get("cached_input_tokens").and_then(Value::as_i64) {
            counters.insert("cache_read_input_tokens".to_string(), Value::from(count));
        }
    }
    if !counters.is_empty()
        && !counters.contains_key("input_tokens")
        && !counters.contains_key("prompt_tokens")
        && !counters.contains_key("output_tokens")
        && !counters.contains_key("completion_tokens")
    {
        counters.insert("input_tokens".to_string(), Value::from(0));
        counters.insert("output_tokens".to_string(), Value::from(0));
    }
    (!counters.is_empty()).then_some(Value::Object(counters))
}

/// Inserts transcript-recorded token usage into message metadata under the
/// `usage` key the savings dashboard reads. Probes each candidate value in
/// order and keeps the first recognized counters object.
pub fn append_usage_metadata(map: &mut serde_json::Map<String, Value>, candidates: &[&Value]) {
    if map.contains_key("usage") {
        return;
    }
    if let Some(usage) = candidates
        .iter()
        .find_map(|value| usage_counters_from(value))
    {
        map.insert("usage".to_string(), usage);
    }
}

fn collect_tool_names(value: &Value, tools: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_tool_names(item, tools);
            }
        }
        Value::Object(map) => {
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_use" | "tool_call" | "function_call")
            ) {
                if let Some(name) = map.get("name").and_then(Value::as_str) {
                    tools.push(name.to_string());
                }
            }
            for key in ["tool_call", "functionCall", "function_call", "function"] {
                if let Some(name) = map
                    .get(key)
                    .and_then(Value::as_object)
                    .and_then(|nested| nested.get("name"))
                    .and_then(Value::as_str)
                {
                    tools.push(name.to_string());
                }
            }
            if let Some(tool_calls) = map.get("tool_calls") {
                collect_tool_names(tool_calls, tools);
            }
        }
        _ => {}
    }
}

fn title_text_from_stored_content(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| visible_text_from_content(&value))
        .unwrap_or_else(|| text.to_string())
}

fn visible_text_from_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(visible_text_from_content)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        Value::Object(map) => {
            for key in ["text", "content", "message"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    return Some(text.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Build a session title from the first user message, if any.
pub fn title_from_messages(messages: &[SessionMessageRecord]) -> Option<String> {
    messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| preview_title(&title_text_from_stored_content(&message.text)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::one_line_truncated;
    use super::usage_counters_from;

    #[test]
    fn one_line_truncated_collapses_and_clips() {
        assert_eq!(one_line_truncated("a\n b\t c", 100), "a b c");
        assert_eq!(one_line_truncated("abcdef", 3), "abc…");
    }

    #[test]
    fn usage_counters_keep_cache_only_rows_actual() {
        let Some(usage) = usage_counters_from(&json!({
            "usage": {
                "cache_read_input_tokens": 123,
                "total_tokens": 123
            }
        })) else {
            panic!("cache-only usage should be retained");
        };

        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["cache_read_input_tokens"], 123);
        assert_eq!(usage["total_tokens"], 123);
    }

    #[test]
    fn usage_counters_normalize_openai_cached_input_alias() {
        let Some(usage) = usage_counters_from(&json!({
            "usage": {
                "cached_input_tokens": 456,
                "total_tokens": 456
            }
        })) else {
            panic!("OpenAI cache alias should be retained");
        };

        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["cache_read_input_tokens"], 456);
        assert_eq!(usage["total_tokens"], 456);
    }
}
