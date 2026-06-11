//! Provider-neutral transcript ingestion framework.
//!
//! Every agent transcript — Cursor, Claude Code, Codex, Vibe, … — converges to
//! the same provider-neutral [`SessionMessageRecord`] rows in a per-project
//! `sessions.db`. This module factors the *incremental, fail-open* machinery
//! out of the original Cursor-specific implementation so any adapter can plug
//! in by implementing [`TranscriptSource`].
//!
//! ## Incremental cursors
//!
//! Sources differ in how they store transcripts, so three cursor kinds are
//! supported, all persisted through the existing `parse_offsets` table
//! ([`GlobalDb::get_parse_offset`]/[`GlobalDb::set_parse_offset`]) keyed by file
//! path. The stored [`StoredCursor`] is `(position, mtime)` where `position`
//! means:
//!
//! * [`stream_new_jsonl`] — **`ByteOffset`**: append-only JSONL (Cursor, Claude,
//!   Codex, …). `position` is the byte offset of the next unread line; we seek
//!   there and stream only new lines.
//! * [`read_changed_file`] — **`ContentHash`**: full-file-rewrite JSON (Cline,
//!   Roo Code, Kilo, …). `position` is a stable 64-bit prefix of the content
//!   hash; combined with `mtime` it detects rewrites. On change the whole
//!   document is re-parsed and re-upserted — idempotent `ON CONFLICT` upserts
//!   make re-adding unchanged messages a no-op.
//! * [`read_new_rows`] — **`RowCursor`**: SQLite-backed stores (Zed, Copilot CLI
//!   `session-store.db`). `position` is the last-seen `rowid`; we select rows
//!   with a greater `rowid`.
//!
//! All three are fail-open: any I/O or parse error yields "nothing new" rather
//! than propagating, so ingestion never blocks an agent.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::global_db::{GlobalDb, ParseOffset};
use crate::sessions::{SessionMessageRecord, SessionRecord};

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

/// Provider-neutral session metadata an adapter derives while parsing.
///
/// The driver merges this with any existing row so a session's original
/// `started_at`/`title` survive incremental appends.
pub struct SessionDraft {
    pub session_id: String,
    pub project_key: String,
    pub project_path: String,
    pub title: Option<String>,
    pub metadata_json: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
}

/// The result of parsing only the *new* portion of one transcript file.
pub struct ParsedTranscript {
    pub draft: SessionDraft,
    pub messages: Vec<SessionMessageRecord>,
    pub new_cursor: StoredCursor,
}

/// A pluggable transcript provider.
///
/// Implementors locate their transcript files for a project and parse only the
/// content appended/changed since the last run. The shared [`ingest_source`]
/// driver handles offset persistence and idempotent session/message upserts.
///
/// `Send + Sync` is required so boxed sources can be driven from detached
/// background tasks (e.g. the serve-side startup sweep).
pub trait TranscriptSource: Send + Sync {
    /// Stable provider id stored on every session/message row (e.g. `"claude"`).
    fn provider(&self) -> &'static str;

    /// Candidate transcript files to consider for `project_root`. May scan
    /// per-project and/or OS-specific global directories. Non-existent paths
    /// are tolerated by the driver.
    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf>;

    /// Parse only the new content of `path` given the previously stored cursor.
    ///
    /// Returns `None` to mean "ingest nothing and do not advance the cursor"
    /// (unreadable file, hot-path byte cap exceeded, or the transcript does not
    /// belong to `project_root`). Returns `Some` with a possibly-empty message
    /// list otherwise; an empty list still advances the cursor (e.g. only
    /// non-message lines were appended).
    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript>;
}

/// Drive a single source to completion against `db`, ingesting every transcript
/// it locates for `project_root`. Fail-open: per-file errors are swallowed.
///
/// `max_new_bytes` bounds how much newly-appended content a byte-offset source
/// will read in one call (used to keep per-prompt hot paths inside budget);
/// pass `None` for an unbounded catch-up.
pub async fn ingest_source(
    db: &GlobalDb,
    source: &dyn TranscriptSource,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for path in source.transcript_paths(project_root) {
        stats = stats.merge(ingest_one(db, source, &path, project_root, max_new_bytes).await);
    }
    stats
}

/// Ingest one transcript file: load the prior cursor, parse new content, persist
/// the advanced cursor, then upsert the session (merging preserved fields) and
/// its new messages.
async fn ingest_one(
    db: &GlobalDb,
    source: &dyn TranscriptSource,
    path: &Path,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestStats {
    let path_str = path.to_string_lossy().to_string();
    let prev_offset = db
        .get_parse_offset_with_file_id(&path_str)
        .await
        .unwrap_or_default();
    let prev = StoredCursor {
        position: prev_offset.byte_offset,
        mtime: prev_offset.mtime,
        file_id: prev_offset.file_id,
    };
    let Some(parsed) = source.parse_new(path, prev, project_root, max_new_bytes) else {
        return TranscriptIngestStats::default();
    };

    if parsed.messages.is_empty() {
        // Non-message append (e.g. blank/undecodable rows) still advances the
        // cursor so the next ingest only sees genuinely new content.
        db.set_parse_offset_with_file_id(
            &path_str,
            ParseOffset {
                byte_offset: parsed.new_cursor.position,
                mtime: parsed.new_cursor.mtime,
                file_id: parsed.new_cursor.file_id,
            },
        )
        .await;
        return TranscriptIngestStats::default();
    }

    let provider = source.provider();
    let draft = parsed.draft;
    let existing = db.get_session(provider, &draft.session_id).await;
    // Preserve the session's original start time and title across appends; only
    // advance ended_at to the latest message seen.
    let started_at = existing
        .as_ref()
        .and_then(|session| session.started_at)
        .or_else(|| {
            parsed
                .messages
                .first()
                .and_then(|message| message.timestamp)
        });
    let title = existing
        .as_ref()
        .and_then(|session| session.title.clone())
        .or(draft.title);
    let ended_at = parsed
        .messages
        .last()
        .and_then(|message| message.timestamp)
        .or_else(|| existing.as_ref().and_then(|session| session.ended_at));

    let session = SessionRecord {
        provider: provider.to_string(),
        session_id: draft.session_id,
        project_key: draft.project_key,
        project_path: draft.project_path,
        title,
        started_at,
        ended_at,
        transcript_path: Some(path.to_string_lossy().to_string()),
        metadata_json: draft.metadata_json,
        parent_session_id: draft.parent_session_id,
        is_subagent: draft.is_subagent,
        agent_id: draft.agent_id,
        parent_tool_use_id: draft.parent_tool_use_id,
    };

    if !db
        .upsert_transcript_batch(
            &session,
            &parsed.messages,
            &path_str,
            ParseOffset {
                byte_offset: parsed.new_cursor.position,
                mtime: parsed.new_cursor.mtime,
                file_id: parsed.new_cursor.file_id,
            },
        )
        .await
    {
        return TranscriptIngestStats::default();
    }
    TranscriptIngestStats {
        sessions_upserted: 1,
        messages_upserted: parsed.messages.len() as u64,
    }
}

/// One newly-read JSONL line: its starting byte offset and decoded value.
pub struct JsonlLine {
    pub offset: i64,
    pub value: Value,
}

/// New JSONL content read from a file, plus the advanced cursor.
pub struct NewJsonl {
    pub lines: Vec<JsonlLine>,
    pub new_cursor: StoredCursor,
}

/// **`ByteOffset`** reader for append-only JSONL.
///
/// Seeks to `prev.position` (when the file has only grown and its mtime has not
/// regressed) and streams complete, newline-terminated lines, decoding each as
/// JSON. Blank and undecodable lines still advance the offset (so they are not
/// re-read) but are omitted from `lines`. A trailing line without a newline is a
/// partial write and is left unconsumed for the next call.
///
/// Returns `None` when the file cannot be stat-ed/opened, or when
/// `max_new_bytes` is set and the unread tail exceeds it (so a hot path can defer
/// a large backlog to a lower-frequency caller without advancing the cursor).
pub fn stream_new_jsonl(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
) -> Option<NewJsonl> {
    let meta = std::fs::metadata(path).ok()?;
    let file_size = meta.len();
    let mtime = file_mtime_secs(&meta);
    let file_id = stable_jsonl_file_id(path, &meta).unwrap_or(0);

    // Resume from the saved offset only when the file has grown (or stayed) and
    // its identity still matches. Legacy cursors without a file id fall back to
    // the old mtime guard.
    let resume = should_resume_jsonl(prev, file_size, mtime, file_id);
    let seek_to = if resume { prev.position } else { 0 };

    if seek_to >= file_size {
        // Nothing new; refresh mtime so we stop re-stat-ing an idle file.
        return Some(NewJsonl {
            lines: Vec::new(),
            new_cursor: StoredCursor {
                position: seek_to,
                mtime,
                file_id,
            },
        });
    }

    if let Some(cap) = max_new_bytes {
        if file_size.saturating_sub(seek_to) > cap {
            return None;
        }
    }

    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    if seek_to > 0 && reader.seek(SeekFrom::Start(seek_to)).is_err() {
        return None;
    }

    let mut lines = Vec::new();
    let mut offset = seek_to;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                // A line without a trailing newline is a partial write at EOF:
                // stop without consuming it so the next call re-reads it whole.
                if !line.ends_with('\n') {
                    break;
                }
                let line_offset = offset;
                offset = offset.saturating_add(n as u64);
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                    lines.push(JsonlLine {
                        offset: line_offset as i64,
                        value,
                    });
                }
            }
        }
    }

    Some(NewJsonl {
        lines,
        new_cursor: StoredCursor {
            position: offset,
            mtime,
            file_id,
        },
    })
}

/// Full contents of a changed file plus the advanced cursor.
pub struct ChangedFile {
    pub contents: String,
    pub new_cursor: StoredCursor,
}

/// **`ContentHash`** reader for full-file-rewrite JSON.
///
/// Detects a change via `(content_hash64, mtime)` versus the stored cursor and,
/// on change, returns the whole file so the caller can re-derive every message
/// with deterministic ids. Idempotent upserts make re-adding unchanged messages
/// a no-op. Returns `None` when the file cannot be read or is unchanged since
/// the last run.
pub fn read_changed_file(path: &Path, prev: StoredCursor) -> Option<ChangedFile> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = file_mtime_secs(&meta);
    let contents = std::fs::read_to_string(path).ok()?;
    let hash = content_hash64(&contents);

    // Unchanged since last run (we have read it before and neither content hash
    // nor mtime moved) -> nothing to do.
    if prev.position == hash && prev.mtime == mtime && (prev.position != 0 || prev.mtime != 0) {
        return None;
    }

    Some(ChangedFile {
        contents,
        new_cursor: StoredCursor {
            position: hash,
            mtime,
            file_id: 0,
        },
    })
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
    let mut result_rows = conn
        .query(select_sql, libsql::params![prev.position as i64])
        .await
        .ok()?;

    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    while let Ok(Some(row)) = result_rows.next().await {
        let Ok(rowid) = row.get::<i64>(0) else {
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

/// Recursively collect files with the given extension under `dir`, bounded by
/// `max_depth` to avoid runaway traversal. Returns an empty vec when `dir` is
/// missing or unreadable. Used by global-store adapters (Claude, Codex) whose
/// transcripts live in nested date/slug directories.
pub(crate) fn collect_files_with_ext(dir: &Path, ext: &str, max_depth: u8) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files_inner(dir, ext, max_depth, 0, &mut out);
    out
}

fn collect_files_inner(dir: &Path, ext: &str, max_depth: u8, depth: u8, out: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_inner(&path, ext, max_depth, depth + 1, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

/// Compare two paths for equality, canonicalizing when possible so that
/// symlinks/`..`/trailing differences do not cause false mismatches. Falls back
/// to a literal comparison when canonicalization fails (e.g. a path that no
/// longer exists).
pub(crate) fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// File modification time in epoch seconds, or 0 when unavailable.
fn file_mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

const JSONL_HEAD_FINGERPRINT_BYTES: usize = 1024;

fn should_resume_jsonl(prev: StoredCursor, file_size: u64, mtime: u64, file_id: u64) -> bool {
    if prev.position == 0 || file_size < prev.position {
        return false;
    }
    if prev.file_id != 0 && file_id != 0 {
        return prev.file_id == file_id;
    }
    mtime >= prev.mtime
}

fn stable_jsonl_file_id(path: &Path, meta: &std::fs::Metadata) -> Option<u64> {
    let mut hasher = Sha256::new();
    hasher.update(b"tokensave-jsonl-file-id-v1");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(meta.dev().to_le_bytes());
        hasher.update(meta.ino().to_le_bytes());
    }
    hasher.update(jsonl_head_fingerprint(path)?.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Some(u64::from_be_bytes(bytes))
}

fn jsonl_head_fingerprint(path: &Path) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    // Hash only the first logical line prefix so append-only writes keep a
    // stable identity even for initially tiny files.
    let _ = reader.read_until(b'\n', &mut buf).ok()?;
    if buf.len() > JSONL_HEAD_FINGERPRINT_BYTES {
        buf.truncate(JSONL_HEAD_FINGERPRINT_BYTES);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"tokensave-jsonl-head-v1");
    hasher.update(&buf);
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Some(u64::from_be_bytes(bytes))
}

/// Stable 64-bit content hash prefix suitable for the existing integer
/// `parse_offsets.byte_offset` column.
fn content_hash64(contents: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// Collapse whitespace and clip to a short preview suitable for a session title.
pub(crate) fn preview_title(text: &str) -> String {
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
pub(crate) fn message_storage_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

/// Return lossless storage text plus tool names discovered in either structured
/// content blocks or a sibling `tool_calls` field.
pub(crate) fn content_storage_text_and_tools(
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

pub(crate) fn append_tool_calls_metadata(
    map: &mut serde_json::Map<String, Value>,
    message: &Value,
) {
    if let Some(tool_calls) = message.get("tool_calls") {
        map.insert("tool_calls".to_string(), tool_calls.clone());
    }
}

/// Token-usage counter keys recognized by the savings dashboard
/// (`dashboard/savings_api.rs` `MESSAGE_TOKENS_CTE`): both the Anthropic
/// (`input_tokens`/`output_tokens`/`cache_*`) and `OpenAI`
/// (`prompt_tokens`/`completion_tokens`) shapes, plus `total_tokens` for
/// reference.
const USAGE_COUNTER_KEYS: [&str; 7] = [
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "total_tokens",
];

/// Extracts a `usage` counters object from a transcript record/message,
/// keeping only recognized numeric token counters (so arbitrarily large or
/// provider-private payloads never bloat `metadata_json`). Returns `None`
/// when the value has no `usage` object or it carries no recognized counters.
pub(crate) fn usage_counters_from(value: &Value) -> Option<Value> {
    let usage = value.get("usage")?.as_object()?;
    let mut counters = serde_json::Map::new();
    for key in USAGE_COUNTER_KEYS {
        if let Some(count) = usage.get(key).and_then(Value::as_i64) {
            counters.insert(key.to_string(), Value::from(count));
        }
    }
    (!counters.is_empty()).then_some(Value::Object(counters))
}

/// Inserts transcript-recorded token usage into message metadata under the
/// `usage` key the savings dashboard reads. Probes each candidate value in
/// order and keeps the first recognized counters object.
pub(crate) fn append_usage_metadata(
    map: &mut serde_json::Map<String, Value>,
    candidates: &[&Value],
) {
    if map.contains_key("usage") {
        return;
    }
    if let Some(usage) = candidates.iter().find_map(|value| usage_counters_from(value)) {
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
pub(crate) fn title_from_messages(messages: &[SessionMessageRecord]) -> Option<String> {
    messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| preview_title(&title_text_from_stored_content(&message.text)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn stream_new_jsonl_reads_only_appended_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();

        let first = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
        assert_eq!(first.lines.len(), 2);

        // Re-reading from the advanced cursor yields nothing.
        let again = stream_new_jsonl(&path, first.new_cursor, None).unwrap();
        assert_eq!(again.lines.len(), 0);

        // Appending one line yields only that line on the next read.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"a\":3}\n").unwrap();
        drop(f);
        let third = stream_new_jsonl(&path, again.new_cursor, None).unwrap();
        assert_eq!(third.lines.len(), 1);
        assert_eq!(third.lines[0].value["a"], 3);
    }

    #[test]
    fn stream_new_jsonl_defers_partial_final_line_and_respects_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}").unwrap(); // second line unterminated

        let read = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
        assert_eq!(read.lines.len(), 1, "partial final line must be deferred");

        // A cap smaller than the unread tail defers the whole read (no cursor advance).
        assert!(stream_new_jsonl(&path, StoredCursor::default(), Some(1)).is_none());
    }

    #[test]
    fn stream_new_jsonl_resets_offset_when_file_identity_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        // Keep byte length stable across rewrite to simulate same-size rotation.
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();

        let first = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
        assert_eq!(first.lines.len(), 2);

        std::fs::write(&path, "{\"a\":9}\n{\"a\":8}\n").unwrap();
        // Simulate a non-regressing mtime guard; identity must still force a reset.
        let stale = StoredCursor {
            mtime: 0,
            ..first.new_cursor
        };
        let rewritten = stream_new_jsonl(&path, stale, None).unwrap();
        assert_eq!(rewritten.lines.len(), 2);
        assert_eq!(rewritten.lines[0].value["a"], 9);
        assert_eq!(rewritten.lines[1].value["a"], 8);
    }

    #[test]
    fn read_changed_file_detects_change_and_noops_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.json");
        std::fs::write(&path, "[{\"role\":\"user\"}]").unwrap();

        let changed = read_changed_file(&path, StoredCursor::default()).unwrap();
        assert!(changed.contents.contains("user"));
        // Unchanged file → None.
        assert!(read_changed_file(&path, changed.new_cursor).is_none());
    }

    #[tokio::test]
    async fn read_new_rows_tracks_last_rowid() {
        // A synthetic SQLite-backed source exercises the RowCursor kind.
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE turns (role TEXT, text TEXT)", ())
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO turns (role, text) VALUES ('user', 'hello'), ('assistant', 'hi')",
            (),
        )
        .await
        .unwrap();

        let sql = "SELECT rowid, role, text FROM turns WHERE rowid > ? ORDER BY rowid";
        let map = |_rowid: i64, row: &libsql::Row| row.get::<String>(2).ok();
        let first = read_new_rows(&conn, sql, StoredCursor::default(), map)
            .await
            .unwrap();
        assert_eq!(first.items, vec!["hello".to_string(), "hi".to_string()]);
        assert_eq!(first.new_cursor.position, 2);

        // No new rows past the advanced cursor.
        let again = read_new_rows(&conn, sql, first.new_cursor, map)
            .await
            .unwrap();
        assert_eq!(again.items.len(), 0);

        conn.execute(
            "INSERT INTO turns (role, text) VALUES ('user', 'again')",
            (),
        )
        .await
        .unwrap();
        let third = read_new_rows(&conn, sql, again.new_cursor, map)
            .await
            .unwrap();
        assert_eq!(third.items, vec!["again".to_string()]);
        assert_eq!(third.new_cursor.position, 3);
    }
}
