//! Async executor binding for the store-version ledger in
//! `tracedecay-rusqlite-runtime`.

use tracedecay_rusqlite_runtime::exact_sql::ExactSqlValue;
use tracedecay_rusqlite_runtime::schema_version::{self, SchemaRead, SchemaWrite};

use super::engine::{Error as EngineError, Executor, QueryExecutor, Value};

pub use tracedecay_rusqlite_runtime::schema_version::{
    GIT_CORRELATION_COMPONENT, GRAPH_COMPONENT, LCM_COMPONENT, SESSION_TEMPORAL_COMPONENT,
    SchemaVersionError, TABLE, WORKFLOW_INDEX_COMPONENT, read_version_sync,
};

struct EngineSchema<T>(T);

fn map_error(operation: &'static str, error: EngineError) -> SchemaVersionError {
    SchemaVersionError::Database {
        operation,
        message: error.to_string(),
        busy: error.is_busy_or_locked(),
    }
}

fn to_engine(value: &ExactSqlValue) -> Value {
    match value {
        ExactSqlValue::Null => Value::Null,
        ExactSqlValue::Integer(value) => Value::Integer(*value),
        ExactSqlValue::Real(value) => Value::Real(*value),
        ExactSqlValue::Text(value) => Value::Text(value.clone()),
        ExactSqlValue::Blob(value) => Value::Blob(value.clone()),
    }
}

fn to_exact(value: &Value) -> ExactSqlValue {
    match value {
        Value::Null => ExactSqlValue::Null,
        Value::Integer(value) => ExactSqlValue::Integer(*value),
        Value::Real(value) => ExactSqlValue::Real(*value),
        Value::Text(value) => ExactSqlValue::Text(value.clone()),
        Value::Blob(value) => ExactSqlValue::Blob(value.clone()),
    }
}

fn row_cells(row: &super::engine::Row) -> Result<Vec<ExactSqlValue>, SchemaVersionError> {
    let mut cells = Vec::new();
    loop {
        let index = i32::try_from(cells.len()).map_err(|_| SchemaVersionError::Database {
            operation: "read schema version",
            message: "schema version row is wider than an i32 column index".to_owned(),
            busy: false,
        })?;
        match row.get::<Value>(index) {
            Ok(value) => cells.push(to_exact(&value)),
            Err(EngineError::InvalidColumn(_)) if !cells.is_empty() => return Ok(cells),
            Err(error) => return Err(map_error("read schema version", error)),
        }
    }
}

impl<T> SchemaRead for EngineSchema<&T>
where
    T: QueryExecutor + ?Sized,
{
    async fn schema_query(
        &self,
        sql: &str,
        params: &[ExactSqlValue],
    ) -> Result<Vec<Vec<ExactSqlValue>>, SchemaVersionError> {
        let params = params.iter().map(to_engine).collect::<Vec<_>>();
        let mut rows = self
            .0
            .query(sql, params)
            .await
            .map_err(|error| map_error("query schema version", error))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| map_error("query schema version", error))?
        {
            out.push(row_cells(&row)?);
        }
        Ok(out)
    }
}

impl<T> SchemaWrite for EngineSchema<&T>
where
    T: Executor + ?Sized,
{
    async fn schema_execute(
        &self,
        sql: &str,
        params: &[ExactSqlValue],
    ) -> Result<(), SchemaVersionError> {
        let params = params.iter().map(to_engine).collect::<Vec<_>>();
        self.0
            .execute(sql, params)
            .await
            .map(|_| ())
            .map_err(|error| map_error("write schema version", error))
    }

    async fn schema_execute_batch(&self, sql: &str) -> Result<(), SchemaVersionError> {
        self.0
            .execute_batch(sql)
            .await
            .map_err(|error| map_error("write schema version", error))
    }
}

pub async fn read_version(
    store: &(impl QueryExecutor + ?Sized),
    component: &str,
) -> Result<Option<i64>, SchemaVersionError> {
    schema_version::read_version(&EngineSchema(store), component).await
}

pub async fn record_version(
    store: &(impl Executor + ?Sized),
    component: &str,
    version: i64,
) -> Result<(), SchemaVersionError> {
    schema_version::record_version(&EngineSchema(store), component, version).await
}

pub async fn retire_named_ledgers(
    store: &(impl Executor + ?Sized),
) -> Result<(), SchemaVersionError> {
    schema_version::retire_named_ledgers(&EngineSchema(store)).await
}

pub async fn retire_user_version(
    store: &(impl Executor + ?Sized),
) -> Result<(), SchemaVersionError> {
    schema_version::retire_user_version(&EngineSchema(store)).await
}
