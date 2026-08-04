//! Session/git correlation index.
//!
//! Stores branch/worktree spans and commit attribution in the per-project
//! `sessions.db`, alongside `sessions`, `session_messages`, and LCM tables.
//! Sessions can switch branches or worktrees, so attribution is span-based:
//! repeated observations widen nearby spans, while branch switches or long
//! gaps open new spans.

use std::collections::HashSet;
use std::fmt::Write as _;

use libsql::{Connection, Value, params};
use serde::{Deserialize, Serialize};

use crate::SessionMessageRecord;

mod backfill;

pub use backfill::*;

/// Schema version recorded in `session_schema_migrations`.
pub const GIT_CORRELATION_SCHEMA_VERSION: i64 = 3;

const MIGRATION_NAME: &str = "git_correlation";

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

const MESSAGE_WORKTREE_KEYS: [&str; 9] = [
    "codex_turn_worktree",
    "claude_message_worktree",
    "cursor_event_worktree",
    "kiro_workspace_worktree",
    "cline_like_task_worktree",
    "vibe_session_worktree",
    "codex_session_worktree",
    "claude_session_worktree",
    "hermes_session_worktree",
];

/// Default gap (seconds) within which a new observation extends the newest
/// matching span instead of opening a new one. Tool-use events inside one
/// working stretch arrive far more often than this; a longer silence most
/// likely means the session went idle or moved elsewhere.
pub const DEFAULT_SPAN_MERGE_GAP_SECS: i64 = 30 * 60;

/// Hard cap on rows returned by [`sessions_for`].
pub const MAX_SESSIONS_FOR_LIMIT: usize = 100;

/// `git_correlation_meta` key holding the auto-backfill activity watermark:
/// the highest session-activity timestamp the incremental backfill has already
/// attempted. See [`run_incremental_backfill`].
pub const AUTO_BACKFILL_WATERMARK_KEY: &str = "auto_backfill_activity_watermark";

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
    /// Direct producer evidence, not a span/time inference.
    Direct,
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
            Self::Direct => "direct",
            Self::WithinSpan => "within_span",
            Self::ExtendedWindow => "extended_window",
            Self::Reflog => "reflog",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "within_span" => Some(Self::WithinSpan),
            "extended_window" => Some(Self::ExtendedWindow),
            "reflog" => Some(Self::Reflog),
            _ => None,
        }
    }
}

/// What a commit/session relationship actually proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitRelation {
    /// Direct evidence says this session created the commit.
    Produced,
    /// The session merely saw the commit or overlapped it in time.
    Observed,
}

impl CommitRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Observed => "observed",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "produced" => Some(Self::Produced),
            "observed" => Some(Self::Observed),
            _ => None,
        }
    }
}

/// Durable evidence class behind a commit relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitEvidence {
    /// Successful tool result containing the produced commit ref.
    ToolResult,
    /// Exact host-emitted commit event.
    HostEvent,
    /// The host reported this commit as current HEAD.
    HeadObservation,
    /// Reconstructed from reflog branch history plus a session window.
    ReflogOverlap,
    /// Inferred only from branch/worktree/time overlap.
    TimeOverlap,
}

impl CommitEvidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolResult => "tool_result",
            Self::HostEvent => "host_event",
            Self::HeadObservation => "head_observation",
            Self::ReflogOverlap => "reflog_overlap",
            Self::TimeOverlap => "time_overlap",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "tool_result" => Some(Self::ToolResult),
            "host_event" => Some(Self::HostEvent),
            "head_observation" => Some(Self::HeadObservation),
            "reflog_overlap" => Some(Self::ReflogOverlap),
            "time_overlap" => Some(Self::TimeOverlap),
            _ => None,
        }
    }
}

/// Relation selector for commit queries. Producer evidence is the safe default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommitRelationFilter {
    #[default]
    Produced,
    Observed,
    All,
}

impl CommitRelationFilter {
    pub fn parse(value: Option<&str>) -> Result<Self, GitCorrelationError> {
        match value.unwrap_or("produced") {
            "produced" => Ok(Self::Produced),
            "observed" => Ok(Self::Observed),
            "all" => Ok(Self::All),
            other => Err(GitCorrelationError::InvalidArgument(format!(
                "relation must be one of produced, observed, all (got `{other}`)"
            ))),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Observed => "observed",
            Self::All => "all",
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
    pub relation: CommitRelation,
    pub evidence: CommitEvidence,
    /// Evidence-class confidence on a fixed 0-100 scale.
    pub confidence: i64,
    /// Source message or host event that supplied direct evidence, when known.
    pub evidence_message_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "is_default")]
    pub event_count: i64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub span_count: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_overlap_kind: Option<SpanOverlapKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<CommitRelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CommitEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_message_id: Option<String>,
}

/// Lexically normalizes a worktree path for stable equality: trims
/// whitespace, converts backslashes to forward slashes, and strips trailing
/// slashes (keeping a lone `/`). Deliberately does **not** hit the
/// filesystem — writers should pass already-resolved worktree roots (e.g.
/// from [`crate::worktree::git_worktree_root`]); this keeps readers and
/// writers agreeing even when the path no longer exists.
pub fn normalize_worktree(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("//?/UNC/") {
        normalized = format!("//{stripped}");
    } else if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_string();
    }
    if let Some(stripped) = normalized.strip_prefix("/private/var/") {
        normalized = format!("/var/{stripped}");
    }
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

/// In-process rate limiter for live hook-route span observations.
#[derive(Debug, Default)]
pub struct SpanObservationDebounce {
    last_write: std::collections::HashMap<String, i64>,
}

/// Default minimum spacing between recorded hook-route observations for one key.
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
pub async fn ensure_git_correlation_schema(conn: &Connection) -> Result<(), GitCorrelationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        );",
    )
    .await?;
    let version = schema_version(conn).await?;
    if version.is_some_and(|version| version > GIT_CORRELATION_SCHEMA_VERSION) {
        return Err(GitCorrelationError::Db(format!(
            "database uses newer git correlation schema {} (this binary supports {})",
            version.unwrap_or_default(),
            GIT_CORRELATION_SCHEMA_VERSION
        )));
    }
    if version == Some(GIT_CORRELATION_SCHEMA_VERSION) {
        return Ok(());
    }
    let rebuild_commit_table = table_exists(conn, "commit_sessions").await?;

    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let migration = async {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_git_spans (
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
        CREATE TABLE IF NOT EXISTS git_correlation_meta (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );",
        )
        .await?;
        if rebuild_commit_table {
            conn.execute(
                "ALTER TABLE commit_sessions RENAME TO commit_sessions_legacy_v3",
                (),
            )
            .await?;
        }
        conn.execute_batch(
            "CREATE TABLE commit_sessions (
            commit_sha TEXT NOT NULL,
            provider TEXT NOT NULL DEFAULT '',
            session_id TEXT NOT NULL,
            branch TEXT,
            worktree TEXT,
            committed_at INTEGER NOT NULL,
            span_overlap_kind TEXT NOT NULL
                CHECK(span_overlap_kind IN ('direct', 'within_span', 'extended_window', 'reflog')),
            span_id INTEGER,
            relation TEXT NOT NULL DEFAULT 'observed'
                CHECK(relation IN ('produced', 'observed')),
            evidence TEXT NOT NULL DEFAULT 'time_overlap'
                CHECK(evidence IN ('tool_result', 'host_event', 'head_observation', 'reflog_overlap', 'time_overlap')),
            confidence INTEGER NOT NULL DEFAULT 20
                CHECK(confidence BETWEEN 0 AND 100),
            evidence_message_id TEXT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(commit_sha, provider, session_id)
        );",
        )
        .await?;
        if rebuild_commit_table {
            conn.execute(
                "INSERT INTO commit_sessions (
                    commit_sha, provider, session_id, branch, worktree,
                    committed_at, span_overlap_kind, span_id,
                    relation, evidence, confidence, evidence_message_id, created_at
                 )
                 SELECT commit_sha, provider, session_id, branch, worktree,
                    committed_at, span_overlap_kind, span_id,
                    'observed',
                    CASE WHEN span_overlap_kind = 'reflog'
                         THEN 'reflog_overlap' ELSE 'time_overlap' END,
                    CASE WHEN span_overlap_kind = 'reflog' THEN 30 ELSE 20 END,
                    NULL, created_at
                 FROM commit_sessions_legacy_v3",
                (),
            )
            .await?;
            conn.execute("DROP TABLE commit_sessions_legacy_v3", ())
                .await?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_commit_sessions_session
                ON commit_sessions(provider, session_id, committed_at);
             CREATE INDEX IF NOT EXISTS idx_commit_sessions_branch
                ON commit_sessions(branch, committed_at);",
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
        Ok::<(), GitCorrelationError>(())
    }
    .await;
    match migration {
        Ok(()) => {
            if let Err(err) = conn.execute("COMMIT", ()).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err.into())
            } else {
                Ok(())
            }
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(err)
        }
    }
}

async fn schema_version(conn: &Connection) -> Result<Option<i64>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![MIGRATION_NAME],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| row.get(0).map_err(GitCorrelationError::from))
        .transpose()
}

async fn table_exists(conn: &Connection, table: &str) -> Result<bool, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
        )
        .await?;
    Ok(rows
        .next()
        .await?
        .is_some_and(|row| row.get::<i64>(0).ok() == Some(1)))
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::Text(text.to_string()))
}

/// Resolves provider-reported commit candidates against the repository and
/// turns them into durable producer evidence. Ambiguous, missing, or non-commit
/// object ids are ignored; transcript ingest can safely retry them later.
pub fn direct_commit_records(
    messages: &[SessionMessageRecord],
    project_root: &std::path::Path,
) -> Vec<CommitSessionRecord> {
    if !messages.iter().any(|message| {
        message.metadata_json.as_deref().is_some_and(|json| {
            json.contains("\"produced_commit_candidates\"")
                || json.contains("\"observed_commit_candidates\"")
        })
    }) {
        return Vec::new();
    }
    let Ok(repo) = gix::discover(project_root) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    // Producer evidence is collected first so it always claims the
    // (sha, provider, session) slot ahead of a weaker head observation of the
    // same commit made by the same session.
    for kind in [DirectEvidenceKind::Produced, DirectEvidenceKind::Observed] {
        for message in messages {
            let Some(metadata_value) = message
                .metadata_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            else {
                continue;
            };
            let Some(metadata) = metadata_value.as_object() else {
                continue;
            };
            let Some(candidates) = metadata
                .get(kind.metadata_key())
                .and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            for candidate in candidates.iter().filter_map(serde_json::Value::as_str) {
                if !(7..=64).contains(&candidate.len())
                    || !candidate.chars().all(|ch| ch.is_ascii_hexdigit())
                {
                    continue;
                }
                let Ok(spec) = repo.rev_parse_single(candidate) else {
                    continue;
                };
                let Ok(object) = spec.object() else {
                    continue;
                };
                let Ok(commit) = object.try_into_commit() else {
                    continue;
                };
                let sha = commit.id.to_string();
                if !seen.insert((
                    sha.clone(),
                    message.provider.clone(),
                    message.session_id.clone(),
                )) {
                    continue;
                }
                let worktree = metadata_worktree(metadata)
                    .map(normalize_worktree)
                    .or_else(|| Some(normalize_worktree(&project_root.to_string_lossy())));
                let branch = metadata
                    .get("git_branch")
                    .or_else(|| metadata.get("codex_git_branch"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let committed_at = commit.time().ok().map_or_else(
                    || message.timestamp.unwrap_or_default(),
                    |time| time.seconds,
                );
                let (relation, evidence, confidence) = match kind {
                    DirectEvidenceKind::Produced => {
                        let evidence = match metadata
                            .get("produced_commit_evidence")
                            .and_then(serde_json::Value::as_str)
                        {
                            Some("host_event") => CommitEvidence::HostEvent,
                            _ => CommitEvidence::ToolResult,
                        };
                        (CommitRelation::Produced, evidence, 100)
                    }
                    // A printed HEAD proves the session saw the commit, not that
                    // it made it: observed relation, sub-100 head-observation.
                    DirectEvidenceKind::Observed => (
                        CommitRelation::Observed,
                        CommitEvidence::HeadObservation,
                        HEAD_OBSERVATION_CONFIDENCE,
                    ),
                };
                records.push(CommitSessionRecord {
                    commit_sha: sha,
                    provider: message.provider.clone(),
                    session_id: message.session_id.clone(),
                    branch,
                    worktree,
                    committed_at,
                    span_overlap_kind: SpanOverlapKind::Direct,
                    span_id: None,
                    relation,
                    evidence,
                    confidence,
                    evidence_message_id: Some(message.message_id.clone()),
                });
            }
        }
    }
    records
}

/// Confidence for a commit a session printed as current HEAD: stronger than a
/// pure time-overlap guess, well below direct producer evidence.
const HEAD_OBSERVATION_CONFIDENCE: i64 = 60;

/// Which direct-evidence candidate list a `direct_commit_records` pass reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectEvidenceKind {
    Produced,
    Observed,
}

impl DirectEvidenceKind {
    const fn metadata_key(self) -> &'static str {
        match self {
            Self::Produced => "produced_commit_candidates",
            Self::Observed => "observed_commit_candidates",
        }
    }
}

/// Derives durable branch/worktree observations from provider message
/// metadata. These rows survive worktree deletion and make transcript ingest,
/// rather than a live hook, the source of truth for historical locations.
pub fn ingest_span_observations(messages: &[SessionMessageRecord]) -> Vec<SpanObservation> {
    let mut observations = Vec::new();
    for message in messages {
        let Some(ts) = message.timestamp else {
            continue;
        };
        let Some(json) = message.metadata_json.as_deref() else {
            continue;
        };
        if !json.contains("_worktree\"") {
            continue;
        }
        let Some(metadata_value) = serde_json::from_str::<serde_json::Value>(json).ok() else {
            continue;
        };
        let Some(metadata) = metadata_value.as_object() else {
            continue;
        };
        let Some(worktree) = metadata_worktree(metadata).filter(|path| !path.is_empty()) else {
            continue;
        };
        let branch = metadata
            .get("git_branch")
            .or_else(|| metadata.get("codex_git_branch"))
            .and_then(serde_json::Value::as_str)
            .filter(|branch| !branch.is_empty())
            .map(str::to_string);
        let thread_id = metadata
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        observations.push(SpanObservation {
            provider: message.provider.clone(),
            session_id: message.session_id.clone(),
            thread_id,
            branch,
            worktree: normalize_worktree(worktree),
            ts,
            source: SpanSource::Ingest,
        });
    }
    observations
}

fn metadata_worktree(metadata: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    MESSAGE_WORKTREE_KEYS
        .into_iter()
        .find_map(|key| metadata.get(key).and_then(serde_json::Value::as_str))
}

/// Folds one observation into the span table: extends the newest span for
/// the same (provider, session, branch, worktree) when the observation lands
/// within `merge_gap_secs` of it, otherwise inserts a new span. Returns the
/// affected `span_id`.
///
/// Runs in a `BEGIN IMMEDIATE` transaction so concurrent writers converge on
/// widened spans instead of interleaved half-updates.
pub async fn record_span_observation(
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

pub async fn record_span_observation_in_transaction(
    conn: &Connection,
    observation: &SpanObservation,
    merge_gap_secs: i64,
) -> Result<i64, GitCorrelationError> {
    let worktree = normalize_worktree(&observation.worktree);
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
                worktree.as_str(),
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
            worktree.as_str(),
            observation.ts,
            observation.source.as_str(),
        ],
    )
    .await?;
    Ok(conn.last_insert_rowid())
}

/// Inserts one commit attribution row. Stronger evidence replaces weaker
/// evidence; identical or weaker replays are no-ops. Returns `true` when the
/// row was inserted or strengthened.
pub async fn upsert_commit_session(
    conn: &Connection,
    record: &CommitSessionRecord,
) -> Result<bool, GitCorrelationError> {
    let worktree = record.worktree.as_deref().map(normalize_worktree);
    let inserted = conn
        .execute(
            "INSERT INTO commit_sessions (
                commit_sha, provider, session_id, branch, worktree,
                committed_at, span_overlap_kind, span_id,
                relation, evidence, confidence, evidence_message_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(commit_sha, provider, session_id) DO UPDATE SET
                branch = excluded.branch,
                worktree = excluded.worktree,
                committed_at = excluded.committed_at,
                span_overlap_kind = excluded.span_overlap_kind,
                span_id = excluded.span_id,
                relation = excluded.relation,
                evidence = excluded.evidence,
                confidence = excluded.confidence,
                evidence_message_id = excluded.evidence_message_id
             WHERE (excluded.relation = 'produced' AND commit_sessions.relation != 'produced')
                OR (excluded.relation = commit_sessions.relation
                    AND excluded.confidence > commit_sessions.confidence)",
            params![
                record.commit_sha.as_str(),
                record.provider.as_str(),
                record.session_id.as_str(),
                opt_text(record.branch.as_deref()),
                opt_text(worktree.as_deref()),
                record.committed_at,
                record.span_overlap_kind.as_str(),
                record.span_id.map_or(Value::Null, Value::Integer),
                record.relation.as_str(),
                record.evidence.as_str(),
                record.confidence,
                opt_text(record.evidence_message_id.as_deref()),
            ],
        )
        .await?;
    Ok(inserted > 0)
}

mod attribution;
pub use attribution::{
    ScannedCommit, SpanScanTarget, SpanWindow, commit_overlap_kind, match_commit_to_spans,
};
pub use attribution::{read_meta_value, run_commit_attribution_sweep, write_meta_value};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

/// Returns sessions correlated with a branch, worktree, or commit, most
/// recently active first. Branch/worktree queries aggregate span rows per
/// session; commit queries return attribution rows (abbreviated shas match
/// by prefix). `since`/`until` bound span overlap (branch/worktree) or
/// commit time (commit).
pub async fn sessions_for(
    conn: &Connection,
    query: &SessionsForQuery,
) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
    sessions_for_with_relation(conn, query, CommitRelationFilter::Produced).await
}

pub async fn sessions_for_with_relation(
    conn: &Connection,
    query: &SessionsForQuery,
    relation: CommitRelationFilter,
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
        GitRefFilter::Commit(sha) => commit_hits(conn, sha, query, relation, limit).await,
    }
}

/// Resolves the `(provider, session_id)` pairs matching all present git filters.
pub async fn session_ids_for_scope(
    conn: &Connection,
    filter: &GitScopeFilter,
) -> Result<Option<Vec<(String, String)>>, GitCorrelationError> {
    if filter.is_empty() {
        return Ok(None);
    }
    if !correlation_tables_present(conn).await? {
        return Ok(Some(Vec::new()));
    }
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
        Some(existing) => {
            let next: HashSet<_> = next.into_iter().collect();
            existing
                .into_iter()
                .filter(|pair| next.contains(pair))
                .collect()
        }
    }
}

async fn span_session_ids(
    conn: &Connection,
    ref_predicate: &str,
    ref_value: Value,
) -> Result<Vec<(String, String)>, GitCorrelationError> {
    // Canonicalize to one identity per session (see `span_hits`): `MAX(provider)`
    // collapses the hook-route (`provider ''`) and ingest rows so scope
    // intersection compares matching `(provider, session_id)` pairs.
    let sql = format!(
        "SELECT MAX(provider), session_id FROM session_git_spans \
         WHERE {ref_predicate} GROUP BY session_id"
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
    // Prefer producer evidence, but fall back to every session correlated with
    // the commit when no producer row exists. A store upgraded from schema v2
    // whose transcripts were later pruned keeps only observed/overlap rows
    // (they exist precisely to survive worktree deletion); a hard
    // `relation = 'produced'` filter would drop them and make the commit look
    // untouched forever. `MAX(provider)` collapses the hook-route (`provider
    // ''`) and ingest identities of one session into a single row.
    let mut rows = conn
        .query(
            "SELECT MAX(provider), session_id FROM commit_sessions c
             WHERE (commit_sha = ?1 OR commit_sha LIKE ?2)
               AND (c.relation = 'produced'
                    OR NOT EXISTS (
                        SELECT 1 FROM commit_sessions p
                        WHERE (p.commit_sha = ?1 OR p.commit_sha LIKE ?2)
                          AND p.relation = 'produced'))
             GROUP BY session_id",
            params![sha, format!("{sha}%")],
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push((row.get(0)?, row.get(1)?));
    }
    Ok(ids)
}

/// Individual EXISTS clauses for git-scope filters, each with its bound
/// values. Callers combine with ` AND ` (message search) or ` OR ` (workflow
/// runs on a git ref).
///
/// Span rows may carry `provider = ''` (raw hook routes are provider-agnostic),
/// so scoping matches on `session_id` alone rather than also constraining the
/// provider.
pub fn git_scope_exists_clauses(
    filter: &GitScopeFilter,
    session_column: &str,
) -> Vec<(String, Vec<Value>)> {
    let mut clauses = Vec::new();
    if let Some(branch) = &filter.branch {
        clauses.push((
            format!(
                "EXISTS (SELECT 1 FROM session_git_spans g \
                 WHERE g.session_id = {session_column} AND g.branch = ?)"
            ),
            vec![Value::Text(branch.clone())],
        ));
    }
    if let Some(worktree) = &filter.worktree {
        clauses.push((
            format!(
                "EXISTS (SELECT 1 FROM session_git_spans g \
                 WHERE g.session_id = {session_column} AND g.worktree = ?)"
            ),
            vec![Value::Text(worktree.clone())],
        ));
    }
    if let Some(commit) = &filter.commit {
        // Prefer producer evidence, but fall back to any correlation when no
        // producer row exists for the commit (see `commit_session_ids`): a
        // pruned v2-upgraded store keeps only observed rows, and dropping them
        // would erase the commit scope entirely.
        let pattern = format!("{commit}%");
        clauses.push((
            format!(
                "EXISTS (SELECT 1 FROM commit_sessions c \
                 WHERE c.session_id = {session_column} \
                 AND (c.commit_sha = ? OR c.commit_sha LIKE ?) \
                 AND (c.relation = 'produced' \
                      OR NOT EXISTS (SELECT 1 FROM commit_sessions p \
                                     WHERE (p.commit_sha = ? OR p.commit_sha LIKE ?) \
                                       AND p.relation = 'produced')))"
            ),
            vec![
                Value::Text(commit.clone()),
                Value::Text(pattern.clone()),
                Value::Text(commit.clone()),
                Value::Text(pattern),
            ],
        ));
    }
    clauses
}

/// One AND-combined EXISTS predicate plus bound values for a git-scope
/// constraint, correlated to an outer row via `session_column` (e.g.
/// `m.session_id`). Returns `None` when the filter is empty.
pub fn git_scope_exists_predicate(
    filter: &GitScopeFilter,
    session_column: &str,
) -> Option<(String, Vec<Value>)> {
    let clauses = git_scope_exists_clauses(filter, session_column);
    if clauses.is_empty() {
        return None;
    }
    let sql = clauses
        .iter()
        .map(|(clause, _)| clause.as_str())
        .collect::<Vec<_>>()
        .join(" AND ");
    let values = clauses.into_iter().flat_map(|(_, values)| values).collect();
    Some((sql, values))
}

/// True when the git-correlation tables exist in `conn`'s database. Search
/// paths use this to short-circuit git-scoped queries against stores predating
/// the git-correlation schema (returning empty rather than a `no such table`
/// error).
pub async fn tables_present(conn: &Connection) -> Result<bool, GitCorrelationError> {
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

/// Per-project health of the session↔git correlation index. Surfaced by
/// diagnostics and by [`sessions_for`]'s empty-result path so an empty index is
/// never mistaken for "no sessions matched".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorrelationIndexHealth {
    /// Whether the `session_git_spans` / `commit_sessions` tables exist. A
    /// read-only store written before the correlation schema shipped has none.
    pub tables_present: bool,
    /// Rows in `session_git_spans`. Zero means the index was never populated.
    pub span_count: i64,
    /// Rows in `commit_sessions`.
    pub commit_count: i64,
    /// Newest `session_git_spans.updated_at`, or `None` when empty.
    pub last_span_write: Option<i64>,
    /// The auto-backfill activity watermark, or `None` when a pass never ran.
    pub backfill_watermark: Option<i64>,
}

impl CorrelationIndexHealth {
    /// True when the correlation index holds no spans — either the tables are
    /// missing or no observation/backfill ever wrote a row. Distinct from a
    /// populated index that simply had no rows matching a given git ref.
    pub const fn is_empty(&self) -> bool {
        self.span_count == 0
    }

    /// Whether the index lacks the row family needed by this reference kind.
    pub const fn is_empty_for(&self, git_ref: &GitRefFilter) -> bool {
        match git_ref {
            GitRefFilter::Branch(_) | GitRefFilter::Worktree(_) => self.span_count == 0,
            GitRefFilter::Commit(_) => self.commit_count == 0,
        }
    }
}

/// Reads the correlation index health for a project store. Cheap: two counts
/// plus a metadata lookup. Never runs DDL, so a store predating the schema
/// reports `tables_present = false` with zero counts rather than erroring.
pub async fn correlation_index_health(
    conn: &Connection,
) -> Result<CorrelationIndexHealth, GitCorrelationError> {
    if !correlation_tables_present(conn).await? {
        return Ok(CorrelationIndexHealth {
            tables_present: false,
            span_count: 0,
            commit_count: 0,
            last_span_write: None,
            backfill_watermark: None,
        });
    }
    let mut span_rows = conn
        .query(
            "SELECT COUNT(*), MAX(updated_at) FROM session_git_spans",
            (),
        )
        .await?;
    let (span_count, last_span_write) = match span_rows.next().await? {
        Some(row) => (row.get::<i64>(0)?, row.get::<Option<i64>>(1)?),
        None => (0, None),
    };
    let mut commit_rows = conn
        .query("SELECT COUNT(*) FROM commit_sessions", ())
        .await?;
    let commit_count = match commit_rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    };
    let backfill_watermark = read_meta_value(conn, AUTO_BACKFILL_WATERMARK_KEY).await?;
    Ok(CorrelationIndexHealth {
        tables_present: true,
        span_count,
        commit_count,
        last_span_write,
        backfill_watermark,
    })
}

async fn span_hits(
    conn: &Connection,
    ref_predicate: &str,
    ref_value: Value,
    query: &SessionsForQuery,
    limit: i64,
) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
    // Group by `session_id` alone, not `(provider, session_id)`: hook-route
    // spans store `provider = ''` while transcript ingest stores the real
    // provider, so keying on both splits one session into two rows with its
    // event/span counts divided between them. `MAX(provider)` picks the real
    // (non-empty) provider as the session's single canonical identity.
    let mut sql = format!(
        "SELECT MAX(provider), session_id,
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
        " GROUP BY session_id
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
            relation: None,
            evidence: None,
            confidence: None,
            evidence_message_id: None,
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
    relation: CommitRelationFilter,
    limit: i64,
) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
    let mut sql = "SELECT provider, session_id, branch, worktree,
                commit_sha, committed_at, span_overlap_kind,
                relation, evidence, confidence, evidence_message_id
         FROM commit_sessions
         WHERE (commit_sha = ?1 OR commit_sha LIKE ?2)"
        .to_string();
    // `parse_commit_sha` guarantees hex-only input, so the LIKE pattern
    // cannot contain wildcards other than the appended one.
    let mut query_params = vec![Value::Text(sha.to_string()), Value::Text(format!("{sha}%"))];
    if relation != CommitRelationFilter::All {
        query_params.push(Value::Text(relation.as_str().to_string()));
        let _ = write!(sql, " AND relation = ?{}", query_params.len());
    }
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
    // One session can hold two rows for the same commit — a hook-route
    // observation (`provider ''`) and an ingest/producer row — because the
    // primary key includes provider. Collapse them into a single canonical hit
    // per session, keeping the strongest evidence and the real provider, so a
    // session is never double-counted for one commit.
    let mut order: Vec<String> = Vec::new();
    let mut by_session: std::collections::HashMap<String, SessionGitCorrelationHit> =
        std::collections::HashMap::new();
    while let Some(row) = rows.next().await? {
        let overlap: String = row.get(6)?;
        let relation: String = row.get(7)?;
        let evidence: String = row.get(8)?;
        let candidate = SessionGitCorrelationHit {
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
            relation: CommitRelation::from_db(&relation),
            evidence: CommitEvidence::from_db(&evidence),
            confidence: row.get(9)?,
            evidence_message_id: row.get(10)?,
        };
        if let Some(existing) = by_session.get_mut(&candidate.session_id) {
            merge_commit_hit(existing, candidate);
        } else {
            order.push(candidate.session_id.clone());
            by_session.insert(candidate.session_id.clone(), candidate);
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|session_id| by_session.remove(&session_id))
        .collect())
}

/// Folds a second commit hit for the same session into `existing`, keeping the
/// stronger evidence and preferring a non-empty (real) provider.
fn merge_commit_hit(existing: &mut SessionGitCorrelationHit, candidate: SessionGitCorrelationHit) {
    if existing.provider.is_empty() && !candidate.provider.is_empty() {
        existing.provider.clone_from(&candidate.provider);
    }
    if commit_hit_strength(&candidate) > commit_hit_strength(existing) {
        let provider = if candidate.provider.is_empty() {
            existing.provider.clone()
        } else {
            candidate.provider.clone()
        };
        *existing = SessionGitCorrelationHit {
            provider,
            ..candidate
        };
    }
}

/// Ranks a commit hit so producer evidence beats observation, breaking ties on
/// confidence. Used to pick one canonical row per session.
fn commit_hit_strength(hit: &SessionGitCorrelationHit) -> (u8, i64) {
    let relation_rank = match hit.relation {
        Some(CommitRelation::Produced) => 2,
        Some(CommitRelation::Observed) => 1,
        None => 0,
    };
    (relation_rank, hit.confidence.unwrap_or(0))
}
