use std::path::{Path, PathBuf};

use tracedecay_runtime_core::db::engine::Value as EngineValue;
use tracedecay_runtime_core::errors::TraceDecayError;
use tracedecay_sessions::runtime::SessionMessageSearchResult;

use crate::{AnalyticsEventRecord, project_path_alias_key};

pub(crate) const GLOBAL_DB_PATH_ENV: &str = "TRACEDECAY_GLOBAL_DB";

pub(crate) fn global_db_path_override() -> Option<PathBuf> {
    std::env::var_os(GLOBAL_DB_PATH_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn global_db_operation_error(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> TraceDecayError {
    TraceDecayError::database_operation(operation, source)
}

pub(crate) fn global_db_operation_message(
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
    tracedecay_runtime_core::config::user_data_dir().map(|dir| dir.join("global.db"))
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

/// Reads the `TRACEDECAY_<suffix>` environment variable.
///
/// Byte-for-byte the root `config::brand_env`, kept local because the branded
/// prefix is a naming rule with no dependencies — reaching up to root
/// `src/config.rs` for one `std::env::var` call would be the only reason this
/// crate needed the composition root. Collapse the two once the kernel owns
/// the brand prefix.
pub(crate) fn brand_env(suffix: &str) -> Option<String> {
    std::env::var(format!("TRACEDECAY_{suffix}")).ok()
}

/// Rough token count for `text`, four characters to the token.
///
/// Mirrors the root `context::read_modes::estimate_tokens` heuristic. LCM
/// summary drafts and transcript rows record this number, so it has to be the
/// same arithmetic on both sides of the split; it is deliberately duplicated
/// rather than reached for, since `context::read_modes` is an MCP read handler
/// that pulls in the whole root graph database.
#[must_use]
pub fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    chars.div_ceil(4).min(u32::MAX as usize) as u32
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
    if let Some(value) = brand_env("ENABLE_GLOBAL_DB") {
        return if env_value_truthy(&value) {
            AccountingMode::EnabledByEnv
        } else {
            AccountingMode::DisabledByEnv
        };
    }
    if brand_env("DISABLE_GLOBAL_DB").is_some_and(|value| env_value_truthy(&value)) {
        return AccountingMode::DisabledByEnv;
    }
    AccountingMode::Default
}

/// Convenience wrapper over [`global_accounting_mode`].
pub fn global_accounting_enabled() -> bool {
    global_accounting_mode().enabled()
}

pub(crate) fn row_to_analytics_event(
    row: &tracedecay_runtime_core::db::engine::Row,
) -> Option<AnalyticsEventRecord> {
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

pub(crate) fn push_optional_analytics_filter(
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

pub(crate) fn analytics_scope_query(
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
pub(crate) const SESSION_MESSAGE_SEARCH_MAX_FETCH: usize = 200;

/// Stable inventory downrank for a BM25 result page: transcript inventory/
/// listing messages and prose branch/worktree rosters are moved below
/// substantive hits while preserving the relative BM25 order within each
/// group. Applied before truncation so a downranked hit still surfaces when it
/// is the only match. Mirrors the lcm/grep re-rank.
pub(crate) fn downrank_inventory_messages(results: &mut Vec<SessionMessageSearchResult>) {
    if results.len() < 2 {
        return;
    }
    let mut substantive = Vec::with_capacity(results.len());
    let mut inventory = Vec::new();
    for result in results.drain(..) {
        if tracedecay_sessions::compatibility::is_inventory_text(&result.message.text) {
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
pub(crate) fn interleave_workflow_search_results(
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

pub(crate) fn session_fts_query(query: &str) -> String {
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

pub(crate) fn like_pattern(query: &str) -> String {
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

pub(crate) fn repo_identity_aliases(git_common_dir: Option<&Path>) -> Vec<String> {
    let mut aliases = Vec::new();
    if let Some(path) = git_common_dir {
        aliases.push(format!("git-common-dir:{}", project_path_alias_key(path)));
    }
    aliases
}

pub(crate) fn git_remote_search_alias(remote: Option<&str>) -> Option<String> {
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

pub(crate) fn normalize_git_remote_url(remote: &str) -> Option<String> {
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

pub(crate) async fn table_column_exists(
    conn: &(impl tracedecay_runtime_core::db::engine::QueryExecutor + ?Sized),
    table: &str,
    column: &str,
) -> tracedecay_runtime_core::db::engine::Result<bool> {
    let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
        conn,
        "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2 COLLATE NOCASE",
        tracedecay_runtime_core::db::engine::params![table, column],
    )
    .await?;
    Ok(rows.next().await?.is_some())
}

pub(crate) async fn add_table_column_after_missing_check(
    conn: &(impl tracedecay_runtime_core::db::engine::Executor + ?Sized),
    table: &str,
    column: &str,
    ddl: &str,
) -> tracedecay_runtime_core::db::engine::Result<bool> {
    match tracedecay_runtime_core::db::engine::Executor::execute(conn, ddl, ()).await {
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

pub(crate) async fn ensure_table_columns(
    conn: &(impl tracedecay_runtime_core::db::engine::Executor + ?Sized),
    table: &str,
    columns: &[(&str, &str)],
) -> tracedecay_runtime_core::db::engine::Result<()> {
    for &(column, ddl) in columns {
        if !table_column_exists(conn, table, column).await? {
            add_table_column_after_missing_check(conn, table, column, ddl).await?;
        }
    }
    Ok(())
}

pub(crate) async fn ensure_session_parent_columns(
    conn: &(impl tracedecay_runtime_core::db::engine::Executor + ?Sized),
) -> tracedecay_runtime_core::db::engine::Result<()> {
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
    tracedecay_runtime_core::db::engine::Executor::execute(
        conn,
        "CREATE INDEX IF NOT EXISTS idx_sessions_parent
            ON sessions(provider, parent_session_id)",
        (),
    )
    .await?;
    Ok(())
}

pub(crate) async fn ensure_parse_offset_columns(
    conn: &(impl tracedecay_runtime_core::db::engine::Executor + ?Sized),
) -> tracedecay_runtime_core::db::engine::Result<()> {
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

pub(crate) async fn ensure_code_project_native_root_columns(
    conn: &(impl tracedecay_runtime_core::db::engine::Executor + ?Sized),
) -> tracedecay_runtime_core::db::engine::Result<()> {
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
