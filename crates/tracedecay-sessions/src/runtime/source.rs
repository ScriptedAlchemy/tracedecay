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
//! supported, all persisted through the authoritative [`TranscriptStore`]
//! implementation and its existing `parse_offsets` table keyed by file path.
//! The stored [`StoredCursor`] is `(position, mtime)` where `position`
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
//! Source I/O and parse misses remain fail-open, but authoritative store errors
//! propagate so catch-up cannot report stale data as a successful zero-work
//! pass. Shared cursor/title/content helpers live in
//! [`crate::runtime::shared`] so the Hermes `SQLite` sweep can reuse them
//! without importing from this driver module.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_store::{ParseOffset, TranscriptStoreError, TranscriptWriteBatch};

/// The identity primitive lives in the domain crate so the capture kernels,
/// which sit below this crate, share the exact framing. Re-exported here
/// because this module is where the session runtime's callers already import
/// it from.
pub use tracedecay_domain::canonical_text::canonical_framed_sha256;

use crate::admission::{WireReadOutcome, read_bounded_to_string};
pub use crate::runtime::shared::{NewRows, StoredCursor, TranscriptIngestStats};
#[allow(unused_imports)]
pub use crate::runtime::shared::{
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools,
    message_storage_text, paths_equal, preview_title, read_new_rows, title_from_messages,
    usage_counters_from,
};
use crate::runtime::store_port::TranscriptIngestStore;
use crate::runtime::{SessionMessageRecord, SessionRecord};

pub type TranscriptIngestResult<T> = Result<T, TranscriptIngestError>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TranscriptIngestError {
    #[error(transparent)]
    Store(#[from] TranscriptStoreError),
    #[error("transcript scan failed to {operation} {path}")]
    ScanIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("transcript changed generation while scanning {path}")]
    ScanGenerationChanged { path: PathBuf },
    #[error(transparent)]
    Privacy(#[from] tracedecay_runtime_core::privacy::PrivacySanitizerError),
    #[error(transparent)]
    Domain(#[from] tracedecay_domain::DomainError),
    #[error(transparent)]
    ObservationContract(#[from] tracedecay_domain::ObservationContractError),
    #[error("{provider} record at {offset}..{end_offset} is non-durable: {reason}")]
    NonDurableRecord {
        provider: &'static str,
        offset: u64,
        end_offset: u64,
        reason: &'static str,
    },
    #[error("{provider} frame state is invalid")]
    InvalidFrameState { provider: &'static str },
    #[error("{provider} transcript has no injective source identity: {path}")]
    InvalidSourceIdentity {
        provider: &'static str,
        path: PathBuf,
    },
    #[error("transcript cursor key mismatch: expected {expected}, found {actual}")]
    CursorKeyMismatch { expected: String, actual: String },
}

impl TranscriptIngestError {
    fn scan_io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::ScanIo {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

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

fn log_jsonl_oversized_skip(path: &Path, offset: u64, byte_len: u64) {
    tracing::debug!(
        transcript_path = %path.display(),
        line_offset = offset,
        byte_len,
        "skipping oversized transcript jsonl line"
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

/// Durable identity for one transcript cursor.
///
/// Physical paths preserve the V1 text key. Opaque keys let providers retain
/// an injective identity when a native path cannot be represented as Unicode;
/// their legacy path is kept solely for one-way cursor migration and health
/// compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCursorKey {
    durable: DurableTranscriptCursorKey,
    legacy_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DurableTranscriptCursorKey {
    Path(PathBuf),
    Opaque(String),
}

impl TranscriptCursorKey {
    pub fn for_path(path: &Path) -> Self {
        Self {
            durable: DurableTranscriptCursorKey::Path(path.to_path_buf()),
            legacy_path: None,
        }
    }

    pub fn opaque(key: impl Into<String>, legacy_path: &Path) -> Self {
        Self {
            durable: DurableTranscriptCursorKey::Opaque(key.into()),
            legacy_path: Some(legacy_path.to_path_buf()),
        }
    }

    /// Exact durable text used by opaque keys, or the V1 lossy path text.
    pub fn durable_text(&self) -> String {
        match &self.durable {
            DurableTranscriptCursorKey::Path(path) => path.to_string_lossy().into_owned(),
            DurableTranscriptCursorKey::Opaque(key) => key.clone(),
        }
    }

    pub fn store_path(&self) -> PathBuf {
        match &self.durable {
            DurableTranscriptCursorKey::Path(path) => path.clone(),
            DurableTranscriptCursorKey::Opaque(key) => PathBuf::from(key),
        }
    }

    fn legacy_path(&self) -> Option<&Path> {
        self.legacy_path.as_deref()
    }
}

/// One typed cursor checkpoint for a transcript source.
///
/// Keeping the durable key attached to both ends of a scan prevents consumers
/// from advancing a cursor reconstructed from an ambient or lossy path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCursorCheckpoint {
    pub key: TranscriptCursorKey,
    pub state: StoredCursor,
}

/// Loaded cursor state plus the compare-and-swap expectations needed to
/// preserve opaque-key migration and the legacy health mirror.
pub struct LoadedTranscriptCursor {
    pub checkpoint: TranscriptCursorCheckpoint,
    durable_offset: ParseOffset,
    legacy_offset: Option<ParseOffset>,
}

pub async fn load_transcript_cursor<S: TranscriptIngestStore>(
    store: &S,
    key: TranscriptCursorKey,
) -> TranscriptIngestResult<LoadedTranscriptCursor> {
    let durable_offset = store.get_parse_offset(&key.store_path()).await?;
    let legacy_offset = if let Some(legacy_path) = key.legacy_path() {
        Some(store.get_parse_offset(legacy_path).await?)
    } else {
        None
    };
    // Opaque keys cannot safely inherit a lossy V1 path cursor: distinct
    // native paths may share that alias. Replay once into the injective key,
    // then keep mirroring the legacy cursor for health compatibility.
    let effective = durable_offset;
    Ok(LoadedTranscriptCursor {
        checkpoint: TranscriptCursorCheckpoint {
            key,
            state: StoredCursor {
                position: effective.byte_offset,
                mtime: effective.mtime,
                file_id: effective.file_id,
            },
        },
        durable_offset,
        legacy_offset,
    })
}

/// A pluggable transcript provider.
///
/// Implementors locate their transcript files for a project and parse only the
/// content appended/changed since the last run. The shared [`try_ingest_source`]
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

    /// Bounded discovery used by multi-source ingest admission.
    ///
    /// Default applies [`TranscriptDiscoveryBounds`] after `transcript_paths`.
    /// Providers that enumerate via [`collect_files_with_ext_bounded`] should
    /// override so limits are enforced before materialization.
    fn discover_transcript_paths(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        bound_path_list(self.transcript_paths(project_root), bounds)
    }

    /// Discover one deterministic page beginning at `start_offset`.
    ///
    /// The omitted count covers paths before and after the returned page. It
    /// lets the scheduler report backpressure without retaining the whole
    /// source corpus. Providers with a streaming enumerator may override this
    /// method to apply the offset before materializing paths.
    fn discover_transcript_paths_page(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
        start_offset: usize,
    ) -> (FileDiscoveryReport, usize) {
        let mut paths = self.transcript_paths(project_root);
        paths.sort();
        paths.dedup();
        let total_paths = paths.len();
        let report = bound_path_list(paths.into_iter().skip(start_offset), bounds);
        let omitted_paths = total_paths.saturating_sub(report.paths.len());
        (report, omitted_paths)
    }

    /// Durable identity used to load and advance this transcript's parse
    /// cursor. Providers may override this when the physical path cannot be
    /// represented injectively by the store's legacy text key.
    fn cursor_key(&self, transcript_path: &Path) -> TranscriptCursorKey {
        TranscriptCursorKey::for_path(transcript_path)
    }

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

    /// Fallible parse boundary used by production ingestion. Legacy adapters
    /// inherit their existing fail-open `Option` behavior; adapters with typed
    /// scanner failures override this method.
    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        Ok(self.parse_new(path, prev, project_root, max_new_bytes))
    }
}

/// Fallible production boundary that drives a single source to completion
/// against `store`, ingesting every transcript it locates for `project_root`.
/// Source parse misses are skipped; authoritative store failures abort the
/// pass.
///
/// `max_new_bytes` bounds how much newly-appended content a byte-offset source
/// will read in one call (used to keep per-prompt hot paths inside budget);
/// pass `None` for an unbounded catch-up.
pub async fn try_ingest_source<S: TranscriptIngestStore>(
    store: &S,
    source: &dyn TranscriptSource,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<TranscriptIngestStats> {
    try_ingest_source_with_store(store, source, project_root, max_new_bytes).await
}

pub async fn try_ingest_source_with_store<S: TranscriptIngestStore>(
    store: &S,
    source: &dyn TranscriptSource,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<TranscriptIngestStats> {
    let mut stats = TranscriptIngestStats::default();
    let discovery =
        source.discover_transcript_paths(project_root, TranscriptDiscoveryBounds::default_walk());
    for path in discovery.paths {
        stats = stats.merge(ingest_one(store, source, &path, project_root, max_new_bytes).await?);
    }
    Ok(stats)
}

/// Ingest one transcript file: load the prior durable cursor through the store
/// contract, parse new content, and submit one atomic session/message/cursor
/// batch. The root adapter extends that write with git evidence in the same
/// authoritative registered session-database transaction.
async fn ingest_one<S: TranscriptIngestStore>(
    store: &S,
    source: &dyn TranscriptSource,
    path: &Path,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<TranscriptIngestStats> {
    let loaded = load_transcript_cursor(store, source.cursor_key(path)).await?;
    let previous = loaded.checkpoint.clone();
    let Some(parsed) = source.try_parse_new(path, previous.state, project_root, max_new_bytes)?
    else {
        return Ok(TranscriptIngestStats::default());
    };
    persist_parsed_transcript(
        store,
        source.provider(),
        path,
        project_root,
        loaded,
        &previous,
        parsed,
    )
    .await
}

/// Persist an already parsed transcript through the authoritative V1 batch and
/// git-evidence transaction. Observation coordinators reuse this after their
/// one-pass privacy parse and Claude fold.
pub async fn persist_parsed_transcript<S: TranscriptIngestStore>(
    store: &S,
    provider: &'static str,
    _path: &Path,
    project_root: &Path,
    loaded: LoadedTranscriptCursor,
    expected_previous: &TranscriptCursorCheckpoint,
    mut parsed: ParsedTranscript,
) -> TranscriptIngestResult<TranscriptIngestStats> {
    if loaded.checkpoint.key != expected_previous.key {
        return Err(TranscriptIngestError::CursorKeyMismatch {
            expected: expected_previous.key.durable_text(),
            actual: loaded.checkpoint.key.durable_text(),
        });
    }
    let cursor_key = loaded.checkpoint.key;
    let cursor_path = cursor_key.store_path();
    let durable_offset = loaded.durable_offset;
    let legacy_offset = loaded.legacy_offset;
    let is_backfill = loaded.checkpoint.state != expected_previous.state;
    let next_offset = if is_backfill {
        // Backfill/retry scans may start before V1. Upsert their deterministic
        // rows idempotently while the V1 CAS remains pinned to its newer state.
        durable_offset
    } else {
        ParseOffset {
            byte_offset: parsed.new_cursor.position,
            mtime: parsed.new_cursor.mtime,
            file_id: parsed.new_cursor.file_id,
        }
    };
    if parsed.messages.is_empty() {
        let batch = TranscriptWriteBatch::advance_offset(cursor_path, durable_offset, next_offset)?;
        store.persist_transcript_batch(batch).await?;
        mirror_legacy_cursor(store, &cursor_key, legacy_offset, next_offset).await?;
        return Ok(TranscriptIngestStats::default());
    }
    protect_parsed_transcript_structural_ids(&mut parsed)?;
    let commit_records =
        crate::runtime::git_correlation::direct_commit_records(&parsed.messages, project_root);
    let span_observations =
        crate::runtime::git_correlation::ingest_span_observations(&parsed.messages);
    let draft = parsed.draft;
    let existing = store.get_session(provider, &draft.session_id).await?;
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
    let parsed_ended_at = parsed.messages.last().and_then(|message| message.timestamp);
    let ended_at = existing
        .as_ref()
        .and_then(|session| session.ended_at)
        .into_iter()
        .chain(parsed_ended_at)
        .max();

    let project_key = existing
        .as_ref()
        .map(|session| session.project_key.clone())
        .unwrap_or(draft.project_key);
    let project_path = existing
        .as_ref()
        .map(|session| session.project_path.clone())
        .unwrap_or(draft.project_path);
    let metadata_json = merge_session_metadata(
        existing
            .as_ref()
            .and_then(|session| session.metadata_json.as_deref()),
        draft.metadata_json,
    );
    let parent_session_id = existing
        .as_ref()
        .and_then(|session| session.parent_session_id.clone())
        .or(draft.parent_session_id);
    let is_subagent =
        existing.as_ref().is_some_and(|session| session.is_subagent) || draft.is_subagent;
    let agent_id = existing
        .as_ref()
        .and_then(|session| session.agent_id.clone())
        .or(draft.agent_id);
    let parent_tool_use_id = existing
        .as_ref()
        .and_then(|session| session.parent_tool_use_id.clone())
        .or(draft.parent_tool_use_id);

    let session = SessionRecord {
        provider: provider.to_string(),
        session_id: draft.session_id,
        project_key,
        project_path,
        title,
        started_at,
        ended_at,
        transcript_path: Some(cursor_key.durable_text()),
        metadata_json,
        parent_session_id,
        is_subagent,
        agent_id,
        parent_tool_use_id,
    };

    let messages_upserted = parsed.messages.len() as u64;
    let batch = TranscriptWriteBatch::upsert_with_cursor(
        cursor_path,
        session,
        parsed.messages,
        durable_offset,
        next_offset,
    )?;
    store
        .persist_transcript_batch_with_git_evidence(batch, &commit_records, &span_observations)
        .await?;
    mirror_legacy_cursor(store, &cursor_key, legacy_offset, next_offset).await?;
    // Live-activity tap: this is the one chokepoint every provider's transcript
    // ingest funnels through, so it is where "an agent said something in this
    // project" becomes observable. Published only after the durable batch
    // commits, so the dashboard never lights work that did not land. The project
    // id is left for the dashboard to resolve from the registry — ingest holds a
    // project root, not a registered identity, and must not pay a lookup here.
    store
        .record_session_ingest_activity(project_root, messages_upserted, provider)
        .await;
    Ok(TranscriptIngestStats {
        sessions_upserted: 1,
        messages_upserted,
    })
}

fn protect_parsed_transcript_structural_ids(
    parsed: &mut ParsedTranscript,
) -> Result<(), tracedecay_runtime_core::privacy::PrivacySanitizerError> {
    fn protect(
        value: &mut String,
    ) -> Result<(), tracedecay_runtime_core::privacy::PrivacySanitizerError> {
        *value = tracedecay_runtime_core::privacy::protect_sensitive_structural_id(value)?;
        Ok(())
    }

    protect(&mut parsed.draft.session_id)?;
    for value in [
        &mut parsed.draft.parent_session_id,
        &mut parsed.draft.agent_id,
        &mut parsed.draft.parent_tool_use_id,
    ] {
        value.iter_mut().try_for_each(protect)?;
    }
    for message in &mut parsed.messages {
        protect(&mut message.message_id)?;
        protect(&mut message.session_id)?;
    }
    Ok(())
}

fn merge_session_metadata(existing: Option<&str>, incoming: Option<String>) -> Option<String> {
    let previous = existing.and_then(|value| serde_json::from_str::<Value>(value).ok());
    let next = incoming
        .as_deref()
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    match (previous, next) {
        (Some(Value::Object(mut previous)), Some(Value::Object(next))) => {
            for (key, value) in next {
                if matches!(key.as_str(), "pr_links" | "edited_files")
                    && let Some(Value::Array(existing_values)) = previous.get_mut(&key)
                {
                    if let Value::Array(incoming_values) = value {
                        merge_session_metadata_rollup(&key, existing_values, incoming_values);
                    }
                    continue;
                }
                previous.entry(key).or_insert(value);
            }
            Some(Value::Object(previous).to_string())
        }
        (_, Some(next)) => Some(next.to_string()),
        (Some(previous), None) => Some(previous.to_string()),
        (None, None) => incoming.or_else(|| existing.map(str::to_string)),
    }
}

fn merge_session_metadata_rollup(key: &str, existing: &mut Vec<Value>, incoming: Vec<Value>) {
    for value in incoming {
        if !existing
            .iter()
            .any(|current| session_metadata_rollup_items_match(key, current, &value))
        {
            existing.push(value);
        }
    }
}

fn session_metadata_rollup_items_match(key: &str, left: &Value, right: &Value) -> bool {
    match key {
        "pr_links" => {
            let left_identity = (left.get("pr_url"), left.get("pr_number"));
            let right_identity = (right.get("pr_url"), right.get("pr_number"));
            if left_identity.0.is_none()
                && left_identity.1.is_none()
                && right_identity.0.is_none()
                && right_identity.1.is_none()
            {
                left == right
            } else {
                left_identity == right_identity
            }
        }
        "edited_files" => match (left.get("path"), right.get("path")) {
            (Some(left_path), Some(right_path)) => left_path == right_path,
            _ => left == right,
        },
        _ => left == right,
    }
}

async fn mirror_legacy_cursor<S: TranscriptIngestStore>(
    store: &S,
    cursor_key: &TranscriptCursorKey,
    legacy_offset: Option<ParseOffset>,
    next_offset: ParseOffset,
) -> TranscriptIngestResult<()> {
    let (Some(legacy_path), Some(legacy_offset)) = (cursor_key.legacy_path(), legacy_offset) else {
        return Ok(());
    };
    if legacy_offset == next_offset {
        return Ok(());
    }
    let batch = TranscriptWriteBatch::advance_offset(
        legacy_path.to_path_buf(),
        legacy_offset,
        next_offset,
    )?;
    store.persist_transcript_batch(batch).await?;
    Ok(())
}

mod discovery;
mod jsonl;

pub use discovery::{FileDiscoveryLimit, FileDiscoveryReport, TranscriptDiscoveryBounds};
pub use discovery::{
    bound_path_list, collect_files_with_ext_bounded, os_str_byte_len, path_byte_len,
};

#[cfg(test)]
pub use jsonl::try_stream_new_jsonl_raw_strict;
pub use jsonl::{
    JsonlFrameDeferral, JsonlResumeState, MAX_JSONL_RECORD_BYTES, RawJsonlFrame,
    RawJsonlFrameReader, RawJsonlSkippedReason, STRICT_JSONL_BATCH_BYTES,
    try_stream_new_jsonl_raw_strict_with_resume,
};
pub use jsonl::{JsonlLine, NewJsonl, stream_new_jsonl};
#[cfg(test)]
use jsonl::{
    MAX_JSONL_FRAMES_PER_BATCH, MalformedJsonlPolicy, StrictJsonlOutcome,
    stream_new_jsonl_raw_strict, stream_new_jsonl_strict, stream_new_jsonl_with_policy,
};

pub fn preflight_strict_jsonl(
    provider: &'static str,
    path: &Path,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<()> {
    let frames = try_stream_new_jsonl_raw_strict_with_resume(
        path,
        previous,
        max_new_bytes,
        MAX_JSONL_RECORD_BYTES,
        None,
    )?;
    if let Some(JsonlFrameDeferral::Malformed { offset }) = frames.deferred {
        return Err(TranscriptIngestError::NonDurableRecord {
            provider,
            offset,
            end_offset: frames.read_through.max(offset),
            reason: "malformed_jsonl_frame",
        });
    }
    Ok(())
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
pub fn read_changed_file(path: &Path, prev: StoredCursor, max_bytes: u64) -> Option<ChangedFile> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) => {
            log_source_skip(path, "stat transcript file", &error);
            return None;
        }
    };
    let mtime = file_mtime_secs(&meta);
    let contents = read_file_to_string_bounded(path, max_bytes)?;
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
pub fn read_changed_with_companion(
    primary: &Path,
    companion: &Path,
    prev: StoredCursor,
    max_bytes: u64,
) -> Option<ChangedFile> {
    let meta = match std::fs::metadata(primary) {
        Ok(meta) => meta,
        Err(error) => {
            log_source_skip(primary, "stat primary transcript file", &error);
            return None;
        }
    };
    let mtime = file_mtime_secs(&meta);
    let contents = read_file_to_string_bounded(primary, max_bytes)?;
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
            let companion_contents = read_file_to_string_bounded(companion, max_bytes)?;
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

fn read_file_to_string_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            log_source_skip(path, "open transcript file", &error);
            return None;
        }
    };
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    match read_bounded_to_string(&mut file, max_bytes) {
        Ok(WireReadOutcome::Ready(contents)) => Some(contents),
        Ok(WireReadOutcome::Oversized) => None,
        Err(error) => {
            log_source_skip(path, "read transcript file", &error);
            None
        }
    }
}

/// Recursively collect files with the given extension under `dir`, bounded by
/// `max_depth` and [`TranscriptDiscoveryBounds::default_walk`] (file count,
/// path bytes, metadata charge, cumulative discovery bytes). Directory
/// symlinks are not followed. Returns an empty vec when `dir` is missing or
/// unreadable. Used by global-store adapters (Claude, Codex) whose transcripts
/// live in nested date/slug directories.
#[cfg(test)]
pub fn collect_files_with_ext(dir: &Path, ext: &str, max_depth: u8) -> Vec<PathBuf> {
    collect_files_with_ext_bounded(
        dir,
        ext,
        max_depth,
        TranscriptDiscoveryBounds::default_walk(),
    )
    .paths
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

fn stable_jsonl_file_id(
    file: &mut std::fs::File,
    meta: &std::fs::Metadata,
) -> std::io::Result<u64> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-file-id-v1");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(meta.dev().to_le_bytes());
        hasher.update(meta.ino().to_le_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        if let Ok(information) = tracedecay_runtime_core::windows_file::information(file) {
            hasher.update(information.volume_serial_number.to_le_bytes());
            hasher.update(information.file_index.to_le_bytes());
        } else {
            // Some virtual file systems do not expose native handle
            // identity. Keep creation time plus the head fingerprint as
            // the deterministic fallback used before native IDs existed.
            hasher.update(0_u32.to_le_bytes());
            hasher.update(0_u64.to_le_bytes());
        }
        hasher.update(meta.creation_time().to_le_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Creation time is stable across appends and changes when a transcript
        // is replaced on platforms without a native file-id implementation.
        if let Ok(created) = meta.created() {
            if let Ok(created) = created.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(created.as_nanos().to_le_bytes());
            }
        }
    }
    hasher.update(jsonl_head_fingerprint(file)?.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_be_bytes(bytes))
}

fn jsonl_head_fingerprint(file: &mut std::fs::File) -> std::io::Result<u64> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    // Hash only the first logical line prefix so append-only writes keep a
    // stable identity even for initially tiny files.
    let _ = reader
        .by_ref()
        .take(JSONL_HEAD_FINGERPRINT_BYTES as u64)
        .read_until(b'\n', &mut buf)?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-head-v1");
    hasher.update(&buf);
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_be_bytes(bytes))
}

/// Stable 64-bit content hash prefix suitable for the existing integer
/// `parse_offsets.byte_offset` column.
pub fn content_hash64(contents: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests;
