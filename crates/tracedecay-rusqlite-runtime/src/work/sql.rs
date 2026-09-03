//! Shared registered exact-SQL plumbing for every Work table.

use super::*;

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
