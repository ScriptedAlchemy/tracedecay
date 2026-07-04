//! Session ↔ git correlation index.
//!
//! Correlates agent sessions (and their LCM messages) with the git artifacts
//! they touched: branches, worktrees, and commits. The raw signals already
//! exist — hook route metadata carries `(session_id, thread_id, cwd,
//! worktree, branch)` per tool use, and the session store has every ingested
//! message with timestamps — but they were never joined. This module owns the
//! join tables and their query API.
//!
//! ## Where the index lives
//!
//! The tables live in the **per-project session store** (`sessions.db`, the
//! same file that holds `sessions`, `session_messages`, and the LCM tables;
//! see [`crate::global_db::GlobalDb::open_at`]). Rationale:
//!
//! - Every query surface joins against `sessions` / `session_messages` /
//!   `lcm_raw_messages`, which are per-project rows. Keeping the correlation
//!   rows in the same file makes branch/worktree/commit filters a plain SQL
//!   join instead of a cross-database merge.
//! - Branches, worktrees, and commits are inherently project-scoped: a
//!   branch name is only meaningful relative to one repository.
//! - Hook route rows in the global analytics store are a *signal source*,
//!   not a query surface: live attribution and the backfill command resolve
//!   each observation's project (via its worktree/cwd) and write the span
//!   into that project's store.
//!
//! ## Model
//!
//! Sessions can switch branches (and hop worktrees) mid-session, so
//! attribution is **span-based**: a [`SessionGitSpan`] says "session S was
//! active on branch B in worktree W between `first_ts` and `last_ts`".
//! Repeated observations of the same (session, branch, worktree) extend the
//! newest matching span when they arrive within a merge gap; a branch switch
//! (or a long silence) starts a new span, so A → B → A produces three spans.
//!
//! A commit is attributed to a session when its commit time falls inside a
//! session span on the same branch/worktree (see [`SpanOverlapKind`]); the
//! result is a [`CommitSessionRecord`] row keyed by `(commit_sha, provider,
//! session_id)`.

use std::fmt::Write as _;

use libsql::{params, Connection, Value};
use serde::{Deserialize, Serialize};

/// Schema version recorded in `session_schema_migrations` under
/// [`MIGRATION_NAME`]. Bump when the DDL below changes shape.
///
/// - v1: `session_git_spans` + `commit_sessions`.
/// - v2: `git_correlation_meta` key/value table for sweep watermarks.
pub const GIT_CORRELATION_SCHEMA_VERSION: i64 = 2;

const MIGRATION_NAME: &str = "git_correlation";

/// Default gap (seconds) within which a new observation extends the newest
/// matching span instead of opening a new one. Tool-use events inside one
/// working stretch arrive far more often than this; a longer silence most
/// likely means the session went idle or moved elsewhere.
pub const DEFAULT_SPAN_MERGE_GAP_SECS: i64 = 30 * 60;

/// Hard cap on rows returned by [`sessions_for`].
pub const MAX_SESSIONS_FOR_LIMIT: usize = 100;

/// Errors from the git-correlation store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCorrelationError {
    /// Underlying database failure.
    Db(String),
    /// Caller-supplied argument was invalid (bad ref kind, empty value, …).
    InvalidArgument(String),
}

impl std::fmt::Display for GitCorrelationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(message) => write!(f, "git correlation db error: {message}"),
            Self::InvalidArgument(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for GitCorrelationError {}

impl From<libsql::Error> for GitCorrelationError {
    fn from(err: libsql::Error) -> Self {
        Self::Db(err.to_string())
    }
}

/// Where a span row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanSource {
    /// Live hook route metadata observed while the session ran.
    HookRoute,
    /// Derived during transcript ingest/sync.
    Ingest,
    /// Reconstructed by the historical backfill command.
    Backfill,
}

impl SpanSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HookRoute => "hook_route",
            Self::Ingest => "ingest",
            Self::Backfill => "backfill",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "hook_route" => Some(Self::HookRoute),
            "ingest" => Some(Self::Ingest),
            "backfill" => Some(Self::Backfill),
            _ => None,
        }
    }
}

/// How a commit's timestamp related to the session span that claimed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanOverlapKind {
    /// Commit time fell strictly inside `[first_ts, last_ts]` of a span on
    /// the same branch/worktree.
    WithinSpan,
    /// Commit time fell inside the span extended by the merge gap (commits
    /// often land moments after the last recorded tool use).
    ExtendedWindow,
    /// Attributed via `git reflog` checkout history rather than a recorded
    /// span (backfill of sessions that predate span recording).
    Reflog,
}

impl SpanOverlapKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WithinSpan => "within_span",
            Self::ExtendedWindow => "extended_window",
            Self::Reflog => "reflog",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "within_span" => Some(Self::WithinSpan),
            "extended_window" => Some(Self::ExtendedWindow),
            "reflog" => Some(Self::Reflog),
            _ => None,
        }
    }
}

/// One recorded stretch of session activity on a branch/worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGitSpan {
    pub span_id: i64,
    /// Session provider id (`claude`, `codex`, …). Empty when the signal
    /// source did not identify the provider (raw hook routes are
    /// provider-agnostic); queries treat `''` as "unknown".
    pub provider: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    /// `None` = detached HEAD or branch unknown at observation time.
    pub branch: Option<String>,
    /// Normalized absolute worktree root path (see [`normalize_worktree`]).
    pub worktree: String,
    pub first_ts: i64,
    pub last_ts: i64,
    pub event_count: i64,
    pub source: SpanSource,
}

/// One live observation to be folded into the span table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanObservation {
    pub provider: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub branch: Option<String>,
    pub worktree: String,
    pub ts: i64,
    pub source: SpanSource,
}

/// One commit attributed to one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSessionRecord {
    /// Full 40-hex (or 64-hex for sha256 repos) lowercase commit id.
    pub commit_sha: String,
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub committed_at: i64,
    pub span_overlap_kind: SpanOverlapKind,
    /// Span row that claimed the commit, when attribution was span-based.
    pub span_id: Option<i64>,
}

/// A parsed, validated git reference to correlate sessions against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRefFilter {
    Branch(String),
    /// Normalized worktree root path.
    Worktree(String),
    /// Lowercase hex commit sha, possibly abbreviated (>= 6 chars).
    Commit(String),
}

impl GitRefFilter {
    /// Parses a `(kind, value)` pair from tool arguments. Kinds are
    /// `branch`, `worktree`, and `commit`; values are trimmed and
    /// normalized per kind.
    pub fn parse(kind: &str, value: &str) -> Result<Self, GitCorrelationError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(GitCorrelationError::InvalidArgument(
                "value must be a non-empty string".to_string(),
            ));
        }
        match kind {
            "branch" => Ok(Self::Branch(value.to_string())),
            "worktree" => Ok(Self::Worktree(normalize_worktree(value))),
            "commit" => parse_commit_sha(value).map(Self::Commit),
            other => Err(GitCorrelationError::InvalidArgument(format!(
                "git_ref must be one of branch, worktree, commit (got `{other}`)"
            ))),
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Branch(_) => "branch",
            Self::Worktree(_) => "worktree",
            Self::Commit(_) => "commit",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Branch(value) | Self::Worktree(value) | Self::Commit(value) => value,
        }
    }
}

/// Optional git-scope filters shared by `tracedecay_message_search` and
/// `tracedecay_lcm_grep` (`branch` / `worktree` / `commit` arguments).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitScopeFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl GitScopeFilter {
    /// Builds a validated filter from raw optional argument strings.
    pub fn from_args(
        branch: Option<&str>,
        worktree: Option<&str>,
        commit: Option<&str>,
    ) -> Result<Self, GitCorrelationError> {
        let branch = branch
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let worktree = worktree
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalize_worktree);
        let commit = match commit.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => Some(parse_commit_sha(value)?),
            None => None,
        };
        Ok(Self {
            branch,
            worktree,
            commit,
        })
    }

    pub const fn is_empty(&self) -> bool {
        self.branch.is_none() && self.worktree.is_none() && self.commit.is_none()
    }
}

/// Query request for [`sessions_for`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsForQuery {
    pub git_ref: GitRefFilter,
    /// Inclusive lower bound on span/commit activity (unix seconds).
    pub since: Option<i64>,
    /// Inclusive upper bound on span/commit activity (unix seconds).
    pub until: Option<i64>,
    pub limit: usize,
}

/// One session correlated with the queried git ref.
///
/// Branch/worktree queries aggregate span rows per session (`first_ts`,
/// `last_ts`, `event_count`, `span_count`, `sources` populated); commit
/// queries return commit attribution rows (`commit_sha`, `committed_at`,
/// `span_overlap_kind` populated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGitCorrelationHit {
    pub provider: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ts: Option<i64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub event_count: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub span_count: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_overlap_kind: Option<SpanOverlapKind>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if signature
fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// Lexically normalizes a worktree path for stable equality: trims
/// whitespace, converts backslashes to forward slashes, and strips trailing
/// slashes (keeping a lone `/`). Deliberately does **not** hit the
/// filesystem — writers should pass already-resolved worktree roots (e.g.
/// from [`crate::worktree::git_worktree_root`]); this keeps readers and
/// writers agreeing even when the path no longer exists.
pub fn normalize_worktree(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

/// Validates and lowercases a (possibly abbreviated) commit sha. Requires
/// 6–64 hex characters: shorter prefixes are too ambiguous to index-match
/// against the attribution table.
fn parse_commit_sha(value: &str) -> Result<String, GitCorrelationError> {
    let ok = (6..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        return Err(GitCorrelationError::InvalidArgument(
            "commit must be 6-64 hexadecimal characters".to_string(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

/// True when an observation at `ts` should extend a span covering
/// `[first_ts, last_ts]` (same session/branch/worktree assumed) instead of
/// opening a new span: within the span or within `gap_secs` of either edge.
pub fn observation_extends_span(first_ts: i64, last_ts: i64, ts: i64, gap_secs: i64) -> bool {
    ts >= first_ts.saturating_sub(gap_secs) && ts <= last_ts.saturating_add(gap_secs)
}

/// In-process rate limiter for live hook-route span observations. A burst of
/// tool-use hook events for one `(provider, session, branch, worktree)` key
/// arrives far faster than spans need re-widening (spans merge anyway), so at
/// most one DB write per `min_interval_secs` per key is recorded. Purely
/// advisory: dropping an observation only widens a span slightly less, never
/// loses attribution, so this never has to persist across restarts.
#[derive(Debug, Default)]
pub struct SpanObservationDebounce {
    last_write: std::collections::HashMap<String, i64>,
}

/// Default minimum spacing between recorded hook-route observations for one
/// key. Matches [`DEFAULT_SPAN_MERGE_GAP_SECS`] granularity: writing more
/// often than this cannot change which spans exist.
pub const DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS: i64 = 30;

impl SpanObservationDebounce {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` (and records `ts` as the new watermark) when an
    /// observation at `ts` for this key should be written; returns `false`
    /// when a write for the same key happened within `min_interval_secs`.
    /// An out-of-order (older) `ts` never suppresses a write.
    pub fn should_record(&mut self, key: &str, ts: i64, min_interval_secs: i64) -> bool {
        if let Some(&last) = self.last_write.get(key) {
            if ts >= last && ts - last < min_interval_secs {
                return false;
            }
        }
        self.last_write.insert(key.to_string(), ts);
        true
    }
}

/// Builds the debounce key for one observation. Detached HEAD (branch `None`)
/// gets a distinct key from any named branch so a branch switch is never
/// debounced away.
pub fn span_debounce_key(
    provider: &str,
    session_id: &str,
    branch: Option<&str>,
    worktree: &str,
) -> String {
    format!(
        "{provider}\u{1f}{session_id}\u{1f}{}\u{1f}{worktree}",
        branch.unwrap_or("\u{0}")
    )
}

/// Creates the correlation tables when missing. Version-gated via
/// `session_schema_migrations` like the LCM schema; idempotent.
pub(crate) async fn ensure_git_correlation_schema(
    conn: &Connection,
) -> Result<(), GitCorrelationError> {
    if schema_version(conn)
        .await
        .is_some_and(|version| version >= GIT_CORRELATION_SCHEMA_VERSION)
    {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS session_git_spans (
            span_id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL DEFAULT '',
            session_id TEXT NOT NULL,
            thread_id TEXT,
            branch TEXT,
            worktree TEXT NOT NULL,
            first_ts INTEGER NOT NULL,
            last_ts INTEGER NOT NULL,
            event_count INTEGER NOT NULL DEFAULT 1,
            source TEXT NOT NULL CHECK(source IN ('hook_route', 'ingest', 'backfill')),
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            CHECK(first_ts <= last_ts)
        );
        CREATE INDEX IF NOT EXISTS idx_session_git_spans_session
            ON session_git_spans(provider, session_id, last_ts);
        CREATE INDEX IF NOT EXISTS idx_session_git_spans_branch
            ON session_git_spans(branch, last_ts);
        CREATE INDEX IF NOT EXISTS idx_session_git_spans_worktree
            ON session_git_spans(worktree, last_ts);
        CREATE TABLE IF NOT EXISTS commit_sessions (
            commit_sha TEXT NOT NULL,
            provider TEXT NOT NULL DEFAULT '',
            session_id TEXT NOT NULL,
            branch TEXT,
            worktree TEXT,
            committed_at INTEGER NOT NULL,
            span_overlap_kind TEXT NOT NULL
                CHECK(span_overlap_kind IN ('within_span', 'extended_window', 'reflog')),
            span_id INTEGER,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(commit_sha, provider, session_id)
        );
        CREATE INDEX IF NOT EXISTS idx_commit_sessions_session
            ON commit_sessions(provider, session_id, committed_at);
        CREATE INDEX IF NOT EXISTS idx_commit_sessions_branch
            ON commit_sessions(branch, committed_at);
        CREATE TABLE IF NOT EXISTS git_correlation_meta (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );",
    )
    .await?;
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = unixepoch()",
        params![MIGRATION_NAME, GIT_CORRELATION_SCHEMA_VERSION],
    )
    .await?;
    Ok(())
}

async fn schema_version(conn: &Connection) -> Option<i64> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![MIGRATION_NAME],
        )
        .await
        .ok()?;
    rows.next().await.ok()??.get(0).ok()
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::Text(text.to_string()))
}

/// Folds one observation into the span table: extends the newest span for
/// the same (provider, session, branch, worktree) when the observation lands
/// within `merge_gap_secs` of it, otherwise inserts a new span. Returns the
/// affected `span_id`.
///
/// Runs in a `BEGIN IMMEDIATE` transaction so concurrent writers converge on
/// widened spans instead of interleaved half-updates.
pub(crate) async fn record_span_observation(
    conn: &Connection,
    observation: &SpanObservation,
    merge_gap_secs: i64,
) -> Result<i64, GitCorrelationError> {
    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let result = record_span_observation_in_transaction(conn, observation, merge_gap_secs).await;
    match result {
        Ok(span_id) => {
            if let Err(err) = conn.execute("COMMIT", ()).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err.into())
            } else {
                Ok(span_id)
            }
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(err)
        }
    }
}

async fn record_span_observation_in_transaction(
    conn: &Connection,
    observation: &SpanObservation,
    merge_gap_secs: i64,
) -> Result<i64, GitCorrelationError> {
    // `branch IS ?` is NULL-safe: a detached-HEAD observation only extends a
    // detached-HEAD span, never a named-branch span.
    let mut rows = conn
        .query(
            "SELECT span_id, first_ts, last_ts
             FROM session_git_spans
             WHERE provider = ?1 AND session_id = ?2
               AND branch IS ?3 AND worktree = ?4
             ORDER BY last_ts DESC
             LIMIT 1",
            params![
                observation.provider.as_str(),
                observation.session_id.as_str(),
                opt_text(observation.branch.as_deref()),
                observation.worktree.as_str(),
            ],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        let span_id: i64 = row.get(0)?;
        let first_ts: i64 = row.get(1)?;
        let last_ts: i64 = row.get(2)?;
        if observation_extends_span(first_ts, last_ts, observation.ts, merge_gap_secs) {
            conn.execute(
                "UPDATE session_git_spans SET
                    first_ts = MIN(first_ts, ?2),
                    last_ts = MAX(last_ts, ?2),
                    event_count = event_count + 1,
                    thread_id = COALESCE(?3, thread_id),
                    updated_at = unixepoch()
                 WHERE span_id = ?1",
                params![
                    span_id,
                    observation.ts,
                    opt_text(observation.thread_id.as_deref()),
                ],
            )
            .await?;
            return Ok(span_id);
        }
    }
    conn.execute(
        "INSERT INTO session_git_spans (
            provider, session_id, thread_id, branch, worktree,
            first_ts, last_ts, event_count, source
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1, ?7)",
        params![
            observation.provider.as_str(),
            observation.session_id.as_str(),
            opt_text(observation.thread_id.as_deref()),
            opt_text(observation.branch.as_deref()),
            observation.worktree.as_str(),
            observation.ts,
            observation.source.as_str(),
        ],
    )
    .await?;
    Ok(conn.last_insert_rowid())
}

/// Inserts one commit attribution row; existing rows win (idempotent for
/// re-runs of live attribution and the backfill command). Returns `true`
/// when a new row was inserted.
pub(crate) async fn upsert_commit_session(
    conn: &Connection,
    record: &CommitSessionRecord,
) -> Result<bool, GitCorrelationError> {
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO commit_sessions (
                commit_sha, provider, session_id, branch, worktree,
                committed_at, span_overlap_kind, span_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.commit_sha.as_str(),
                record.provider.as_str(),
                record.session_id.as_str(),
                opt_text(record.branch.as_deref()),
                opt_text(record.worktree.as_deref()),
                record.committed_at,
                record.span_overlap_kind.as_str(),
                record.span_id.map_or(Value::Null, Value::Integer),
            ],
        )
        .await?;
    Ok(inserted > 0)
}

const COMMIT_SWEEP_WATERMARK_KEY: &str = "commit_attribution_watermark";

async fn read_meta_value(conn: &Connection, key: &str) -> Result<Option<i64>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT value FROM git_correlation_meta WHERE key = ?1",
            params![key],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

async fn write_meta_value(
    conn: &Connection,
    key: &str,
    value: i64,
) -> Result<(), GitCorrelationError> {
    conn.execute(
        "INSERT INTO git_correlation_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
        params![key, value],
    )
    .await?;
    Ok(())
}

/// A `(branch, worktree)` pair a session was observed on, with the widest span
/// window recorded for it. Commit scans run once per pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanScanTarget {
    pub branch: Option<String>,
    pub worktree: String,
    pub window_start: i64,
    pub window_end: i64,
}

/// One span row a candidate commit may fall inside. Kept minimal so the
/// matching logic ([`match_commit_to_spans`]) is a pure function testable
/// without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanWindow {
    pub span_id: i64,
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: String,
    pub first_ts: i64,
    pub last_ts: i64,
}

/// Classifies a commit at `committed_at` against one span: `Some(WithinSpan)`
/// when strictly inside `[first_ts, last_ts]`, `Some(ExtendedWindow)` when
/// inside the span widened by `gap_secs` on either edge, `None` otherwise.
pub fn commit_overlap_kind(
    first_ts: i64,
    last_ts: i64,
    committed_at: i64,
    gap_secs: i64,
) -> Option<SpanOverlapKind> {
    if committed_at >= first_ts && committed_at <= last_ts {
        Some(SpanOverlapKind::WithinSpan)
    } else if committed_at >= first_ts.saturating_sub(gap_secs)
        && committed_at <= last_ts.saturating_add(gap_secs)
    {
        Some(SpanOverlapKind::ExtendedWindow)
    } else {
        None
    }
}

/// Attributes one commit to every span whose window contains it, preferring
/// `WithinSpan` matches. Returns one record per `(span provider, session)`
/// pair so a commit made while several sessions were concurrently active on
/// the same branch is attributed to all of them.
pub fn match_commit_to_spans(
    commit_sha: &str,
    branch: Option<&str>,
    worktree: &str,
    committed_at: i64,
    spans: &[SpanWindow],
    gap_secs: i64,
) -> Vec<CommitSessionRecord> {
    let mut records = Vec::new();
    for span in spans {
        if span.branch.as_deref() != branch || span.worktree != worktree {
            continue;
        }
        let Some(kind) = commit_overlap_kind(span.first_ts, span.last_ts, committed_at, gap_secs)
        else {
            continue;
        };
        records.push(CommitSessionRecord {
            commit_sha: commit_sha.to_string(),
            provider: span.provider.clone(),
            session_id: span.session_id.clone(),
            branch: span.branch.clone(),
            worktree: Some(span.worktree.clone()),
            committed_at,
            span_overlap_kind: kind,
            span_id: Some(span.span_id),
        });
    }
    records
}

/// Loads the `(branch, worktree)` scan targets touched by spans updated at or
/// after `since_ts` (the sweep watermark), each carrying the widest span
/// window observed for it so the git scan can be time-bounded.
async fn scan_targets_since(
    conn: &Connection,
    since_ts: i64,
) -> Result<Vec<SpanScanTarget>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT branch, worktree, MIN(first_ts), MAX(last_ts)
             FROM session_git_spans
             WHERE last_ts >= ?1
             GROUP BY branch, worktree",
            params![since_ts],
        )
        .await?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await? {
        targets.push(SpanScanTarget {
            branch: row.get(0)?,
            worktree: row.get(1)?,
            window_start: row.get(2)?,
            window_end: row.get(3)?,
        });
    }
    Ok(targets)
}

/// Loads span windows for one `(branch, worktree)` pair, used to attribute
/// each scanned commit.
async fn span_windows_for(
    conn: &Connection,
    branch: Option<&str>,
    worktree: &str,
) -> Result<Vec<SpanWindow>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT span_id, provider, session_id, branch, worktree, first_ts, last_ts
             FROM session_git_spans
             WHERE branch IS ?1 AND worktree = ?2",
            params![opt_text(branch), worktree],
        )
        .await?;
    let mut spans = Vec::new();
    while let Some(row) = rows.next().await? {
        spans.push(SpanWindow {
            span_id: row.get(0)?,
            provider: row.get(1)?,
            session_id: row.get(2)?,
            branch: row.get(3)?,
            worktree: row.get(4)?,
            first_ts: row.get(5)?,
            last_ts: row.get(6)?,
        });
    }
    Ok(spans)
}

/// One commit observed by the bounded git scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedCommit {
    pub sha: String,
    pub committed_at: i64,
}

/// Runs the commit-attribution sweep against the correlation store. For each
/// `(branch, worktree)` pair touched since the last sweep, `scan` is asked for
/// the commits on that branch in the pair's span window (widened by
/// `gap_secs`); each commit is matched to the overlapping spans and upserted.
/// Advances the watermark to the newest span `last_ts` seen. Returns the
/// number of attribution rows newly inserted.
///
/// `scan` is injected so the git subprocess stays out of this module and unit
/// tests can drive attribution without a real repository.
pub(crate) async fn run_commit_attribution_sweep<F>(
    conn: &Connection,
    gap_secs: i64,
    mut scan: F,
) -> Result<usize, GitCorrelationError>
where
    F: FnMut(&SpanScanTarget) -> Vec<ScannedCommit>,
{
    if !correlation_tables_present(conn).await? {
        return Ok(0);
    }
    let watermark = read_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    let targets = scan_targets_since(conn, watermark).await?;
    let mut inserted = 0usize;
    let mut new_watermark = watermark;
    for target in &targets {
        new_watermark = new_watermark.max(target.window_end);
        let spans = span_windows_for(conn, target.branch.as_deref(), &target.worktree).await?;
        if spans.is_empty() {
            continue;
        }
        for commit in scan(target) {
            let records = match_commit_to_spans(
                &commit.sha,
                target.branch.as_deref(),
                &target.worktree,
                commit.committed_at,
                &spans,
                gap_secs,
            );
            for record in &records {
                if upsert_commit_session(conn, record).await? {
                    inserted += 1;
                }
            }
        }
    }
    if new_watermark > watermark {
        write_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY, new_watermark).await?;
    }
    Ok(inserted)
}

/// Returns sessions correlated with a branch, worktree, or commit, most
/// recently active first. Branch/worktree queries aggregate span rows per
/// session; commit queries return attribution rows (abbreviated shas match
/// by prefix). `since`/`until` bound span overlap (branch/worktree) or
/// commit time (commit).
pub(crate) async fn sessions_for(
    conn: &Connection,
    query: &SessionsForQuery,
) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
    // Read-only opens never run DDL, so a store written before this schema
    // existed simply has no correlation rows yet — report that as "no
    // matches" rather than a hard `no such table` error.
    if !correlation_tables_present(conn).await? {
        return Ok(Vec::new());
    }
    let limit = query.limit.clamp(1, MAX_SESSIONS_FOR_LIMIT) as i64;
    match &query.git_ref {
        GitRefFilter::Branch(branch) => {
            span_hits(
                conn,
                "branch = ?1",
                Value::Text(branch.clone()),
                query,
                limit,
            )
            .await
        }
        GitRefFilter::Worktree(worktree) => {
            span_hits(
                conn,
                "worktree = ?1",
                Value::Text(worktree.clone()),
                query,
                limit,
            )
            .await
        }
        GitRefFilter::Commit(sha) => commit_hits(conn, sha, query, limit).await,
    }
}

/// Resolves the set of `(provider, session_id)` pairs that satisfy a
/// [`GitScopeFilter`], intersecting each present constraint (a session must
/// match branch AND worktree AND commit when all three are set). Returns
/// `None` when the filter is empty (no scoping requested) and
/// `Some(Vec::new())` when the filter is non-empty but nothing matched (or
/// the correlation tables do not exist yet).
///
/// This is a convenience for callers that want the resolved id set directly;
/// the search paths instead push [`git_scope_exists_predicate`] EXISTS
/// subqueries into their own statement so scoping stays index-served and
/// single-statement.
pub(crate) async fn session_ids_for_scope(
    conn: &Connection,
    filter: &GitScopeFilter,
) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
    if filter.is_empty() {
        return Ok(None);
    }
    if !correlation_tables_present(conn).await? {
        return Ok(Some(Vec::new()));
    }
    // Intersect via a session-id key set, seeded by the first constraint and
    // narrowed by each remaining one.
    let mut result: Option<Vec<(String, String)>> = None;
    if let Some(branch) = &filter.branch {
        let ids = span_session_ids(conn, "branch = ?1", Value::Text(branch.clone())).await?;
        result = Some(intersect_session_ids(result, ids));
    }
    if let Some(worktree) = &filter.worktree {
        let ids = span_session_ids(conn, "worktree = ?1", Value::Text(worktree.clone())).await?;
        result = Some(intersect_session_ids(result, ids));
    }
    if let Some(commit) = &filter.commit {
        let ids = commit_session_ids(conn, commit).await?;
        result = Some(intersect_session_ids(result, ids));
    }
    Ok(Some(result.unwrap_or_default()))
}

fn intersect_session_ids(
    accumulated: Option<Vec<(String, String)>>,
    next: Vec<(String, String)>,
) -> Vec<(String, String)> {
    match accumulated {
        None => next,
        Some(existing) => existing
            .into_iter()
            .filter(|pair| next.contains(pair))
            .collect(),
    }
}

async fn span_session_ids(
    conn: &Connection,
    ref_predicate: &str,
    ref_value: Value,
) -> Result<Vec<(String, String)>, GitCorrelationError> {
    let sql = format!(
        "SELECT DISTINCT provider, session_id FROM session_git_spans WHERE {ref_predicate}"
    );
    let mut rows = conn.query(&sql, vec![ref_value]).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push((row.get(0)?, row.get(1)?));
    }
    Ok(ids)
}

async fn commit_session_ids(
    conn: &Connection,
    sha: &str,
) -> Result<Vec<(String, String)>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT provider, session_id FROM commit_sessions
             WHERE commit_sha = ?1 OR commit_sha LIKE ?2",
            params![sha, format!("{sha}%")],
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push((row.get(0)?, row.get(1)?));
    }
    Ok(ids)
}

/// One EXISTS predicate string plus its bound values for a git-scope
/// constraint, correlated to an outer message row via `session_column`
/// (e.g. `m.session_id` or `r.session_id`). The predicate uses anonymous `?`
/// placeholders, so callers append the returned values in order. Returns
/// `None` when the filter is empty.
///
/// Span rows may carry `provider = ''` (raw hook routes are provider-agnostic),
/// so scoping matches on `session_id` alone rather than also constraining the
/// provider.
pub(crate) fn git_scope_exists_predicate(
    filter: &GitScopeFilter,
    session_column: &str,
) -> Option<(String, Vec<Value>)> {
    if filter.is_empty() {
        return None;
    }
    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    if let Some(branch) = &filter.branch {
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM session_git_spans g \
             WHERE g.session_id = {session_column} AND g.branch = ?)"
        ));
        values.push(Value::Text(branch.clone()));
    }
    if let Some(worktree) = &filter.worktree {
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM session_git_spans g \
             WHERE g.session_id = {session_column} AND g.worktree = ?)"
        ));
        values.push(Value::Text(worktree.clone()));
    }
    if let Some(commit) = &filter.commit {
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM commit_sessions c \
             WHERE c.session_id = {session_column} \
             AND (c.commit_sha = ? OR c.commit_sha LIKE ?))"
        ));
        values.push(Value::Text(commit.clone()));
        values.push(Value::Text(format!("{commit}%")));
    }
    Some((clauses.join(" AND "), values))
}

/// True when the git-correlation tables exist in `conn`'s database. Search
/// paths use this to short-circuit git-scoped queries against stores predating
/// the git-correlation schema (returning empty rather than a `no such table`
/// error).
pub(crate) async fn tables_present(conn: &Connection) -> Result<bool, GitCorrelationError> {
    correlation_tables_present(conn).await
}

async fn correlation_tables_present(conn: &Connection) -> Result<bool, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table'
               AND name IN ('session_git_spans', 'commit_sessions')",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    Ok(row.get::<i64>(0)? == 2)
}

async fn span_hits(
    conn: &Connection,
    ref_predicate: &str,
    ref_value: Value,
    query: &SessionsForQuery,
    limit: i64,
) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
    let mut sql = format!(
        "SELECT provider, session_id,
                MIN(first_ts), MAX(last_ts), SUM(event_count), COUNT(*),
                GROUP_CONCAT(DISTINCT source),
                GROUP_CONCAT(DISTINCT branch),
                GROUP_CONCAT(DISTINCT worktree)
         FROM session_git_spans
         WHERE {ref_predicate}"
    );
    let mut query_params = vec![ref_value];
    if let Some(since) = query.since {
        query_params.push(Value::Integer(since));
        let _ = write!(sql, " AND last_ts >= ?{}", query_params.len());
    }
    if let Some(until) = query.until {
        query_params.push(Value::Integer(until));
        let _ = write!(sql, " AND first_ts <= ?{}", query_params.len());
    }
    query_params.push(Value::Integer(limit));
    let _ = write!(
        sql,
        " GROUP BY provider, session_id
          ORDER BY MAX(last_ts) DESC
          LIMIT ?{}",
        query_params.len()
    );

    let mut rows = conn.query(&sql, query_params).await?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        let sources: Option<String> = row.get(6)?;
        let branches: Option<String> = row.get(7)?;
        let worktrees: Option<String> = row.get(8)?;
        hits.push(SessionGitCorrelationHit {
            provider: row.get(0)?,
            session_id: row.get(1)?,
            branch: single_concat_value(branches.as_deref()),
            worktree: single_concat_value(worktrees.as_deref()),
            first_ts: row.get(2)?,
            last_ts: row.get(3)?,
            event_count: row.get::<Option<i64>>(4)?.unwrap_or(0),
            span_count: row.get::<Option<i64>>(5)?.unwrap_or(0),
            sources: sources
                .as_deref()
                .map(|joined| joined.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            commit_sha: None,
            committed_at: None,
            span_overlap_kind: None,
        });
    }
    Ok(hits)
}

/// A `GROUP_CONCAT(DISTINCT …)` column collapses to its single value when
/// every aggregated row agreed; report nothing when the rows disagreed
/// (multiple branches/worktrees for one session) rather than a joined blob.
fn single_concat_value(joined: Option<&str>) -> Option<String> {
    joined
        .filter(|value| !value.is_empty() && !value.contains(','))
        .map(str::to_string)
}

async fn commit_hits(
    conn: &Connection,
    sha: &str,
    query: &SessionsForQuery,
    limit: i64,
) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
    let mut sql = "SELECT provider, session_id, branch, worktree,
                commit_sha, committed_at, span_overlap_kind
         FROM commit_sessions
         WHERE (commit_sha = ?1 OR commit_sha LIKE ?2)"
        .to_string();
    // `parse_commit_sha` guarantees hex-only input, so the LIKE pattern
    // cannot contain wildcards other than the appended one.
    let mut query_params = vec![Value::Text(sha.to_string()), Value::Text(format!("{sha}%"))];
    if let Some(since) = query.since {
        query_params.push(Value::Integer(since));
        let _ = write!(sql, " AND committed_at >= ?{}", query_params.len());
    }
    if let Some(until) = query.until {
        query_params.push(Value::Integer(until));
        let _ = write!(sql, " AND committed_at <= ?{}", query_params.len());
    }
    query_params.push(Value::Integer(limit));
    let _ = write!(
        sql,
        " ORDER BY committed_at DESC LIMIT ?{}",
        query_params.len()
    );

    let mut rows = conn.query(&sql, query_params).await?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        let overlap: String = row.get(6)?;
        hits.push(SessionGitCorrelationHit {
            provider: row.get(0)?,
            session_id: row.get(1)?,
            branch: row.get(2)?,
            worktree: row.get(3)?,
            first_ts: None,
            last_ts: None,
            event_count: 0,
            span_count: 0,
            sources: Vec::new(),
            commit_sha: row.get(4)?,
            committed_at: row.get(5)?,
            span_overlap_kind: SpanOverlapKind::from_db(&overlap),
        });
    }
    Ok(hits)
}

// ── Historical backfill ─────────────────────────────────────────────────
//
// The live path folds observations in as sessions run, but sessions that
// predate span recording leave only three offline signals: session-store
// timestamps, global analytics events, and the git reflog. The backfill
// reconstructs spans and commit attribution from those.

/// One session's declared and message-derived activity bounds, read from the
/// per-project session store. Any field may be `None` when the source row left
/// it unset; [`SessionActivityRow::window`] collapses them into a single
/// `[start, end]` when at least one bound is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionActivityRow {
    pub provider: String,
    pub session_id: String,
    pub project_path: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub message_min_ts: Option<i64>,
    pub message_max_ts: Option<i64>,
}

/// Millisecond/second boundary for stored session timestamps: any value at or
/// above this is treated as unix millis and divided by 1000 (mirrors
/// `GlobalDb::latest_session_activity_secs` and `kiro::normalize_timestamp`).
const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

/// Normalizes a stored session timestamp to unix seconds. Providers persist
/// either seconds or milliseconds; a millis-scale value left un-normalized
/// yields a span window ~1000x too wide, so commit attribution (seconds-scale
/// `git %ct`) never overlaps it and rendered dates land in the far future.
fn normalize_activity_ts(ts: i64) -> i64 {
    if ts >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
        ts / 1000
    } else {
        ts
    }
}

impl SessionActivityRow {
    /// Coarse `[start, end]` window from the widest pair of known bounds, or
    /// `None` when the session carries no usable timestamp at all. Each bound is
    /// normalized to unix seconds (see [`normalize_activity_ts`]) so mixed
    /// seconds/millis rows on legacy stores produce a seconds-scale window.
    pub fn window(&self) -> Option<(i64, i64)> {
        let mut lo: Option<i64> = None;
        let mut hi: Option<i64> = None;
        for ts in [
            self.started_at,
            self.ended_at,
            self.message_min_ts,
            self.message_max_ts,
        ]
        .into_iter()
        .flatten()
        .map(normalize_activity_ts)
        {
            lo = Some(lo.map_or(ts, |cur| cur.min(ts)));
            hi = Some(hi.map_or(ts, |cur| cur.max(ts)));
        }
        match (lo, hi) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        }
    }
}

/// One `HEAD` position in a worktree's reflog timeline: the branch `HEAD`
/// pointed at starting from `from_ts`, or `None` for a detached-HEAD checkout
/// (the target was a raw sha, not a branch name).
pub type BranchTimelineEntry = (i64, Option<String>);

/// Reconstructs a worktree's branch timeline from `git reflog --date=unix`
/// output on `HEAD`. Only `checkout: moving from X to Y` entries advance the
/// timeline; each yields `(entry_ts, branch_of(Y))`, where a target that looks
/// like a raw commit sha is treated as detached HEAD (`None`).
///
/// Returned oldest-first (reflog output is newest-first, so this reverses it),
/// which is the order [`window_branch_segments`] expects. Pure: no IO.
pub fn branch_timeline_from_reflog(reflog_text: &str) -> Vec<BranchTimelineEntry> {
    let mut entries: Vec<BranchTimelineEntry> = Vec::new();
    for line in reflog_text.lines() {
        if let Some(entry) = parse_reflog_checkout_line(line) {
            entries.push(entry);
        }
    }
    entries.reverse();
    entries
}

/// Parses one `git reflog --date=unix` line, returning `(ts, branch)` when it
/// is a `checkout: moving from X to Y` entry (`branch` = `None` when `Y` is a
/// detached sha). Non-checkout and unparseable lines return `None`.
///
/// Expected shape: `<sha> HEAD@{<unix>}: checkout: moving from <X> to <Y>`.
fn parse_reflog_checkout_line(line: &str) -> Option<BranchTimelineEntry> {
    let ts_open = line.find("HEAD@{")? + "HEAD@{".len();
    let ts_close = line[ts_open..].find('}')? + ts_open;
    let ts: i64 = line[ts_open..ts_close].trim().parse().ok()?;

    let rest = line.get(ts_close + 1..)?.trim_start();
    let message = rest.strip_prefix(':').map_or(rest, str::trim_start);
    let moving = message.strip_prefix("checkout: moving from ")?;
    // Split on the last ` to ` so a branch name containing ` to ` in X does
    // not confuse the split of the target Y.
    let (_from, to) = moving.rsplit_once(" to ")?;
    let target = to.trim();
    if target.is_empty() {
        return None;
    }
    Some((ts, branch_from_checkout_target(target)))
}

/// Classifies a checkout target: a 7–64 char all-hex token is a detached-HEAD
/// commit (`None`); anything else is treated as a branch name.
fn branch_from_checkout_target(target: &str) -> Option<String> {
    let looks_like_sha =
        (7..=64).contains(&target.len()) && target.chars().all(|c| c.is_ascii_hexdigit());
    if looks_like_sha {
        None
    } else {
        Some(target.to_string())
    }
}

/// One branch segment of a session's activity window: the branch `HEAD`
/// pointed at (per the reflog timeline) over `[start, end]`, clamped to the
/// window. `None` branch = detached HEAD during that stretch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowBranchSegment {
    pub branch: Option<String>,
    pub start: i64,
    pub end: i64,
}

/// Intersects an activity window `[win_start, win_end]` with a worktree's
/// branch `timeline` (oldest-first, from [`branch_timeline_from_reflog`]),
/// yielding the branch segments the session overlapped. The leading stretch —
/// before the first timeline entry that lands after `win_start` — is
/// attributed to `initial_branch` (callers pass the branch `HEAD` currently
/// points at as the floor).
///
/// Pure: no IO. Segments are clamped to the window and returned oldest-first.
pub fn window_branch_segments(
    win_start: i64,
    win_end: i64,
    timeline: &[BranchTimelineEntry],
    initial_branch: Option<&str>,
) -> Vec<WindowBranchSegment> {
    if win_start > win_end {
        return Vec::new();
    }
    let mut current_branch: Option<String> = initial_branch.map(str::to_string);
    // Advance to the last timeline entry at or before win_start; that entry's
    // target is the branch HEAD held when the window opened.
    let mut idx = 0;
    while idx < timeline.len() && timeline[idx].0 <= win_start {
        current_branch.clone_from(&timeline[idx].1);
        idx += 1;
    }
    let mut segments: Vec<WindowBranchSegment> = Vec::new();
    let mut seg_start = win_start;
    while idx < timeline.len() && timeline[idx].0 <= win_end {
        let (change_ts, next_branch) = &timeline[idx];
        // Only a *real* branch change ends the current segment; a checkout that
        // lands back on the same branch (or reflog noise) must not fragment it.
        if *next_branch != current_branch && *change_ts > seg_start {
            segments.push(WindowBranchSegment {
                branch: current_branch.clone(),
                start: seg_start,
                end: *change_ts,
            });
            seg_start = *change_ts;
        }
        current_branch.clone_from(next_branch);
        idx += 1;
    }
    if seg_start <= win_end {
        segments.push(WindowBranchSegment {
            branch: current_branch,
            start: seg_start,
            end: win_end,
        });
    }
    segments
}

/// Reason a session was skipped by the backfill (counted and reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillSkipReason {
    /// Session had no usable timestamp in any signal source.
    NoActivityWindow,
    /// `project_path` was empty or not a resolvable git worktree.
    NotAWorktree,
    /// A git command for this session's repo failed; failed open.
    GitError,
}

impl BackfillSkipReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoActivityWindow => "no_activity_window",
            Self::NotAWorktree => "not_a_worktree",
            Self::GitError => "git_error",
        }
    }
}

/// Tunables for [`run_backfill`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillOptions {
    /// Inclusive lower bound (unix seconds) on session activity and commit
    /// times. Sessions whose activity ends before this are skipped.
    pub since: i64,
    /// Maximum number of sessions to scan.
    pub limit_sessions: usize,
    /// Span merge gap forwarded to [`record_span_observation`].
    pub merge_gap_secs: i64,
    /// Hard cap on commits parsed from a single `git log` invocation.
    pub max_commits_per_repo: usize,
    /// When true, derive and count everything but write nothing.
    pub dry_run: bool,
}

impl Default for BackfillOptions {
    fn default() -> Self {
        Self {
            since: 0,
            limit_sessions: 500,
            merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
            max_commits_per_repo: 5_000,
            dry_run: false,
        }
    }
}

/// Outcome counters for one backfill run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillStats {
    pub sessions_scanned: usize,
    pub spans_written: usize,
    pub commits_attributed: usize,
    pub skipped_no_window: usize,
    pub skipped_not_worktree: usize,
    pub skipped_git_error: usize,
}

impl BackfillStats {
    fn record_skip(&mut self, reason: BackfillSkipReason) {
        match reason {
            BackfillSkipReason::NoActivityWindow => self.skipped_no_window += 1,
            BackfillSkipReason::NotAWorktree => self.skipped_not_worktree += 1,
            BackfillSkipReason::GitError => self.skipped_git_error += 1,
        }
    }

    pub const fn skipped_total(&self) -> usize {
        self.skipped_no_window + self.skipped_not_worktree + self.skipped_git_error
    }
}

/// Abstracts the git subprocess surface the backfill needs, so tests can run
/// the core against a real repo ([`SystemGit`]) or a canned fixture.
pub trait GitReflogSource {
    /// `git reflog --date=unix HEAD` text for `worktree`, or `None` on error.
    fn reflog(&self, worktree: &std::path::Path) -> Option<String>;
    /// The branch `HEAD` currently points at in `worktree` (`None` = detached
    /// or unknown), used as the leading-segment floor.
    fn current_branch(&self, worktree: &std::path::Path) -> Option<String>;
    /// `git log <branch> --pretty=%H %ct --since=<since>` text for `worktree`,
    /// newest-first. `None` on error.
    fn commit_log(&self, worktree: &std::path::Path, branch: &str, since: i64) -> Option<String>;
}

/// Real git-subprocess implementation of [`GitReflogSource`].
pub struct SystemGit;

impl SystemGit {
    fn output(worktree: &std::path::Path, args: &[&str]) -> Option<String> {
        let output = std::process::Command::new(crate::git::git_program())
            .args(args)
            .current_dir(worktree)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }
}

impl GitReflogSource for SystemGit {
    fn reflog(&self, worktree: &std::path::Path) -> Option<String> {
        Self::output(worktree, &["reflog", "--date=unix", "HEAD"])
    }

    fn current_branch(&self, worktree: &std::path::Path) -> Option<String> {
        let raw = Self::output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "HEAD" {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn commit_log(&self, worktree: &std::path::Path, branch: &str, since: i64) -> Option<String> {
        Self::output(
            worktree,
            &[
                "log",
                branch,
                "--pretty=%H %ct",
                &format!("--since={since}"),
            ],
        )
    }
}

/// Parses `git log --pretty=%H %ct` output into `(sha, committed_at)` pairs,
/// capping at `max`. Malformed and non-hex lines are skipped. Pure.
pub fn parse_commit_log(log_text: &str, max: usize) -> Vec<(String, i64)> {
    let mut commits = Vec::new();
    for line in log_text.lines() {
        if commits.len() >= max {
            break;
        }
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else { continue };
        let Some(ts) = parts.next().and_then(|t| t.parse::<i64>().ok()) else {
            continue;
        };
        if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            commits.push((sha.to_ascii_lowercase(), ts));
        }
    }
    commits
}

/// Runs the historical backfill against one project's session store.
///
/// `session_store` is the per-project `sessions.db` (already open, and — for a
/// real run — writable). `analytics_events` are global analytics rows whose
/// `session_id` matches a scanned session; only their timestamps are consumed
/// (branch data is never assumed present). `git` supplies the reflog/log
/// subprocess surface. Fail-open: a broken repo or session is counted and
/// skipped, never aborting the run.
///
/// When `opts.dry_run` is set no rows are written; the returned counts reflect
/// what *would* have been written.
pub async fn run_backfill(
    session_store: &crate::global_db::GlobalDb,
    analytics_events: &[crate::global_db::AnalyticsEventRecord],
    git: &dyn GitReflogSource,
    opts: &BackfillOptions,
) -> Result<BackfillStats, GitCorrelationError> {
    let mut stats = BackfillStats::default();
    let rows = session_store
        .session_activity_rows(opts.limit_sessions)
        .await
        .map_err(GitCorrelationError::Db)?;

    // Index analytics timestamps by (provider, session_id) for O(1) lookup.
    let mut analytics_ts: std::collections::HashMap<(String, String), Vec<i64>> =
        std::collections::HashMap::new();
    for event in analytics_events {
        if let Some(session_id) = event.session_id.as_deref() {
            analytics_ts
                .entry((event.provider.clone(), session_id.to_string()))
                .or_default()
                .push(event.timestamp);
        }
    }

    for row in &rows {
        stats.sessions_scanned += 1;
        if let Err(reason) =
            backfill_one_session(session_store, git, opts, row, &analytics_ts, &mut stats).await
        {
            stats.record_skip(reason);
        }
    }
    Ok(stats)
}

async fn backfill_one_session(
    session_store: &crate::global_db::GlobalDb,
    git: &dyn GitReflogSource,
    opts: &BackfillOptions,
    row: &SessionActivityRow,
    analytics_ts: &std::collections::HashMap<(String, String), Vec<i64>>,
    stats: &mut BackfillStats,
) -> Result<(), BackfillSkipReason> {
    let (mut win_start, win_end) = row.window().ok_or(BackfillSkipReason::NoActivityWindow)?;
    if win_end < opts.since {
        return Err(BackfillSkipReason::NoActivityWindow);
    }
    win_start = win_start.max(opts.since);
    if win_start > win_end {
        return Err(BackfillSkipReason::NoActivityWindow);
    }

    if row.project_path.trim().is_empty() {
        return Err(BackfillSkipReason::NotAWorktree);
    }
    let worktree_path = std::path::Path::new(row.project_path.trim());
    let worktree_root = crate::worktree::git_worktree_root(worktree_path)
        .ok_or(BackfillSkipReason::NotAWorktree)?;
    let worktree = normalize_worktree(&worktree_root.to_string_lossy());

    let reflog_text = git
        .reflog(&worktree_root)
        .ok_or(BackfillSkipReason::GitError)?;
    let timeline = branch_timeline_from_reflog(&reflog_text);
    let current_branch = git.current_branch(&worktree_root);

    // Extra observation timestamps: analytics event times inside the
    // (since-clamped) window, which refine span boundaries within a segment.
    let mut analytics_within: Vec<i64> = Vec::new();
    if let Some(times) = analytics_ts.get(&(row.provider.clone(), row.session_id.clone())) {
        for &ts in times {
            if ts >= win_start && ts <= win_end {
                analytics_within.push(ts);
            }
        }
    }

    let segments = window_branch_segments(win_start, win_end, &timeline, current_branch.as_deref());

    for segment in &segments {
        // Every segment yields a span: seed it with its own clamped edges so an
        // interior segment (e.g. a mid-session branch switch) is recorded even
        // when the global window edges fall outside it. Analytics timestamps
        // inside the segment refine the boundaries; record_span_observation
        // merges observations on the same branch within the merge gap.
        let mut segment_ts = vec![segment.start, segment.end];
        segment_ts.extend(
            analytics_within
                .iter()
                .copied()
                .filter(|&ts| ts >= segment.start && ts <= segment.end),
        );
        for &ts in &segment_ts {
            if !opts.dry_run {
                session_store
                    .git_record_span_observation(
                        &SpanObservation {
                            provider: row.provider.clone(),
                            session_id: row.session_id.clone(),
                            thread_id: None,
                            branch: segment.branch.clone(),
                            worktree: worktree.clone(),
                            ts,
                            source: SpanSource::Backfill,
                        },
                        opts.merge_gap_secs,
                    )
                    .await
                    .map_err(|_| BackfillSkipReason::GitError)?;
            }
        }
        stats.spans_written += 1;

        // Attribute commits on this segment's branch within the segment window.
        let Some(branch) = segment.branch.as_deref() else {
            continue;
        };
        let Some(log_text) = git.commit_log(&worktree_root, branch, segment.start) else {
            continue;
        };
        for (sha, committed_at) in parse_commit_log(&log_text, opts.max_commits_per_repo) {
            if committed_at < segment.start || committed_at > segment.end {
                continue;
            }
            if opts.dry_run {
                stats.commits_attributed += 1;
                continue;
            }
            let inserted = session_store
                .git_upsert_commit_session(&CommitSessionRecord {
                    commit_sha: sha,
                    provider: row.provider.clone(),
                    session_id: row.session_id.clone(),
                    branch: Some(branch.to_string()),
                    worktree: Some(worktree.clone()),
                    committed_at,
                    span_overlap_kind: SpanOverlapKind::WithinSpan,
                    span_id: None,
                })
                .await
                .map_err(|_| BackfillSkipReason::GitError)?;
            if inserted {
                stats.commits_attributed += 1;
            }
        }
    }
    Ok(())
}

/// Reads per-session activity windows for the backfill. See
/// [`crate::global_db::GlobalDb::session_activity_rows`].
pub(crate) async fn session_activity_rows(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<SessionActivityRow>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT s.provider, s.session_id, s.project_path,
                    s.started_at, s.ended_at,
                    MIN(m.timestamp), MAX(m.timestamp)
             FROM sessions s
             LEFT JOIN session_messages m
                    ON m.provider = s.provider AND m.session_id = s.session_id
             GROUP BY s.provider, s.session_id
             ORDER BY COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) DESC
             LIMIT ?1",
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
        .map_err(|e| format!("failed to query session activity rows: {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("failed to read session activity row: {e}"))?
    {
        out.push(SessionActivityRow {
            provider: row
                .get(0)
                .map_err(|e| format!("failed to decode provider: {e}"))?,
            session_id: row
                .get(1)
                .map_err(|e| format!("failed to decode session_id: {e}"))?,
            project_path: row
                .get(2)
                .map_err(|e| format!("failed to decode project_path: {e}"))?,
            started_at: row
                .get(3)
                .map_err(|e| format!("failed to decode started_at: {e}"))?,
            ended_at: row
                .get(4)
                .map_err(|e| format!("failed to decode ended_at: {e}"))?,
            message_min_ts: row
                .get(5)
                .map_err(|e| format!("failed to decode message_min_ts: {e}"))?,
            message_max_ts: row
                .get(6)
                .map_err(|e| format!("failed to decode message_max_ts: {e}"))?,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use libsql::Builder;

    use super::*;

    fn observation(
        session_id: &str,
        branch: Option<&str>,
        worktree: &str,
        ts: i64,
    ) -> SpanObservation {
        SpanObservation {
            provider: "claude".to_string(),
            session_id: session_id.to_string(),
            thread_id: None,
            branch: branch.map(str::to_string),
            worktree: worktree.to_string(),
            ts,
            source: SpanSource::HookRoute,
        }
    }

    async fn test_conn() -> Connection {
        let db = Builder::new_local(":memory:")
            .build()
            .await
            .expect("in-memory db");
        let conn = db.connect().expect("connect");
        ensure_git_correlation_schema(&conn)
            .await
            .expect("schema should apply");
        conn
    }

    #[test]
    fn normalize_worktree_strips_trailing_slashes_and_backslashes() {
        assert_eq!(normalize_worktree("/repo/wt/"), "/repo/wt");
        assert_eq!(normalize_worktree("  /repo/wt  "), "/repo/wt");
        assert_eq!(normalize_worktree("/repo/wt///"), "/repo/wt");
        assert_eq!(normalize_worktree("/"), "/");
        assert_eq!(normalize_worktree("C:\\repo\\wt\\"), "C:/repo/wt");
    }

    #[test]
    fn git_ref_filter_parses_and_validates_kinds() {
        assert_eq!(
            GitRefFilter::parse("branch", " feature/x "),
            Ok(GitRefFilter::Branch("feature/x".to_string()))
        );
        assert_eq!(
            GitRefFilter::parse("worktree", "/repo/wt/"),
            Ok(GitRefFilter::Worktree("/repo/wt".to_string()))
        );
        assert_eq!(
            GitRefFilter::parse("commit", "ABCDEF12"),
            Ok(GitRefFilter::Commit("abcdef12".to_string()))
        );
        assert!(GitRefFilter::parse("commit", "abc").is_err());
        assert!(GitRefFilter::parse("commit", "not-hex-at-all").is_err());
        assert!(GitRefFilter::parse("tag", "v1.0").is_err());
        assert!(GitRefFilter::parse("branch", "   ").is_err());
    }

    #[test]
    fn observation_extends_span_only_within_gap() {
        assert!(observation_extends_span(100, 200, 150, 60));
        assert!(observation_extends_span(100, 200, 260, 60));
        assert!(observation_extends_span(100, 200, 40, 60));
        assert!(!observation_extends_span(100, 200, 261, 60));
        assert!(!observation_extends_span(100, 200, 39, 60));
    }

    #[test]
    fn git_scope_filter_reports_emptiness_and_validates_commit() {
        let empty = GitScopeFilter::from_args(None, Some("  "), None).unwrap();
        assert!(empty.is_empty());
        let filter =
            GitScopeFilter::from_args(Some("main"), Some("/repo/"), Some("ABC123")).unwrap();
        assert_eq!(filter.branch.as_deref(), Some("main"));
        assert_eq!(filter.worktree.as_deref(), Some("/repo"));
        assert_eq!(filter.commit.as_deref(), Some("abc123"));
        assert!(GitScopeFilter::from_args(None, None, Some("xyz")).is_err());
    }

    #[tokio::test]
    async fn schema_is_idempotent() {
        let conn = test_conn().await;
        ensure_git_correlation_schema(&conn)
            .await
            .expect("second ensure should be a no-op");
    }

    #[tokio::test]
    async fn observations_merge_within_gap_and_split_on_branch_switch() {
        let conn = test_conn().await;
        let first =
            record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
                .await
                .unwrap();
        let merged =
            record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_300), 600)
                .await
                .unwrap();
        assert_eq!(first, merged, "in-gap observation should extend the span");

        // Branch switch mid-session opens a second span; switching back
        // within the gap of the first span extends it again (A → B → A).
        let switched =
            record_span_observation(&conn, &observation("s1", Some("feat"), "/repo", 1_400), 600)
                .await
                .unwrap();
        assert_ne!(first, switched);
        let back =
            record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_700), 600)
                .await
                .unwrap();
        assert_eq!(first, back);

        // Out-of-gap observation on the same branch opens a new span.
        let idle_gap =
            record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 9_000), 600)
                .await
                .unwrap();
        assert_ne!(first, idle_gap);

        // Detached HEAD (branch = NULL) never merges into a named span.
        let detached =
            record_span_observation(&conn, &observation("s1", None, "/repo", 1_750), 600)
                .await
                .unwrap();
        assert_ne!(first, detached);
    }

    #[tokio::test]
    async fn sessions_for_branch_worktree_and_commit_round_trip() {
        let conn = test_conn().await;
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
            .await
            .unwrap();
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_200), 600)
            .await
            .unwrap();
        record_span_observation(
            &conn,
            &observation("s2", Some("main"), "/repo/wt", 5_000),
            600,
        )
        .await
        .unwrap();
        record_span_observation(&conn, &observation("s3", Some("feat"), "/repo", 2_000), 600)
            .await
            .unwrap();

        let hits = sessions_for(
            &conn,
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].session_id, "s2", "most recent activity first");
        assert_eq!(hits[1].session_id, "s1");
        assert_eq!(hits[1].event_count, 2);
        assert_eq!(hits[1].span_count, 1);
        assert_eq!(hits[1].first_ts, Some(1_000));
        assert_eq!(hits[1].last_ts, Some(1_200));
        assert_eq!(hits[1].sources, vec!["hook_route".to_string()]);
        assert_eq!(hits[1].branch.as_deref(), Some("main"));
        assert_eq!(hits[1].worktree.as_deref(), Some("/repo"));

        // Time-scoped: only the span overlapping [4000, 6000] survives.
        let scoped = sessions_for(
            &conn,
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: Some(4_000),
                until: Some(6_000),
                limit: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].session_id, "s2");

        let by_worktree = sessions_for(
            &conn,
            &SessionsForQuery {
                git_ref: GitRefFilter::Worktree("/repo/wt".to_string()),
                since: None,
                until: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(by_worktree.len(), 1);
        assert_eq!(by_worktree[0].session_id, "s2");

        let record = CommitSessionRecord {
            commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            provider: "claude".to_string(),
            session_id: "s1".to_string(),
            branch: Some("main".to_string()),
            worktree: Some("/repo".to_string()),
            committed_at: 1_150,
            span_overlap_kind: SpanOverlapKind::WithinSpan,
            span_id: None,
        };
        assert!(upsert_commit_session(&conn, &record).await.unwrap());
        assert!(
            !upsert_commit_session(&conn, &record).await.unwrap(),
            "second upsert should be an idempotent no-op"
        );

        let by_commit = sessions_for(
            &conn,
            &SessionsForQuery {
                git_ref: GitRefFilter::Commit("abcdef12".to_string()),
                since: None,
                until: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(by_commit.len(), 1);
        assert_eq!(by_commit[0].session_id, "s1");
        assert_eq!(
            by_commit[0].commit_sha.as_deref(),
            Some("abcdef1234567890abcdef1234567890abcdef12")
        );
        assert_eq!(by_commit[0].committed_at, Some(1_150));
        assert_eq!(
            by_commit[0].span_overlap_kind,
            Some(SpanOverlapKind::WithinSpan)
        );
    }

    #[test]
    fn branch_timeline_parses_checkouts_and_detached_and_skips_noise() {
        let reflog = concat!(
            "def456 HEAD@{1700000300}: commit: work on feat\n",
            "def456 HEAD@{1700000200}: checkout: moving from main to feat\n",
            "abc123 HEAD@{1700000150}: checkout: moving from feat to a1b2c3d4e5f6\n",
            "abc123 HEAD@{1700000100}: checkout: moving from a1b2c3d4e5f6 to main\n",
            "abc123 HEAD@{1700000050}: clone: from origin\n",
        );
        let timeline = branch_timeline_from_reflog(reflog);
        // Oldest-first, only checkout lines, detached HEAD → None.
        assert_eq!(
            timeline,
            vec![
                (1_700_000_100, Some("main".to_string())),
                (1_700_000_150, None),
                (1_700_000_200, Some("feat".to_string())),
            ]
        );
    }

    #[test]
    fn branch_timeline_ignores_unparseable_lines() {
        assert!(branch_timeline_from_reflog("").is_empty());
        assert!(branch_timeline_from_reflog("garbage without a head marker").is_empty());
        // Non-checkout HEAD entries do not advance the timeline.
        assert!(
            branch_timeline_from_reflog("abc HEAD@{1700000000}: reset: moving to HEAD").is_empty()
        );
    }

    #[test]
    fn window_segments_split_on_mid_window_branch_switch() {
        // Session ran [100, 300]; HEAD switched main→feat at 200.
        let timeline = vec![
            (150, Some("main".to_string())),
            (200, Some("feat".to_string())),
        ];
        let segments = window_branch_segments(100, 300, &timeline, Some("main"));
        assert_eq!(
            segments,
            vec![
                WindowBranchSegment {
                    branch: Some("main".to_string()),
                    start: 100,
                    end: 200,
                },
                WindowBranchSegment {
                    branch: Some("feat".to_string()),
                    start: 200,
                    end: 300,
                },
            ]
        );
    }

    #[test]
    fn window_segments_use_initial_branch_before_first_entry() {
        // No timeline entry inside the window → whole window is initial_branch.
        let segments = window_branch_segments(100, 300, &[], Some("main"));
        assert_eq!(
            segments,
            vec![WindowBranchSegment {
                branch: Some("main".to_string()),
                start: 100,
                end: 300,
            }]
        );
        // An entry at or before win_start sets the floor branch.
        let timeline = vec![(50, Some("feat".to_string()))];
        let segments = window_branch_segments(100, 300, &timeline, Some("main"));
        assert_eq!(segments[0].branch.as_deref(), Some("feat"));
    }

    #[test]
    fn window_segments_empty_when_start_after_end() {
        assert!(window_branch_segments(300, 100, &[], Some("main")).is_empty());
    }

    #[test]
    fn session_activity_row_window_spans_widest_bounds() {
        let row = SessionActivityRow {
            provider: "claude".to_string(),
            session_id: "s1".to_string(),
            project_path: "/repo".to_string(),
            started_at: Some(200),
            ended_at: None,
            message_min_ts: Some(150),
            message_max_ts: Some(400),
        };
        assert_eq!(row.window(), Some((150, 400)));
        let empty = SessionActivityRow {
            started_at: None,
            ended_at: None,
            message_min_ts: None,
            message_max_ts: None,
            ..row
        };
        assert_eq!(empty.window(), None);
    }

    #[test]
    fn session_activity_row_window_normalizes_millis_bounds() {
        // Legacy/mixed stores can hold millisecond-scale message timestamps
        // (see `latest_session_activity_secs`). Left un-normalized the window
        // would be ~1000x too wide, so a seconds-scale git commit time could
        // never fall inside it. `window()` must collapse to the seconds scale.
        let commit_ts = 1_700_000_500; // seconds-scale git %ct
        let row = SessionActivityRow {
            provider: "claude".to_string(),
            session_id: "s1".to_string(),
            project_path: "/repo".to_string(),
            started_at: None,
            ended_at: None,
            message_min_ts: Some(1_700_000_000_000), // millis
            message_max_ts: Some(1_700_001_000_000), // millis
        };
        let (start, end) = row.window().expect("window from millis bounds");
        assert_eq!((start, end), (1_700_000_000, 1_700_001_000));
        // The seconds-scale commit now lands inside the seconds-scale span,
        // which a millis-scale window could never contain.
        assert_eq!(
            commit_overlap_kind(start, end, commit_ts, 600),
            Some(SpanOverlapKind::WithinSpan)
        );
    }

    #[test]
    fn parse_commit_log_skips_malformed_and_caps() {
        let log = concat!(
            "ABCDEF1234 1700000000\n",
            "not-a-sha 1700000100\n",
            "deadbeef xyz\n",
            "cafebabe 1700000200\n",
        );
        let commits = parse_commit_log(log, 10);
        assert_eq!(
            commits,
            vec![
                ("abcdef1234".to_string(), 1_700_000_000),
                ("cafebabe".to_string(), 1_700_000_200),
            ]
        );
        assert_eq!(parse_commit_log(log, 1).len(), 1);
    }

    #[test]
    fn debounce_suppresses_bursts_but_admits_after_interval() {
        let mut debounce = SpanObservationDebounce::new();
        let key = span_debounce_key("", "s1", Some("main"), "/repo");
        // First observation always writes; a burst inside the interval is
        // suppressed; a later observation past the interval writes again.
        assert!(debounce.should_record(&key, 1_000, 30));
        assert!(!debounce.should_record(&key, 1_005, 30));
        assert!(!debounce.should_record(&key, 1_029, 30));
        assert!(debounce.should_record(&key, 1_030, 30));
        assert!(!debounce.should_record(&key, 1_040, 30));
    }

    #[test]
    fn debounce_keys_separate_branch_and_worktree_and_detached() {
        let mut debounce = SpanObservationDebounce::new();
        let main = span_debounce_key("", "s1", Some("main"), "/repo");
        let feat = span_debounce_key("", "s1", Some("feat"), "/repo");
        let other_wt = span_debounce_key("", "s1", Some("main"), "/repo/wt");
        let detached = span_debounce_key("", "s1", None, "/repo");
        // A branch switch is never debounced away by a prior branch's write.
        assert!(debounce.should_record(&main, 1_000, 30));
        assert!(debounce.should_record(&feat, 1_001, 30));
        assert!(debounce.should_record(&other_wt, 1_002, 30));
        assert!(debounce.should_record(&detached, 1_003, 30));
        // Distinct keys for the four cases.
        assert_ne!(main, feat);
        assert_ne!(main, other_wt);
        assert_ne!(main, detached);
    }

    #[test]
    fn debounce_admits_out_of_order_older_observation() {
        let mut debounce = SpanObservationDebounce::new();
        let key = span_debounce_key("", "s1", Some("main"), "/repo");
        assert!(debounce.should_record(&key, 1_000, 30));
        // An out-of-order (older) timestamp is never suppressed.
        assert!(debounce.should_record(&key, 900, 30));
    }

    #[test]
    fn commit_overlap_kind_classifies_within_extended_and_outside() {
        assert_eq!(
            commit_overlap_kind(100, 200, 150, 60),
            Some(SpanOverlapKind::WithinSpan)
        );
        assert_eq!(
            commit_overlap_kind(100, 200, 100, 60),
            Some(SpanOverlapKind::WithinSpan)
        );
        assert_eq!(
            commit_overlap_kind(100, 200, 260, 60),
            Some(SpanOverlapKind::ExtendedWindow)
        );
        assert_eq!(
            commit_overlap_kind(100, 200, 40, 60),
            Some(SpanOverlapKind::ExtendedWindow)
        );
        assert_eq!(commit_overlap_kind(100, 200, 261, 60), None);
        assert_eq!(commit_overlap_kind(100, 200, 39, 60), None);
    }

    #[test]
    fn match_commit_to_spans_filters_by_branch_worktree_and_window() {
        let spans = vec![
            SpanWindow {
                span_id: 1,
                provider: "claude".to_string(),
                session_id: "s1".to_string(),
                branch: Some("main".to_string()),
                worktree: "/repo".to_string(),
                first_ts: 100,
                last_ts: 200,
            },
            // Concurrent session on the same branch/worktree.
            SpanWindow {
                span_id: 2,
                provider: String::new(),
                session_id: "s2".to_string(),
                branch: Some("main".to_string()),
                worktree: "/repo".to_string(),
                first_ts: 120,
                last_ts: 190,
            },
            // Different branch — must not match a main commit.
            SpanWindow {
                span_id: 3,
                provider: "claude".to_string(),
                session_id: "s3".to_string(),
                branch: Some("feat".to_string()),
                worktree: "/repo".to_string(),
                first_ts: 100,
                last_ts: 200,
            },
            // Different worktree — must not match.
            SpanWindow {
                span_id: 4,
                provider: "claude".to_string(),
                session_id: "s4".to_string(),
                branch: Some("main".to_string()),
                worktree: "/repo/wt".to_string(),
                first_ts: 100,
                last_ts: 200,
            },
        ];

        // A within-window commit is attributed to both concurrent main spans.
        let records = match_commit_to_spans("deadbeef", Some("main"), "/repo", 150, &spans, 60);
        assert_eq!(records.len(), 2);
        let ids: Vec<i64> = records.iter().filter_map(|r| r.span_id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert!(records
            .iter()
            .all(|r| r.span_overlap_kind == SpanOverlapKind::WithinSpan));
        assert_eq!(records[0].session_id, "s1");
        assert_eq!(records[0].worktree.as_deref(), Some("/repo"));

        // A just-past-the-edge commit lands in the extended window only.
        let extended = match_commit_to_spans("cafef00d", Some("main"), "/repo", 250, &spans, 60);
        assert_eq!(extended.len(), 2);
        assert!(extended
            .iter()
            .all(|r| r.span_overlap_kind == SpanOverlapKind::ExtendedWindow));

        // A commit outside every window attributes nothing.
        assert!(
            match_commit_to_spans("beefcafe", Some("main"), "/repo", 500, &spans, 60).is_empty()
        );
        // A commit on an unrecorded branch attributes nothing.
        assert!(
            match_commit_to_spans("beefcafe", Some("other"), "/repo", 150, &spans, 60).is_empty()
        );
    }

    #[tokio::test]
    async fn commit_attribution_sweep_attributes_and_advances_watermark() {
        let conn = test_conn().await;
        // One session active on main in /repo over [1000, 2000]. The 1000-wide
        // gap between the two observations exceeds the 600 merge gap, so a third
        // in-window observation keeps them a single contiguous span.
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
            .await
            .unwrap();
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_500), 600)
            .await
            .unwrap();
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 2_000), 600)
            .await
            .unwrap();

        // Sweep with an injected scan returning one in-window commit.
        let inserted = run_commit_attribution_sweep(&conn, 600, |target| {
            assert_eq!(target.branch.as_deref(), Some("main"));
            assert_eq!(target.worktree, "/repo");
            vec![ScannedCommit {
                sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
                committed_at: 1_500,
            }]
        })
        .await
        .unwrap();
        assert_eq!(inserted, 1);

        // The commit is now queryable by prefix.
        let hits = sessions_for(
            &conn,
            &SessionsForQuery {
                git_ref: GitRefFilter::Commit("abcdef12".to_string()),
                since: None,
                until: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert_eq!(hits[0].span_overlap_kind, Some(SpanOverlapKind::WithinSpan));

        // Re-running the sweep is idempotent: even if the boundary span is
        // re-scanned (watermark uses `>=` so nothing is ever missed), the
        // commit is already attributed and the upsert inserts nothing more.
        let again = run_commit_attribution_sweep(&conn, 600, |_| {
            vec![ScannedCommit {
                sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
                committed_at: 1_500,
            }]
        })
        .await
        .unwrap();
        assert_eq!(again, 0, "re-attribution of the same commit is a no-op");
    }
}
