//! Owner-scoped V2 fact lineage schema and derived-projection writers.

use serde::Serialize;
use tracedecay_domain::FactOwnerV1;

#[cfg(test)]
use crate::db::engine;
use crate::db::engine::Executor;
use crate::errors::{Result, TraceDecayError};

mod schema;
#[cfg(test)]
mod tests;
mod types;
mod writers;

pub(in crate::db) use schema::create_schema;
use types::OwnerKey;
pub(super) use writers::{
    clear_memory_v2_bank_dirty_in_transaction, delete_memory_v2_bank_in_transaction,
    mark_memory_v2_bank_dirty_in_transaction, upsert_memory_v2_bank_in_transaction,
};

const OPERATION: &str = "memory_v2_store_v1";
const BANK_VECTOR_BYTES: usize = 8 + 2048 * 4;
const BANK_VECTOR_HEADER: [u8; 8] = 2048_u64.to_le_bytes();

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

fn json_text(value: &(impl Serialize + ?Sized)) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| db_message(OPERATION, "canonical JSON encoding failed"))
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

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_owned(),
    }
}
