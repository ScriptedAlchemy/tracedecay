// Rust guideline compliant 2025-10-17
use std::path::Path;
use std::sync::{Arc, OnceLock};

use tracedecay_domain::{FactOwnerV1, SourceStoreId};
use tracedecay_rusqlite_runtime::{CheckpointBlockers, CheckpointOutcome, CheckpointRequest};
use tracedecay_store::{
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestProbeV1,
};

// The store-runtime registry moved into this kernel, so the facade retains the
// concrete handle rather than an erased port.
use crate::db::engine::{Connection, ReadSnapshot, Transaction, TransactionBehavior};
use crate::errors::{Result, TraceDecayError};
use crate::store_runtime::registry::StoreRuntimeHandle;

use super::{
    CapturedMemoryV2Frontiers, DatabaseAuthority, DatabaseAuthorityRole,
    MemoryV2BackfillBatchOutcome, memory_v2,
};

mod facade;
mod integrity;
mod memory_v2_authority;
mod pragmas;
mod query_write;
mod registry;
mod runtime_lifecycle;
mod snapshot_maintenance;
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
mod test_runtime;

#[cfg(test)]
pub(crate) use pragmas::{adaptive_cache_sizes, platform_safe_mmap_size};
use registry::{DatabaseInner, database_slot};

/// `SQLite` database backed by one daemon-owned native runtime attachment.
#[cfg_attr(
    not(any(feature = "test-helpers", feature = "test-transport")),
    doc = r"
Production builds do not expose writable daemonless fixture runtimes.

```compile_fail
use tracedecay::db::{Database, TestDatabaseRuntimeMode};

let _ = (Database::publish_test_runtime, TestDatabaseRuntimeMode::Initialize);
```
"
)]
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

pub enum DatabaseAccessMode {
    ReadOnly,
    ReadWrite,
}

impl DatabaseAccessMode {
    const fn is_writable(&self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

const NODES_FTS_CORRUPTION: &str = "malformed inverted index for FTS5 table main.nodes_fts";

struct DatabaseCheckpointProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for DatabaseCheckpointProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DatabaseHealth {
    Healthy,
    FtsOnlyCorruption(String),
    Corrupt(String),
}

static DATABASE_HEALTH_GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// A writer connection that cannot outlive the canonical database's writer
/// lane. It is another capability over the same physical attachment, never a
/// second path-derived `SQLite` open.
pub struct DatabaseWriterConnection<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    conn: Connection,
}

/// Driver-neutral graph query facade.
///
/// The retained graph connection remains private to this adapter while the
/// daemon runtime cutover replaces its physical owner.
#[derive(Clone)]
pub struct DatabaseEngineConnection {
    conn: Connection,
}

pub(crate) struct DatabaseEngineStatement<'a> {
    target: DatabaseEngineStatementTarget<'a>,
    sql: String,
}

pub struct DatabaseEngineReadSnapshot {
    snapshot: ReadSnapshot,
}

enum DatabaseEngineStatementTarget<'a> {
    Transaction(&'a Transaction),
}

/// Driver-neutral transaction used by the canonical memory store during the
/// physical database cutover.
pub enum DatabaseMemoryTransaction<'a> {
    Read(DatabaseEngineReadSnapshot),
    Write(DatabaseWriteTransaction<'a>),
}

/// Opaque, serialized access to memory mutations for integration fixtures.
///
/// This capability intentionally exposes neither the writable connection nor
/// arbitrary SQL execution.
#[doc(hidden)]
pub struct DatabaseMemoryWriter<'a> {
    writer: DatabaseWriterConnection<'a>,
}

/// An immediate transaction that retains the canonical writer lane until the
/// transaction commits, rolls back, or is dropped.
pub struct DatabaseWriteTransaction<'a> {
    transaction: Transaction,
    guard: tokio::sync::MutexGuard<'a, ()>,
}

fn registered_attachment_required(operation: &str, db_path: &Path) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: format!(
            "database '{}' is not mounted in the canonical runtime registry",
            db_path.display()
        ),
    }
}

fn database_checkpoint_probe() -> Result<DatabaseCheckpointProbe> {
    let cancellation_id = RuntimeCancellationIdV1::new("cancellation.database-checkpoint")
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to build checkpoint cancellation identity: {error}"),
            operation: "checkpoint".to_owned(),
        })?;
    let deadline_id =
        RuntimeDeadlineIdV1::new("deadline.database-checkpoint").map_err(|error| {
            TraceDecayError::Database {
                message: format!("failed to build checkpoint deadline identity: {error}"),
                operation: "checkpoint".to_owned(),
            }
        })?;
    Ok(DatabaseCheckpointProbe {
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id,
            generation: 1,
        },
        deadline: RuntimeDeadlineV1 { deadline_id },
    })
}

fn database_query_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_owned(),
    }
}

async fn database_health<Q>(conn: &Q, operation: &str) -> Result<DatabaseHealth>
where
    Q: crate::db::engine::QueryExecutor,
{
    let mut rows =
        conn.query("PRAGMA quick_check", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to run quick_check: {e}"),
                operation: operation.to_string(),
            })?;
    let mut results = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("failed to read quick_check result: {e}"),
        operation: operation.to_string(),
    })? {
        results.push(
            row.get::<String>(0)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to decode quick_check result: {e}"),
                    operation: operation.to_string(),
                })?,
        );
    }

    if results.as_slice() == ["ok"] {
        return Ok(DatabaseHealth::Healthy);
    }
    if !results.is_empty()
        && results
            .iter()
            .all(|result| is_nodes_fts_only_corruption(result))
    {
        return Ok(DatabaseHealth::FtsOnlyCorruption(results.join("; ")));
    }
    let problem = if results.is_empty() {
        "PRAGMA quick_check returned no rows".to_string()
    } else {
        results.join("; ")
    };
    Ok(DatabaseHealth::Corrupt(problem))
}

fn is_nodes_fts_only_corruption(problem: &str) -> bool {
    let problem = problem.trim();
    matches!(
        problem,
        NODES_FTS_CORRUPTION | "malformed inverted index for FTS5 table nodes_fts"
    ) || (problem.contains("fts5: corruption found") && problem.contains("nodes_fts"))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub use test_runtime::{TestDatabaseRuntimeMode, TestDatabaseRuntimeScope};

#[cfg(test)]
mod tests;
