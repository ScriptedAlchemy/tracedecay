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
//! than propagating, so ingestion never blocks an agent. Shared cursor/title/
//! content helpers live in [`crate::sessions::shared`] so the Hermes `SQLite`
//! sweep can reuse them without importing from this driver module.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::global_db::{GlobalDb, ParseOffset};
#[allow(unused_imports)]
pub(crate) use crate::sessions::shared::{
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools,
    message_storage_text, paths_equal, preview_title, read_new_rows, title_from_messages,
    usage_counters_from,
};
pub use crate::sessions::shared::{NewRows, StoredCursor, TranscriptIngestStats};
use crate::sessions::{SessionMessageRecord, SessionRecord};

fn log_source_skip(path: &Path, action: &'static str, error: &impl std::fmt::Display) {
    tracing::debug!(
        transcript_path = %path.display(),
        action,
        error = %error,
        "skipping transcript source input"
    );
}

fn log_jsonl_decode_skip(path: &Path, offset: u64, error: &serde_json::Error) {
    tracing::debug!(
        transcript_path = %path.display(),
        line_offset = offset,
        error = %error,
        "skipping undecodable transcript jsonl line"
    );
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
    let prev_offset = db.get_parse_offset(&path_str).await.unwrap_or_default();
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
        db.set_parse_offset(
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
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) => {
            log_source_skip(path, "stat jsonl transcript", &error);
            return None;
        }
    };
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
            tracing::debug!(
                transcript_path = %path.display(),
                unread_bytes = file_size.saturating_sub(seek_to),
                max_new_bytes = cap,
                "deferring transcript source backlog beyond configured cap"
            );
            return None;
        }
    }

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            log_source_skip(path, "open jsonl transcript", &error);
            return None;
        }
    };
    let mut reader = BufReader::new(file);
    if seek_to > 0 {
        if let Err(error) = reader.seek(SeekFrom::Start(seek_to)) {
            log_source_skip(path, "seek jsonl transcript", &error);
            return None;
        }
    }

    let mut lines = Vec::new();
    let mut offset = seek_to;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Err(error) => {
                log_source_skip(path, "read jsonl transcript line", &error);
                break;
            }
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
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => lines.push(JsonlLine {
                        offset: line_offset as i64,
                        value,
                    }),
                    Err(error) => log_jsonl_decode_skip(path, line_offset, &error),
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
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) => {
            log_source_skip(path, "stat transcript file", &error);
            return None;
        }
    };
    let mtime = file_mtime_secs(&meta);
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            log_source_skip(path, "read transcript file", &error);
            return None;
        }
    };
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

/// Like [`read_changed_file`], but treats `primary` as changed when either its
/// own content hash moves or a companion sidecar file's hash moves. The stored
/// cursor's `position` is a combined hash of both files so a sidecar-only
/// update (e.g. Cline `ui_messages.json` usage counters) triggers a re-ingest.
pub(crate) fn read_changed_with_companion(
    primary: &Path,
    companion: &Path,
    prev: StoredCursor,
) -> Option<ChangedFile> {
    let meta = match std::fs::metadata(primary) {
        Ok(meta) => meta,
        Err(error) => {
            log_source_skip(primary, "stat primary transcript file", &error);
            return None;
        }
    };
    let mtime = file_mtime_secs(&meta);
    let contents = match std::fs::read_to_string(primary) {
        Ok(contents) => contents,
        Err(error) => {
            log_source_skip(primary, "read primary transcript file", &error);
            return None;
        }
    };
    let primary_hash = content_hash64(&contents);
    let (companion_hash, companion_mtime) = companion
        .is_file()
        .then(|| {
            let companion_meta = match std::fs::metadata(companion) {
                Ok(meta) => meta,
                Err(error) => {
                    log_source_skip(companion, "stat companion transcript file", &error);
                    return None;
                }
            };
            let companion_contents = match std::fs::read_to_string(companion) {
                Ok(contents) => contents,
                Err(error) => {
                    log_source_skip(companion, "read companion transcript file", &error);
                    return None;
                }
            };
            Some((
                content_hash64(&companion_contents),
                file_mtime_secs(&companion_meta),
            ))
        })
        .flatten()
        .unwrap_or((0, 0));
    let combined_hash = content_hash64(&format!("{primary_hash:016x}:{companion_hash:016x}"));
    let combined_mtime = mtime.max(companion_mtime);

    if prev.position == combined_hash
        && prev.mtime == combined_mtime
        && (prev.position != 0 || prev.mtime != 0)
    {
        return None;
    }

    Some(ChangedFile {
        contents,
        new_cursor: StoredCursor {
            position: combined_hash,
            mtime: combined_mtime,
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
pub(crate) fn content_hash64(contents: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
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

    #[test]
    fn stream_new_jsonl_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");

        assert!(stream_new_jsonl(&path, StoredCursor::default(), None).is_none());
    }

    #[test]
    fn stream_new_jsonl_skips_invalid_json_lines_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.jsonl");
        std::fs::write(&path, "not-json\n{\"a\":2}\n").unwrap();

        let read = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
        assert_eq!(read.lines.len(), 1);
        assert_eq!(read.lines[0].value["a"], 2);
    }

    #[test]
    fn read_changed_file_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");

        assert!(read_changed_file(&path, StoredCursor::default()).is_none());
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

    #[tokio::test]
    async fn read_new_rows_returns_none_for_invalid_query() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();

        let rows = read_new_rows(
            &conn,
            "SELECT not_a_column FROM missing_table WHERE rowid > ? ORDER BY rowid",
            StoredCursor::default(),
            |_rowid: i64, row: &libsql::Row| row.get::<String>(0).ok(),
        )
        .await;

        assert!(rows.is_none());
    }
}
