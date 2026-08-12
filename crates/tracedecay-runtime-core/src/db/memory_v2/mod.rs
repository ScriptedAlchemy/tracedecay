//! Owner-scoped V2 fact lineage schema.

#[cfg(test)]
use serde::Serialize;
#[cfg(test)]
use tracedecay_domain::FactOwnerV1;

use crate::db::engine::Executor;
#[cfg(test)]
use crate::errors::Result;
use crate::errors::TraceDecayError;

mod schema;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod types;

pub(in crate::db) use schema::{FINAL_SCHEMA_BATCHES, create_schema};
#[cfg(test)]
use types::OwnerKey;

#[cfg(test)]
const OPERATION: &str = "memory_v2_store_v1";

pub(in crate::db) trait MemoryV2Executor: Executor + Sync {}

impl<T> MemoryV2Executor for T where T: Executor + Sync + ?Sized {}

#[cfg(test)]
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

#[cfg(test)]
fn json_text(value: &(impl Serialize + ?Sized)) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| db_message(OPERATION, "canonical JSON encoding failed"))
}

fn db_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: storage operation failed: {error}"),
        operation: operation.to_owned(),
    }
}

#[cfg(test)]
fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_owned(),
    }
}
