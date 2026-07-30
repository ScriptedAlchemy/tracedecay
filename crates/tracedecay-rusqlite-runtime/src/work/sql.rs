//! Shared registered migration-SQL plumbing for every Work table.

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

pub(crate) fn authority_params_owned(authority: &WorkAuthority) -> Vec<MigrationSqlValue> {
    authority_params(authority)
        .into_iter()
        .map(|value| MigrationSqlValue::Text(value.to_owned()))
        .collect()
}

pub(crate) fn migration_statement(
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlStatement, crate::migration_sql::MigrationSqlError> {
    MigrationSqlStatement::new(sql.to_owned(), params)
}

pub(crate) trait RegisteredWorkQuery {
    fn work_query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError>;
}

impl RegisteredWorkQuery for MigrationSqlHandle {
    fn work_query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError> {
        self.query(statement, Duration::from_secs(5))
    }
}

impl RegisteredWorkQuery for MigrationSqlTransaction {
    fn work_query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError> {
        self.query(statement)
    }
}

pub(crate) fn registered_work_query(
    source: &impl RegisteredWorkQuery,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError> {
    source.work_query(migration_statement(sql, params)?)
}

pub(crate) fn migration_text(values: &[MigrationSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        MigrationSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

pub(crate) fn migration_integer(values: &[MigrationSqlValue], index: usize) -> Option<i64> {
    match values.get(index)? {
        MigrationSqlValue::Integer(value) => Some(*value),
        _ => None,
    }
}

pub(crate) fn invalid_storage(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_owned())
}
