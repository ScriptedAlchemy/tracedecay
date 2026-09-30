use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::engine::Value as EngineValue;

use crate::{AnalyticsEventRecord, project_path_alias_key};

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

/// How [`global_accounting_enabled`] reached its decision; the dashboard
/// surfaces this so an empty ledger can be explained honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountingMode {
    /// No env override, global accounting is on by default.
    Default,
    /// A truthy `TRACEDECAY_DISABLE_GLOBAL_DB` disabled it.
    DisabledByEnv,
}

impl AccountingMode {
    pub fn enabled(self) -> bool {
        !matches!(self, Self::DisabledByEnv)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::DisabledByEnv => "disabled_by_env",
        }
    }
}

/// Canonical truthy-env-value test shared by every boolean env flag: trims,
/// case-folds, and accepts `1`/`true`/`yes`/`on`. (Two parsers used to
/// coexist with diverging semantics, e.g. `TRACEDECAY_DISABLE_GLOBAL_DB=on`
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
/// Savings dashboard reads the ledger, an opt-in gate here silently left
/// the ledger empty while lifetime counters kept growing. A truthy
/// `TRACEDECAY_DISABLE_GLOBAL_DB` is the one opt-out.
pub fn global_accounting_mode() -> AccountingMode {
    if env_flag("TRACEDECAY_DISABLE_GLOBAL_DB") {
        AccountingMode::DisabledByEnv
    } else {
        AccountingMode::Default
    }
}

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
