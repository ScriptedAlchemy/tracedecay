//! User-level database that tracks all `TraceDecay` projects and their saved tokens.
//!
//! Stored at `~/.tracedecay/global.db`, this database holds one row per project
//! with the project's database path and its cumulative tokens-saved count. Read
//! paths are generally best-effort; authoritative open and maintenance
//! interfaces preserve failures for callers that must fail closed.

use std::path::{Path, PathBuf};

pub use tracedecay_store::ParseOffset;

use crate::db::engine::{Value as EngineValue, WalCheckpointExecutor};
use crate::errors::TraceDecayError;
use crate::sessions::{
    SessionMessageRecord, SessionMessageSearchResult, SessionRecord, SessionSearchFilters,
    lcm::LcmSummaryRequest,
};

pub mod configuration;
mod git_index_transactions;
pub(crate) mod observation;
mod observation_projection;
pub(crate) use observation_projection::{
    project_observation_with_engine, rebuild_projection_with_engine,
};
mod observation_store;
mod project_registry;
mod registered;
mod registered_accounting;
mod registered_analytics;
mod registered_dashboard;
mod registered_lcm;
mod registered_sessions;
pub(crate) mod schema_contract;
pub(crate) mod schema_stages;
pub(crate) use schema_stages::ensure_registered_schema;
pub(crate) mod session_temporal;
pub(crate) use session_temporal::operations as session_temporal_operations;
mod transcript;

pub(crate) use git_index_transactions::{
    GitIndexReadExecutor, GlobalDbGitIndexTransactionStore, ensure_git_index_transaction_schema,
};
pub use observation_store::{ProjectObservationStoreError, ProjectObservationStoreResolution};
use project_registry::project_path_alias_key;
pub(crate) use registered::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
pub(crate) use session_temporal::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};
pub(crate) use transcript::TranscriptPersistenceError;

const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

/// Scopes a `tracedecay_message_search` to the agent transcripts of one
/// workflow run, mirroring `GitScopeFilter` as a search-only concern. The run's
/// messages are the messages of its agents (rows in `workflow_agents`); see
/// The session-message search applies this with an `EXISTS` pushdown.
/// Serializes so the applied filter echoes cleanly into the payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkflowScopeFilter {
    /// The `wf_*` run whose agents' messages to keep.
    pub run_id: String,
    /// When set, narrows the scope to just this one agent of the run
    /// (matched on `workflow_agents.agent_label`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// Internal query context for the session-message search fan-in.
///
/// The optional scoped filters are intentionally grouped with the common search
/// fields so every search entry point feeds the same SQL builder.
#[derive(Clone, Copy)]
struct SessionMessageSearchQuery<'a> {
    provider: Option<&'a str>,
    project_key: Option<&'a str>,
    query: &'a str,
    limit: usize,
    filters: SessionSearchFilters<'a>,
    git_filter: Option<&'a crate::sessions::git_correlation::GitScopeFilter>,
    workflow_filter: Option<&'a WorkflowScopeFilter>,
}

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
    /// Exclusive upper bound on `timestamp` (unix seconds). `None` = unbounded.
    pub until: Option<i64>,
    /// Exclusive row-id cursor used by bounded reverse-chronological scans.
    pub before_id: Option<i64>,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
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
/// the registered session-store health route.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
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

/// One transcript session plus its parsed messages, for projection-only
/// multi-session upserts from stores such as Hermes `state.db`.
///
/// This compatibility DTO remains local because projection-only persistence is
/// intentionally outside the authoritative transcript store contract.
#[derive(Debug, Clone)]
pub struct TranscriptBatch {
    pub session: SessionRecord,
    pub messages: Vec<SessionMessageRecord>,
}

/// Whether a transcript batch writes the full dual store (LCM raw + searchable
/// projection) or only the `session_messages` projection.
/// User-level database tracking all `TraceDecay` projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTemporalRepairStage {
    RepairState,
    PrepareSchema,
    AuthorityEffects,
    AuthorityReceipts,
    AuthorityCursorKeys,
    AuthorityRefresh,
    AuthorityGenerations,
    AuthorityOwnership,
}

impl SessionTemporalRepairStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::RepairState => "repair_state",
            Self::PrepareSchema => "prepare_schema",
            Self::AuthorityEffects => "authority_effects",
            Self::AuthorityReceipts => "authority_receipts",
            Self::AuthorityCursorKeys => "authority_cursor_keys",
            Self::AuthorityRefresh => "authority_refresh",
            Self::AuthorityGenerations => "authority_generations",
            Self::AuthorityOwnership => "authority_ownership",
        }
    }

    fn parse(value: &str) -> crate::errors::Result<Self> {
        match value {
            "repair_state" => Ok(Self::RepairState),
            "prepare_schema" => Ok(Self::PrepareSchema),
            "authority_effects" => Ok(Self::AuthorityEffects),
            "authority_receipts" => Ok(Self::AuthorityReceipts),
            "authority_cursor_keys" => Ok(Self::AuthorityCursorKeys),
            "authority_refresh" => Ok(Self::AuthorityRefresh),
            "authority_generations" => Ok(Self::AuthorityGenerations),
            "authority_ownership" => Ok(Self::AuthorityOwnership),
            _ => Err(global_db_operation_message(
                "read session temporal repair progress",
                format!("unknown repair stage '{value}'"),
            )),
        }
    }

    fn authority_audit(self) -> Option<usize> {
        match self {
            Self::AuthorityCursorKeys => Some(1),
            Self::AuthorityRefresh => Some(2),
            Self::AuthorityGenerations => Some(3),
            Self::AuthorityOwnership => Some(4),
            Self::RepairState
            | Self::PrepareSchema
            | Self::AuthorityEffects
            | Self::AuthorityReceipts => None,
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::RepairState => Some(Self::PrepareSchema),
            Self::PrepareSchema => Some(Self::AuthorityEffects),
            Self::AuthorityEffects => Some(Self::AuthorityReceipts),
            Self::AuthorityReceipts => Some(Self::AuthorityCursorKeys),
            Self::AuthorityCursorKeys => Some(Self::AuthorityRefresh),
            Self::AuthorityRefresh => Some(Self::AuthorityGenerations),
            Self::AuthorityGenerations => Some(Self::AuthorityOwnership),
            Self::AuthorityOwnership => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTemporalRepairOutcome {
    NotRequired,
    Pending { stage: SessionTemporalRepairStage },
    Complete,
}

const SESSION_TEMPORAL_REPAIR_NAME: &str = "session-temporal-v1";

#[derive(Debug, Clone, Copy)]
struct SessionTemporalRepairCheckpoint {
    stage: SessionTemporalRepairStage,
    cursor: i64,
}

pub(crate) async fn enqueue_session_temporal_store_repair(
    database: &RegisteredGlobalDb,
) -> crate::errors::Result<SessionTemporalRepairOutcome> {
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| global_db_operation_error("enqueue session temporal repair", error))?;
    if !connection_table_exists(&transaction, "session_messages").await?
        || !connection_table_exists(&transaction, "observations").await?
    {
        transaction
            .rollback()
            .await
            .map_err(|error| global_db_operation_error("rollback skipped session repair", error))?;
        return Ok(SessionTemporalRepairOutcome::NotRequired);
    }
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS session_temporal_repair_progress (
                repair_name TEXT PRIMARY KEY,
                stage TEXT NOT NULL,
                cursor INTEGER NOT NULL DEFAULT 0 CHECK(cursor >= 0),
                requested_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             )",
        )
        .await
        .map_err(|error| global_db_operation_error("create session repair checkpoint", error))?;
    transaction
        .execute(
            "INSERT INTO session_temporal_repair_progress (
                repair_name, stage, cursor, requested_at, updated_at
             ) VALUES (?1, ?2, 0, unixepoch(), unixepoch())
             ON CONFLICT(repair_name) DO NOTHING",
            crate::db::engine::params![
                SESSION_TEMPORAL_REPAIR_NAME,
                SessionTemporalRepairStage::RepairState.as_str()
            ],
        )
        .await
        .map_err(|error| global_db_operation_error("enqueue session temporal repair", error))?;
    let checkpoint = read_session_temporal_repair_checkpoint(&transaction)
        .await?
        .ok_or_else(|| {
            global_db_operation_message(
                "enqueue session temporal repair",
                "repair checkpoint disappeared before commit",
            )
        })?;
    transaction
        .commit()
        .await
        .map_err(|error| global_db_operation_error("commit session repair request", error))?;
    Ok(SessionTemporalRepairOutcome::Pending {
        stage: checkpoint.stage,
    })
}

pub(crate) async fn session_temporal_store_repair_status(
    database: &RegisteredGlobalDb,
) -> crate::errors::Result<SessionTemporalRepairOutcome> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| global_db_operation_error("snapshot session repair status", error))?;
    Ok(
        match read_session_temporal_repair_checkpoint(&snapshot).await? {
            Some(checkpoint) => SessionTemporalRepairOutcome::Pending {
                stage: checkpoint.stage,
            },
            None => SessionTemporalRepairOutcome::NotRequired,
        },
    )
}

pub(crate) async fn advance_session_temporal_store_repair(
    database: &RegisteredGlobalDb,
) -> crate::errors::Result<SessionTemporalRepairOutcome> {
    advance_session_temporal_store_repair_with_page_rows(
        database,
        schema_contract::SESSION_TEMPORAL_REPAIR_AUDIT_PAGE_ROWS,
    )
    .await
}

pub(crate) async fn advance_session_temporal_store_repair_with_page_rows(
    database: &RegisteredGlobalDb,
    page_rows: i64,
) -> crate::errors::Result<SessionTemporalRepairOutcome> {
    debug_assert!(page_rows > 0);
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| global_db_operation_error("advance session temporal repair", error))?;
    let Some(checkpoint) = read_session_temporal_repair_checkpoint(&transaction).await? else {
        transaction
            .rollback()
            .await
            .map_err(|error| global_db_operation_error("rollback idle session repair", error))?;
        return Ok(SessionTemporalRepairOutcome::NotRequired);
    };
    let stage = checkpoint.stage;

    let repair = async {
        let (next_stage, next_cursor) = match stage {
            SessionTemporalRepairStage::RepairState => {
                session_temporal::repair_session_temporal_state(&transaction).await?;
                // Repair state temporarily drops immutable guards. Restore every
                // authority trigger before this batch becomes visible.
                schema_contract::ensure_authority_invariant_schema(&transaction).await?;
                (stage.next(), 0)
            }
            SessionTemporalRepairStage::PrepareSchema => {
                session_temporal::ensure_session_temporal_schema(&transaction).await?;
                schema_contract::ensure_authority_invariant_schema(&transaction).await?;
                (stage.next(), 0)
            }
            SessionTemporalRepairStage::AuthorityEffects => {
                let (cursor, complete) =
                    schema_contract::validate_session_temporal_effect_authority_page_with_limit(
                        &transaction,
                        checkpoint.cursor,
                        page_rows,
                    )
                    .await?;
                (
                    if complete { stage.next() } else { Some(stage) },
                    if complete { 0 } else { cursor },
                )
            }
            SessionTemporalRepairStage::AuthorityReceipts => {
                let (cursor, complete) =
                    schema_contract::validate_session_temporal_receipt_authority_page_with_limit(
                        &transaction,
                        checkpoint.cursor,
                        page_rows,
                    )
                    .await?;
                (
                    if complete { stage.next() } else { Some(stage) },
                    if complete { 0 } else { cursor },
                )
            }
            _ => {
                let audit_index = stage.authority_audit().ok_or_else(|| {
                    global_db_operation_message(
                        "advance session temporal repair",
                        "repair stage has no authority audit",
                    )
                })?;
                schema_contract::validate_session_temporal_repair_authority_audit(
                    &transaction,
                    audit_index,
                )
                .await?;
                (stage.next(), 0)
            }
        };

        if let Some(next) = next_stage {
            transaction
                .execute(
                    "UPDATE session_temporal_repair_progress
                     SET stage = ?1, cursor = ?2, updated_at = unixepoch()
                     WHERE repair_name = ?3",
                    crate::db::engine::params![
                        next.as_str(),
                        next_cursor,
                        SESSION_TEMPORAL_REPAIR_NAME
                    ],
                )
                .await
                .map_err(|error| {
                    global_db_operation_error("checkpoint session temporal repair", error)
                })?;
            Ok(SessionTemporalRepairOutcome::Pending { stage: next })
        } else {
            transaction
                .execute(
                    "DELETE FROM session_temporal_repair_progress WHERE repair_name = ?1",
                    crate::db::engine::params![SESSION_TEMPORAL_REPAIR_NAME],
                )
                .await
                .map_err(|error| {
                    global_db_operation_error("complete session temporal repair", error)
                })?;
            Ok(SessionTemporalRepairOutcome::Complete)
        }
    }
    .await;

    match repair {
        Ok(outcome) => {
            transaction.commit().await.map_err(|error| {
                global_db_operation_error("commit session temporal repair batch", error)
            })?;
            Ok(outcome)
        }
        Err(error) => {
            transaction.rollback().await.map_err(|rollback_error| {
                global_db_operation_message(
                    "rollback failed session temporal repair batch",
                    format!("{rollback_error}; original repair failure: {error}"),
                )
            })?;
            Err(error)
        }
    }
}

pub(crate) async fn repair_session_temporal_store(
    database: &RegisteredGlobalDb,
) -> crate::errors::Result<()> {
    let mut outcome = enqueue_session_temporal_store_repair(database).await?;
    while matches!(outcome, SessionTemporalRepairOutcome::Pending { .. }) {
        outcome = advance_session_temporal_store_repair(database).await?;
    }
    match outcome {
        SessionTemporalRepairOutcome::NotRequired | SessionTemporalRepairOutcome::Complete => {
            Ok(())
        }
        SessionTemporalRepairOutcome::Pending { .. } => {
            unreachable!("session repair loop exits only on a terminal outcome")
        }
    }
}

async fn read_session_temporal_repair_checkpoint(
    conn: &(impl crate::db::engine::QueryExecutor + ?Sized),
) -> crate::errors::Result<Option<SessionTemporalRepairCheckpoint>> {
    if !connection_table_exists(conn, "session_temporal_repair_progress").await? {
        return Ok(None);
    }
    let mut rows = crate::db::engine::QueryExecutor::query(
        conn,
        "SELECT stage, cursor
         FROM session_temporal_repair_progress
         WHERE repair_name = ?1",
        crate::db::engine::params![SESSION_TEMPORAL_REPAIR_NAME],
    )
    .await
    .map_err(|error| global_db_operation_error("read session temporal repair progress", error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error("read session temporal repair progress", error))?
        .map(|row| {
            let stage = row
                .get::<String>(0)
                .map_err(|error| {
                    global_db_operation_error("read session temporal repair progress", error)
                })
                .and_then(|stage| SessionTemporalRepairStage::parse(&stage))?;
            let cursor = row.get::<i64>(1).map_err(|error| {
                global_db_operation_error("read session temporal repair progress", error)
            })?;
            Ok(SessionTemporalRepairCheckpoint { stage, cursor })
        })
        .transpose()
}

async fn connection_table_exists(
    conn: &(impl crate::db::engine::QueryExecutor + ?Sized),
    table: &str,
) -> crate::errors::Result<bool> {
    let mut rows = crate::db::engine::QueryExecutor::query(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        crate::db::engine::params![table],
    )
    .await
    .map_err(|error| global_db_operation_error("inspect global database schema", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| global_db_operation_error("inspect global database schema", error))?
        .is_some())
}

const GLOBAL_DB_PATH_ENV: &str = "TRACEDECAY_GLOBAL_DB";

fn global_db_path_override() -> Option<PathBuf> {
    std::env::var_os(GLOBAL_DB_PATH_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn global_db_mmap_size_guard() -> u64 {
    0
}

fn global_db_operation_error(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> TraceDecayError {
    TraceDecayError::database_operation(operation, source)
}

fn global_db_operation_message(
    operation: &'static str,
    message: impl Into<String>,
) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
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

fn row_to_analytics_event(row: &crate::db::engine::Row) -> Option<AnalyticsEventRecord> {
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
    values: &mut Vec<EngineValue>,
    column: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        values.push(EngineValue::Text(value.to_string()));
        clauses.push(format!("{column} = ?{}", values.len()));
    }
}

fn analytics_scope_query(
    select: &str,
    project_id: Option<&str>,
    since: i64,
    fixed_clauses: &[&str],
) -> (String, Vec<EngineValue>) {
    let mut sql = select.to_string();
    let mut clauses = fixed_clauses
        .iter()
        .map(|clause| (*clause).to_string())
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    push_optional_analytics_filter(&mut clauses, &mut values, "project_id", project_id);
    values.push(EngineValue::Integer(since));
    clauses.push(format!("timestamp >= ?{}", values.len()));
    sql.push_str(" WHERE ");
    sql.push_str(&clauses.join(" AND "));
    (sql, values)
}

/// Upper bound on the BM25 over-fetch that precedes the inventory downrank in
/// the session-message search. Keeps the pre-rerank fetch bounded even for
/// large caller limits.
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

/// Merge independently ranked transcript and canonical-workflow hits by rank
/// tier. Workflow facts lead each tier because they are the authoritative
/// structured representation; borrowing the paired transcript score keeps the
/// merged page comparable when project shards are ranked again by the caller.
fn interleave_workflow_search_results(
    transcript_results: Vec<SessionMessageSearchResult>,
    workflow_results: Vec<SessionMessageSearchResult>,
) -> Vec<SessionMessageSearchResult> {
    let capacity = transcript_results
        .len()
        .saturating_add(workflow_results.len());
    let mut transcript_results = transcript_results.into_iter();
    let mut workflow_results = workflow_results.into_iter();
    let mut merged = Vec::with_capacity(capacity);

    loop {
        let transcript_result = transcript_results.next();
        let workflow_result = workflow_results.next();
        if transcript_result.is_none() && workflow_result.is_none() {
            break;
        }
        if let Some(mut workflow_result) = workflow_result {
            if let Some(transcript_result) = transcript_result.as_ref() {
                workflow_result.score = transcript_result.score;
            }
            merged.push(workflow_result);
        }
        if let Some(transcript_result) = transcript_result {
            merged.push(transcript_result);
        }
    }

    merged
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
        aliases.push(format!("git-common-dir:{}", project_path_alias_key(path)));
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

fn normalize_git_remote_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    let mut normalized = remote.trim_end_matches('/').to_string();
    if let Some(rest) = normalized.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        normalized = format!("https://{host}/{path}");
    }
    if let Some(stripped) = normalized.strip_suffix(".git") {
        normalized = stripped.to_string();
    }
    Some(normalized.to_ascii_lowercase())
}

async fn table_column_exists(
    conn: &(impl crate::db::engine::QueryExecutor + ?Sized),
    table: &str,
    column: &str,
) -> crate::db::engine::Result<bool> {
    let mut rows = crate::db::engine::QueryExecutor::query(
        conn,
        "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2 COLLATE NOCASE",
        crate::db::engine::params![table, column],
    )
    .await?;
    Ok(rows.next().await?.is_some())
}

async fn add_table_column_after_missing_check(
    conn: &(impl crate::db::engine::Executor + ?Sized),
    table: &str,
    column: &str,
    ddl: &str,
) -> crate::db::engine::Result<bool> {
    match crate::db::engine::Executor::execute(conn, ddl, ()).await {
        Ok(_) => Ok(true),
        Err(error) => {
            if table_column_exists(conn, table, column).await? {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

async fn ensure_table_columns(
    conn: &(impl crate::db::engine::Executor + ?Sized),
    table: &str,
    columns: &[(&str, &str)],
) -> crate::db::engine::Result<()> {
    for &(column, ddl) in columns {
        if !table_column_exists(conn, table, column).await? {
            add_table_column_after_missing_check(conn, table, column, ddl).await?;
        }
    }
    Ok(())
}

async fn ensure_session_parent_columns(
    conn: &(impl crate::db::engine::Executor + ?Sized),
) -> crate::db::engine::Result<()> {
    ensure_table_columns(
        conn,
        "sessions",
        &[
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
        ],
    )
    .await?;
    crate::db::engine::Executor::execute(
        conn,
        "CREATE INDEX IF NOT EXISTS idx_sessions_parent
            ON sessions(provider, parent_session_id)",
        (),
    )
    .await?;
    Ok(())
}

async fn ensure_parse_offset_columns(
    conn: &(impl crate::db::engine::Executor + ?Sized),
) -> crate::db::engine::Result<()> {
    ensure_table_columns(
        conn,
        "parse_offsets",
        &[(
            "file_id",
            "ALTER TABLE parse_offsets ADD COLUMN file_id INTEGER NOT NULL DEFAULT 0",
        )],
    )
    .await
}

async fn ensure_code_project_native_root_columns(
    conn: &(impl crate::db::engine::Executor + ?Sized),
) -> crate::db::engine::Result<()> {
    ensure_table_columns(
        conn,
        "code_projects",
        &[
            (
                "primary_root_platform",
                "ALTER TABLE code_projects ADD COLUMN primary_root_platform TEXT",
            ),
            (
                "primary_root_bytes",
                "ALTER TABLE code_projects ADD COLUMN primary_root_bytes BLOB",
            ),
            (
                "primary_root_last_seen_at",
                "ALTER TABLE code_projects ADD COLUMN primary_root_last_seen_at INTEGER",
            ),
        ],
    )
    .await
}

impl RegisteredGlobalDb {
    /// Checkpoints the registered store's WAL through its authorized writer.
    pub(crate) async fn checkpoint_result(&self) -> Result<(), TraceDecayError> {
        let writer = self.writer_connection().map_err(|error| {
            global_db_operation_error("open registered WAL checkpoint writer", error)
        })?;
        let mut rows = writer
            .checkpoint_wal_truncate()
            .await
            .map_err(|error| global_db_operation_error("checkpoint registered WAL", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?
            .ok_or_else(|| {
                global_db_operation_message(
                    "checkpoint registered WAL",
                    "WAL checkpoint returned no status row",
                )
            })?;
        let busy: i64 = row
            .get(0)
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?;
        let log_frames: i64 = row
            .get(1)
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?;
        let checkpointed_frames: i64 = row
            .get(2)
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?;
        if busy != 0 || checkpointed_frames < log_frames {
            return Err(global_db_operation_message(
                "checkpoint registered WAL",
                format!(
                    "WAL checkpoint incomplete: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
                ),
            ));
        }
        Ok(())
    }

    pub(crate) async fn checkpoint(&self) {
        if let Err(error) = self.checkpoint_result().await {
            eprintln!("[tracedecay] registered database WAL checkpoint failed: {error}");
        }
    }

    pub(crate) async fn prune_global_retention(
        &self,
        config: &crate::retention::RetentionConfig,
        now_secs: i64,
    ) -> crate::errors::Result<Vec<crate::retention::RetentionTableReport>> {
        let transaction = self.begin_write_transaction().await?;
        let report = crate::retention::prune_global_tables(
            &transaction,
            config,
            crate::retention::RetentionMode::Apply,
            now_secs,
        )
        .await?;
        transaction.commit().await.map_err(|error| {
            global_db_operation_error("commit registered global retention prune", error)
        })?;
        Ok(report)
    }

    pub(crate) async fn global_retention_report(
        &self,
        config: &crate::retention::RetentionConfig,
        now_secs: i64,
    ) -> crate::errors::Result<Vec<crate::retention::RetentionTableReport>> {
        let transaction = self.begin_write_transaction().await?;
        let report = crate::retention::prune_global_tables(
            &transaction,
            config,
            crate::retention::RetentionMode::DryRun,
            now_secs,
        )
        .await;
        let rollback = transaction.rollback().await;
        match (report, rollback) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(global_db_operation_error(
                "rollback registered global retention report",
                error,
            )),
        }
    }
}

#[cfg(all(test, not(windows)))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod checkpoint_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod session_temporal_repair_tests;
