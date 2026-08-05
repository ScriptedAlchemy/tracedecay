//! Owner-scoped final-V2 fact lineage schema.

use serde::Serialize;
use tracedecay_domain::{FactOwnerV1, SourceStoreId};

use crate::db::engine::{self, Executor};
use crate::errors::{Result, TraceDecayError};

mod archive;
mod schema;
#[cfg(test)]
mod tests;
mod types;
mod writers;

pub use archive::{
    MemoryV2ArchiveDatabase, export_memory_v2_owner_archive, import_memory_v2_owner_archive,
    list_memory_v2_archive_owners, plan_memory_v2_owner_archive_import,
};
pub(in crate::db) use schema::create_schema;
pub(super) use schema::install_final_shape;
use types::OwnerKey;
pub(super) use writers::{
    clear_memory_v2_compatibility_bank_dirty_in_transaction,
    delete_memory_v2_compatibility_bank_in_transaction,
    mark_memory_v2_compatibility_bank_dirty_in_transaction,
    upsert_memory_v2_compatibility_bank_in_transaction,
};

const OPERATION: &str = "memory_v2";
const V1_COMPATIBILITY_SOURCE_STORE: &str = "legacy-memory-v1";
const COMPATIBILITY_BANK_VECTOR_BYTES: usize = 8 + 2048 * 4;
const COMPATIBILITY_BANK_VECTOR_HEADER: [u8; 8] = 2048_u64.to_le_bytes();

pub(in crate::db) trait MemoryV2Executor: Executor + Sync {}

impl<T> MemoryV2Executor for T where T: Executor + Sync + ?Sized {}

fn owner_key(owner: &FactOwnerV1) -> Result<OwnerKey> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    let (kind, project_id) = match owner {
        FactOwnerV1::Profile => ("profile", String::new()),
        FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
    };
    Ok(OwnerKey {
        kind,
        project_id,
        json: json_text(owner)?,
    })
}

fn validate_scope(owner: &FactOwnerV1, source_store_id: &SourceStoreId) -> Result<()> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    source_store_id
        .validate()
        .map_err(|_| db_message(OPERATION, "source store identity is invalid"))?;
    Ok(())
}

fn validate_v1_compatibility_source(source_store_id: &SourceStoreId) -> Result<()> {
    if source_store_id.as_str() == V1_COMPATIBILITY_SOURCE_STORE {
        Ok(())
    } else {
        Err(db_message(
            OPERATION,
            "V1 compatibility mappings require the fixed legacy-memory-v1 source store",
        ))
    }
}

fn json_text(value: &(impl Serialize + ?Sized)) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| db_message(OPERATION, "canonical JSON encoding failed"))
}

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
        .ok_or_else(|| db_message(OPERATION, "scalar query returned no row"))?
        .get(0)
        .map_err(|error| db_error(OPERATION, error))
}

async fn optional_string(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<Option<String>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get(0).map_err(|error| db_error(OPERATION, error)))
        .transpose()
}

async fn optional_i64(
    conn: &impl MemoryV2Executor,
    sql: &str,
    params: impl engine::IntoParams,
) -> Result<Option<i64>> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get(0).map_err(|error| db_error(OPERATION, error)))
        .transpose()
}

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

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_owned(),
    }
}

#[cfg(test)]
async fn begin(conn: &engine::Connection, operation: &str) -> Result<engine::Transaction> {
    conn.transaction_with_behavior(engine::TransactionBehavior::Immediate)
        .await
        .map_err(|error| db_error(operation, error))
}

#[cfg(test)]
async fn finish_transaction<T>(
    transaction: engine::Transaction,
    result: Result<T>,
    operation: &str,
) -> Result<T> {
    match result {
        Ok(value) => match transaction.commit().await {
            Ok(()) => Ok(value),
            Err(commit_error) => Err(db_message(
                operation,
                format!("commit failed; writer transaction retired: {commit_error}"),
            )),
        },
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

#[cfg(test)]
async fn scalar_i64(conn: &impl MemoryV2Executor, sql: &str) -> Result<i64> {
    scalar_i64_params(conn, sql, ()).await
}
