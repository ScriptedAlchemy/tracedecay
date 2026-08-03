//! User-level database that tracks all `TraceDecay` projects and their saved tokens.
//!
//! Stored at `~/.tracedecay/global.db`, this DB holds one row per project with
//! the project's DB path and its cumulative
//! tokens-saved count. All operations are best-effort: failures are silently
//! ignored so they never block the main MCP server loop.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use libsql::{Builder, Connection, Database as LibsqlDatabase, OpenFlags, Value, params};
use serde_json::Value as JsonValue;

use crate::db::DatabaseAuthority;
use crate::sessions::{
    SessionMessageRecord, SessionMessageSearchResult, SessionRecord, SessionSearchFilters,
    lcm::{
        LcmSourceRef, LcmSummaryNode, LcmSummaryNodeDraft, LcmSummaryRequest,
        LcmSummarySourceMessage, LcmSummarySourceRange,
    },
};

const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

pub use tracedecay_sessions::runtime::workflow_index::WorkflowScopeFilter;

/// Total savings + call count for a project (or all projects when `project` is None).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsTotal {
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsDay {
    /// Start-of-day epoch seconds (UTC).
    pub day: i64,
    pub saved_tokens: u64,
    pub calls: u64,
}

/// One freshly computed token count headed for the dashboard sidecar cache
/// (see [`GlobalDb::save_token_counts`]).
#[derive(Debug, Clone)]
pub struct TokenCountUpsert {
    pub provider: String,
    pub message_id: String,
    pub text_len: i64,
    pub encoder: &'static str,
    pub token_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventInsert {
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventRecord {
    pub id: i64,
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsToolCounts {
    pub tool_name: String,
    pub calls: i64,
    pub errors: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsHintCounts {
    pub category: String,
    pub emitted: i64,
    pub followed: i64,
    pub ignored: i64,
    pub suppressed: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToolUsageRow {
    pub tool_names: String,
    pub text: String,
    pub metadata_json: String,
}

/// One ingested session message, projected to the fields the hint-outcome
/// correlator needs: the timestamp/ordinal that order activity after a hint and
/// the tool-activity carriers (`kind='tool_event'` + `tool_names` for Codex,
/// `tool_names`/`metadata_json.tool_events` for Claude/Cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionActivityRow {
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub kind: Option<String>,
    pub tool_names: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnalyticsEventQuery {
    pub provider: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub event_kind: Option<String>,
    /// Inclusive lower bound on `timestamp` (unix seconds). `None` = unbounded.
    pub since: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingCodexCompactionSummary {
    pub node_id: String,
    pub request: LcmSummaryRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeProjectRecord {
    pub project_id: String,
    pub canonical_root: String,
    pub display_root: String,
    pub git_common_dir: Option<String>,
    pub git_remote_url: Option<String>,
    pub default_branch: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectAliasRecord {
    pub alias_path: String,
    pub project_id: String,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreInstanceUpsert {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreInstanceRecord {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub created_at: i64,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphScopeUpsert {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphScopeRecord {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreArtifactUpsert {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreArtifactRecord {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectStoreResolution {
    pub project: CodeProjectRecord,
    pub store: StoreInstanceRecord,
    pub graph_scopes: Vec<GraphScopeRecord>,
    pub artifacts: Vec<StoreArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectStoreContext {
    pub store: StoreInstanceRecord,
    pub graph_scopes: Vec<GraphScopeRecord>,
    pub artifacts: Vec<StoreArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectRegistryContext {
    pub project: CodeProjectRecord,
    pub aliases: Vec<ProjectAliasRecord>,
    pub stores: Vec<ProjectStoreContext>,
}

/// Transcript-ingest backlog snapshot for a session store. See
/// [`GlobalDb::session_ingest_health`].
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SessionIngestHealth {
    /// Transcripts referenced by sessions that still exist on disk.
    pub tracked_transcripts: u64,
    /// Transcripts with un-ingested appended bytes.
    pub pending_transcripts: u64,
    /// Total un-ingested bytes across pending transcripts.
    pub pending_bytes: u64,
    /// Largest single-transcript backlog. The hook ingest caps are
    /// per-transcript, so this (not the total) decides whether the hooks can
    /// still drain the backlog on their own.
    pub max_transcript_pending_bytes: u64,
    /// Newest transcript mtime recorded at ingest time (Unix seconds).
    pub last_ingest_unix: Option<i64>,
}

/// Persisted parse cursor for a transcript path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ParseOffset {
    pub byte_offset: u64,
    pub mtime: u64,
    pub file_id: u64,
}

pub use tracedecay_sessions::runtime::hermes::TranscriptBatch;

/// Whether a transcript batch writes the full dual store (LCM raw + searchable
/// projection) or only the `session_messages` projection.
#[derive(Debug, Clone, Copy)]
enum TranscriptWriteMode {
    Full,
    ProjectionOnly,
}

/// User-level database tracking all `TraceDecay` projects.
pub struct GlobalDb {
    inner: Arc<GlobalDbInner>,
}

#[doc(hidden)]
pub struct GlobalDbInner {
    conn: Connection,
    storage_root: PathBuf,
    db_path: PathBuf,
    _db: LibsqlDatabase,
    _authority: DatabaseAuthority,
    _slot: Option<GlobalDbSlot>,
}

impl Deref for GlobalDb {
    type Target = GlobalDbInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Default)]
struct GlobalDbSchemaState {
    ensured: bool,
}

type GlobalDbSlot = Arc<tokio::sync::Mutex<GlobalDbSchemaState>>;

static GLOBAL_DB_SLOTS: LazyLock<
    std::sync::Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<GlobalDbSchemaState>>>>,
> = LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn global_db_slot(authority: &DatabaseAuthority) -> GlobalDbSlot {
    let identity = authority.canonical_database_path();
    let mut slots = GLOBAL_DB_SLOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slots.retain(|_, slot| slot.strong_count() > 0);
    if let Some(slot) = slots.get(identity).and_then(Weak::upgrade) {
        return slot;
    }
    let slot = Arc::new(tokio::sync::Mutex::new(GlobalDbSchemaState::default()));
    slots.insert(identity.to_path_buf(), Arc::downgrade(&slot));
    slot
}

struct TranscriptSummarySources {
    refs: Vec<LcmSourceRef>,
    source_token_count: i64,
    source_time_start: Option<i64>,
    source_time_end: Option<i64>,
    excerpts: Vec<TranscriptSummaryExcerpt>,
}

struct TranscriptSummaryExcerpt {
    role: String,
    text: String,
}

const CODEX_COMPACTION_SUMMARY_PROMPT: &str = concat!(
    "Summarize the visible transcript messages that Codex compacted. ",
    "Preserve durable user intent, implementation decisions, file/module names, ",
    "unresolved tasks, and verification status. Return only the summary text."
);

const GLOBAL_DB_PATH_ENV: &str = "TRACEDECAY_GLOBAL_DB";

fn global_db_path_override() -> Option<PathBuf> {
    std::env::var_os(GLOBAL_DB_PATH_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn global_db_mmap_size_guard() -> u64 {
    0
}

/// Returns the path to the global database: `global.db` inside the user-level
/// data dir (`~/.tracedecay/` by default).
pub fn global_db_path() -> Option<PathBuf> {
    if let Some(path) = global_db_path_override() {
        return Some(path);
    }
    crate::config::user_data_dir().map(|dir| dir.join("global.db"))
}

/// True when `TRACEDECAY_GLOBAL_DB` pins the global DB to an explicit path.
/// Consumers treat the override as an operator decision that wins over project
/// store discovery.
pub fn global_db_path_is_overridden() -> bool {
    global_db_path_override().is_some()
}

/// How [`global_accounting_enabled`] reached its decision; the dashboard
/// surfaces this so an empty ledger can be explained honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountingMode {
    /// No env override — global accounting is on by default.
    Default,
    /// `TRACEDECAY_ENABLE_GLOBAL_DB` explicitly enabled it.
    EnabledByEnv,
    /// `TRACEDECAY_ENABLE_GLOBAL_DB` (falsy value) or
    /// `TRACEDECAY_DISABLE_GLOBAL_DB` explicitly disabled it.
    DisabledByEnv,
}

impl AccountingMode {
    pub fn enabled(self) -> bool {
        !matches!(self, Self::DisabledByEnv)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::EnabledByEnv => "enabled_by_env",
            Self::DisabledByEnv => "disabled_by_env",
        }
    }
}

/// Canonical truthy-env-value test shared by every boolean env flag: trims,
/// case-folds, and accepts `1`/`true`/`yes`/`on`. (Two parsers used to
/// coexist with diverging semantics — e.g. `TRACEDECAY_DISABLE_GLOBAL_DB=on`
/// was silently ignored while the LCM doctor flag honored it.)
pub fn env_value_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// True when the named env var is set to a truthy value.
pub fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| env_value_truthy(&value))
}

/// Whether user-level global accounting (the cross-project `savings_ledger`
/// plus worldwide-counter flushes in the MCP server) is enabled.
///
/// Enabled **by default**: every other writer of the user-level `global.db`
/// (CLI sync, hooks, `tracedecay cost`, the dashboard) is ungated, and the
/// Savings dashboard reads the ledger — an opt-in gate here silently left
/// the ledger empty while lifetime counters kept growing. Precedence:
///
/// 1. `TRACEDECAY_ENABLE_GLOBAL_DB` set → its truthiness decides.
/// 2. `TRACEDECAY_DISABLE_GLOBAL_DB` truthy → disabled.
/// 3. Otherwise → enabled.
pub fn global_accounting_mode() -> AccountingMode {
    if let Some(value) = crate::config::brand_env("ENABLE_GLOBAL_DB") {
        return if env_value_truthy(&value) {
            AccountingMode::EnabledByEnv
        } else {
            AccountingMode::DisabledByEnv
        };
    }
    if crate::config::brand_env("DISABLE_GLOBAL_DB").is_some_and(|value| env_value_truthy(&value)) {
        return AccountingMode::DisabledByEnv;
    }
    AccountingMode::Default
}

/// Convenience wrapper over [`global_accounting_mode`].
pub fn global_accounting_enabled() -> bool {
    global_accounting_mode().enabled()
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

fn estimated_tokens_from_chars(char_count: i64) -> i64 {
    ((char_count.max(0) + 3) / 4).max(1)
}

fn estimate_summary_tokens(text: &str) -> i64 {
    i64::from(crate::context::read_modes::estimate_tokens(text))
}

fn transcript_summary_text(
    message: &SessionMessageRecord,
    metadata: &JsonValue,
    sources: &TranscriptSummarySources,
) -> String {
    if metadata.get("summary_body").and_then(JsonValue::as_str) == Some("plaintext") {
        return message.text.clone();
    }
    let Some(source_summary) = extractive_transcript_summary(&sources.excerpts) else {
        return message.text.clone();
    };
    let codex_body = metadata
        .get("summary_body")
        .and_then(JsonValue::as_str)
        .unwrap_or("unavailable");
    format!(
        "TraceDecay-generated Codex compaction summary from visible transcript messages. Codex's own compaction body is {codex_body} in the rollout.\n\n{source_summary}"
    )
}

fn extractive_transcript_summary(excerpts: &[TranscriptSummaryExcerpt]) -> Option<String> {
    let meaningful = excerpts
        .iter()
        .filter_map(|excerpt| {
            let text = normalize_summary_excerpt(&excerpt.text);
            if text.is_empty() {
                None
            } else {
                Some((&excerpt.role, text))
            }
        })
        .collect::<Vec<_>>();
    if meaningful.is_empty() {
        return None;
    }

    let mut selected = Vec::new();
    if meaningful.len() <= 12 {
        selected.extend(meaningful.iter());
    } else {
        selected.extend(meaningful.iter().take(4));
        selected.extend(meaningful.iter().skip(meaningful.len().saturating_sub(8)));
    }

    let mut summary = String::from("Visible source highlights:");
    for (role, text) in selected {
        let role = role.trim();
        let role = if role.is_empty() { "unknown" } else { role };
        let line = truncate_summary_excerpt(text, 320);
        let _ = write!(summary, "\n- {role}: {line}");
    }
    Some(summary)
}

fn normalize_summary_excerpt(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_summary_excerpt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", text.chars().take(keep).collect::<String>())
}

fn row_to_session(row: &libsql::Row) -> Option<SessionRecord> {
    Some(SessionRecord {
        provider: row.get(0).ok()?,
        session_id: row.get(1).ok()?,
        project_key: row.get(2).ok()?,
        project_path: row.get(3).ok()?,
        title: row.get(4).ok()?,
        started_at: row.get(5).ok()?,
        ended_at: row.get(6).ok()?,
        transcript_path: row.get(7).ok()?,
        metadata_json: row.get(8).ok()?,
        parent_session_id: row.get(9).ok()?,
        is_subagent: row.get::<i64>(10).unwrap_or(0) != 0,
        agent_id: row.get(11).ok()?,
        parent_tool_use_id: row.get(12).ok()?,
    })
}

fn row_to_message(row: &libsql::Row, offset: i32) -> Option<SessionMessageRecord> {
    Some(SessionMessageRecord {
        provider: row.get(offset).ok()?,
        message_id: row.get(offset + 1).ok()?,
        session_id: row.get(offset + 2).ok()?,
        role: row.get(offset + 3).ok()?,
        timestamp: row.get(offset + 4).ok()?,
        ordinal: row.get(offset + 5).ok()?,
        text: row.get(offset + 6).ok()?,
        kind: row.get(offset + 7).ok()?,
        model: row.get(offset + 8).ok()?,
        tool_names: row.get(offset + 9).ok()?,
        source_path: row.get(offset + 10).ok()?,
        source_offset: row.get(offset + 11).ok()?,
        metadata_json: row.get(offset + 12).ok()?,
    })
}

fn row_to_analytics_event(row: &libsql::Row) -> Option<AnalyticsEventRecord> {
    Some(AnalyticsEventRecord {
        id: row.get(0).ok()?,
        provider: row.get(1).ok()?,
        project_id: row.get(2).ok()?,
        session_id: row.get(3).ok()?,
        timestamp: row.get(4).ok()?,
        event_kind: row.get(5).ok()?,
        hook_name: row.get(6).ok()?,
        tool_name: row.get(7).ok()?,
        tool_category: row.get(8).ok()?,
        skill_name: row.get(9).ok()?,
        hint_category: row.get(10).ok()?,
        hint_id: row.get(11).ok()?,
        outcome: row.get(12).ok()?,
        metadata_json: row.get(13).ok()?,
    })
}

fn push_optional_analytics_filter(
    clauses: &mut Vec<String>,
    values: &mut Vec<Value>,
    column: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        values.push(Value::Text(value.to_string()));
        clauses.push(format!("{column} = ?{}", values.len()));
    }
}

fn analytics_scope_query(
    select: &str,
    project_id: Option<&str>,
    since: i64,
    fixed_clauses: &[&str],
) -> (String, Vec<Value>) {
    let mut sql = select.to_string();
    let mut clauses = fixed_clauses
        .iter()
        .map(|clause| (*clause).to_string())
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    push_optional_analytics_filter(&mut clauses, &mut values, "project_id", project_id);
    values.push(Value::Integer(since));
    clauses.push(format!("timestamp >= ?{}", values.len()));
    sql.push_str(" WHERE ");
    sql.push_str(&clauses.join(" AND "));
    (sql, values)
}

fn row_to_code_project(row: &libsql::Row, offset: i32) -> Option<CodeProjectRecord> {
    Some(CodeProjectRecord {
        project_id: row.get(offset).ok()?,
        canonical_root: row.get(offset + 1).ok()?,
        display_root: row.get(offset + 2).ok()?,
        git_common_dir: row.get(offset + 3).ok()?,
        git_remote_url: row.get(offset + 4).ok()?,
        default_branch: row.get(offset + 5).ok()?,
        created_at: row.get(offset + 6).ok()?,
        last_seen_at: row.get(offset + 7).ok()?,
    })
}

fn row_to_store_instance(row: &libsql::Row, offset: i32) -> Option<StoreInstanceRecord> {
    Some(StoreInstanceRecord {
        store_id: row.get(offset).ok()?,
        project_id: row.get(offset + 1).ok()?,
        store_kind: row.get(offset + 2).ok()?,
        storage_mode: row.get(offset + 3).ok()?,
        store_relpath: row.get(offset + 4).ok()?,
        manifest_relpath: row.get(offset + 5).ok()?,
        created_at: row.get(offset + 6).ok()?,
        last_verified_at: row.get(offset + 7).ok()?,
        last_write_at: row.get(offset + 8).ok()?,
    })
}

fn row_to_graph_scope(row: &libsql::Row, offset: i32) -> Option<GraphScopeRecord> {
    let writable = row.get::<i64>(offset + 7).ok()? != 0;
    Some(GraphScopeRecord {
        graph_scope_id: row.get(offset).ok()?,
        project_id: row.get(offset + 1).ok()?,
        store_id: row.get(offset + 2).ok()?,
        branch_name: row.get(offset + 3).ok()?,
        db_relpath: row.get(offset + 4).ok()?,
        parent_scope_id: row.get(offset + 5).ok()?,
        last_synced_at: row.get(offset + 6).ok()?,
        writable,
    })
}

fn row_to_store_artifact(row: &libsql::Row, offset: i32) -> Option<StoreArtifactRecord> {
    Some(StoreArtifactRecord {
        store_id: row.get(offset).ok()?,
        artifact_kind: row.get(offset + 1).ok()?,
        relpath: row.get(offset + 2).ok()?,
        size_bytes: row.get(offset + 3).ok()?,
        schema_version: row.get(offset + 4).ok()?,
        updated_at: row.get(offset + 5).ok()?,
    })
}

/// Upper bound on the BM25 over-fetch that precedes the inventory downrank in
/// [`GlobalDb::search_session_messages_filtered_inner`]. Keeps the pre-rerank
/// fetch bounded even for large caller limits.
const SESSION_MESSAGE_SEARCH_MAX_FETCH: usize = 200;

/// Stable inventory downrank for a BM25 result page: transcript inventory/
/// listing messages and prose branch/worktree rosters (per the shared
/// [`crate::sessions::message_noise`] classifier) are moved below substantive
/// hits while preserving the relative BM25 order within each group. Applied
/// before truncation so a downranked hit still surfaces when it is the only
/// match. Mirrors the lcm/grep re-rank (`sessions::lcm::query::rerank_grep_hits`).
fn downrank_inventory_messages(results: &mut Vec<SessionMessageSearchResult>) {
    if results.len() < 2 {
        return;
    }
    let mut substantive = Vec::with_capacity(results.len());
    let mut inventory = Vec::new();
    for result in results.drain(..) {
        if crate::sessions::message_noise::is_inventory_text(&result.message.text) {
            inventory.push(result);
        } else {
            substantive.push(result);
        }
    }
    substantive.append(&mut inventory);
    *results = substantive;
}

fn session_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter_map(|word| {
            let sanitized: String = word.chars().filter(|c| *c != '"').collect();
            if sanitized.is_empty() {
                None
            } else {
                Some(format!("\"{sanitized}\"*"))
            }
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for ch in query.chars() {
        match ch {
            '%' | '_' | '\\' => {
                pattern.push('\\');
                pattern.push(ch);
            }
            _ => pattern.push(ch),
        }
    }
    pattern.push('%');
    pattern
}

fn repo_identity_aliases(git_common_dir: Option<&Path>) -> Vec<String> {
    let mut aliases = Vec::new();
    if let Some(path) = git_common_dir {
        aliases.push(format!(
            "git-common-dir:{}",
            GlobalDb::canonical_project_key(path)
        ));
    }
    aliases
}

fn git_remote_search_alias(remote: Option<&str>) -> Option<String> {
    let remote = remote?.trim().trim_end_matches('/');
    if remote.is_empty() {
        return None;
    }
    let name = remote
        .rsplit_once('/')
        .map(|(_, name)| name)
        .or_else(|| remote.rsplit_once(':').map(|(_, name)| name))
        .unwrap_or(remote)
        .trim()
        .trim_end_matches('/');
    if name.is_empty() || name.contains('@') || name.contains("://") {
        return None;
    }
    Some(format!("git-remote-name:{}", name.to_ascii_lowercase()))
}

fn project_identity_aliases(project_root: &Path, git_common_dir: Option<&Path>) -> Vec<String> {
    let mut aliases = Vec::with_capacity(2);
    aliases.push(GlobalDb::canonical_project_key(project_root));
    aliases.extend(repo_identity_aliases(git_common_dir));
    aliases
}

fn normalize_git_remote_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    let mut normalized = remote.trim_end_matches('/').to_string();
    if let Some(rest) = normalized.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            normalized = format!("https://{host}/{path}");
        }
    }
    if let Some(stripped) = normalized.strip_suffix(".git") {
        normalized = stripped.to_string();
    }
    Some(normalized.to_ascii_lowercase())
}

async fn session_column_exists(conn: &Connection, column: &str) -> bool {
    let Ok(mut rows) = conn.query("PRAGMA table_info(sessions)", ()).await else {
        return false;
    };
    while let Ok(Some(row)) = rows.next().await {
        if row.get::<String>(1).ok().as_deref() == Some(column) {
            return true;
        }
    }
    false
}

async fn ensure_session_parent_columns(conn: &Connection) -> Option<()> {
    for (column, ddl) in [
        (
            "parent_session_id",
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
        ),
        (
            "is_subagent",
            "ALTER TABLE sessions ADD COLUMN is_subagent INTEGER NOT NULL DEFAULT 0",
        ),
        ("agent_id", "ALTER TABLE sessions ADD COLUMN agent_id TEXT"),
        (
            "parent_tool_use_id",
            "ALTER TABLE sessions ADD COLUMN parent_tool_use_id TEXT",
        ),
    ] {
        if !session_column_exists(conn, column).await {
            add_session_parent_column_after_missing_check(conn, column, ddl).await?;
        }
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_parent
            ON sessions(provider, parent_session_id)",
        (),
    )
    .await
    .ok()?;
    Some(())
}

async fn parse_offsets_column_exists(conn: &Connection, column: &str) -> bool {
    let Ok(mut rows) = conn.query("PRAGMA table_info(parse_offsets)", ()).await else {
        return false;
    };
    while let Ok(Some(row)) = rows.next().await {
        if row.get::<String>(1).ok().as_deref() == Some(column) {
            return true;
        }
    }
    false
}

async fn add_parse_offset_column_after_missing_check(
    conn: &Connection,
    column: &str,
    ddl: &str,
) -> Option<()> {
    match conn.execute(ddl, ()).await {
        Ok(_) => Some(()),
        Err(_) if parse_offsets_column_exists(conn, column).await => Some(()),
        Err(_) => None,
    }
}

async fn ensure_parse_offset_columns(conn: &Connection) -> Option<()> {
    if !parse_offsets_column_exists(conn, "file_id").await {
        add_parse_offset_column_after_missing_check(
            conn,
            "file_id",
            "ALTER TABLE parse_offsets ADD COLUMN file_id INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
    }
    Some(())
}

async fn add_session_parent_column_after_missing_check(
    conn: &Connection,
    column: &str,
    ddl: &str,
) -> Option<()> {
    match conn.execute(ddl, ()).await {
        Ok(_) => Some(()),
        Err(_) if session_column_exists(conn, column).await => Some(()),
        Err(_) => None,
    }
}

/// Process-global switch for the detached structured-row backfill sweep that
/// [`GlobalDb::open_at`] schedules. On by default; tests flip it off so they can
/// drive [`GlobalDb::run_structured_backfill`] synchronously against a
/// deterministic store.
static BACKGROUND_STRUCTURED_BACKFILL_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Whether this process is a long-lived host that may run the detached
/// structured-row sweep. Off by default: one-shot CLI and (crucially) hook
/// processes never opt in, so [`GlobalDb::spawn_structured_backfill`] is a
/// no-op for them. A hook process exits within milliseconds of the open that
/// scheduled the sweep, and dropping its runtime cancels the sweep's async
/// task mid-parse — the parsed rows and the cursor advance are discarded, so a
/// hook-spawned sweep makes zero durable progress and only adds exit latency.
/// The long-lived MCP `serve` loop and the daemon set this via
/// [`mark_process_long_lived_for_structured_backfill`]; because every store is
/// also opened under one of those hosts, coverage still converges there.
static STRUCTURED_BACKFILL_LONG_LIVED_PROCESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Store paths (by db path) that already have a structured-row sweep running in
/// this process, so concurrent opens don't stack duplicate sweeps.
fn structured_backfill_in_flight() -> &'static std::sync::Mutex<HashSet<PathBuf>> {
    static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    IN_FLIGHT.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// Enables or disables the detached background structured-row sweep. Intended
/// for tests that need the sweep to run only when they explicitly drive it via
/// [`GlobalDb::run_structured_backfill`].
#[doc(hidden)]
pub fn set_background_structured_backfill_enabled(enabled: bool) {
    BACKGROUND_STRUCTURED_BACKFILL_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Marks this process as a long-lived host (the MCP `serve` loop or the daemon)
/// that is allowed to run the detached structured-row sweep. Called once at
/// those entry points before any store is opened. One-shot CLI and hook
/// processes must never call this: see [`STRUCTURED_BACKFILL_LONG_LIVED_PROCESS`].
pub fn mark_process_long_lived_for_structured_backfill() {
    STRUCTURED_BACKFILL_LONG_LIVED_PROCESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Resets the long-lived-process gate after tests that exercise daemon-only
/// background behavior in a shared test process.
#[doc(hidden)]
pub fn reset_process_long_lived_for_structured_backfill() {
    STRUCTURED_BACKFILL_LONG_LIVED_PROCESS.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Whether [`GlobalDb::spawn_structured_backfill`] will schedule a sweep: the
/// background switch is on *and* this process is a long-lived host. This is the
/// single predicate the spawn path consults, exposed so tests can assert that a
/// one-shot process never spawns the sweep.
#[doc(hidden)]
pub fn structured_backfill_will_spawn() -> bool {
    BACKGROUND_STRUCTURED_BACKFILL_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
        && STRUCTURED_BACKFILL_LONG_LIVED_PROCESS.load(std::sync::atomic::Ordering::Relaxed)
}

impl GlobalDb {
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    async fn open_local(
        db_path: &Path,
        read_only: bool,
        authority: DatabaseAuthority,
        slot: Option<GlobalDbSlot>,
    ) -> Option<Self> {
        let authority = authority.hold_for(db_path, "open global database").ok()?;
        let db_path = authority.canonical_database_path().to_path_buf();
        let storage_root = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let builder = if read_only {
            Builder::new_local(&db_path).flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        } else {
            Builder::new_local(&db_path)
        };
        let db = builder.build().await.ok()?;
        let conn = db.connect().ok()?;

        conn.execute_batch(&format!(
            "PRAGMA mmap_size = {};",
            global_db_mmap_size_guard()
        ))
        .await
        .ok()?;

        let pragmas = if read_only {
            "PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;"
                .to_string()
        } else {
            let journal_mode = crate::db::platform_safe_journal_mode();
            let synchronous = crate::db::platform_safe_synchronous_mode();
            format!(
                "PRAGMA journal_mode = {journal_mode};
                 PRAGMA busy_timeout = 5000;
                 PRAGMA synchronous = {synchronous};
                 PRAGMA foreign_keys = ON;"
            )
        };
        conn.execute_batch(&pragmas).await.ok()?;

        Some(Self {
            inner: Arc::new(GlobalDbInner {
                conn,
                storage_root,
                db_path,
                _db: db,
                _authority: authority,
                _slot: slot,
            }),
        })
    }

    /// Opens (or creates) the global database at an explicit path. Returns
    /// `None` if the directory cannot be created or the DB fails to open.
    ///
    /// Concurrent first opens of the same fresh store used to race each
    /// other's `PRAGMA journal_mode = WAL`, DDL batch, and migration
    /// transactions: all but one connection silently got `None`, which
    /// disabled global accounting (ledger recording) for the unlucky
    /// callers' entire session. Schema initialization is singleflight per
    /// canonical database identity; after it completes, every caller opens an
    /// independent connection so caller-managed transactions cannot interleave
    /// on one shared libSQL session. `SQLite` still serializes actual writers.
    pub async fn open_at(db_path: &std::path::Path) -> Option<Self> {
        Self::best_effort_open(Self::try_open_at(db_path).await)
    }

    /// Result-preserving counterpart to [`Self::open_at`]. Authority failures
    /// retain their exact ownership/profile diagnostic; storage-open failures
    /// remain `Ok(None)` under the global database's best-effort contract.
    pub async fn try_open_at(db_path: &std::path::Path) -> crate::errors::Result<Option<Self>> {
        Self::try_open_at_with_backfill(db_path, true).await
    }

    /// Opens and ensures a writable session store without starting detached
    /// structured backfill. Bulk multi-store catch-up uses this to avoid
    /// launching one competing backfill task per registered project.
    pub async fn open_at_without_structured_backfill(db_path: &std::path::Path) -> Option<Self> {
        Self::best_effort_open(Self::try_open_at_without_structured_backfill(db_path).await)
    }

    /// Result-preserving counterpart to
    /// [`Self::open_at_without_structured_backfill`].
    pub async fn try_open_at_without_structured_backfill(
        db_path: &std::path::Path,
    ) -> crate::errors::Result<Option<Self>> {
        Self::try_open_at_with_backfill(db_path, false).await
    }

    fn best_effort_open(result: crate::errors::Result<Option<Self>>) -> Option<Self> {
        match result {
            Ok(db) => db,
            Err(error) => {
                eprintln!("[tracedecay] global database open rejected: {error}");
                None
            }
        }
    }

    async fn try_open_at_with_backfill(
        db_path: &std::path::Path,
        spawn_structured_backfill: bool,
    ) -> crate::errors::Result<Option<Self>> {
        let authority = DatabaseAuthority::for_runtime(db_path, "open global database")?;
        let canonical_path = authority.canonical_database_path().to_path_buf();
        let slot = global_db_slot(&authority);
        let mut schema = slot.lock().await;
        if schema.ensured {
            drop(schema);
            let Some(db) =
                Self::open_local(&canonical_path, false, authority, Some(Arc::clone(&slot))).await
            else {
                return Ok(None);
            };
            if spawn_structured_backfill {
                db.spawn_structured_backfill();
            }
            return Ok(Some(db));
        }
        if let Some(parent) = canonical_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return Ok(None);
            }
        }
        let Some(db) = Self::open_at_unsynchronized(
            &canonical_path,
            spawn_structured_backfill,
            authority,
            Arc::clone(&slot),
        )
        .await
        else {
            return Ok(None);
        };
        schema.ensured = true;
        Ok(Some(db))
    }

    async fn open_at_unsynchronized(
        db_path: &std::path::Path,
        spawn_structured_backfill: bool,
        authority: DatabaseAuthority,
        slot: GlobalDbSlot,
    ) -> Option<Self> {
        let db = Self::open_local(db_path, false, authority, Some(slot)).await?;

        db.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS code_projects (
                project_id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL,
                display_root TEXT NOT NULL,
                git_common_dir TEXT,
                git_remote_url TEXT,
                default_branch TEXT,
                created_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS project_aliases (
                alias_path TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                last_seen_at INTEGER NOT NULL,
                FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS store_instances (
                store_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                store_kind TEXT NOT NULL,
                storage_mode TEXT NOT NULL,
                store_relpath TEXT NOT NULL,
                manifest_relpath TEXT,
                created_at INTEGER NOT NULL,
                last_verified_at INTEGER,
                last_write_at INTEGER,
                FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS graph_scopes (
                graph_scope_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                store_id TEXT NOT NULL,
                branch_name TEXT NOT NULL,
                db_relpath TEXT NOT NULL,
                parent_scope_id TEXT,
                last_synced_at INTEGER,
                writable INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE,
                FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS store_artifacts (
                store_id TEXT NOT NULL,
                artifact_kind TEXT NOT NULL,
                relpath TEXT NOT NULL,
                size_bytes INTEGER,
                schema_version TEXT,
                updated_at INTEGER,
                PRIMARY KEY (store_id, artifact_kind, relpath),
                FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_project_aliases_project_id
                ON project_aliases(project_id);
            CREATE INDEX IF NOT EXISTS idx_store_instances_project_id
                ON store_instances(project_id);
            CREATE INDEX IF NOT EXISTS idx_graph_scopes_project_store
                ON graph_scopes(project_id, store_id)",
            )
            .await
            .ok()?;
        let _ = db.migrate_project_rows_to_canonical_keys().await;

        db.conn
            .execute_batch(
            "CREATE TABLE IF NOT EXISTS turns (
                message_id TEXT PRIMARY KEY,
                project_hash TEXT NOT NULL,
                session_id TEXT NOT NULL,
                model TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL,
                category TEXT NOT NULL,
                tool_names TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_turns_timestamp ON turns(timestamp);
            CREATE INDEX IF NOT EXISTS idx_turns_project ON turns(project_hash);
            CREATE INDEX IF NOT EXISTS idx_turns_model ON turns(model);
            CREATE TABLE IF NOT EXISTS parse_offsets (
                file_path TEXT PRIMARY KEY,
                byte_offset INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                file_id INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS savings_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                before_tokens INTEGER NOT NULL,
                after_tokens INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_savings_ledger_ts ON savings_ledger(ts);
            CREATE INDEX IF NOT EXISTS idx_savings_ledger_project ON savings_ledger(project_path);
            CREATE TABLE IF NOT EXISTS analytics_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                project_id TEXT NOT NULL,
                session_id TEXT,
                timestamp INTEGER NOT NULL,
                event_kind TEXT NOT NULL,
                hook_name TEXT,
                tool_name TEXT,
                tool_category TEXT,
                skill_name TEXT,
                hint_category TEXT,
                hint_id TEXT,
                outcome TEXT,
                metadata_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_analytics_events_provider_project_session
                ON analytics_events(provider, project_id, session_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_analytics_events_kind
                ON analytics_events(event_kind, timestamp);
            CREATE INDEX IF NOT EXISTS idx_analytics_events_project_time
                ON analytics_events(project_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_analytics_events_timestamp
                ON analytics_events(timestamp);
            CREATE TABLE IF NOT EXISTS sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                title TEXT,
                started_at INTEGER,
                ended_at INTEGER,
                transcript_path TEXT,
                metadata_json TEXT,
                parent_session_id TEXT,
                is_subagent INTEGER NOT NULL DEFAULT 0,
                agent_id TEXT,
                parent_tool_use_id TEXT,
                PRIMARY KEY(provider, session_id)
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_project
                ON sessions(provider, project_key);
            CREATE INDEX IF NOT EXISTS idx_sessions_started_at
                ON sessions(started_at);
            CREATE TABLE IF NOT EXISTS session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                timestamp INTEGER,
                ordinal INTEGER NOT NULL,
                text TEXT NOT NULL,
                kind TEXT,
                model TEXT,
                tool_names TEXT,
                source_path TEXT,
                source_offset INTEGER,
                metadata_json TEXT,
                PRIMARY KEY(provider, message_id),
                FOREIGN KEY(provider, session_id)
                    REFERENCES sessions(provider, session_id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_session_messages_session
                ON session_messages(provider, session_id, ordinal);
            CREATE INDEX IF NOT EXISTS idx_session_messages_timestamp
                ON session_messages(timestamp);
            CREATE INDEX IF NOT EXISTS idx_session_messages_source
                ON session_messages(source_path);
            CREATE VIRTUAL TABLE IF NOT EXISTS session_messages_fts USING fts5(
                text, role, kind, model, tool_names,
                content='session_messages', content_rowid='rowid'
            );
            CREATE TRIGGER IF NOT EXISTS session_messages_fts_insert
                AFTER INSERT ON session_messages BEGIN
                    INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
                    VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
                END;
            CREATE TRIGGER IF NOT EXISTS session_messages_fts_delete
                AFTER DELETE ON session_messages BEGIN
                    INSERT INTO session_messages_fts(session_messages_fts, rowid, text, role, kind, model, tool_names)
                    VALUES ('delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names);
                END;
            CREATE TRIGGER IF NOT EXISTS session_messages_fts_update
                AFTER UPDATE ON session_messages BEGIN
                    INSERT INTO session_messages_fts(session_messages_fts, rowid, text, role, kind, model, tool_names)
                    VALUES ('delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names);
                    INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
                    VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
                END",
        )
        .await
        .ok()?;
        ensure_session_parent_columns(&db.conn).await?;
        ensure_parse_offset_columns(&db.conn).await?;
        crate::sessions::lcm::schema::ensure_lcm_schema(&db.conn)
            .await
            .ok()?;
        crate::sessions::git_correlation::ensure_git_correlation_schema(&db.conn)
            .await
            .ok()?;
        crate::sessions::workflow_index::ensure_workflow_index_schema(&db.conn)
            .await
            .ok()?;
        // One-off self-heal: re-derive timestamps and token-usage counters
        // for legacy messages ingested before extraction existed.
        // Marker-guarded (runs once per store) and fail-open, like the LCM
        // schema migrations above.
        let _ = crate::sessions::transcript_backfill::backfill_transcript_facts(&db.conn).await;
        // Recover structured rows skipped by legacy transcript parsers. This
        // runs on every open (per hook event, per CLI/MCP invocation), so it
        // must not block: schedule it on a detached background task rather than
        // synchronously reading and re-parsing a batch of multi-MB transcripts.
        if spawn_structured_backfill {
            db.spawn_structured_backfill();
        }

        Some(db)
    }

    /// Opens a writable database at an explicit path assuming its schema was
    /// already ensured by a prior [`Self::open_at`] in this process: skips the
    /// DDL batch and LCM migrations while still applying the per-connection
    /// PRAGMAs. Long-lived servers use this to avoid re-paying the schema
    /// ensure on every tool call (the caller tracks which paths are ensured).
    /// This raw open never participates in or updates the full-open schema
    /// slot, so it cannot make a later [`Self::open_at`] skip initialization.
    pub async fn open_at_assuming_schema(db_path: &std::path::Path) -> Option<Self> {
        if !db_path.is_file() {
            return None;
        }
        let authority =
            DatabaseAuthority::for_runtime(db_path, "open global database assuming schema").ok()?;
        let canonical_path = authority.canonical_database_path().to_path_buf();
        Self::open_local(&canonical_path, false, authority, None).await
    }

    /// Opens an existing database without creating directories, creating schema,
    /// or running LCM carry-forward migrations.
    pub async fn open_read_only_at(db_path: &std::path::Path) -> Option<Self> {
        if !db_path.is_file() {
            return None;
        }
        let authority =
            DatabaseAuthority::for_runtime(db_path, "open global database read-only").ok()?;
        let canonical_path = authority.canonical_database_path().to_path_buf();
        Self::open_local(&canonical_path, true, authority, None).await
    }

    /// Opens (or creates) the global database. Returns `None` if the home
    /// directory cannot be determined or the DB fails to open.
    pub async fn open() -> Option<Self> {
        Self::best_effort_open(Self::try_open().await)
    }

    /// Result-preserving counterpart to [`Self::open`].
    pub async fn try_open() -> crate::errors::Result<Option<Self>> {
        let Some(db_path) = global_db_path() else {
            return Ok(None);
        };
        Self::try_open_at(&db_path).await
    }

    /// Raw connection for crate-internal read layers (the dashboard HTTP
    /// server queries the LCM tables directly).
    pub(crate) fn dashboard_connection(&self) -> Connection {
        self.conn.clone()
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Schedules the structured-row backfill sweep on a detached task so the
    /// hot `open_at` path returns immediately instead of synchronously
    /// re-reading and re-parsing a batch of multi-MB transcripts.
    ///
    /// Concurrency: the sweep opens its own connection, its writes are
    /// idempotent upserts keyed on `(provider, message_id)`, and its path
    /// watermark advances per file under a cross-process lock — so overlapping
    /// with live ingest is safe. A process-wide in-flight guard (keyed by store
    /// path) skips the spawn when a sweep for this store is already running in
    /// *this* process; a sibling file lock (see
    /// `transcript_backfill::try_acquire_structured_backfill_lock`) excludes
    /// other processes so concurrent hook processes never run duplicate sweeps.
    ///
    /// Only long-lived hosts spawn: [`structured_backfill_will_spawn`] gates on
    /// [`mark_process_long_lived_for_structured_backfill`], so short-lived hook
    /// and one-shot CLI processes never schedule the sweep at all (their
    /// runtime would drop the task mid-parse before it made durable progress).
    /// The daemon and MCP server open every store too, so coverage converges
    /// there.
    fn spawn_structured_backfill(&self) {
        if !structured_backfill_will_spawn() {
            return;
        }
        let db_path = self.db_path.clone();
        {
            let mut in_flight = match structured_backfill_in_flight().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !in_flight.insert(db_path.clone()) {
                // A sweep for this store is already in flight in this process.
                return;
            }
        }
        tokio::spawn(async move {
            // The scheduling open already ensured the schema. Use a separate
            // raw connection so backfill transactions never share the
            // caller's libSQL session or publish schema state.
            if let Some(db) = GlobalDb::open_at_assuming_schema(&db_path).await {
                let _ = crate::sessions::transcript_backfill::backfill_structured_rows(&db).await;
            }
            let mut in_flight = match structured_backfill_in_flight().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            in_flight.remove(&db_path);
        });
    }

    /// Runs the structured-row backfill sweep synchronously to completion for
    /// the current bounded batch, returning the number of rows inserted.
    /// `open_at` drives this in the background; callers (and tests) that need a
    /// deterministic sweep invoke it directly.
    pub async fn run_structured_backfill(&self) -> Option<u64> {
        crate::sessions::transcript_backfill::backfill_structured_rows(self)
            .await
            .map(|stats| stats.inserted())
    }

    /// Transcript-ingest backlog for the session store backing this DB.
    ///
    /// For every session with a known transcript path, compares the on-disk
    /// transcript size against the persisted parse offset. `pending_bytes` is
    /// the total un-ingested tail across transcripts; `last_ingest_unix` is
    /// the newest transcript mtime recorded at ingest time. Surfaced by status
    /// and doctor checks so a stalled ingest is visible instead of silently
    /// eroding trust in session recall.
    pub async fn session_ingest_health(&self) -> SessionIngestHealth {
        self.session_ingest_health_for_provider(None).await
    }

    pub async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> SessionIngestHealth {
        let mut health = SessionIngestHealth::default();
        let rows = if let Some(provider) = provider {
            self.conn
                .query(
                    "SELECT DISTINCT transcript_path FROM sessions
                     WHERE provider = ?1
                       AND transcript_path IS NOT NULL
                       AND transcript_path != ''
                     LIMIT 1000",
                    params![provider],
                )
                .await
        } else {
            self.conn
                .query(
                    "SELECT DISTINCT transcript_path FROM sessions
                 WHERE transcript_path IS NOT NULL AND transcript_path != ''
                 LIMIT 1000",
                    (),
                )
                .await
        };
        let Ok(mut rows) = rows else {
            return health;
        };
        let mut paths = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(path) = row.get::<String>(0) {
                paths.push(path);
            }
        }
        for path in paths {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            health.tracked_transcripts += 1;
            let cursor = self.get_parse_offset(&path).await.unwrap_or_default();
            if cursor.mtime > 0 {
                let mtime = cursor.mtime as i64;
                health.last_ingest_unix = Some(
                    health
                        .last_ingest_unix
                        .map_or(mtime, |prev| prev.max(mtime)),
                );
            }
            let pending = meta.len().saturating_sub(cursor.byte_offset);
            if pending > 0 {
                health.pending_transcripts += 1;
                health.pending_bytes = health.pending_bytes.saturating_add(pending);
                health.max_transcript_pending_bytes =
                    health.max_transcript_pending_bytes.max(pending);
            }
        }
        health
    }

    /// Returns tracked transcript paths that still contain an unresolved
    /// workspace placeholder. Cursor should expand `${workspaceFolder}` before
    /// a transcript path is persisted; if it reaches the session DB literally,
    /// catch-up and recall will look at a non-existent path.
    pub async fn literal_workspace_placeholder_transcript_paths(
        &self,
        limit: usize,
    ) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT DISTINCT transcript_path FROM sessions
                 WHERE transcript_path IS NOT NULL
                   AND transcript_path != ''
                   AND (transcript_path LIKE '%${workspaceFolder}%'
                        OR transcript_path LIKE '%$workspaceFolder%')
                 ORDER BY transcript_path
                 LIMIT ?1",
                params![i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .await
        else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(path) = row.get::<String>(0) {
                paths.push(path);
            }
        }
        paths
    }

    /// Canonical registry key for a project path. Falls back to the lossy path
    /// string when canonicalization fails (e.g. the path no longer exists) so
    /// upserts and lookups always agree on a single key per project, instead of
    /// creating divergent rows for `/p`, `/p/`, and symlinked spellings (#6).
    pub fn canonical_project_key(project_path: &Path) -> String {
        std::fs::canonicalize(project_path)
            .unwrap_or_else(|_| project_path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    pub fn is_explicit_project_path_selector(selector: &str) -> bool {
        let selector = selector.trim();
        !selector.is_empty()
            && (Path::new(selector).is_absolute()
                || selector == "."
                || selector == ".."
                || selector.contains('/')
                || selector.contains('\\'))
    }

    async fn migrate_project_rows_to_canonical_keys(&self) -> Option<()> {
        let mut rows = self
            .conn
            .query("SELECT path, tokens_saved FROM projects", ())
            .await
            .ok()?;
        let mut replacements = Vec::new();
        while let Some(row) = rows.next().await.ok()? {
            let old_path: String = row.get(0).ok()?;
            let tokens_saved: i64 = row.get(1).ok()?;
            let canonical_path = Self::canonical_project_key(Path::new(&old_path));
            if old_path != canonical_path {
                replacements.push((old_path, canonical_path, tokens_saved));
            }
        }

        for (old_path, canonical_path, tokens_saved) in replacements {
            self.conn
                .execute(
                    "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)
                     ON CONFLICT(path) DO UPDATE SET
                        tokens_saved = MAX(tokens_saved, excluded.tokens_saved)",
                    params![canonical_path, tokens_saved],
                )
                .await
                .ok()?;
            self.conn
                .execute("DELETE FROM projects WHERE path = ?1", params![old_path])
                .await
                .ok()?;
        }
        Some(())
    }

    pub async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Option<CodeProjectRecord> {
        let now = crate::tracedecay::current_timestamp();
        let canonical_root = Self::canonical_project_key(project_root);
        let display_root = project_root.to_string_lossy().to_string();
        let git_common_dir_text = git_common_dir.map(|path| path.to_string_lossy().to_string());
        self.conn
            .execute(
                "INSERT INTO code_projects
                 (project_id, canonical_root, display_root, git_common_dir, git_remote_url,
                  default_branch, created_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(project_id) DO UPDATE SET
                    canonical_root = excluded.canonical_root,
                    display_root = excluded.display_root,
                    git_common_dir = excluded.git_common_dir,
                    git_remote_url = excluded.git_remote_url,
                    default_branch = excluded.default_branch,
                    last_seen_at = excluded.last_seen_at",
                params![
                    project_id,
                    canonical_root,
                    display_root,
                    opt_text(git_common_dir_text.as_deref()),
                    opt_text(git_remote_url),
                    opt_text(default_branch),
                    now,
                ],
            )
            .await
            .ok()?;
        self.upsert_project_alias(project_root, project_id).await?;
        for alias in repo_identity_aliases(git_common_dir) {
            self.upsert_project_alias_key(&alias, project_id).await?;
        }
        if let Some(alias) = git_remote_search_alias(git_remote_url) {
            self.upsert_project_alias_key(&alias, project_id).await?;
        }
        self.get_code_project(project_id).await
    }

    pub async fn upsert_project_alias(
        &self,
        alias_path: &Path,
        project_id: &str,
    ) -> Option<ProjectAliasRecord> {
        let alias = Self::canonical_project_key(alias_path);
        self.upsert_project_alias_key(&alias, project_id).await
    }

    async fn upsert_project_alias_key(
        &self,
        alias: &str,
        project_id: &str,
    ) -> Option<ProjectAliasRecord> {
        let now = crate::tracedecay::current_timestamp();
        self.conn
            .execute(
                "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(alias_path) DO UPDATE SET
                    project_id = excluded.project_id,
                    last_seen_at = excluded.last_seen_at",
                params![alias, project_id, now],
            )
            .await
            .ok()?;
        let mut rows = self
            .conn
            .query(
                "SELECT alias_path, project_id, last_seen_at
                 FROM project_aliases WHERE alias_path = ?1",
                params![alias],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(ProjectAliasRecord {
            alias_path: row.get(0).ok()?,
            project_id: row.get(1).ok()?,
            last_seen_at: row.get(2).ok()?,
        })
    }

    pub async fn upsert_store_instance(
        &self,
        upsert: StoreInstanceUpsert,
    ) -> Option<StoreInstanceRecord> {
        let now = crate::tracedecay::current_timestamp();
        self.conn
            .execute(
                "INSERT INTO store_instances
                 (store_id, project_id, store_kind, storage_mode, store_relpath,
                  manifest_relpath, created_at, last_verified_at, last_write_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(store_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    store_kind = excluded.store_kind,
                    storage_mode = excluded.storage_mode,
                    store_relpath = excluded.store_relpath,
                    manifest_relpath = excluded.manifest_relpath,
                    last_verified_at = excluded.last_verified_at,
                    last_write_at = excluded.last_write_at",
                params![
                    upsert.store_id.as_str(),
                    upsert.project_id.as_str(),
                    upsert.store_kind.as_str(),
                    upsert.storage_mode.as_str(),
                    upsert.store_relpath.as_str(),
                    opt_text(upsert.manifest_relpath.as_deref()),
                    now,
                    opt_i64(upsert.last_verified_at),
                    opt_i64(upsert.last_write_at),
                ],
            )
            .await
            .ok()?;
        self.get_store_instance(&upsert.store_id).await
    }

    pub async fn upsert_graph_scope(&self, upsert: GraphScopeUpsert) -> Option<GraphScopeRecord> {
        self.conn
            .execute(
                "INSERT INTO graph_scopes
                 (graph_scope_id, project_id, store_id, branch_name, db_relpath,
                  parent_scope_id, last_synced_at, writable)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(graph_scope_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    store_id = excluded.store_id,
                    branch_name = excluded.branch_name,
                    db_relpath = excluded.db_relpath,
                    parent_scope_id = excluded.parent_scope_id,
                    last_synced_at = excluded.last_synced_at,
                    writable = excluded.writable",
                params![
                    upsert.graph_scope_id.as_str(),
                    upsert.project_id.as_str(),
                    upsert.store_id.as_str(),
                    upsert.branch_name.as_str(),
                    upsert.db_relpath.as_str(),
                    opt_text(upsert.parent_scope_id.as_deref()),
                    opt_i64(upsert.last_synced_at),
                    i64::from(upsert.writable),
                ],
            )
            .await
            .ok()?;
        self.get_graph_scope(&upsert.graph_scope_id).await
    }

    pub async fn upsert_store_artifact(
        &self,
        upsert: StoreArtifactUpsert,
    ) -> Option<StoreArtifactRecord> {
        self.conn
            .execute(
                "INSERT INTO store_artifacts
                 (store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(store_id, artifact_kind, relpath) DO UPDATE SET
                    size_bytes = excluded.size_bytes,
                    schema_version = excluded.schema_version,
                    updated_at = excluded.updated_at",
                params![
                    upsert.store_id.as_str(),
                    upsert.artifact_kind.as_str(),
                    upsert.relpath.as_str(),
                    opt_i64(upsert.size_bytes),
                    opt_text(upsert.schema_version.as_deref()),
                    opt_i64(upsert.updated_at),
                ],
            )
            .await
            .ok()?;
        let mut rows = self
            .conn
            .query(
                "SELECT store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
                 FROM store_artifacts
                 WHERE store_id = ?1 AND artifact_kind = ?2 AND relpath = ?3",
                params![
                    upsert.store_id.as_str(),
                    upsert.artifact_kind.as_str(),
                    upsert.relpath.as_str()
                ],
            )
            .await
            .ok()?;
        row_to_store_artifact(&rows.next().await.ok()??, 0)
    }

    pub async fn resolve_project_store_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<ProjectStoreResolution> {
        let alias = Self::canonical_project_key(alias_path);
        self.resolve_project_store_by_alias_key(&alias).await
    }

    pub async fn resolve_project_store_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Option<ProjectStoreResolution> {
        let project_id = self
            .project_id_by_identity(project_root, git_common_dir)
            .await?;
        let project = self.get_code_project(&project_id).await?;
        self.resolve_project_store_for_project(&project).await
    }

    pub async fn resolve_unique_project_store_by_git_remote(
        &self,
        git_remote_url: &str,
    ) -> Option<ProjectStoreResolution> {
        let remote = normalize_git_remote_url(git_remote_url)?;
        let match_project = {
            let mut rows = self
                .conn
                .query(
                    "SELECT project_id, canonical_root, display_root, git_common_dir,
                            git_remote_url, default_branch, created_at, last_seen_at
                     FROM code_projects
                     WHERE git_remote_url IS NOT NULL AND git_remote_url != ''
                     ORDER BY project_id",
                    (),
                )
                .await
                .ok()?;
            let mut match_project = None;
            let mut ambiguous = false;
            while let Some(row) = rows.next().await.ok()? {
                let project = row_to_code_project(&row, 0)?;
                let Some(stored_remote) = project
                    .git_remote_url
                    .as_deref()
                    .and_then(normalize_git_remote_url)
                else {
                    continue;
                };
                if stored_remote == remote {
                    if match_project.is_some() {
                        ambiguous = true;
                        break;
                    }
                    match_project = Some(project);
                }
            }
            if ambiguous { None } else { match_project }
        }?;
        self.resolve_project_store_for_project(&match_project).await
    }

    async fn resolve_project_store_by_alias_key(
        &self,
        alias: &str,
    ) -> Option<ProjectStoreResolution> {
        let project_id = self.project_id_by_alias_key(alias).await?;
        let project = self.get_code_project(&project_id).await?;
        self.resolve_project_store_for_project(&project).await
    }

    async fn resolve_project_store_for_project(
        &self,
        project: &CodeProjectRecord,
    ) -> Option<ProjectStoreResolution> {
        let store = {
            let mut rows = self
                .conn
                .query(
                    "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                            manifest_relpath, created_at, last_verified_at, last_write_at
                     FROM store_instances
                     WHERE project_id = ?1
                     ORDER BY COALESCE(last_verified_at, created_at) DESC, store_id
                     LIMIT 1",
                    params![project.project_id.as_str()],
                )
                .await
                .ok()?;
            row_to_store_instance(&rows.next().await.ok()??, 0)?
        };
        let graph_scopes = self.list_graph_scopes_for_store(&store.store_id).await;
        let artifacts = self.list_store_artifacts(&store.store_id).await;
        Some(ProjectStoreResolution {
            project: project.clone(),
            store,
            graph_scopes,
            artifacts,
        })
    }

    /// Lists registered code projects, preserving query and row-decoding errors.
    ///
    /// Destructive maintenance callers must use this instead of the best-effort
    /// [`Self::list_code_projects`] wrapper so a registry failure cannot be
    /// mistaken for an empty registry.
    pub async fn try_list_code_projects(
        &self,
        limit: usize,
    ) -> crate::errors::Result<Vec<CodeProjectRecord>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut rows = self
            .conn
            .query(
                "SELECT project_id, canonical_root, display_root, git_common_dir,
                        git_remote_url, default_branch, created_at, last_seen_at
                 FROM code_projects
                 ORDER BY last_seen_at DESC, project_id
                 LIMIT ?1",
                params![limit],
            )
            .await?;
        let mut projects = Vec::new();
        while let Some(row) = rows.next().await? {
            let project = row_to_code_project(&row, 0).ok_or_else(|| {
                crate::errors::TraceDecayError::Database {
                    message: "failed to decode code project registry row".to_string(),
                    operation: "list code projects".to_string(),
                }
            })?;
            projects.push(project);
        }
        Ok(projects)
    }

    /// Lists registered code projects on the daemon's best-effort path.
    pub async fn list_code_projects(&self, limit: usize) -> Vec<CodeProjectRecord> {
        self.try_list_code_projects(limit).await.unwrap_or_default()
    }

    /// Returns registered code projects whose `last_seen_at` is within the last
    /// `since_secs` seconds, most-recently-seen first, capped at `limit`.
    ///
    /// Used by the git-metadata watcher to register only projects seen recently
    /// (e.g. within 14 days), bounded by `watch_max_projects`.
    pub async fn code_projects_seen_within(
        &self,
        since_secs: i64,
        limit: usize,
    ) -> Vec<CodeProjectRecord> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let cutoff = crate::tracedecay::current_timestamp().saturating_sub(since_secs);
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT project_id, canonical_root, display_root, git_common_dir,
                        git_remote_url, default_branch, created_at, last_seen_at
                 FROM code_projects
                 WHERE last_seen_at >= ?1
                 ORDER BY last_seen_at DESC, project_id
                 LIMIT ?2",
                params![cutoff, limit],
            )
            .await
        else {
            return Vec::new();
        };
        let mut projects = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Some(project) = row_to_code_project(&row, 0) {
                projects.push(project);
            }
        }
        projects
    }

    /// Removes registered code-project rows by exact project id.
    ///
    /// Dependent registry rows in `project_aliases`, `store_instances`,
    /// `graph_scopes`, and `store_artifacts` cascade through foreign keys.
    /// This only removes registry metadata; it never deletes project files or
    /// profile-sharded store files on disk.
    pub async fn delete_code_projects(&self, project_ids: &[String]) -> usize {
        const CHUNK: usize = 256;
        let mut total: usize = 0;
        for chunk in project_ids.chunks(CHUNK) {
            let placeholders = vec!["?"; chunk.len()];
            let sql = format!(
                "DELETE FROM code_projects WHERE project_id IN ({})",
                placeholders.join(",")
            );
            let values: Vec<libsql::Value> = chunk
                .iter()
                .map(|project_id| libsql::Value::Text(project_id.clone()))
                .collect();
            if let Ok(n) = self.conn.execute(&sql, values).await {
                total = total.saturating_add(n as usize);
            }
        }
        total
    }

    pub async fn search_code_projects(&self, query: &str, limit: usize) -> Vec<CodeProjectRecord> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut patterns: Vec<String> = query.split_whitespace().map(like_pattern).collect();
        if patterns.is_empty() {
            patterns.push(like_pattern(query));
        }
        let mut clauses = Vec::with_capacity(patterns.len());
        for index in 1..=patterns.len() {
            clauses.push(format!(
                "(cp.project_id LIKE ?{index} ESCAPE '\\'
                    OR cp.canonical_root LIKE ?{index} ESCAPE '\\'
                    OR cp.display_root LIKE ?{index} ESCAPE '\\'
                    OR COALESCE(cp.git_common_dir, '') LIKE ?{index} ESCAPE '\\'
                    OR COALESCE(cp.default_branch, '') LIKE ?{index} ESCAPE '\\'
                    OR COALESCE(pa.alias_path, '') LIKE ?{index} ESCAPE '\\')"
            ));
        }
        let limit_param = patterns.len() + 1;
        let sql = format!(
            "SELECT DISTINCT cp.project_id, cp.canonical_root, cp.display_root,
                    cp.git_common_dir, cp.git_remote_url, cp.default_branch,
                    cp.created_at, cp.last_seen_at
             FROM code_projects cp
             LEFT JOIN project_aliases pa ON pa.project_id = cp.project_id
             WHERE {}
             ORDER BY cp.last_seen_at DESC, cp.project_id
             LIMIT ?{limit_param}",
            clauses.join(" OR ")
        );
        let mut params: Vec<libsql::Value> =
            patterns.into_iter().map(libsql::Value::Text).collect();
        params.push(libsql::Value::Integer(limit));
        let Ok(mut rows) = self
            .conn
            .query(&sql, libsql::params_from_iter(params))
            .await
        else {
            return Vec::new();
        };
        let mut projects = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Some(project) = row_to_code_project(&row, 0) {
                projects.push(project);
            }
        }
        projects
    }

    pub async fn project_registry_context_by_id(
        &self,
        project_id: &str,
    ) -> Option<ProjectRegistryContext> {
        let project = self.get_code_project(project_id).await?;
        Some(ProjectRegistryContext {
            aliases: self.list_aliases_for_project(project_id).await,
            stores: self.list_store_contexts_for_project(project_id).await,
            project,
        })
    }

    pub async fn project_registry_contexts_for_projects(
        &self,
        projects: &[CodeProjectRecord],
    ) -> Vec<ProjectRegistryContext> {
        if projects.is_empty() {
            return Vec::new();
        }
        let project_ids = projects
            .iter()
            .map(|project| project.project_id.clone())
            .collect::<Vec<_>>();
        let mut aliases_by_project = BTreeMap::<String, Vec<ProjectAliasRecord>>::new();
        let Some(mut rows) = self
            .query_string_ids(
                "SELECT alias_path, project_id, last_seen_at
                 FROM project_aliases
                 WHERE project_id IN ({})
                 ORDER BY alias_path",
                &project_ids,
            )
            .await
        else {
            return projects
                .iter()
                .cloned()
                .map(|project| ProjectRegistryContext {
                    project,
                    aliases: Vec::new(),
                    stores: Vec::new(),
                })
                .collect();
        };
        while let Ok(Some(row)) = rows.next().await {
            let alias = ProjectAliasRecord {
                alias_path: row.get(0).unwrap_or_default(),
                project_id: row.get(1).unwrap_or_default(),
                last_seen_at: row.get(2).unwrap_or_default(),
            };
            aliases_by_project
                .entry(alias.project_id.clone())
                .or_default()
                .push(alias);
        }

        let mut stores = Vec::new();
        if let Some(mut rows) = self
            .query_string_ids(
                "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                        manifest_relpath, created_at, last_verified_at, last_write_at
                 FROM store_instances
                 WHERE project_id IN ({})
                 ORDER BY COALESCE(last_verified_at, created_at) DESC, store_id",
                &project_ids,
            )
            .await
        {
            while let Ok(Some(row)) = rows.next().await {
                if let Some(store) = row_to_store_instance(&row, 0) {
                    stores.push(store);
                }
            }
        }
        let store_ids = stores
            .iter()
            .map(|store| store.store_id.clone())
            .collect::<Vec<_>>();
        let mut graph_scopes_by_store = BTreeMap::<String, Vec<GraphScopeRecord>>::new();
        let mut artifacts_by_store = BTreeMap::<String, Vec<StoreArtifactRecord>>::new();
        if !store_ids.is_empty() {
            if let Some(mut rows) = self
                .query_string_ids(
                    "SELECT graph_scope_id, project_id, store_id, branch_name, db_relpath,
                            parent_scope_id, last_synced_at, writable
                     FROM graph_scopes
                     WHERE store_id IN ({})
                     ORDER BY branch_name, graph_scope_id",
                    &store_ids,
                )
                .await
            {
                while let Ok(Some(row)) = rows.next().await {
                    if let Some(scope) = row_to_graph_scope(&row, 0) {
                        graph_scopes_by_store
                            .entry(scope.store_id.clone())
                            .or_default()
                            .push(scope);
                    }
                }
            }
            if let Some(mut rows) = self
                .query_string_ids(
                    "SELECT store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
                     FROM store_artifacts
                     WHERE store_id IN ({})
                     ORDER BY artifact_kind, relpath",
                    &store_ids,
                )
                .await
            {
                while let Ok(Some(row)) = rows.next().await {
                    if let Some(artifact) = row_to_store_artifact(&row, 0) {
                        artifacts_by_store
                            .entry(artifact.store_id.clone())
                            .or_default()
                            .push(artifact);
                    }
                }
            }
        }
        let mut stores_by_project = BTreeMap::<String, Vec<ProjectStoreContext>>::new();
        for store in stores {
            stores_by_project
                .entry(store.project_id.clone())
                .or_default()
                .push(ProjectStoreContext {
                    graph_scopes: graph_scopes_by_store
                        .remove(&store.store_id)
                        .unwrap_or_default(),
                    artifacts: artifacts_by_store
                        .remove(&store.store_id)
                        .unwrap_or_default(),
                    store,
                });
        }

        let mut contexts = Vec::with_capacity(projects.len());
        for project in projects {
            contexts.push(ProjectRegistryContext {
                project: project.clone(),
                aliases: aliases_by_project
                    .remove(&project.project_id)
                    .unwrap_or_default(),
                stores: stores_by_project
                    .remove(&project.project_id)
                    .unwrap_or_default(),
            });
        }
        contexts
    }

    async fn query_string_ids(&self, sql_template: &str, ids: &[String]) -> Option<libsql::Rows> {
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = sql_template.replace("{}", &placeholders);
        let values = ids
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect::<Vec<_>>();
        self.conn
            .query(&sql, libsql::params_from_iter(values))
            .await
            .ok()
    }

    pub async fn project_registry_context_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<ProjectRegistryContext> {
        let alias = Self::canonical_project_key(alias_path);
        self.project_registry_context_by_alias_key(&alias).await
    }

    pub async fn project_registry_context_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Option<ProjectRegistryContext> {
        let project_id = self
            .project_id_by_identity(project_root, git_common_dir)
            .await?;
        self.project_registry_context_by_id(&project_id).await
    }

    async fn project_registry_context_by_alias_key(
        &self,
        alias: &str,
    ) -> Option<ProjectRegistryContext> {
        let project_id = self.project_id_by_alias_key(alias).await?;
        self.project_registry_context_by_id(&project_id).await
    }

    async fn project_id_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Option<String> {
        match crate::storage::read_repository_identity_marker(project_root) {
            Ok(Some(marker)) => return Some(marker.project_id),
            Ok(None) => {}
            Err(_) => return None,
        }
        for alias in project_identity_aliases(project_root, git_common_dir) {
            if let Some(project_id) = self.project_id_by_alias_key(&alias).await {
                return Some(project_id);
            }
        }
        None
    }

    async fn project_id_by_alias_key(&self, alias: &str) -> Option<String> {
        let mut rows = self
            .conn
            .query(
                "SELECT project_id FROM project_aliases WHERE alias_path = ?1",
                params![alias],
            )
            .await
            .ok()?;
        rows.next().await.ok()??.get(0).ok()
    }

    pub async fn get_code_project(&self, project_id: &str) -> Option<CodeProjectRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT project_id, canonical_root, display_root, git_common_dir,
                        git_remote_url, default_branch, created_at, last_seen_at
                 FROM code_projects WHERE project_id = ?1",
                params![project_id],
            )
            .await
            .ok()?;
        row_to_code_project(&rows.next().await.ok()??, 0)
    }

    async fn list_aliases_for_project(&self, project_id: &str) -> Vec<ProjectAliasRecord> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT alias_path, project_id, last_seen_at
                 FROM project_aliases WHERE project_id = ?1
                 ORDER BY alias_path",
                params![project_id],
            )
            .await
        else {
            return Vec::new();
        };
        let mut aliases = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            aliases.push(ProjectAliasRecord {
                alias_path: row.get(0).unwrap_or_default(),
                project_id: row.get(1).unwrap_or_default(),
                last_seen_at: row.get(2).unwrap_or_default(),
            });
        }
        aliases
    }

    async fn list_store_contexts_for_project(&self, project_id: &str) -> Vec<ProjectStoreContext> {
        let stores = {
            let Ok(mut rows) = self
                .conn
                .query(
                    "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                            manifest_relpath, created_at, last_verified_at, last_write_at
                     FROM store_instances WHERE project_id = ?1
                     ORDER BY COALESCE(last_verified_at, created_at) DESC, store_id",
                    params![project_id],
                )
                .await
            else {
                return Vec::new();
            };
            let mut stores = Vec::new();
            while let Ok(Some(row)) = rows.next().await {
                if let Some(store) = row_to_store_instance(&row, 0) {
                    stores.push(store);
                }
            }
            stores
        };
        let mut contexts = Vec::new();
        for store in stores {
            contexts.push(ProjectStoreContext {
                graph_scopes: self.list_graph_scopes_for_store(&store.store_id).await,
                artifacts: self.list_store_artifacts(&store.store_id).await,
                store,
            });
        }
        contexts
    }

    async fn get_store_instance(&self, store_id: &str) -> Option<StoreInstanceRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                        manifest_relpath, created_at, last_verified_at, last_write_at
                 FROM store_instances WHERE store_id = ?1",
                params![store_id],
            )
            .await
            .ok()?;
        row_to_store_instance(&rows.next().await.ok()??, 0)
    }

    async fn get_graph_scope(&self, graph_scope_id: &str) -> Option<GraphScopeRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT graph_scope_id, project_id, store_id, branch_name, db_relpath,
                        parent_scope_id, last_synced_at, writable
                 FROM graph_scopes WHERE graph_scope_id = ?1",
                params![graph_scope_id],
            )
            .await
            .ok()?;
        row_to_graph_scope(&rows.next().await.ok()??, 0)
    }

    async fn list_graph_scopes_for_store(&self, store_id: &str) -> Vec<GraphScopeRecord> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT graph_scope_id, project_id, store_id, branch_name, db_relpath,
                        parent_scope_id, last_synced_at, writable
                 FROM graph_scopes WHERE store_id = ?1
                 ORDER BY branch_name, graph_scope_id",
                params![store_id],
            )
            .await
        else {
            return Vec::new();
        };
        let mut scopes = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Some(scope) = row_to_graph_scope(&row, 0) {
                scopes.push(scope);
            }
        }
        scopes
    }

    async fn list_store_artifacts(&self, store_id: &str) -> Vec<StoreArtifactRecord> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
                 FROM store_artifacts WHERE store_id = ?1
                 ORDER BY artifact_kind, relpath",
                params![store_id],
            )
            .await
        else {
            return Vec::new();
        };
        let mut artifacts = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Some(artifact) = row_to_store_artifact(&row, 0) {
                artifacts.push(artifact);
            }
        }
        artifacts
    }

    /// Registers or updates a project's tokens-saved count. Best-effort.
    pub async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        let path_str = Self::canonical_project_key(project_path);
        let _ = self
            .conn
            .execute(
                "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET
                    tokens_saved = MAX(tokens_saved, excluded.tokens_saved)",
                params![path_str, tokens_saved as i64],
            )
            .await;
    }

    /// Returns the stored `tokens_saved` count for a specific project, or 0 if not found.
    pub async fn get_project_tokens(&self, project_path: &Path) -> u64 {
        let path_str = Self::canonical_project_key(project_path);
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT tokens_saved FROM projects WHERE path = ?1",
                params![path_str],
            )
            .await
        else {
            return 0;
        };
        match rows.next().await {
            Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0) as u64,
            _ => 0,
        }
    }

    /// Returns the sum of `tokens_saved` across all tracked projects.
    pub async fn global_tokens_saved(&self) -> Option<u64> {
        let mut rows = self
            .conn
            .query("SELECT COALESCE(SUM(tokens_saved), 0) FROM projects", ())
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        let total: i64 = row.get(0).ok()?;
        Some(total as u64)
    }

    /// Insert a new ledger row. Best-effort; errors are reported to stderr via eprintln
    /// but never propagated.
    pub async fn record_savings(
        &self,
        project_path: &str,
        tool_name: &str,
        before_tokens: u64,
        after_tokens: u64,
        ts: i64,
    ) {
        let project_path = Self::canonical_project_key(Path::new(project_path));
        let result = self
            .conn
            .execute(
                "INSERT INTO savings_ledger (ts, project_path, tool_name, before_tokens, after_tokens) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    ts,
                    project_path,
                    tool_name,
                    before_tokens as i64,
                    after_tokens as i64
                ],
            )
            .await;
        if let Err(e) = result {
            eprintln!("[tracedecay] savings_ledger insert failed: {e}");
        }
    }

    /// Sum (before-after) across the ledger entries, with `ts >= since`. Optionally
    /// filter by canonical project path. Returns zeros on any DB error.
    pub async fn sum_savings(&self, project: Option<&str>, since: i64) -> SavingsTotal {
        let project = project.map(|p| Self::canonical_project_key(Path::new(p)));
        let sql_with_project = "SELECT COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), COUNT(*) \
             FROM savings_ledger WHERE project_path = ?1 AND ts >= ?2";
        let sql_all = "SELECT COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), COUNT(*) \
             FROM savings_ledger WHERE ts >= ?1";

        let rows = match project.as_deref() {
            Some(p) => self.conn.query(sql_with_project, params![p, since]).await,
            None => self.conn.query(sql_all, params![since]).await,
        };
        let Ok(mut rows) = rows else {
            return SavingsTotal {
                saved_tokens: 0,
                calls: 0,
            };
        };
        match rows.next().await {
            Ok(Some(row)) => SavingsTotal {
                saved_tokens: row.get::<i64>(0).unwrap_or(0).max(0) as u64,
                calls: row.get::<i64>(1).unwrap_or(0).max(0) as u64,
            },
            _ => SavingsTotal {
                saved_tokens: 0,
                calls: 0,
            },
        }
    }

    /// Group ledger entries by UTC calendar day. Newest-first.
    pub async fn savings_history(&self, project: Option<&str>, since: i64) -> Vec<SavingsDay> {
        let project = project.map(|p| Self::canonical_project_key(Path::new(p)));
        let sql_with_project = "SELECT (ts/86400)*86400 AS day, \
                    COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), \
                    COUNT(*) \
             FROM savings_ledger WHERE project_path = ?1 AND ts >= ?2 \
             GROUP BY day ORDER BY day DESC";
        let sql_all = "SELECT (ts/86400)*86400 AS day, \
                    COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), \
                    COUNT(*) \
             FROM savings_ledger WHERE ts >= ?1 \
             GROUP BY day ORDER BY day DESC";

        let rows = match project.as_deref() {
            Some(p) => self.conn.query(sql_with_project, params![p, since]).await,
            None => self.conn.query(sql_all, params![since]).await,
        };
        let Ok(mut rows) = rows else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            out.push(SavingsDay {
                day: row.get::<i64>(0).unwrap_or(0),
                saved_tokens: row.get::<i64>(1).unwrap_or(0).max(0) as u64,
                calls: row.get::<i64>(2).unwrap_or(0).max(0) as u64,
            });
        }
        out
    }

    /// Ensures the dashboard token-count sidecar table exists.
    ///
    /// Dashboard-scope only: called once when the dashboard opens the global
    /// accounting DB. Deliberately NOT part of the shared `open_at` DDL batch
    /// so project-local session stores keep their schema untouched.
    pub async fn ensure_token_count_cache(&self) -> bool {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS dashboard_token_counts (
                    store TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    text_len INTEGER NOT NULL,
                    encoder TEXT NOT NULL,
                    token_count INTEGER NOT NULL,
                    computed_at INTEGER NOT NULL,
                    PRIMARY KEY (store, provider, message_id)
                )",
            )
            .await
            .is_ok()
    }

    /// Loads every cached token count for one session store. Returns
    /// `(provider, message_id, text_len, token_count)` tuples; empty on any
    /// error (including the sidecar table not existing yet).
    pub async fn load_token_counts(&self, store: &str) -> Vec<(String, String, i64, i64)> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT provider, message_id, text_len, token_count
                 FROM dashboard_token_counts WHERE store = ?1",
                params![store],
            )
            .await
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let (Ok(provider), Ok(message_id), Ok(text_len), Ok(tokens)) = (
                row.get::<String>(0),
                row.get::<String>(1),
                row.get::<i64>(2),
                row.get::<i64>(3),
            ) else {
                continue;
            };
            out.push((provider, message_id, text_len, tokens));
        }
        out
    }

    /// Upserts freshly computed token counts for one session store.
    /// Best-effort: the cache is an optimization, so errors are swallowed.
    pub async fn save_token_counts(&self, store: &str, rows: &[TokenCountUpsert]) {
        let now = crate::tracedecay::current_timestamp();
        for row in rows {
            let _ = self
                .conn
                .execute(
                    "INSERT OR REPLACE INTO dashboard_token_counts
                     (store, provider, message_id, text_len, encoder, token_count, computed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        store,
                        row.provider.as_str(),
                        row.message_id.as_str(),
                        row.text_len,
                        row.encoder,
                        row.token_count,
                        now
                    ],
                )
                .await;
        }
    }

    /// Removes a project's row from the global DB. Best-effort.
    pub async fn delete_project(&self, project_path: &Path) {
        let path_str = Self::canonical_project_key(project_path);
        let _ = self
            .conn
            .execute("DELETE FROM projects WHERE path = ?1", params![path_str])
            .await;
    }

    /// Removes many project rows in a single statement. Returns the number of
    /// rows actually deleted (0 on any error). Best-effort.
    ///
    /// Chunks the input at 256 paths per statement to stay well clear of
    /// `SQLite`'s default 999-parameter limit while still reducing N round trips
    /// to ⌈N/256⌉.
    pub async fn delete_projects(&self, project_paths: &[String]) -> usize {
        const CHUNK: usize = 256;
        let mut total: usize = 0;
        for chunk in project_paths.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
            let sql = format!(
                "DELETE FROM projects WHERE path IN ({})",
                placeholders.join(",")
            );
            let values: Vec<libsql::Value> = chunk
                .iter()
                .map(|p| libsql::Value::Text(Self::canonical_project_key(Path::new(p))))
                .collect();
            if let Ok(n) = self.conn.execute(&sql, values).await {
                total = total.saturating_add(n as usize);
            }
        }
        total
    }

    /// Applies the configured retention windows to the global-database
    /// telemetry tables (`analytics_events`, `session_messages`), deleting
    /// rows older than each table's window. Session data is only touched when
    /// the operator has set an explicit window for it.
    pub async fn prune_global_retention(
        &self,
        config: &crate::retention::RetentionConfig,
        now_secs: i64,
    ) -> crate::errors::Result<Vec<crate::retention::RetentionTableReport>> {
        crate::retention::prune_global_tables(
            &self.conn,
            config,
            crate::retention::RetentionMode::Apply,
            now_secs,
        )
        .await
    }

    /// Dry-run counterpart of [`Self::prune_global_retention`]: reports how
    /// many rows each window *would* remove without deleting anything.
    pub async fn global_retention_report(
        &self,
        config: &crate::retention::RetentionConfig,
        now_secs: i64,
    ) -> crate::errors::Result<Vec<crate::retention::RetentionTableReport>> {
        crate::retention::prune_global_tables(
            &self.conn,
            config,
            crate::retention::RetentionMode::DryRun,
            now_secs,
        )
        .await
    }

    /// Returns all tracked project paths.
    pub async fn list_project_paths(&self) -> Vec<String> {
        let Ok(mut rows) = self.conn.query("SELECT path FROM projects", ()).await else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(path) = row.get::<String>(0) {
                paths.push(path);
            }
        }
        paths
    }

    /// Returns filesystem aliases from the modern project registry.
    /// Synthetic identity aliases (for example `git-common-dir:...`) are
    /// intentionally excluded because transcript attribution requires paths.
    pub async fn list_project_alias_paths(&self) -> Vec<String> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT alias_path FROM project_aliases ORDER BY alias_path",
                (),
            )
            .await
        else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(path) = row.get::<String>(0)
                && Path::new(&path).is_absolute()
            {
                paths.push(path);
            }
        }
        paths
    }

    /// Inserts or replaces a provider session. Returns `false` on any DB error.
    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        self.conn
            .execute(
                "INSERT INTO sessions
                 (provider, session_id, project_key, project_path, title, started_at, ended_at,
                  transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                  parent_tool_use_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(provider, session_id) DO UPDATE SET
                    project_key = excluded.project_key,
                    project_path = excluded.project_path,
                    title = excluded.title,
                    started_at = excluded.started_at,
                    ended_at = excluded.ended_at,
                    transcript_path = excluded.transcript_path,
                    metadata_json = excluded.metadata_json,
                    parent_session_id = excluded.parent_session_id,
                    is_subagent = excluded.is_subagent,
                    agent_id = excluded.agent_id,
                    parent_tool_use_id = excluded.parent_tool_use_id",
                params![
                    session.provider.as_str(),
                    session.session_id.as_str(),
                    session.project_key.as_str(),
                    session.project_path.as_str(),
                    opt_text(session.title.as_deref()),
                    opt_i64(session.started_at),
                    opt_i64(session.ended_at),
                    opt_text(session.transcript_path.as_deref()),
                    opt_text(session.metadata_json.as_deref()),
                    opt_text(session.parent_session_id.as_deref()),
                    i64::from(session.is_subagent),
                    opt_text(session.agent_id.as_deref()),
                    opt_text(session.parent_tool_use_id.as_deref()),
                ],
            )
            .await
            .is_ok()
    }

    /// Returns a single provider session by its provider-local ID.
    pub async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT provider, session_id, project_key, project_path, title, started_at,
                        ended_at, transcript_path, metadata_json, parent_session_id,
                        is_subagent, agent_id, parent_tool_use_id
                 FROM sessions WHERE provider = ?1 AND session_id = ?2",
                params![provider, session_id],
            )
            .await
            .ok()?;
        row_to_session(&rows.next().await.ok()??)
    }

    pub async fn append_analytics_event(
        &self,
        event: &AnalyticsEventInsert,
    ) -> Result<i64, String> {
        let mut rows = self
            .conn
            .query(
                "INSERT INTO analytics_events
                 (provider, project_id, session_id, timestamp, event_kind, hook_name,
                  tool_name, tool_category, skill_name, hint_category, hint_id, outcome,
                  metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 RETURNING id",
                params![
                    event.provider.as_str(),
                    event.project_id.as_str(),
                    opt_text(event.session_id.as_deref()),
                    event.timestamp,
                    event.event_kind.as_str(),
                    opt_text(event.hook_name.as_deref()),
                    opt_text(event.tool_name.as_deref()),
                    opt_text(event.tool_category.as_deref()),
                    opt_text(event.skill_name.as_deref()),
                    opt_text(event.hint_category.as_deref()),
                    opt_text(event.hint_id.as_deref()),
                    opt_text(event.outcome.as_deref()),
                    opt_text(event.metadata_json.as_deref()),
                ],
            )
            .await
            .map_err(|e| format!("failed to append analytics event: {e}"))?;
        let row = rows
            .next()
            .await
            .map_err(|e| format!("failed to read appended analytics event id: {e}"))?
            .ok_or_else(|| "append analytics event returned no id".to_string())?;
        row.get::<i64>(0)
            .map_err(|e| format!("failed to decode appended analytics event id: {e}"))
    }

    pub async fn append_analytics_events(
        &self,
        events: &[AnalyticsEventInsert],
    ) -> Result<Vec<i64>, String> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        self.conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| format!("failed to begin analytics event batch: {e}"))?;

        let mut ids = Vec::with_capacity(events.len());
        for event in events {
            match self.append_analytics_event(event).await {
                Ok(id) => ids.push(id),
                Err(err) => {
                    let _ = self.conn.execute("ROLLBACK", ()).await;
                    return Err(err);
                }
            }
        }

        if let Err(err) = self.conn.execute("COMMIT", ()).await {
            let _ = self.conn.execute("ROLLBACK", ()).await;
            return Err(format!("failed to commit analytics event batch: {err}"));
        }

        Ok(ids)
    }

    pub async fn session_message_count(&self) -> Result<i64, String> {
        let mut rows = self
            .conn
            .query("SELECT COUNT(*) FROM session_messages", ())
            .await
            .map_err(|e| format!("failed to count session messages: {e}"))?;
        let row = rows
            .next()
            .await
            .map_err(|e| format!("failed to read session message count: {e}"))?
            .ok_or_else(|| "session message count returned no row".to_string())?;
        row.get::<i64>(0)
            .map_err(|e| format!("failed to decode session message count: {e}"))
    }

    pub async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> Result<i64, String> {
        let mut rows = self
            .conn
            .query(
                "SELECT COUNT(*)
                 FROM session_messages m
                 JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
                 WHERE s.project_key = ?1",
                libsql::params![project_key],
            )
            .await
            .map_err(|e| format!("failed to count project session messages: {e}"))?;
        let row = rows
            .next()
            .await
            .map_err(|e| format!("failed to read project session message count: {e}"))?
            .ok_or_else(|| "project session message count returned no row".to_string())?;
        row.get::<i64>(0)
            .map_err(|e| format!("failed to decode project session message count: {e}"))
    }

    /// Session messages for one provider session with `timestamp >= since_ts`,
    /// ordered oldest-first, capped at `limit`. Powers the hint-outcome
    /// correlator's bounded post-hint activity scan. Rows without a timestamp
    /// are excluded because the correlator's horizon is time-anchored.
    pub async fn session_messages_after(
        &self,
        provider: &str,
        session_id: &str,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<SessionActivityRow>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = self
            .conn
            .query(
                "SELECT timestamp, ordinal, kind, tool_names, metadata_json
                 FROM session_messages
                 WHERE provider = ?1 AND session_id = ?2
                   AND timestamp IS NOT NULL AND timestamp >= ?3
                 ORDER BY timestamp, ordinal
                 LIMIT ?4",
                params![
                    provider,
                    session_id,
                    since_ts,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
            )
            .await
            .map_err(|e| format!("failed to query session messages after hint: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("failed to read session messages after hint: {e}"))?
        {
            out.push(SessionActivityRow {
                timestamp: row.get::<Option<i64>>(0).ok().flatten(),
                ordinal: row.get::<i64>(1).unwrap_or_default(),
                kind: row.get::<Option<String>>(2).ok().flatten(),
                tool_names: row.get::<Option<String>>(3).ok().flatten(),
                metadata_json: row.get::<Option<String>>(4).ok().flatten(),
            });
        }
        Ok(out)
    }

    pub async fn query_analytics_events(
        &self,
        query: &AnalyticsEventQuery,
    ) -> Result<Vec<AnalyticsEventRecord>, String> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }

        let mut sql = String::from(
            "SELECT id, provider, project_id, session_id, timestamp, event_kind,
                    hook_name, tool_name, tool_category, skill_name, hint_category,
                    hint_id, outcome, metadata_json
             FROM analytics_events",
        );
        let mut clauses = Vec::new();
        let mut values = Vec::new();
        for (column, value) in [
            ("provider", query.provider.as_deref()),
            ("project_id", query.project_id.as_deref()),
            ("session_id", query.session_id.as_deref()),
            ("event_kind", query.event_kind.as_deref()),
        ] {
            push_optional_analytics_filter(&mut clauses, &mut values, column, value);
        }
        if let Some(since) = query.since {
            values.push(Value::Integer(since));
            clauses.push(format!("timestamp >= ?{}", values.len()));
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        values.push(Value::Integer(
            i64::try_from(query.limit).unwrap_or(i64::MAX),
        ));
        let limit_param = values.len();
        let _ = write!(
            sql,
            " ORDER BY timestamp DESC, id DESC LIMIT ?{limit_param}"
        );

        let mut rows = self
            .conn
            .query(&sql, libsql::params_from_iter(values))
            .await
            .map_err(|e| format!("failed to query analytics events: {e}"))?;
        let mut events = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("failed to read analytics events: {e}"))?
        {
            let event = row_to_analytics_event(&row)
                .ok_or_else(|| "failed to decode analytics event row".to_string())?;
            events.push(event);
        }
        events.reverse();
        Ok(events)
    }

    pub async fn count_analytics_events(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<i64, String> {
        let (sql, values) = analytics_scope_query(
            "SELECT COUNT(*) FROM analytics_events",
            project_id,
            since,
            &[],
        );
        let mut rows = self
            .conn
            .query(&sql, libsql::params_from_iter(values))
            .await
            .map_err(|e| format!("failed to count analytics events: {e}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("failed to read analytics event count: {e}"))?
        else {
            return Ok(0);
        };
        row.get::<i64>(0)
            .map_err(|e| format!("failed to decode analytics event count: {e}"))
    }

    pub async fn query_analytics_tool_counts(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<Vec<AnalyticsToolCounts>, String> {
        let (mut sql, values) = analytics_scope_query(
            "SELECT tool_name,
                    COUNT(*) AS calls,
                    SUM(CASE WHEN outcome = 'error' THEN 1 ELSE 0 END) AS errors
             FROM analytics_events",
            project_id,
            since,
            &[
                "event_kind = 'mcp_tool_call'",
                "tool_name IS NOT NULL",
                "tool_name <> ''",
            ],
        );
        sql.push_str(" GROUP BY tool_name ORDER BY calls DESC, tool_name");
        let mut rows = self
            .conn
            .query(&sql, libsql::params_from_iter(values))
            .await
            .map_err(|e| format!("failed to query analytics tool counts: {e}"))?;
        let mut counts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("failed to read analytics tool counts: {e}"))?
        {
            counts.push(AnalyticsToolCounts {
                tool_name: row
                    .get::<String>(0)
                    .map_err(|e| format!("failed to decode analytics tool name: {e}"))?,
                calls: row
                    .get::<i64>(1)
                    .map_err(|e| format!("failed to decode analytics tool calls: {e}"))?,
                errors: row
                    .get::<i64>(2)
                    .map_err(|e| format!("failed to decode analytics tool errors: {e}"))?,
            });
        }
        Ok(counts)
    }

    pub async fn query_analytics_hint_counts(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<Vec<AnalyticsHintCounts>, String> {
        let (mut sql, values) = analytics_scope_query(
            "SELECT hint_category,
                    SUM(CASE WHEN event_kind IN ('hint_emitted', 'hint_escalated', 'missing_session') THEN 1 ELSE 0 END) AS emitted,
                    SUM(CASE WHEN event_kind = 'hint_outcome' AND LOWER(TRIM(COALESCE(outcome, ''))) = 'acted' THEN 1 ELSE 0 END) AS followed,
                    SUM(CASE WHEN event_kind = 'hint_outcome' AND LOWER(TRIM(COALESCE(outcome, ''))) = 'ignored' THEN 1 ELSE 0 END) AS ignored,
                    SUM(CASE WHEN event_kind LIKE 'suppressed_%' THEN 1 ELSE 0 END) AS suppressed
             FROM analytics_events",
            project_id,
            since,
            &[
                "hint_category IS NOT NULL",
                "hint_category <> ''",
                "(event_kind IN ('hint_emitted', 'hint_escalated', 'missing_session', 'hint_outcome') OR event_kind LIKE 'suppressed_%')",
            ],
        );
        sql.push_str(" GROUP BY hint_category ORDER BY hint_category");
        let mut rows = self
            .conn
            .query(&sql, libsql::params_from_iter(values))
            .await
            .map_err(|e| format!("failed to query analytics hint counts: {e}"))?;
        let mut counts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("failed to read analytics hint counts: {e}"))?
        {
            counts.push(AnalyticsHintCounts {
                category: row
                    .get::<String>(0)
                    .map_err(|e| format!("failed to decode analytics hint category: {e}"))?,
                emitted: row
                    .get::<i64>(1)
                    .map_err(|e| format!("failed to decode analytics emitted count: {e}"))?,
                followed: row
                    .get::<i64>(2)
                    .map_err(|e| format!("failed to decode analytics followed count: {e}"))?,
                ignored: row
                    .get::<i64>(3)
                    .map_err(|e| format!("failed to decode analytics ignored count: {e}"))?,
                suppressed: row
                    .get::<i64>(4)
                    .map_err(|e| format!("failed to decode analytics suppressed count: {e}"))?,
            });
        }
        Ok(counts)
    }

    pub async fn session_tool_usage_rows(
        &self,
        limit: usize,
    ) -> Result<Vec<SessionToolUsageRow>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(tool_names, '') AS tool_names,
                        COALESCE(text, '') AS text,
                        COALESCE(metadata_json, '') AS metadata_json
                 FROM session_messages
                 ORDER BY timestamp, ordinal
                 LIMIT ?1",
                [i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .await
            .map_err(|e| format!("failed to query session tool usage rows: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("failed to read session tool usage rows: {e}"))?
        {
            out.push(SessionToolUsageRow {
                tool_names: row
                    .get::<String>(0)
                    .map_err(|e| format!("failed to decode session tool usage tool_names: {e}"))?,
                text: row
                    .get::<String>(1)
                    .map_err(|e| format!("failed to decode session tool usage text: {e}"))?,
                metadata_json: row.get::<String>(2).map_err(|e| {
                    format!("failed to decode session tool usage metadata_json: {e}")
                })?,
            });
        }
        Ok(out)
    }

    /// Timestamp of the most recent ingested session message, in unix
    /// seconds, or `None` when no timestamped messages exist. Providers store
    /// either seconds or milliseconds; compare the newest value from each unit
    /// bucket after normalization. Each bucket lookup is backed by
    /// `idx_session_messages_timestamp`, keeping scheduler ticks bounded.
    pub async fn latest_session_activity_secs(&self) -> Option<i64> {
        let mut rows = self
            .conn
            .query(
                "WITH latest_seconds AS (
                    SELECT timestamp FROM session_messages
                    WHERE timestamp IS NOT NULL
                      AND timestamp < ?1
                    ORDER BY timestamp DESC
                    LIMIT 1
                 ),
                 latest_millis AS (
                    SELECT timestamp FROM session_messages
                    WHERE timestamp >= ?1
                    ORDER BY timestamp DESC
                    LIMIT 1
                 )
                 SELECT timestamp FROM latest_seconds
                 UNION ALL
                 SELECT timestamp FROM latest_millis",
                [UNIX_TIMESTAMP_MILLIS_THRESHOLD],
            )
            .await
            .ok()?;
        let mut latest: Option<i64> = None;
        while let Some(row) = rows.next().await.ok()? {
            let timestamp = row.get::<i64>(0).ok()?;
            let normalized = if timestamp >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
                timestamp / 1000
            } else {
                timestamp
            };
            latest = Some(latest.map_or(normalized, |current| current.max(normalized)));
        }
        latest
    }

    /// Inserts or replaces a provider message. Returns `false` on any DB error.
    pub async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool {
        if self.conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
            return false;
        }

        if !self.upsert_session_message_in_existing_tx(message).await {
            let _ = self.conn.execute("ROLLBACK", ()).await;
            return false;
        }
        if self.conn.execute("COMMIT", ()).await.is_ok() {
            return true;
        }
        let _ = self.conn.execute("ROLLBACK", ()).await;
        false
    }

    async fn upsert_session_message_in_existing_tx(&self, message: &SessionMessageRecord) -> bool {
        let raw_result = crate::sessions::lcm::raw::upsert_raw_message_with_payload(
            &self.conn,
            &self.storage_root,
            message,
        )
        .await;
        match raw_result {
            Ok(raw) => {
                if !self
                    .upsert_session_message_projection(
                        message,
                        &raw.projection_text,
                        raw.projection_metadata_json.as_deref(),
                    )
                    .await
                {
                    return false;
                }
                self.upsert_lcm_summary_for_transcript_summary(message)
                    .await
            }
            Err(_) => false,
        }
    }

    /// Inserts messages whose `(provider, message_id)` key is absent, leaving
    /// existing rows untouched. Returns inserted row count, or `None` after
    /// rolling back on any database error.
    pub(crate) async fn insert_absent_session_messages(
        &self,
        messages: &[SessionMessageRecord],
    ) -> Option<u64> {
        if messages.is_empty() {
            return Some(0);
        }
        // Do the presence filtering as plain reads *before* taking the write
        // lock. The old code ran the per-message existence probe inside
        // `BEGIN IMMEDIATE`, holding the store's single-writer slot for the
        // whole batch just to discover most rows were already present.
        let present = self.present_session_message_keys(messages).await?;
        let absent: Vec<&SessionMessageRecord> = messages
            .iter()
            .filter(|message| {
                !present.contains(&(message.provider.clone(), message.message_id.clone()))
            })
            .collect();
        if absent.is_empty() {
            return Some(0);
        }

        if self.conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
            return None;
        }
        let mut inserted = 0u64;
        for message in absent {
            // The presence probe ran outside this transaction, so a concurrent
            // live ingest could insert this key in the small TOCTOU window.
            // That is harmless: `upsert_session_message_in_existing_tx` writes
            // through `ON CONFLICT(provider, message_id) DO UPDATE` upserts, and
            // the row re-parsed from the *same* transcript is byte-for-byte what
            // the racing writer stored — the update rewrites identical content
            // rather than clobbering the row with foreign data.
            if !self.upsert_session_message_in_existing_tx(message).await {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                return None;
            }
            inserted += 1;
        }
        if self.conn.execute("COMMIT", ()).await.is_ok() {
            return Some(inserted);
        }
        let _ = self.conn.execute("ROLLBACK", ()).await;
        None
    }

    /// Collects the `(provider, message_id)` keys from `messages` that already
    /// exist in `session_messages`, probing in chunks of 500 ids grouped by
    /// provider. Runs as plain reads *outside* any write transaction so the
    /// presence filtering never holds the store's single writer slot. `None`
    /// on a query error, so the caller aborts rather than treating an
    /// unreadable row as absent.
    async fn present_session_message_keys(
        &self,
        messages: &[SessionMessageRecord],
    ) -> Option<HashSet<(String, String)>> {
        const CHUNK: usize = 500;
        let mut by_provider: HashMap<&str, Vec<&str>> = HashMap::new();
        for message in messages {
            by_provider
                .entry(message.provider.as_str())
                .or_default()
                .push(message.message_id.as_str());
        }
        let mut present: HashSet<(String, String)> = HashSet::new();
        for (provider, ids) in by_provider {
            for chunk in ids.chunks(CHUNK) {
                let placeholders = vec!["?"; chunk.len()].join(", ");
                let sql = format!(
                    "SELECT message_id FROM session_messages
                     WHERE provider = ? AND message_id IN ({placeholders})"
                );
                let mut values: Vec<Value> = Vec::with_capacity(chunk.len() + 1);
                values.push(Value::Text(provider.to_string()));
                for id in chunk {
                    values.push(Value::Text((*id).to_string()));
                }
                let mut rows = self
                    .conn
                    .query(&sql, libsql::params_from_iter(values))
                    .await
                    .ok()?;
                loop {
                    match rows.next().await {
                        Ok(Some(row)) => {
                            if let Ok(message_id) = row.get::<String>(0) {
                                present.insert((provider.to_string(), message_id));
                            }
                        }
                        Ok(None) => break,
                        Err(_) => return None,
                    }
                }
            }
        }
        Some(present)
    }

    async fn upsert_lcm_summary_for_transcript_summary(
        &self,
        message: &SessionMessageRecord,
    ) -> bool {
        if message.kind.as_deref() != Some("summary") {
            return true;
        }
        let Some(metadata_json) = message.metadata_json.as_deref() else {
            return true;
        };
        let Ok(metadata) = serde_json::from_str::<JsonValue>(metadata_json) else {
            return true;
        };
        if metadata.get("source").and_then(JsonValue::as_str) != Some("codex_context_compacted") {
            return true;
        }
        let Ok(sources) = self.transcript_summary_sources(message).await else {
            return false;
        };
        if sources.refs.is_empty() {
            return true;
        }
        let depth = metadata
            .get("codex_compaction_depth")
            .and_then(JsonValue::as_i64)
            .unwrap_or(1)
            .max(1);
        let summary_text = transcript_summary_text(message, &metadata, &sources);
        let mut summary_metadata = metadata.as_object().cloned().unwrap_or_default();
        if summary_metadata
            .get("summary_body")
            .and_then(JsonValue::as_str)
            == Some("encrypted")
            && !sources.excerpts.is_empty()
        {
            summary_metadata.insert(
                "tracedecay_summary_source".to_string(),
                JsonValue::String("visible_transcript_source_messages".to_string()),
            );
            summary_metadata.insert(
                "codex_summary_body".to_string(),
                JsonValue::String("encrypted".to_string()),
            );
        }
        let summary_metadata_json =
            serde_json::to_string(&JsonValue::Object(summary_metadata)).ok();
        let draft = LcmSummaryNodeDraft {
            provider: message.provider.clone(),
            conversation_id: message.session_id.clone(),
            session_id: message.session_id.clone(),
            depth,
            summary_text: summary_text.clone(),
            source_refs: sources.refs,
            summary_token_count: estimate_summary_tokens(&summary_text),
            source_token_count: sources.source_token_count,
            source_time_start: sources.source_time_start,
            source_time_end: sources.source_time_end.or(message.timestamp),
            expand_hint: Some("Codex context compaction boundary".to_string()),
            metadata_json: summary_metadata_json.or_else(|| Some(metadata_json.to_string())),
        };
        crate::sessions::lcm::dag::insert_summary_node_in_transaction(&self.conn, draft)
            .await
            .is_ok()
    }

    async fn transcript_summary_sources(
        &self,
        message: &SessionMessageRecord,
    ) -> Result<TranscriptSummarySources, libsql::Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT r.store_id, r.timestamp,
                        length(COALESCE(r.content, r.snippet_text, '')),
                        r.role,
                        substr(COALESCE(r.content, r.snippet_text, ''), 1, 4000)
                 FROM lcm_raw_messages r
                 JOIN session_messages m
                   ON m.provider = r.provider
                  AND m.message_id = r.message_id
                 WHERE r.provider = ?1
                   AND r.session_id = ?2
                   AND r.ordinal < ?3
                   AND r.ordinal > COALESCE((
                       SELECT MAX(prev.ordinal)
                       FROM session_messages prev
                       WHERE prev.provider = ?1
                         AND prev.session_id = ?2
                         AND prev.ordinal < ?3
                         AND COALESCE(prev.kind, 'message') = 'summary'
                   ), -9223372036854775808)
                   AND COALESCE(m.kind, 'message') <> 'summary'
                 ORDER BY r.store_id",
                params![
                    message.provider.as_str(),
                    message.session_id.as_str(),
                    message.ordinal,
                ],
            )
            .await?;

        let mut refs = Vec::new();
        let mut source_token_count = 0_i64;
        let mut source_time_start = None;
        let mut source_time_end = None;
        let mut excerpts = Vec::new();
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            let timestamp: Option<i64> = row.get(1)?;
            let char_count: i64 = row.get(2)?;
            let role: String = row.get(3)?;
            let excerpt_text: String = row.get(4)?;
            refs.push(LcmSourceRef::RawMessage { store_id });
            source_token_count =
                source_token_count.saturating_add(estimated_tokens_from_chars(char_count));
            if !excerpt_text.trim().is_empty() {
                excerpts.push(TranscriptSummaryExcerpt {
                    role,
                    text: excerpt_text,
                });
            }
            if let Some(timestamp) = timestamp {
                source_time_start = Some(
                    source_time_start.map_or(timestamp, |start| std::cmp::min(start, timestamp)),
                );
                source_time_end =
                    Some(source_time_end.map_or(timestamp, |end| std::cmp::max(end, timestamp)));
            }
        }

        Ok(TranscriptSummarySources {
            refs,
            source_token_count,
            source_time_start,
            source_time_end,
            excerpts,
        })
    }

    /// Atomically upserts one transcript session + all parsed messages and then
    /// advances the parse cursor. Any failure rolls back the entire batch so a
    /// follow-up ingest can safely replay from the previous offset.
    pub async fn upsert_transcript_batch(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> bool {
        self.upsert_transcript_batch_with_git_evidence(
            session,
            messages,
            &[],
            &[],
            parse_offset_path,
            parse_offset,
        )
        .await
    }

    /// Atomically persists transcript rows, direct commit evidence, and the
    /// parse cursor so a failed attribution write is replayed on the next sync.
    pub(crate) async fn upsert_transcript_batch_with_git_evidence(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        commit_records: &[crate::sessions::git_correlation::CommitSessionRecord],
        span_observations: &[crate::sessions::git_correlation::SpanObservation],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> bool {
        let batch = TranscriptBatch {
            session: session.clone(),
            messages: messages.to_vec(),
        };
        self.upsert_transcript_batches_inner(
            std::slice::from_ref(&batch),
            commit_records,
            span_observations,
            parse_offset_path,
            parse_offset,
            TranscriptWriteMode::Full,
        )
        .await
    }

    /// Atomically upserts several transcript sessions (and their messages),
    /// writing only the searchable `session_messages` projection — never
    /// `lcm_raw_messages` — and then advances one shared parse cursor.
    ///
    /// Used by the Hermes `state.db` sweep: Hermes already ingests its raw
    /// conversation losslessly into the LCM store at runtime (the generated
    /// plugin's `lcm_preflight` active-message ingest) and via the one-time
    /// legacy-store migration, under its own message ids. Writing raw rows
    /// again from the transcript sweep would duplicate the LCM store, so
    /// Hermes transcripts only fill the provider-neutral projection. Any
    /// failure rolls back the whole batch so a follow-up ingest can safely
    /// replay from the previous cursor.
    pub async fn upsert_transcript_projection_batches(
        &self,
        batches: &[TranscriptBatch],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> bool {
        self.upsert_transcript_batches_inner(
            batches,
            &[],
            &[],
            parse_offset_path,
            parse_offset,
            TranscriptWriteMode::ProjectionOnly,
        )
        .await
    }

    async fn upsert_transcript_batches_inner(
        &self,
        batches: &[TranscriptBatch],
        commit_records: &[crate::sessions::git_correlation::CommitSessionRecord],
        span_observations: &[crate::sessions::git_correlation::SpanObservation],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
        mode: TranscriptWriteMode,
    ) -> bool {
        if self.conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
            return false;
        }
        for batch in batches {
            if !self.upsert_session(&batch.session).await {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                return false;
            }
            for message in &batch.messages {
                let upserted = match mode {
                    TranscriptWriteMode::Full => {
                        self.upsert_session_message_in_existing_tx(message).await
                    }
                    TranscriptWriteMode::ProjectionOnly => {
                        let text = crate::sessions::lcm::raw::derived_text_for_index(&message.text);
                        self.upsert_session_message_projection(
                            message,
                            &text,
                            message.metadata_json.as_deref(),
                        )
                        .await
                    }
                };
                if !upserted {
                    let _ = self.conn.execute("ROLLBACK", ()).await;
                    return false;
                }
            }
        }
        for record in commit_records {
            if crate::sessions::git_correlation::upsert_commit_session(&self.conn, record)
                .await
                .is_err()
            {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                return false;
            }
        }
        for observation in span_observations {
            if crate::sessions::git_correlation::record_span_observation_in_transaction(
                &self.conn,
                observation,
                crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
            )
            .await
            .is_err()
            {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                return false;
            }
        }
        let cursor_set = match mode {
            TranscriptWriteMode::Full => {
                self.set_parse_offset_in_existing_tx(parse_offset_path, parse_offset)
                    .await
            }
            TranscriptWriteMode::ProjectionOnly => {
                self.set_parse_offset_monotonic_in_existing_tx(parse_offset_path, parse_offset)
                    .await
            }
        };
        if !cursor_set {
            let _ = self.conn.execute("ROLLBACK", ()).await;
            return false;
        }
        if self.conn.execute("COMMIT", ()).await.is_ok() {
            return true;
        }
        let _ = self.conn.execute("ROLLBACK", ()).await;
        false
    }

    async fn upsert_session_message_projection(
        &self,
        message: &SessionMessageRecord,
        text: &str,
        metadata_json: Option<&str>,
    ) -> bool {
        self.conn
            .execute(
                "INSERT INTO session_messages
                 (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
                  tool_names, source_path, source_offset, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(provider, message_id) DO UPDATE SET
                    session_id = excluded.session_id,
                    role = excluded.role,
                    timestamp = excluded.timestamp,
                    ordinal = excluded.ordinal,
                    text = excluded.text,
                    kind = excluded.kind,
                    model = excluded.model,
                    tool_names = excluded.tool_names,
                    source_path = excluded.source_path,
                    source_offset = excluded.source_offset,
                    metadata_json = excluded.metadata_json",
                params![
                    message.provider.as_str(),
                    message.message_id.as_str(),
                    message.session_id.as_str(),
                    message.role.as_str(),
                    opt_i64(message.timestamp),
                    message.ordinal,
                    text,
                    opt_text(message.kind.as_deref()),
                    opt_text(message.model.as_deref()),
                    opt_text(message.tool_names.as_deref()),
                    opt_text(message.source_path.as_deref()),
                    opt_i64(message.source_offset),
                    opt_text(metadata_json),
                ],
            )
            .await
            .is_ok()
    }

    /// Returns a single provider message by its provider-local ID.
    pub async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<SessionMessageRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                        model, tool_names, source_path, source_offset, metadata_json
                 FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                params![provider, message_id],
            )
            .await
            .ok()?;
        row_to_message(&rows.next().await.ok()??, 0)
    }

    /// Returns the current LCM schema version recorded for this session DB.
    ///
    /// Intentional integration-test seam for verifying migrations without
    /// routing through MCP/tool handlers.
    pub async fn lcm_schema_version(&self) -> Option<i64> {
        crate::sessions::lcm::schema::schema_version(&self.conn).await
    }

    /// Loads a raw LCM message by provider and provider-local message ID.
    ///
    /// Intentional integration-test seam for asserting raw-store fidelity while
    /// keeping production callers on higher-level LCM query APIs.
    pub async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<crate::sessions::lcm::LcmRawMessage> {
        crate::sessions::lcm::schema::load_raw_message(&self.conn, provider, message_id).await
    }

    /// Loads ordered raw LCM messages for one session with stable store-id pagination.
    pub async fn lcm_load_session(
        &self,
        request: crate::sessions::lcm::LcmLoadSessionRequest,
    ) -> Result<crate::sessions::lcm::LcmLoadSessionPage, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::load_session(&self.conn, request).await
    }

    /// Lists sessions in the raw LCM store ordered by most recent activity.
    ///
    /// `provider = None` spans all providers. Used to select "recently
    /// active" sessions for automation session-replay evidence.
    pub async fn lcm_recent_sessions(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::sessions::lcm::LcmRecentSession>, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::recent_sessions(&self.conn, provider, limit).await
    }

    /// Lists providers that have raw LCM messages for an explicit session id.
    pub async fn lcm_session_providers(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::session_providers(&self.conn, session_id).await
    }

    /// Loads a bounded turn-ordered replay slice (head/tail turns plus top
    /// summary-DAG nodes) for one session.
    pub async fn lcm_session_replay_slice(
        &self,
        request: &crate::sessions::lcm::LcmSessionReplayRequest,
    ) -> Result<crate::sessions::lcm::LcmSessionReplaySlice, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::session_replay_slice(&self.conn, request).await
    }

    /// Searches bounded LCM raw snippets and, optionally, summary node text.
    pub async fn lcm_grep(
        &self,
        request: crate::sessions::lcm::LcmGrepRequest,
    ) -> Result<crate::sessions::lcm::LcmGrepOutcome, crate::sessions::lcm::LcmError> {
        self.lcm_grep_filtered(request, crate::sessions::lcm::LcmGrepFilters::default())
            .await
    }

    /// Searches LCM with query-only relationship and semantic message filters.
    pub async fn lcm_grep_filtered(
        &self,
        request: crate::sessions::lcm::LcmGrepRequest,
        filters: crate::sessions::lcm::LcmGrepFilters,
    ) -> Result<crate::sessions::lcm::LcmGrepOutcome, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::grep(&self.conn, request, filters).await
    }

    /// Expands a raw message, summary node, or external payload with content range metadata.
    pub async fn lcm_expand(
        &self,
        request: crate::sessions::lcm::LcmExpandRequest,
    ) -> Result<crate::sessions::lcm::LcmExpandResponse, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::expand(&self.conn, &self.storage_root, request).await
    }

    /// Assembles bounded LCM retrieval context for host-side query synthesis.
    pub async fn lcm_expand_query(
        &self,
        request: crate::sessions::lcm::LcmExpandQueryRequest,
    ) -> Result<crate::sessions::lcm::LcmExpandQueryResponse, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::expand_query(&self.conn, request).await
    }

    /// Describes a session's LCM raw-message and summary-DAG shape without payload bodies.
    pub async fn lcm_describe(
        &self,
        request: crate::sessions::lcm::LcmDescribeRequest,
    ) -> Result<crate::sessions::lcm::LcmDescribeResponse, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::describe(&self.conn, request).await
    }

    /// Returns Codex compaction summary nodes that still need an auxiliary
    /// Codex app-server summary.
    pub async fn pending_codex_compaction_summary_requests(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PendingCodexCompactionSummary>, crate::sessions::lcm::LcmError> {
        let limit = limit.clamp(1, 100) as i64;
        let mut sql = String::from(
            "SELECT node_id, session_id
             FROM lcm_summary_nodes
             WHERE provider = 'codex'
               AND json_extract(metadata_json, '$.source') = 'codex_context_compacted'
               AND COALESCE(
                     json_extract(metadata_json, '$.tracedecay_summary_source'),
                     ''
                   ) <> 'codex_app_server'",
        );
        let mut query_params = vec![Value::Integer(limit)];
        if let Some(session_id) = session_id {
            sql.push_str(" AND session_id = ?2 ORDER BY depth DESC, created_at DESC LIMIT ?1");
            query_params.push(Value::Text(session_id.to_string()));
        } else {
            sql.push_str(" ORDER BY created_at DESC, depth DESC LIMIT ?1");
        }

        let mut rows = self.conn.query(&sql, query_params).await?;
        let mut pending = Vec::new();
        while let Some(row) = rows.next().await? {
            let node_id: String = row.get(0)?;
            let row_session_id: String = row.get(1)?;
            if let Some(request) = self
                .codex_compaction_summary_request_for_node(&node_id, &row_session_id)
                .await?
            {
                pending.push(PendingCodexCompactionSummary { node_id, request });
            }
        }
        Ok(pending)
    }

    async fn codex_compaction_summary_request_for_node(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<Option<LcmSummaryRequest>, crate::sessions::lcm::LcmError> {
        let mut rows = self
            .conn
            .query(
                "SELECT r.store_id, r.role, COALESCE(r.content, r.snippet_text, '')
                 FROM lcm_summary_sources s
                 JOIN lcm_raw_messages r
                   ON s.source_kind = 'raw_message'
                  AND CAST(s.source_id AS INTEGER) = r.store_id
                 WHERE s.node_id = ?1
                   AND r.provider = 'codex'
                   AND r.session_id = ?2
                 ORDER BY s.ordinal",
                params![node_id, session_id],
            )
            .await?;
        let mut source_messages = Vec::new();
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            let role: String = row.get(1)?;
            let content: String = row.get(2)?;
            source_messages.push(LcmSummarySourceMessage {
                store_id,
                role,
                content,
            });
        }
        let (Some(first), Some(last)) = (source_messages.first(), source_messages.last()) else {
            return Ok(None);
        };
        Ok(Some(LcmSummaryRequest {
            provider: "codex".to_string(),
            session_id: session_id.to_string(),
            focus_topic: Some("Codex context compaction".to_string()),
            prompt: CODEX_COMPACTION_SUMMARY_PROMPT.to_string(),
            source_range: LcmSummarySourceRange {
                from_store_id: first.store_id,
                to_store_id: last.store_id,
            },
            source_messages,
            extraction_request: None,
        }))
    }

    /// Replaces a deterministic Codex compaction placeholder summary with an
    /// auxiliary summary while preserving the exact source lineage.
    pub async fn replace_codex_compaction_summary(
        &self,
        node_id: &str,
        summary_text: &str,
        route: &str,
        model: Option<&str>,
    ) -> Result<LcmSummaryNode, crate::sessions::lcm::LcmError> {
        let mut draft = self.codex_compaction_summary_draft(node_id).await?;
        if draft.provider != "codex" {
            return Err(crate::sessions::lcm::LcmError::SummaryNodeNotFound);
        }
        let mut metadata: serde_json::Map<String, JsonValue> = draft
            .metadata_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if metadata.get("source").and_then(JsonValue::as_str) != Some("codex_context_compacted") {
            return Err(crate::sessions::lcm::LcmError::SummaryNodeNotFound);
        }
        draft.summary_text = summary_text.trim().to_string();
        draft.summary_token_count = estimate_summary_tokens(&draft.summary_text);
        metadata.insert(
            "tracedecay_summary_source".to_string(),
            JsonValue::String(route.to_string()),
        );
        if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
            metadata.insert(
                "codex_auxiliary_model".to_string(),
                JsonValue::String(model.trim().to_string()),
            );
        }
        draft.metadata_json = Some(JsonValue::Object(metadata).to_string());

        self.conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            self.conn
                .execute(
                    "DELETE FROM lcm_summary_sources WHERE node_id = ?1",
                    params![node_id],
                )
                .await?;
            self.conn
                .execute(
                    "DELETE FROM lcm_summary_nodes WHERE node_id = ?1",
                    params![node_id],
                )
                .await?;
            crate::sessions::lcm::dag::insert_summary_node_in_transaction(&self.conn, draft).await
        }
        .await;
        match result {
            Ok(node) => {
                self.conn.execute("COMMIT", ()).await?;
                Ok(node)
            }
            Err(err) => {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
        }
    }

    async fn codex_compaction_summary_draft(
        &self,
        node_id: &str,
    ) -> Result<LcmSummaryNodeDraft, crate::sessions::lcm::LcmError> {
        let mut rows = self
            .conn
            .query(
                "SELECT provider, conversation_id, session_id, depth, summary_text,
                        summary_token_count, source_token_count, source_time_start,
                        source_time_end, expand_hint, metadata_json
                 FROM lcm_summary_nodes
                 WHERE node_id = ?1",
                params![node_id],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(crate::sessions::lcm::LcmError::SummaryNodeNotFound)?;
        let source_refs = self.summary_source_refs(node_id).await?;
        Ok(LcmSummaryNodeDraft {
            provider: row.get(0)?,
            conversation_id: row.get(1)?,
            session_id: row.get(2)?,
            depth: row.get(3)?,
            summary_text: row.get(4)?,
            summary_token_count: row.get(5)?,
            source_token_count: row.get(6)?,
            source_time_start: row.get(7)?,
            source_time_end: row.get(8)?,
            expand_hint: row.get(9)?,
            metadata_json: row.get(10)?,
            source_refs,
        })
    }

    async fn summary_source_refs(
        &self,
        node_id: &str,
    ) -> Result<Vec<LcmSourceRef>, crate::sessions::lcm::LcmError> {
        let mut rows = self
            .conn
            .query(
                "SELECT source_kind, source_id
                 FROM lcm_summary_sources
                 WHERE node_id = ?1
                 ORDER BY ordinal",
                params![node_id],
            )
            .await?;
        let mut refs = Vec::new();
        while let Some(row) = rows.next().await? {
            let source_kind: String = row.get(0)?;
            let source_id: String = row.get(1)?;
            match source_kind.as_str() {
                "raw_message" => refs.push(LcmSourceRef::RawMessage {
                    store_id: source_id.parse().map_err(|err| {
                        crate::sessions::lcm::LcmError::Db(format!(
                            "invalid raw message source id '{source_id}': {err}"
                        ))
                    })?,
                }),
                "summary_node" => refs.push(LcmSourceRef::SummaryNode { node_id: source_id }),
                _ => {
                    return Err(crate::sessions::lcm::LcmError::Db(format!(
                        "invalid summary source kind '{source_kind}'"
                    )));
                }
            }
        }
        Ok(refs)
    }

    /// Reports LCM schema, storage, payload, and currently implemented maintenance counts.
    pub async fn lcm_status(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<crate::sessions::lcm::LcmStatus, crate::sessions::lcm::LcmError> {
        self.lcm_status_with_options(
            provider,
            session_id,
            false,
            &crate::sessions::lcm::LcmGcConfig::default(),
        )
        .await
    }

    pub async fn lcm_status_with_options(
        &self,
        provider: &str,
        session_id: Option<&str>,
        deep: bool,
        gc_config: &crate::sessions::lcm::LcmGcConfig,
    ) -> Result<crate::sessions::lcm::LcmStatus, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::query::status(
            &self.conn,
            &self.storage_root,
            provider,
            session_id,
            deep,
            gc_config,
        )
        .await
    }

    /// Runs LCM doctor diagnostics and safe repair planning/apply actions.
    pub async fn lcm_doctor(
        &self,
        provider: &str,
        session_id: Option<&str>,
        mode: &str,
        apply: bool,
        clean_config: crate::sessions::lcm::LcmCleanConfig,
        gc_config: crate::sessions::lcm::LcmGcConfig,
    ) -> Result<serde_json::Value, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::doctor::doctor(
            &self.conn,
            crate::sessions::lcm::doctor::DoctorRequest {
                storage_root: &self.storage_root,
                db_path: &self.db_path,
                provider,
                session_id,
                mode,
                apply,
                clean_config,
                gc_config,
            },
        )
        .await
    }

    /// Updates durable LCM lifecycle/frontier state and replaces maintenance debt.
    ///
    /// Intentional integration-test seam for lifecycle state setup; production
    /// code updates this state through LCM preflight/compression flows.
    pub async fn lcm_update_lifecycle(
        &self,
        update: crate::sessions::lcm::LcmLifecycleUpdate,
    ) -> Result<crate::sessions::lcm::LcmLifecycleState, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::compression::update_lifecycle(&self.conn, update).await
    }

    /// Loads durable LCM lifecycle/frontier state for a provider conversation.
    ///
    /// Intentional integration-test seam for verifying lifecycle persistence
    /// without coupling tests to compression internals.
    pub async fn lcm_lifecycle_state(
        &self,
        provider: &str,
        conversation_id: &str,
    ) -> Result<crate::sessions::lcm::LcmLifecycleState, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::compression::lifecycle_state(&self.conn, provider, conversation_id)
            .await
    }

    /// Records a compression-boundary session start; a skipped carry-over
    /// starts the durable compression cooldown for the new session.
    pub async fn lcm_session_boundary(
        &self,
        request: crate::sessions::lcm::LcmSessionBoundaryRequest,
    ) -> Result<crate::sessions::lcm::LcmSessionBoundaryResponse, crate::sessions::lcm::LcmError>
    {
        crate::sessions::lcm::compression::record_session_boundary(&self.conn, request).await
    }

    /// Ingests active messages and reports whether deterministic replay changed.
    pub async fn lcm_preflight(
        &self,
        request: crate::sessions::lcm::LcmPreflightRequest,
    ) -> Result<crate::sessions::lcm::LcmPreflightResponse, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::compression::preflight(&self.conn, &self.storage_root, request).await
    }

    /// Runs deterministic LCM compression without invoking an auxiliary LLM.
    pub async fn lcm_compress(
        &self,
        request: crate::sessions::lcm::LcmCompressionRequest,
    ) -> Result<crate::sessions::lcm::LcmCompressionResponse, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::compression::compress(&self.conn, &self.storage_root, request).await
    }

    /// Returns an LCM store bound to an explicit storage root for payload files.
    ///
    /// Intentional integration-test seam for ingesting payload-backed messages
    /// with temporary storage roots.
    pub fn lcm_store(
        &self,
        storage_root: impl AsRef<Path>,
    ) -> crate::sessions::lcm::payload::LcmStore<'_> {
        crate::sessions::lcm::payload::LcmStore::new(
            &self.conn,
            storage_root.as_ref().to_path_buf(),
        )
    }

    /// Inserts or updates an LCM summary node and its ordered source lineage.
    ///
    /// Intentional integration-test seam for constructing DAG fixtures without
    /// invoking the summarization pipeline.
    pub async fn lcm_insert_summary_node(
        &self,
        draft: crate::sessions::lcm::LcmSummaryNodeDraft,
    ) -> Result<crate::sessions::lcm::LcmSummaryNode, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::dag::insert_summary_node(&self.conn, draft).await
    }

    /// Expands one summary node to its direct raw-message or summary-node sources.
    ///
    /// Intentional integration-test seam for asserting DAG lineage expansion
    /// directly while production callers use `lcm_expand`.
    pub async fn lcm_expand_summary_node(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> Result<crate::sessions::lcm::LcmSummaryExpansion, crate::sessions::lcm::LcmError> {
        crate::sessions::lcm::dag::expand_summary_node(&self.conn, provider, session_id, node_id)
            .await
    }

    // ── Session ↔ git correlation ────────────────────────────────────

    /// Folds one live/backfilled git observation into the span table.
    /// See [`crate::sessions::git_correlation::record_span_observation`].
    pub async fn git_record_span_observation(
        &self,
        observation: &crate::sessions::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64, crate::sessions::git_correlation::GitCorrelationError> {
        crate::sessions::git_correlation::record_span_observation(
            &self.conn,
            observation,
            merge_gap_secs,
        )
        .await
    }

    /// Attributes one commit to one session (idempotent).
    /// See [`crate::sessions::git_correlation::upsert_commit_session`].
    pub async fn git_upsert_commit_session(
        &self,
        record: &crate::sessions::git_correlation::CommitSessionRecord,
    ) -> Result<bool, crate::sessions::git_correlation::GitCorrelationError> {
        crate::sessions::git_correlation::upsert_commit_session(&self.conn, record).await
    }

    /// Runs the commit-attribution sweep, delegating branch-scoped git log
    /// reads to `scan`. See
    /// [`crate::sessions::git_correlation::run_commit_attribution_sweep`].
    pub async fn git_run_commit_attribution_sweep<F>(
        &self,
        gap_secs: i64,
        scan: F,
    ) -> Result<usize, crate::sessions::git_correlation::GitCorrelationError>
    where
        F: FnMut(
            &crate::sessions::git_correlation::SpanScanTarget,
        ) -> Vec<crate::sessions::git_correlation::ScannedCommit>,
    {
        crate::sessions::git_correlation::run_commit_attribution_sweep(&self.conn, gap_secs, scan)
            .await
    }

    /// Returns sessions correlated with a branch, worktree, or commit.
    /// See [`crate::sessions::git_correlation::sessions_for`].
    pub async fn git_sessions_for(
        &self,
        query: &crate::sessions::git_correlation::SessionsForQuery,
    ) -> Result<
        Vec<crate::sessions::git_correlation::SessionGitCorrelationHit>,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        crate::sessions::git_correlation::sessions_for(&self.conn, query).await
    }

    /// Returns sessions for a git ref with an explicit commit relationship
    /// selector. Branch and worktree queries are unaffected by the selector.
    pub async fn git_sessions_for_with_relation(
        &self,
        query: &crate::sessions::git_correlation::SessionsForQuery,
        relation: crate::sessions::git_correlation::CommitRelationFilter,
    ) -> Result<
        Vec<crate::sessions::git_correlation::SessionGitCorrelationHit>,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        crate::sessions::git_correlation::sessions_for_with_relation(&self.conn, query, relation)
            .await
    }

    /// Reports the per-project session↔git correlation index health (span/commit
    /// counts, last write, auto-backfill watermark).
    /// See [`crate::sessions::git_correlation::correlation_index_health`].
    pub async fn git_correlation_index_health(
        &self,
    ) -> Result<
        crate::sessions::git_correlation::CorrelationIndexHealth,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        crate::sessions::git_correlation::correlation_index_health(&self.conn).await
    }

    /// Reads one `git_correlation_meta` integer value (e.g. the auto-backfill
    /// watermark). Used by the incremental backfill to resume where it left off.
    pub async fn git_correlation_meta_get(
        &self,
        key: &str,
    ) -> Result<Option<i64>, crate::sessions::git_correlation::GitCorrelationError> {
        crate::sessions::git_correlation::read_meta_value(&self.conn, key).await
    }

    /// Writes one `git_correlation_meta` integer value (upsert).
    pub async fn git_correlation_meta_set(
        &self,
        key: &str,
        value: i64,
    ) -> Result<(), crate::sessions::git_correlation::GitCorrelationError> {
        crate::sessions::git_correlation::write_meta_value(&self.conn, key, value).await
    }

    /// Runs one bounded, idempotent incremental git-span backfill pass, advancing
    /// the persisted watermark.
    /// See [`crate::sessions::git_correlation::run_incremental_backfill`].
    pub async fn git_run_incremental_backfill(
        &self,
        git: &dyn crate::sessions::git_correlation::GitReflogSource,
        limit_sessions: usize,
    ) -> Result<
        crate::sessions::git_correlation::BackfillStats,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        crate::sessions::git_correlation::run_incremental_backfill(self, git, limit_sessions).await
    }

    /// Resolves the `(provider, session_id)` pairs matching a git-scope filter.
    /// See [`crate::sessions::git_correlation::session_ids_for_scope`].
    pub async fn git_session_ids_for_scope(
        &self,
        filter: &crate::sessions::git_correlation::GitScopeFilter,
    ) -> Result<Option<Vec<(String, String)>>, crate::sessions::git_correlation::GitCorrelationError>
    {
        crate::sessions::git_correlation::session_ids_for_scope(&self.conn, filter).await
    }

    // ── Workflow-run index ───────────────────────────────────────────

    /// Inserts or updates one indexed workflow run (idempotent on `run_id`).
    /// See [`crate::sessions::workflow_index::upsert_run`].
    pub async fn workflow_upsert_run(
        &self,
        run: &crate::sessions::workflow_index::WorkflowRun,
    ) -> Result<(), crate::sessions::workflow_index::WorkflowIndexError> {
        crate::sessions::workflow_index::upsert_run(&self.conn, run).await
    }

    /// Inserts or updates one workflow agent (idempotent on
    /// `(run_id, agent_label, agent_id)`).
    /// See [`crate::sessions::workflow_index::upsert_agent`].
    pub async fn workflow_upsert_agent(
        &self,
        agent: &crate::sessions::workflow_index::WorkflowAgent,
    ) -> Result<(), crate::sessions::workflow_index::WorkflowIndexError> {
        crate::sessions::workflow_index::upsert_agent(&self.conn, agent).await
    }

    /// Lists workflow runs spawned by one parent session, newest-first.
    /// See [`crate::sessions::workflow_index::runs_for_session`].
    pub async fn workflow_runs_for_session(
        &self,
        parent_session_id: &str,
        limit: usize,
    ) -> Result<
        Vec<crate::sessions::workflow_index::WorkflowRun>,
        crate::sessions::workflow_index::WorkflowIndexError,
    > {
        crate::sessions::workflow_index::runs_for_session(&self.conn, parent_session_id, limit)
            .await
    }

    /// Fetches one workflow run by its `wf_*` id.
    /// See [`crate::sessions::workflow_index::run_for_id`].
    pub async fn workflow_run_for_id(
        &self,
        run_id: &str,
    ) -> Result<
        Option<crate::sessions::workflow_index::WorkflowRun>,
        crate::sessions::workflow_index::WorkflowIndexError,
    > {
        crate::sessions::workflow_index::run_for_id(&self.conn, run_id).await
    }

    /// Lists the agents of one workflow run in phase order.
    /// See [`crate::sessions::workflow_index::agents_for_run`].
    pub async fn workflow_agents_for_run(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<
        Vec<crate::sessions::workflow_index::WorkflowAgent>,
        crate::sessions::workflow_index::WorkflowIndexError,
    > {
        crate::sessions::workflow_index::agents_for_run(&self.conn, run_id, limit).await
    }

    /// Lists workflow runs that ran on a git branch/worktree/commit, joined
    /// through their parent session's git spans.
    /// See [`crate::sessions::workflow_index::runs_for_git_scope`].
    pub async fn workflow_runs_for_git_scope(
        &self,
        filter: &crate::sessions::git_correlation::GitScopeFilter,
        limit: usize,
    ) -> Result<
        Vec<crate::sessions::workflow_index::WorkflowRun>,
        crate::sessions::workflow_index::WorkflowIndexError,
    > {
        crate::sessions::workflow_index::runs_for_git_scope(&self.conn, filter, limit).await
    }

    /// Lists per-session activity windows for the historical git-correlation
    /// backfill: each row carries the session's declared `started_at`/`ended_at`
    /// plus the min/max `session_messages.timestamp`, so the caller can derive
    /// coarse activity windows without a second query per session. Ordered
    /// newest-first (by the latest known activity), capped at `limit`.
    pub async fn session_activity_rows(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::sessions::git_correlation::SessionActivityRow>, String> {
        crate::sessions::git_correlation::session_activity_rows(&self.conn, limit).await
    }

    /// Lists session activity windows whose activity timestamp is strictly newer
    /// than `since_exclusive`, oldest-first and capped at `limit`. Backs the
    /// incremental auto-backfill's watermark-advancing passes.
    pub async fn session_activity_rows_since(
        &self,
        since_exclusive: i64,
        limit: usize,
    ) -> Result<Vec<crate::sessions::git_correlation::SessionActivityRow>, String> {
        crate::sessions::git_correlation::session_activity_rows_since(
            &self.conn,
            since_exclusive,
            limit,
        )
        .await
    }

    /// Searches message text for a provider, optionally constrained to one project.
    pub async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered(
            provider,
            project_key,
            query,
            limit,
            SessionSearchFilters::default(),
        )
        .await
    }

    /// Searches message text with optional parent/subagent relationship filters.
    pub async fn search_session_messages_filtered(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered_inner(
            Some(provider),
            project_key,
            query,
            limit,
            filters,
            None,
            None,
        )
        .await
    }

    /// Like [`Self::search_session_messages_filtered`], additionally scoping
    /// hits to sessions correlated with a git branch/worktree/commit via
    /// EXISTS pushdown against the git-correlation tables. Pass `provider =
    /// None` to search all providers. A git-scoped call against a store
    /// predating the correlation schema returns no hits.
    pub async fn search_session_messages_git_scoped(
        &self,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
        git_filter: &crate::sessions::git_correlation::GitScopeFilter,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered_inner(
            provider,
            project_key,
            query,
            limit,
            filters,
            Some(git_filter),
            None,
        )
        .await
    }

    /// Like [`Self::search_session_messages_filtered`], additionally scoping
    /// hits to the agent transcripts of one workflow run via EXISTS pushdown
    /// against `workflow_agents`. A run's agents are matched either by the
    /// transcript file the message came from (`workflow_agents.transcript_path
    /// = session_messages.source_path`) or, as a fallback, by the agent's own
    /// session id (`workflow_agents.agent_session_id = session_messages.session_id`),
    /// so the scope holds whichever key the ingest recorded. When
    /// `filter.agent_label` is set the scope narrows to that one agent. A call
    /// against a store predating the workflow-index schema returns no hits.
    pub async fn search_session_messages_workflow_scoped(
        &self,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
        workflow_filter: &WorkflowScopeFilter,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered_inner(
            provider,
            project_key,
            query,
            limit,
            filters,
            None,
            Some(workflow_filter),
        )
        .await
    }

    /// Searches message text across all providers with optional parent/subagent filters.
    pub async fn search_session_messages_all_providers_filtered(
        &self,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered_inner(
            None,
            project_key,
            query,
            limit,
            filters,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // internal fan-in of independent scope/time/git/workflow filters
    async fn search_session_messages_filtered_inner(
        &self,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
        git_filter: Option<&crate::sessions::git_correlation::GitScopeFilter>,
        workflow_filter: Option<&WorkflowScopeFilter>,
    ) -> Vec<SessionMessageSearchResult> {
        // A git-scoped search against a store written before the correlation
        // schema existed can never match; report empty rather than issuing a
        // `no such table` EXISTS subquery.
        if let Some(filter) = git_filter {
            if !filter.is_empty()
                && !crate::sessions::git_correlation::tables_present(&self.conn)
                    .await
                    .unwrap_or(false)
            {
                return Vec::new();
            }
        }
        // Likewise a workflow-scoped search against a store predating the
        // workflow-index schema can never match: short-circuit to empty rather
        // than hitting `no such table: workflow_agents`.
        if workflow_filter.is_some()
            && !crate::sessions::workflow_index::tables_present(&self.conn)
                .await
                .unwrap_or(false)
        {
            return Vec::new();
        }
        let fts_query = session_fts_query(query);
        if fts_query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let literal_terms: Vec<String> = query
            .split_whitespace()
            .filter(|term| term.contains('-'))
            .map(str::to_lowercase)
            .collect();

        let mut sql = "SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                m.provider, m.message_id, m.session_id, m.role, m.timestamp, m.ordinal, m.text,
                m.kind, m.model, m.tool_names, m.source_path, m.source_offset, m.metadata_json,
                bm25(session_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0) AS rank
             FROM session_messages_fts
             JOIN session_messages m ON session_messages_fts.rowid = m.rowid
             JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
             WHERE session_messages_fts MATCH ?1"
            .to_string();
        let mut query_params = vec![Value::Text(fts_query)];
        if let Some(provider) = provider {
            query_params.push(Value::Text(provider.to_string()));
            let _ = write!(sql, " AND m.provider = ?{}", query_params.len());
        }
        if let Some(project_key) = project_key {
            query_params.push(Value::Text(project_key.to_string()));
            let _ = write!(sql, " AND s.project_key = ?{}", query_params.len());
        }
        if let Some(parent_session_id) = filters.parent_session_id {
            query_params.push(Value::Text(parent_session_id.to_string()));
            let _ = write!(sql, " AND s.parent_session_id = ?{}", query_params.len());
        }
        if let Some(predicate) = crate::sessions::message_noise::message_type_predicate_sql(
            "m",
            true,
            filters.message_type,
        ) {
            let _ = write!(sql, " AND {predicate}");
        }
        if let Some(start_time) = filters.time_range.start_time {
            query_params.push(Value::Integer(start_time));
            let _ = write!(
                sql,
                " AND m.timestamp IS NOT NULL AND m.timestamp >= ?{}",
                query_params.len()
            );
        }
        if let Some(end_time) = filters.time_range.end_time {
            query_params.push(Value::Integer(end_time));
            let _ = write!(
                sql,
                " AND m.timestamp IS NOT NULL AND m.timestamp <= ?{}",
                query_params.len()
            );
        }
        if matches!(
            filters.scope,
            crate::sessions::SessionSearchScope::ParentsOnly
        ) {
            sql.push_str(" AND s.is_subagent = 0");
        }
        if matches!(
            filters.scope,
            crate::sessions::SessionSearchScope::SubagentsOnly
        ) {
            sql.push_str(" AND s.is_subagent = 1");
        }
        // Reuse the shared scoping SQL (also used by the lcm/grep path) so the
        // branch/worktree/commit EXISTS semantics stay in one place. Its
        // anonymous `?` placeholders bind to the next sequential positions,
        // which — since the predicate and its values are appended together in
        // order — line up with the numbered placeholders that follow.
        if let Some(filter) = git_filter {
            if let Some((predicate, predicate_values)) =
                crate::sessions::git_correlation::git_scope_exists_predicate(filter, "m.session_id")
            {
                let _ = write!(sql, " AND {predicate}");
                query_params.extend(predicate_values);
            }
        }
        // Workflow-run scoping: reuse the shared EXISTS predicate (also used
        // by future lcm/grep paths) so run/agent correlation semantics stay in
        // one place. Renumber its `?1`, `?2`, … slots to follow the query's
        // existing numbered placeholders, then append the bind values in order.
        if let Some(filter) = workflow_filter {
            let (mut predicate, predicate_values) =
                crate::sessions::workflow_index::workflow_scope_exists_predicate(
                    filter,
                    "m.source_path",
                    "m.session_id",
                );
            let base = query_params.len();
            for slot in (1..=predicate_values.len()).rev() {
                predicate = predicate.replace(&format!("?{slot}"), &format!("?{}", base + slot));
            }
            let _ = write!(sql, " AND {predicate}");
            query_params.extend(predicate_values);
        }
        for term in &literal_terms {
            query_params.push(Value::Text(term.clone()));
            let _ = write!(
                sql,
                " AND instr(lower(m.text), ?{}) > 0",
                query_params.len()
            );
        }
        // Over-fetch before the deterministic inventory downrank so a
        // substantive hit buried below inventory/listing noise in raw BM25
        // order can still surface within the caller's `limit`. The downrank
        // reorders, never drops, then we truncate back to `limit`.
        let fetch_limit = crate::sessions::message_noise::rerank_fetch_limit(
            limit,
            SESSION_MESSAGE_SEARCH_MAX_FETCH,
        );
        query_params.push(Value::Integer(fetch_limit as i64));
        let _ = write!(
            sql,
            " ORDER BY bm25(session_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0)
                      LIMIT ?{}",
            query_params.len()
        );

        let rows_result = self.conn.query(&sql, query_params).await;

        let Ok(mut rows) = rows_result else {
            return Vec::new();
        };

        let mut results = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let Some(session) = row_to_session(&row) else {
                continue;
            };
            let Some(message) = row_to_message(&row, 13) else {
                continue;
            };
            let score = row.get::<f64>(26).map_or(0.0, |rank| -rank);
            results.push(SessionMessageSearchResult {
                session,
                message,
                score,
            });
        }
        results =
            crate::sessions::message_noise::dedupe_related_message_copies(results, |result| {
                crate::sessions::message_noise::RelatedMessageCopyIdentity {
                    provider: &result.session.provider,
                    family_session_id: result
                        .session
                        .parent_session_id
                        .as_deref()
                        .unwrap_or(&result.session.session_id),
                    session_id: &result.session.session_id,
                    is_subagent: result.session.is_subagent,
                    content: &result.message.text,
                }
            });
        downrank_inventory_messages(&mut results);
        results.truncate(limit);
        results
    }

    /// Lists each session's latest Codex goal state (`kind = 'goal'`), newest
    /// first, for the `message_search` `goals` view. One row per session — the
    /// goal row with the highest `ordinal` (byte offset), i.e. the last
    /// lifecycle transition ingested — so the returned `metadata_json.status`
    /// is the current status. `score` is always 0: this is a listing, not a
    /// relevance search. Optionally scoped to one `project_key`.
    pub async fn recent_session_goals(
        &self,
        project_key: Option<&str>,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        if limit == 0 {
            return Vec::new();
        }
        let mut sql = "SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                m.provider, m.message_id, m.session_id, m.role, m.timestamp, m.ordinal, m.text,
                m.kind, m.model, m.tool_names, m.source_path, m.source_offset, m.metadata_json
             FROM session_messages m
             JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
             WHERE m.kind = 'goal'
               AND m.ordinal = (
                   SELECT MAX(m2.ordinal) FROM session_messages m2
                   WHERE m2.provider = m.provider
                     AND m2.session_id = m.session_id
                     AND m2.kind = 'goal'
               )"
        .to_string();
        let mut query_params: Vec<Value> = Vec::new();
        if let Some(project_key) = project_key {
            query_params.push(Value::Text(project_key.to_string()));
            let _ = write!(sql, " AND s.project_key = ?{}", query_params.len());
        }
        query_params.push(Value::Integer(limit as i64));
        let _ = write!(
            sql,
            " ORDER BY COALESCE(m.timestamp, 0) DESC, m.ordinal DESC LIMIT ?{}",
            query_params.len()
        );

        let Ok(mut rows) = self.conn.query(&sql, query_params).await else {
            return Vec::new();
        };
        let mut results = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let Some(session) = row_to_session(&row) else {
                continue;
            };
            let Some(message) = row_to_message(&row, 13) else {
                continue;
            };
            results.push(SessionMessageSearchResult {
                session,
                message,
                score: 0.0,
            });
        }
        results
    }

    // ── Accounting: turns table ──────────────────────────────────────

    /// Insert a parsed turn. Returns `true` if inserted, `false` if duplicate.
    pub async fn insert_turn(&self, turn: &crate::types::CostTurn) -> bool {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO turns
                 (message_id, project_hash, session_id, model, timestamp,
                  input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                  cost_usd, category, tool_names)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    turn.message_id.clone(),
                    turn.project_hash.clone(),
                    turn.session_id.clone(),
                    turn.model.clone(),
                    turn.timestamp as i64,
                    turn.input_tokens as i64,
                    turn.output_tokens as i64,
                    turn.cache_write_tokens as i64,
                    turn.cache_read_tokens as i64,
                    turn.cost_usd,
                    turn.category.clone(),
                    turn.tool_names.clone(),
                ],
            )
            .await
            .is_ok_and(|n| n > 0)
    }

    /// Insert parsed turns in one transaction, returning the number of new rows.
    pub async fn insert_turns(&self, turns: &[crate::types::CostTurn]) -> usize {
        if self.conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
            return 0;
        }

        let mut inserted = 0;
        for turn in turns {
            let result = self
                .conn
                .execute(
                    "INSERT OR IGNORE INTO turns
                     (message_id, project_hash, session_id, model, timestamp,
                      input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                      cost_usd, category, tool_names)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        turn.message_id.clone(),
                        turn.project_hash.clone(),
                        turn.session_id.clone(),
                        turn.model.clone(),
                        turn.timestamp as i64,
                        turn.input_tokens as i64,
                        turn.output_tokens as i64,
                        turn.cache_write_tokens as i64,
                        turn.cache_read_tokens as i64,
                        turn.cost_usd,
                        turn.category.clone(),
                        turn.tool_names.clone(),
                    ],
                )
                .await;
            if let Ok(n) = result {
                inserted += n as usize;
            } else {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                return 0;
            }
        }

        if self.conn.execute("COMMIT", ()).await.is_ok() {
            inserted
        } else {
            let _ = self.conn.execute("ROLLBACK", ()).await;
            0
        }
    }

    /// Total cost in USD since a given unix timestamp.
    pub async fn total_cost_since(&self, since: u64) -> Option<f64> {
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM turns WHERE timestamp >= ?1",
                params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(row.get::<f64>(0).unwrap_or(0.0))
    }

    /// Total input + output tokens since a given unix timestamp.
    pub async fn total_tokens_since(&self, since: u64) -> Option<u64> {
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM turns WHERE timestamp >= ?1",
                params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(row.get::<i64>(0).unwrap_or(0) as u64)
    }

    /// Token breakdown (input, output, `cache_read`) since a given timestamp.
    pub async fn token_breakdown_since(&self, since: u64) -> Option<(u64, u64, u64)> {
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(SUM(input_tokens), 0),
                        COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cache_read_tokens), 0)
                 FROM turns WHERE timestamp >= ?1",
                params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some((
            row.get::<i64>(0).unwrap_or(0) as u64,
            row.get::<i64>(1).unwrap_or(0) as u64,
            row.get::<i64>(2).unwrap_or(0) as u64,
        ))
    }

    /// Cost grouped by model since a given timestamp.
    /// Returns `(model, cost, total_tokens)`.
    pub async fn cost_by_model_since(&self, since: u64) -> Vec<(String, f64, u64)> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT model, SUM(cost_usd), SUM(input_tokens + output_tokens)
                 FROM turns WHERE timestamp >= ?1
                 GROUP BY model ORDER BY SUM(cost_usd) DESC",
                params![since as i64],
            )
            .await
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let model: String = row.get(0).unwrap_or_default();
            let cost: f64 = row.get(1).unwrap_or(0.0);
            let tokens: i64 = row.get(2).unwrap_or(0);
            out.push((model, cost, tokens as u64));
        }
        out
    }

    /// Cost grouped by category since a given timestamp.
    /// Returns `(category, cost, turn_count)`.
    pub async fn cost_by_category_since(&self, since: u64) -> Vec<(String, f64, u64)> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT category, SUM(cost_usd), COUNT(*)
                 FROM turns WHERE timestamp >= ?1
                 GROUP BY category ORDER BY SUM(cost_usd) DESC",
                params![since as i64],
            )
            .await
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let cat: String = row.get(0).unwrap_or_default();
            let cost: f64 = row.get(1).unwrap_or(0.0);
            let count: i64 = row.get(2).unwrap_or(0);
            out.push((cat, cost, count as u64));
        }
        out
    }

    // ── Accounting: parse_offsets table ────────────────────────────────

    /// Returns the saved parse cursor for a JSONL file, including the
    /// optional file identity id, or `None` if the path is not tracked.
    pub async fn get_parse_offset(&self, path: &str) -> Option<ParseOffset> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT byte_offset, mtime, file_id FROM parse_offsets WHERE file_path = ?1",
                params![path],
            )
            .await
        else {
            let mut rows = self
                .conn
                .query(
                    "SELECT byte_offset, mtime FROM parse_offsets WHERE file_path = ?1",
                    params![path],
                )
                .await
                .ok()?;
            let row = rows.next().await.ok()??;
            let offset: i64 = row.get(0).ok()?;
            let mtime: i64 = row.get(1).ok()?;
            return Some(ParseOffset {
                byte_offset: offset as u64,
                mtime: mtime as u64,
                file_id: 0,
            });
        };
        let row = rows.next().await.ok()??;
        let offset: i64 = row.get(0).ok()?;
        let mtime: i64 = row.get(1).ok()?;
        let file_id: i64 = row.get(2).ok()?;
        Some(ParseOffset {
            byte_offset: offset as u64,
            mtime: mtime as u64,
            file_id: file_id as u64,
        })
    }

    /// Saves the parse cursor for a transcript path. Best-effort.
    pub async fn set_parse_offset(&self, path: &str, offset: ParseOffset) {
        let _ = self.set_parse_offset_in_existing_tx(path, offset).await;
    }

    /// Advances a row-style parse cursor without allowing an overlapping,
    /// older sweep to move it backwards.
    pub async fn advance_parse_offset(&self, path: &str, offset: ParseOffset) {
        let _ = self
            .set_parse_offset_monotonic_in_existing_tx(path, offset)
            .await;
    }

    async fn set_parse_offset_monotonic_in_existing_tx(
        &self,
        path: &str,
        offset: ParseOffset,
    ) -> bool {
        self.conn
            .execute(
                "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_path) DO UPDATE SET
                    byte_offset = excluded.byte_offset,
                    mtime = excluded.mtime,
                    file_id = excluded.file_id
                 WHERE excluded.byte_offset >= parse_offsets.byte_offset",
                params![
                    path,
                    offset.byte_offset as i64,
                    offset.mtime as i64,
                    offset.file_id as i64
                ],
            )
            .await
            .is_ok()
    }

    async fn set_parse_offset_in_existing_tx(&self, path: &str, offset: ParseOffset) -> bool {
        if self
            .conn
            .execute(
                "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_path) DO UPDATE SET
                    byte_offset = ?2,
                    mtime = ?3,
                    file_id = ?4",
                params![
                    path,
                    offset.byte_offset as i64,
                    offset.mtime as i64,
                    offset.file_id as i64
                ],
            )
            .await
            .is_ok()
        {
            return true;
        }
        self.conn
            .execute(
                "INSERT INTO parse_offsets (file_path, byte_offset, mtime)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(file_path) DO UPDATE SET
                    byte_offset = ?2,
                    mtime = ?3",
                params![path, offset.byte_offset as i64, offset.mtime as i64],
            )
            .await
            .is_ok()
    }

    /// Checkpoints the WAL. Best-effort.
    pub async fn checkpoint(&self) {
        let _ = self
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .await;
    }

    /// Consumes the `GlobalDb`, closing the underlying connection.
    pub fn close(self) {
        drop(self);
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
