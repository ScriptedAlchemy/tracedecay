//! The one schema-version ledger for every SQLite store.
//!
//! A store records each component's version in `tracedecay_schema_version`.
//! Callers install their own tables; this module only records which version
//! those tables are. A store without a ledger row for a component has never
//! recorded it. Reading never writes, so a refused store keeps the stamp it
//! had.

use std::future::Future;
use std::task::{Context, Poll};

use crate::exact_sql::ExactSqlValue;

/// Ledger table. One row per component.
pub const TABLE: &str = "tracedecay_schema_version";

pub const GRAPH_COMPONENT: &str = "graph";

pub const LCM_COMPONENT: &str = "lcm";

pub const GIT_CORRELATION_COMPONENT: &str = "git_correlation";

pub const WORKFLOW_INDEX_COMPONENT: &str = "workflow_indexing";

pub const SESSION_TEMPORAL_COMPONENT: &str = "session-temporal";

const LEDGER_DDL: &str = "CREATE TABLE IF NOT EXISTS tracedecay_schema_version (
    component TEXT PRIMARY KEY,
    version INTEGER NOT NULL
)";

/// Failure while reading or recording a component version.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaVersionError {
    #[error("{operation}: {message}")]
    Database {
        operation: &'static str,
        message: String,
        busy: bool,
    },
    /// The stored cell was not an integer, so the compiled version cannot stand in for it.
    #[error("schema version for '{component}' is unreadable: {message}")]
    Unreadable { component: String, message: String },
}

impl SchemaVersionError {
    pub const fn is_busy(&self) -> bool {
        matches!(self, Self::Database { busy: true, .. })
    }
}

/// Reads rows for the ledger. Implemented for a `rusqlite` connection here and
/// for the async engine beside that engine.
#[allow(async_fn_in_trait)]
pub trait SchemaRead {
    async fn schema_query(
        &self,
        sql: &str,
        params: &[ExactSqlValue],
    ) -> Result<Vec<Vec<ExactSqlValue>>, SchemaVersionError>;
}

/// Writes the ledger. A read-only connection does not implement this.
#[allow(async_fn_in_trait)]
pub trait SchemaWrite: SchemaRead {
    async fn schema_execute(
        &self,
        sql: &str,
        params: &[ExactSqlValue],
    ) -> Result<(), SchemaVersionError>;

    async fn schema_execute_batch(&self, sql: &str) -> Result<(), SchemaVersionError>;
}

/// The version recorded for `component`. `Ok(None)` means this store has
/// never recorded it. Does not create or drop anything.
pub async fn read_version(
    store: &impl SchemaRead,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    read_recorded(store, component).await
}

/// Records `version` for `component`.
pub async fn record_version(
    store: &impl SchemaWrite,
    component: &str,
    version: i64,
) -> Result<(), SchemaVersionError> {
    ensure_table(store).await?;
    store
        .schema_execute(
            "INSERT INTO tracedecay_schema_version(component, version) VALUES (?1, ?2)
             ON CONFLICT(component) DO UPDATE SET version = excluded.version",
            &[text(component), ExactSqlValue::Integer(version)],
        )
        .await?;
    Ok(())
}

/// Polls a rusqlite-backed ledger future. Those queries run inside the
/// `async` method and never wait, so a pending poll is a programming failure
/// reported as a database error rather than a panic.
pub fn ready<T>(
    future: impl Future<Output = Result<T, SchemaVersionError>>,
) -> Result<T, SchemaVersionError> {
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(result) => result,
        Poll::Pending => Err(SchemaVersionError::Database {
            operation: "schema version",
            message: "schema version query on a local connection did not finish".to_owned(),
            busy: false,
        }),
    }
}

fn text(value: &str) -> ExactSqlValue {
    ExactSqlValue::Text(value.to_owned())
}

fn database(operation: &'static str, message: impl Into<String>) -> SchemaVersionError {
    SchemaVersionError::Database {
        operation,
        message: message.into(),
        busy: false,
    }
}

fn require_i64(value: &ExactSqlValue, component: &str) -> Result<i64, SchemaVersionError> {
    match value {
        ExactSqlValue::Integer(version) => Ok(*version),
        other => Err(SchemaVersionError::Unreadable {
            component: component.to_owned(),
            message: format!("expected an integer, found {other:?}"),
        }),
    }
}

fn cell<'a>(
    row: &'a [ExactSqlValue],
    index: usize,
    operation: &'static str,
) -> Result<&'a ExactSqlValue, SchemaVersionError> {
    row.get(index)
        .ok_or_else(|| database(operation, "schema version row is missing a column"))
}

async fn ensure_table(store: &impl SchemaWrite) -> Result<(), SchemaVersionError> {
    store.schema_execute_batch(LEDGER_DDL).await
}

async fn has_ledger_table(store: &impl SchemaRead) -> Result<bool, SchemaVersionError> {
    let rows = store
        .schema_query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            &[text(TABLE)],
        )
        .await?;
    Ok(!rows.is_empty())
}

async fn read_recorded(
    store: &impl SchemaRead,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    if !has_ledger_table(store).await? {
        return Ok(None);
    }
    let rows = store
        .schema_query(
            "SELECT version FROM tracedecay_schema_version WHERE component = ?1",
            &[text(component)],
        )
        .await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(require_i64(
            cell(row, 0, "read schema version")?,
            component,
        )?)),
        _ => Err(database(
            "read schema version",
            "schema version returned more than one row",
        )),
    }
}

fn rusqlite_error(operation: &'static str, error: rusqlite::Error) -> SchemaVersionError {
    let busy = matches!(
        &error,
        rusqlite::Error::SqliteFailure(code, _) if matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    );
    SchemaVersionError::Database {
        operation,
        message: error.to_string(),
        busy,
    }
}

fn bind(params: &[ExactSqlValue]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .cloned()
        .map(ExactSqlValue::into_rusqlite)
        .collect()
}

fn cell_from_rusqlite(value: rusqlite::types::Value) -> Result<ExactSqlValue, SchemaVersionError> {
    Ok(match value {
        rusqlite::types::Value::Null => ExactSqlValue::Null,
        rusqlite::types::Value::Integer(value) => ExactSqlValue::Integer(value),
        rusqlite::types::Value::Real(value) => ExactSqlValue::Real(value),
        rusqlite::types::Value::Text(value) => ExactSqlValue::Text(value),
        rusqlite::types::Value::Blob(value) => ExactSqlValue::Blob(value),
    })
}

impl SchemaRead for rusqlite::Connection {
    async fn schema_query(
        &self,
        sql: &str,
        params: &[ExactSqlValue],
    ) -> Result<Vec<Vec<ExactSqlValue>>, SchemaVersionError> {
        let mut statement = self
            .prepare(sql)
            .map_err(|error| rusqlite_error("query schema version", error))?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(bind(params)))
            .map_err(|error| rusqlite_error("query schema version", error))?;
        let mut out = Vec::new();
        loop {
            let next = rows
                .next()
                .map_err(|error| rusqlite_error("query schema version", error))?;
            let Some(row) = next else {
                break;
            };
            let width = row.as_ref().column_count();
            let mut cells = Vec::with_capacity(width);
            for index in 0..width {
                let value = row
                    .get(index)
                    .map_err(|error| rusqlite_error("query schema version", error))?;
                cells.push(cell_from_rusqlite(value)?);
            }
            out.push(cells);
        }
        Ok(out)
    }
}

impl SchemaWrite for rusqlite::Connection {
    async fn schema_execute(
        &self,
        sql: &str,
        params: &[ExactSqlValue],
    ) -> Result<(), SchemaVersionError> {
        self.execute(sql, rusqlite::params_from_iter(bind(params)))
            .map(|_| ())
            .map_err(|error| rusqlite_error("write schema version", error))
    }

    async fn schema_execute_batch(&self, sql: &str) -> Result<(), SchemaVersionError> {
        self.execute_batch(sql)
            .map_err(|error| rusqlite_error("write schema version", error))
    }
}

/// Reads a version from a local connection without an async runtime.
pub fn read_version_sync(
    connection: &rusqlite::Connection,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    ready(read_version(connection, component))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_version_reads_back_and_an_unrecorded_one_is_absent() {
        let connection = rusqlite::Connection::open_in_memory().expect("open ledger test database");
        assert_eq!(
            read_version_sync(&connection, LCM_COMPONENT).expect("read absent version"),
            None
        );
        ready(record_version(&connection, LCM_COMPONENT, 15)).expect("record version");
        ready(record_version(&connection, LCM_COMPONENT, 16)).expect("re-record version");
        assert_eq!(
            read_version_sync(&connection, LCM_COMPONENT).expect("read version"),
            Some(16)
        );
        assert_eq!(
            read_version_sync(&connection, GRAPH_COMPONENT).expect("read sibling version"),
            None
        );
    }
}
