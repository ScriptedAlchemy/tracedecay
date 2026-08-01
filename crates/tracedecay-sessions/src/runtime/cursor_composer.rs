//! Cursor **composer** transcript ingestion.
//!
//! Cursor's primary chat history does not live in the
//! `~/.cursor/projects/<slug>/agent-transcripts/**.jsonl` files that
//! [`crate::runtime::cursor`] sweeps — those cover only a slice of activity.
//! The bulk lives in two SQLite-backed stores this module reads **strictly
//! read-only**:
//!
//! 1. The global `~/.config/Cursor/User/globalStorage/state.vscdb` — a
//!    single-table (`cursorDiskKV`) key/value store with:
//!    * `composerData:<composerId>` — one JSON *session envelope* per chat
//!      (name, createdAt/lastUpdatedAt, model, workspace path, an ordered
//!      `fullConversationHeadersOnly` list of bubble ids, todos, git repos, …).
//!    * `bubbleId:<composerId>:<bubbleId>` — one JSON *message record* per turn
//!      (text, thinking, `toolFormerData`, tokenCount, commits, pullRequests …).
//! 2. The newer per-session `~/.cursor/chats/<ws-hash>/<agentId>/store.db` — a
//!    content-addressed blob DAG (`meta` + `blobs`) walked from
//!    `latestRootBlobId`. Best-effort: the plain-JSON `{role,content}` leaf
//!    blobs are ingested; protobuf-framed leaves are tolerated but skipped.
//!
//! ## Read-only safety
//!
//! The live `state.vscdb` here is ~21 GB / 1.4M rows. We open it with a
//! `file:…?immutable=1&mode=ro` URI (`SQLite` skips all locking and never writes
//! a `-wal`/`-shm`), and we only ever issue **indexed** lookups: a single
//! bounded range scan over the `composerData:` key prefix and primary-key
//! (`key = ?`) point lookups for bubbles. No full-table scans. Every `TEXT` /
//! `BLOB` payload is length-gated in SQL (`length` + conditional materialize)
//! against the shared observation / JSONL frame ceilings and the pass byte
//! budget before any Rust `String`/`Vec`/`serde_json::Value` allocation.
//! `store.db` blobs are fetched by id while walking the reachable DAG — never
//! collected via `SELECT id, data FROM blobs`.
//!
//! ## Incremental + dedupe
//!
//! Each composer source advances through its ordered bubbles using the
//! authoritative observation cursor. The cursor is compare-and-swap bound to
//! the snapshot generation and `SnapshotOrder`, so a sweep replays only
//! uncovered positions. Because a composer session id equals the stem of its
//! JSONL transcript for ~94% of sessions, the composer sweep runs *before* the
//! JSONL [`crate::runtime::cursor::CursorSweepSource`] and hands it the set of
//! composer-owned session ids to skip, so the richer composer rows win and no
//! message row is ever double-ingested.

mod capture;
mod ingest;
mod sqlite;
mod store;
#[cfg(test)]
mod tests;

pub use capture::{
    build_cursor_composer_capture_request, build_cursor_composer_envelope_capture_request,
    capture_cursor_composer_observation,
};
pub use ingest::{CursorComposerSource, CursorComposerSweepOutcome, DEFAULT_COMPOSER_ENVELOPE_CAP};
#[cfg(any(test, feature = "test-helpers"))]
pub use tracedecay_capture::cursor_composer::{
    normalize_cursor_composer_observation,
    normalize_cursor_composer_observation_with_projected_message_id,
};

/// Provider id shared with the JSONL Cursor source so both land in the same
/// per-project `sessions.db` namespace and dedupe by `(provider, message_id)`.
pub const PROVIDER: &str = "cursor";
