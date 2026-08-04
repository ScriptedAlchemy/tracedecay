//! Owner-scoped final-V2 fact-lineage schema.

#[cfg(test)]
use crate::db::engine;
use crate::db::engine::Executor;
use crate::errors::{Result, TraceDecayError};

mod schema;
#[cfg(test)]
mod tests;

pub(in crate::db) use schema::create_schema;

#[cfg(test)]
const OPERATION: &str = "memory_v2_schema";

pub(in crate::db) trait MemoryV2Executor: Executor + Sync {}

impl<T> MemoryV2Executor for T where T: Executor + Sync + ?Sized {}

#[cfg(test)]
async fn scalar_i64_params(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<i64> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_error(OPERATION, "scalar query returned no row"))?
        .get(0)
        .map_err(|error| db_error(OPERATION, error))
}

#[cfg(test)]
async fn row_exists(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<bool> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .is_some())
}

fn db_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: storage operation failed: {error}"),
        operation: operation.to_owned(),
    }
}

#[cfg(test)]
async fn scalar_i64(conn: &impl MemoryV2Executor, sql: &str) -> Result<i64> {
    scalar_i64_params(conn, sql, ()).await
}
