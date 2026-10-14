//! The one schema-version ledger for every SQLite store.
//!
//! A store records each component's version in `tracedecay_schema_version`.
//! Callers install their own tables; this module only records which version
//! those tables are. A database written before this ledger — `PRAGMA
//! user_version`, `session_schema_migrations`, `session_temporal_schema_migrations`,
//! or `global_schema_migrations` — is adopted by [`retire_named_ledgers`] and
//! [`retire_user_version`]. Those functions move the stamp and drop the old
//! ledger. Reading never writes, so a refused store keeps the stamp it had.

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

const LEGACY_SESSION: &str = "session_schema_migrations";
const LEGACY_TEMPORAL: &str = "session_temporal_schema_migrations";
const LEGACY_GLOBAL: &str = "global_schema_migrations";

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

/// The version recorded for `component`, including a legacy stamp that has not
/// been retired yet. `Ok(None)` means this store has never recorded it.
/// Does not create or drop anything.
pub async fn read_version(
    store: &impl SchemaRead,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    if let Some(version) = read_recorded(store, component).await? {
        return Ok(Some(version));
    }
    read_component_legacy(store, component).await
}

/// Each legacy ledger held one family of components. A malformed table is that
/// family's unreadable stamp, not a failure of every other component.
async fn read_component_legacy(
    store: &impl SchemaRead,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    if component == GRAPH_COMPONENT {
        let version = user_version(store).await?;
        return Ok((version != 0).then_some(version));
    }
    if component == SESSION_TEMPORAL_COMPONENT {
        return read_named_legacy(store, LEGACY_TEMPORAL, component).await;
    }
    if component == LCM_COMPONENT
        || component == GIT_CORRELATION_COMPONENT
        || component == WORKFLOW_INDEX_COMPONENT
    {
        return read_named_legacy(store, LEGACY_SESSION, component).await;
    }
    if legacy_global_marker(store, component).await? {
        return Ok(Some(1));
    }
    Ok(None)
}

/// Copies every legacy named ledger into [`TABLE`] and drops those tables.
/// A component already recorded in [`TABLE`] keeps that version. A store with
/// no legacy ledger is left untouched, including stores that have not created
/// [`TABLE`] yet.
pub async fn retire_named_ledgers(store: &impl SchemaWrite) -> Result<(), SchemaVersionError> {
    let session = load_name_version_table(store, LEGACY_SESSION).await?;
    let temporal = load_name_version_table(store, LEGACY_TEMPORAL).await?;
    let global = load_global_markers(store).await?;
    let has_stamp = session.as_ref().is_some_and(|rows| !rows.is_empty())
        || temporal.as_ref().is_some_and(|rows| !rows.is_empty())
        || global.as_ref().is_some_and(|names| !names.is_empty());
    if has_stamp {
        ensure_table(store).await?;
        if let Some(rows) = &session {
            for (name, version) in rows {
                insert_ignore(store, name, *version).await?;
            }
        }
        if let Some(rows) = &temporal {
            for (name, version) in rows {
                insert_ignore(store, name, *version).await?;
            }
        }
        if let Some(names) = &global {
            for name in names {
                insert_ignore(store, name, 1).await?;
            }
        }
    }
    if session.is_some() {
        drop_legacy_table(store, LEGACY_SESSION).await?;
    }
    if temporal.is_some() {
        drop_legacy_table(store, LEGACY_TEMPORAL).await?;
    }
    if global.is_some() {
        drop_legacy_table(store, LEGACY_GLOBAL).await?;
    }
    Ok(())
}

/// Copies a non-zero `PRAGMA user_version` into [`GRAPH_COMPONENT`] when that
/// component has no row yet, then clears the pragma. The row is written
/// before the pragma is cleared, so a crash between the two still leaves a stamp.
pub async fn retire_user_version(store: &impl SchemaWrite) -> Result<(), SchemaVersionError> {
    let version = user_version(store).await?;
    if version == 0 {
        return Ok(());
    }
    ensure_table(store).await?;
    if read_recorded(store, GRAPH_COMPONENT).await?.is_none() {
        insert_ignore(store, GRAPH_COMPONENT, version).await?;
    }
    store
        .schema_execute_batch("PRAGMA user_version = 0")
        .await?;
    Ok(())
}

/// Records `version` for `component`, retiring any legacy named ledgers in the
/// same store first so a sibling stamp is not dropped on the floor.
pub async fn record_version(
    store: &impl SchemaWrite,
    component: &str,
    version: i64,
) -> Result<(), SchemaVersionError> {
    retire_named_ledgers(store).await?;
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
    let mut future = Box::pin(future);
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

async fn tables_present(store: &impl SchemaRead) -> Result<Vec<String>, SchemaVersionError> {
    let rows = store
        .schema_query(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name IN (?1, ?2, ?3, ?4)",
            &[
                text(TABLE),
                text(LEGACY_SESSION),
                text(LEGACY_TEMPORAL),
                text(LEGACY_GLOBAL),
            ],
        )
        .await?;
    rows.into_iter()
        .map(|row| match cell(&row, 0, "list schema ledgers")? {
            ExactSqlValue::Text(name) => Ok(name.clone()),
            other => Err(database(
                "list schema ledgers",
                format!("sqlite_master name was {other:?}"),
            )),
        })
        .collect()
}

async fn has_table(store: &impl SchemaRead, name: &str) -> Result<bool, SchemaVersionError> {
    Ok(tables_present(store)
        .await?
        .iter()
        .any(|present| present == name))
}

async fn read_recorded(
    store: &impl SchemaRead,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    if !has_table(store, TABLE).await? {
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

async fn read_named_legacy(
    store: &impl SchemaRead,
    table: &str,
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    if !has_table(store, table).await? {
        return Ok(None);
    }
    let rows = store
        .schema_query(
            &format!("SELECT version FROM {table} WHERE name = ?1"),
            &[text(component)],
        )
        .await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(require_i64(
            cell(row, 0, "read legacy schema version")?,
            component,
        )?)),
        _ => Err(database(
            "read legacy schema version",
            format!("{table} returned more than one row for {component}"),
        )),
    }
}

async fn legacy_global_marker(
    store: &impl SchemaRead,
    component: &str,
) -> Result<bool, SchemaVersionError> {
    if !has_table(store, LEGACY_GLOBAL).await? {
        return Ok(false);
    }
    let rows = store
        .schema_query(
            "SELECT 1 FROM global_schema_migrations WHERE migration = ?1",
            &[text(component)],
        )
        .await?;
    Ok(!rows.is_empty())
}

async fn user_version(store: &impl SchemaRead) -> Result<i64, SchemaVersionError> {
    let rows = store.schema_query("PRAGMA user_version", &[]).await?;
    let Some(row) = rows.first() else {
        return Ok(0);
    };
    require_i64(cell(row, 0, "read user_version")?, GRAPH_COMPONENT)
}

async fn insert_ignore(
    store: &impl SchemaWrite,
    component: &str,
    version: i64,
) -> Result<(), SchemaVersionError> {
    store
        .schema_execute(
            "INSERT INTO tracedecay_schema_version(component, version) VALUES (?1, ?2)
             ON CONFLICT(component) DO NOTHING",
            &[text(component), ExactSqlValue::Integer(version)],
        )
        .await
}

async fn load_name_version_table(
    store: &impl SchemaRead,
    table: &str,
) -> Result<Option<Vec<(String, i64)>>, SchemaVersionError> {
    if !has_table(store, table).await? {
        return Ok(None);
    }
    let rows = store
        .schema_query(&format!("SELECT name, version FROM {table}"), &[])
        .await?;
    let mut stamps = Vec::with_capacity(rows.len());
    for row in &rows {
        let name = match cell(row, 0, "read legacy schema version")? {
            ExactSqlValue::Text(name) => name.clone(),
            other => {
                return Err(database(
                    "read legacy schema version",
                    format!("{table} name was {other:?}"),
                ));
            }
        };
        let version = require_i64(cell(row, 1, "read legacy schema version")?, &name)?;
        stamps.push((name, version));
    }
    Ok(Some(stamps))
}

async fn load_global_markers(
    store: &impl SchemaRead,
) -> Result<Option<Vec<String>>, SchemaVersionError> {
    if !has_table(store, LEGACY_GLOBAL).await? {
        return Ok(None);
    }
    let rows = store
        .schema_query("SELECT migration FROM global_schema_migrations", &[])
        .await?;
    let mut names = Vec::with_capacity(rows.len());
    for row in &rows {
        match cell(row, 0, "read legacy schema marker")? {
            ExactSqlValue::Text(name) => names.push(name.clone()),
            other => {
                return Err(database(
                    "read legacy schema marker",
                    format!("global schema marker was {other:?}"),
                ));
            }
        }
    }
    Ok(Some(names))
}

async fn drop_legacy_table(
    store: &impl SchemaWrite,
    table: &str,
) -> Result<(), SchemaVersionError> {
    store
        .schema_execute_batch(&format!("DROP TABLE {table}"))
        .await
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

    fn connection() -> rusqlite::Connection {
        rusqlite::Connection::open_in_memory().expect("open ledger test database")
    }

    fn version(connection: &rusqlite::Connection, component: &str) -> Option<i64> {
        read_version_sync(connection, component).expect("read version")
    }

    fn table_exists(connection: &rusqlite::Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// The stamps a store created by this binary's previous ledgers can hold.
    /// Adoption must carry each one onto the single ledger and delete the old one.
    #[test]
    fn each_shipped_legacy_stamp_upgrades_onto_the_store_ledger() {
        let cases = [
            ("PRAGMA user_version = 39", GRAPH_COMPONENT, 39, true),
            (
                "CREATE TABLE session_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL,
                    applied_at INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO session_schema_migrations(name, version) VALUES
                    ('lcm', 15),
                    ('git_correlation', 9),
                    ('workflow_indexing', 1);",
                LCM_COMPONENT,
                15,
                false,
            ),
            (
                "CREATE TABLE session_temporal_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL,
                    applied_at INTEGER NOT NULL
                 );
                 INSERT INTO session_temporal_schema_migrations(name, version, applied_at)
                    VALUES ('session-temporal', 8, 1);",
                SESSION_TEMPORAL_COMPONENT,
                8,
                false,
            ),
            (
                "CREATE TABLE global_schema_migrations (migration TEXT PRIMARY KEY);
                 INSERT INTO global_schema_migrations(migration) VALUES
                    ('observations-v2-canonical-autoincrement'),
                    ('observations-unified-identity-v1'),
                    ('observation-repository-captures-v1-shared'),
                    ('session-canonical-bodies-root-refs-v1'),
                    ('session-canonical-bodies-lcm-v1'),
                    ('session-canonical-bodies-lcm-snippet-v1');",
                "observations-unified-identity-v1",
                1,
                false,
            ),
        ];

        for (sql, component, expected, user_version_stamp) in cases {
            let connection = connection();
            connection.execute_batch(sql).expect("install legacy stamp");
            assert_eq!(version(&connection, component), Some(expected));
            assert!(!table_exists(&connection, TABLE));
            if user_version_stamp {
                ready(retire_user_version(&connection)).expect("adopt user_version");
                let pragma: i64 = connection
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .expect("read pragma");
                assert_eq!(pragma, 0);
            } else {
                ready(retire_named_ledgers(&connection)).expect("adopt named ledger");
                assert!(!table_exists(&connection, LEGACY_SESSION));
                assert!(!table_exists(&connection, LEGACY_TEMPORAL));
                assert!(!table_exists(&connection, LEGACY_GLOBAL));
            }
            assert_eq!(version(&connection, component), Some(expected));
            assert!(table_exists(&connection, TABLE));
        }
    }

    #[test]
    fn sibling_session_stamps_move_together() {
        let connection = connection();
        connection
            .execute_batch(
                "CREATE TABLE session_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL
                 );
                 INSERT INTO session_schema_migrations(name, version) VALUES
                    ('lcm', 15),
                    ('git_correlation', 9),
                    ('workflow_indexing', 1);",
            )
            .expect("install session stamps");
        ready(record_version(&connection, LCM_COMPONENT, 15)).expect("record lcm");
        assert_eq!(version(&connection, LCM_COMPONENT), Some(15));
        assert_eq!(version(&connection, GIT_CORRELATION_COMPONENT), Some(9));
        assert_eq!(version(&connection, WORKFLOW_INDEX_COMPONENT), Some(1));
        assert!(!table_exists(&connection, LEGACY_SESSION));
    }

    #[test]
    fn reading_a_legacy_stamp_does_not_change_it() {
        let connection = connection();
        connection
            .execute_batch("PRAGMA user_version = 38")
            .expect("stamp an older graph");
        assert_eq!(version(&connection, GRAPH_COMPONENT), Some(38));
        let pragma: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(pragma, 38);
        assert!(!table_exists(&connection, TABLE));
    }

    #[test]
    fn an_unreadable_legacy_version_is_left_in_place() {
        let connection = connection();
        connection
            .execute_batch(
                "CREATE TABLE session_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL
                 );
                 INSERT INTO session_schema_migrations(name, version) VALUES ('lcm', 'unreadable');",
            )
            .expect("install unreadable stamp");
        let error = ready(retire_named_ledgers(&connection)).expect_err("unreadable stamp");
        assert!(matches!(error, SchemaVersionError::Unreadable { .. }));
        let stored: String = connection
            .query_row(
                "SELECT CAST(version AS TEXT) FROM session_schema_migrations WHERE name = 'lcm'",
                [],
                |row| row.get(0),
            )
            .expect("legacy stamp");
        assert_eq!(stored, "unreadable");
    }

    #[test]
    fn a_recorded_version_wins_over_a_different_legacy_stamp() {
        let connection = connection();
        ready(record_version(&connection, LCM_COMPONENT, 15)).expect("record");
        connection
            .execute_batch(
                "CREATE TABLE session_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL
                 );
                 INSERT INTO session_schema_migrations(name, version) VALUES ('lcm', 6);",
            )
            .expect("reintroduce legacy stamp");
        ready(retire_named_ledgers(&connection)).expect("retire");
        assert_eq!(version(&connection, LCM_COMPONENT), Some(15));
        assert!(!table_exists(&connection, LEGACY_SESSION));
    }
}
