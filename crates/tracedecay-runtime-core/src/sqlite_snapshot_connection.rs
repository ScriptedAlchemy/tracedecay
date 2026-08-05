use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, params_from_iter, types::ValueRef};

use crate::db::engine::{
    Error as EngineError, Executor, IntoParams, QueryExecutor, Row, Rows, Value,
};

pub struct SnapshotConnection {
    pub(super) connection: Arc<Mutex<Connection>>,
    #[cfg_attr(not(test), allow(dead_code))]
    interrupt: rusqlite::InterruptHandle,
}

impl SnapshotConnection {
    pub(super) fn open(path: &Path, flags: OpenFlags) -> crate::db::engine::Result<Self> {
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|error| snapshot_sqlite_error("open snapshot", error))?;
        let interrupt = connection.get_interrupt_handle();
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            interrupt,
        })
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        Executor::execute(self, sql, params).await
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        QueryExecutor::query(self, sql, params).await
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        Executor::execute_batch(self, sql).await
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn interrupt(&self) {
        self.interrupt.interrupt();
    }
}

impl QueryExecutor for SnapshotConnection {
    async fn query<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        let connection = Arc::clone(&self.connection);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| EngineError::Runtime("snapshot connection lock poisoned".into()))?;
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| snapshot_sqlite_error("prepare snapshot query", error))?;
            let columns = statement.column_count();
            let params = params.into_iter().map(engine_value_to_rusqlite);
            let mut rows = statement
                .query(params_from_iter(params))
                .map_err(|error| snapshot_sqlite_error("query snapshot", error))?;
            let mut collected = Vec::new();
            while let Some(row) = rows
                .next()
                .map_err(|error| snapshot_sqlite_error("read snapshot row", error))?
            {
                let values = (0..columns)
                    .map(|column| {
                        row.get_ref(column)
                            .map_err(|error| snapshot_sqlite_error("read snapshot value", error))
                            .and_then(snapshot_value)
                    })
                    .collect::<crate::db::engine::Result<Vec<_>>>()?;
                collected.push(Row::from_values(values));
            }
            Ok(Rows::from_rows(collected))
        })
        .await
        .map_err(|error| EngineError::Runtime(format!("snapshot query task failed: {error}")))?
    }
}

impl Executor for SnapshotConnection {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        let connection = Arc::clone(&self.connection);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| EngineError::Runtime("snapshot connection lock poisoned".into()))?;
            let changed = connection
                .execute(
                    &sql,
                    params_from_iter(params.into_iter().map(engine_value_to_rusqlite)),
                )
                .map_err(|error| snapshot_sqlite_error("execute snapshot statement", error))?;
            u64::try_from(changed)
                .map_err(|_| EngineError::Runtime("snapshot row count overflow".into()))
        })
        .await
        .map_err(|error| EngineError::Runtime(format!("snapshot execute task failed: {error}")))?
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        let connection = Arc::clone(&self.connection);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            connection
                .lock()
                .map_err(|_| EngineError::Runtime("snapshot connection lock poisoned".into()))?
                .execute_batch(&sql)
                .map_err(|error| snapshot_sqlite_error("execute snapshot batch", error))
        })
        .await
        .map_err(|error| EngineError::Runtime(format!("snapshot batch task failed: {error}")))?
    }
}

fn engine_value_to_rusqlite(value: Value) -> rusqlite::types::Value {
    match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Integer(value) => rusqlite::types::Value::Integer(value),
        Value::Real(value) => rusqlite::types::Value::Real(value),
        Value::Text(value) => rusqlite::types::Value::Text(value),
        Value::Blob(value) => rusqlite::types::Value::Blob(value),
    }
}

fn snapshot_value(value: ValueRef<'_>) -> crate::db::engine::Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Integer(value),
        ValueRef::Real(value) => Value::Real(value),
        ValueRef::Text(value) => Value::Text(
            std::str::from_utf8(value)
                .map_err(|error| EngineError::Runtime(format!("invalid snapshot UTF-8: {error}")))?
                .to_owned(),
        ),
        ValueRef::Blob(value) => Value::Blob(value.to_vec()),
    })
}

fn snapshot_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    match error {
        rusqlite::Error::SqliteFailure(code, message) => EngineError::Sqlite {
            operation,
            code: Some(code.extended_code & 0xff),
            extended_code: Some(code.extended_code),
            message: message.unwrap_or_else(|| code.to_string()),
        },
        error => EngineError::Sqlite {
            operation,
            code: None,
            extended_code: None,
            message: error.to_string(),
        },
    }
}
