//! Shared registered exact-SQL plumbing for every Work table.

use super::*;

/// A nonterminal attempt is actionable only until a committed retry receipt
/// replaces its capacity and recovery authority with the named new attempt.
/// Queries using this predicate must alias `work_attempts_v1` as `attempt`.
pub(crate) const ACTIVE_ATTEMPT_PREDICATE: &str = "attempt.terminal = 0
    AND NOT EXISTS (
        SELECT 1 FROM work_retry_receipts_v1 AS retry
        WHERE retry.project_id = attempt.project_id
          AND retry.repository_id = attempt.repository_id
          AND retry.worktree_id = attempt.worktree_id
          AND retry.actor_id = attempt.actor_id
          AND retry.policy_digest = attempt.policy_digest
          AND retry.task_id = attempt.task_id
          AND retry.run_id = attempt.run_id
          AND retry.original_attempt_id = attempt.attempt_id
    )";

pub(crate) fn authority_params(authority: &WorkAuthority) -> [&str; 5] {
    [
        authority.project_id().as_str(),
        authority.repository_id().as_str(),
        authority.worktree_id().as_str(),
        authority.actor_id().as_str(),
        authority.policy_digest().as_str(),
    ]
}

pub(crate) fn authority_params_owned(authority: &WorkAuthority) -> Vec<ExactSqlValue> {
    authority_params(authority)
        .into_iter()
        .map(|value| ExactSqlValue::Text(value.to_owned()))
        .collect()
}

pub(crate) fn exact_sql_statement(
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlStatement, crate::exact_sql::ExactSqlError> {
    ExactSqlStatement::new(sql.to_owned(), params)
}

pub(crate) trait RegisteredWorkQuery {
    fn work_query(
        &self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlRows, crate::exact_sql::ExactSqlError>;
}

impl RegisteredWorkQuery for ExactSqlHandle {
    fn work_query(
        &self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlRows, crate::exact_sql::ExactSqlError> {
        self.query(statement, Duration::from_secs(5))
    }
}

impl RegisteredWorkQuery for ExactSqlTransaction {
    fn work_query(
        &self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlRows, crate::exact_sql::ExactSqlError> {
        self.query(statement)
    }
}

pub(crate) fn registered_work_query(
    source: &impl RegisteredWorkQuery,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, crate::exact_sql::ExactSqlError> {
    source.work_query(exact_sql_statement(sql, params)?)
}

pub(crate) fn exact_sql_text(values: &[ExactSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        ExactSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

pub(crate) fn exact_sql_integer(values: &[ExactSqlValue], index: usize) -> Option<i64> {
    match values.get(index)? {
        ExactSqlValue::Integer(value) => Some(*value),
        _ => None,
    }
}

pub(crate) fn invalid_storage(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_owned())
}
