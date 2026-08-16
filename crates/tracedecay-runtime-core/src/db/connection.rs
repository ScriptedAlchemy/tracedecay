// Rust guideline compliant 2025-10-17
use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};

use tracedecay_rusqlite_runtime::{CheckpointBlockers, CheckpointOutcome, CheckpointRequest};
use tracedecay_store::{
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestProbeV1,
};

// The store-runtime registry moved into this kernel, so the facade retains the
// concrete handle rather than an erased port.
use crate::db::engine::{Connection, ReadSnapshot, Transaction, TransactionBehavior};
use crate::errors::{Result, TraceDecayError};
use crate::store_runtime::registry::StoreRuntimeClientLease;

use super::{DatabaseAuthority, DatabaseAuthorityRole};

mod facade;
mod graph_binding;
mod integrity;
mod memory_graph_reconciliation;
mod pragmas;
mod query_write;
mod registry;
mod retained_maintenance;
mod runtime_lifecycle;
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
mod test_runtime;

pub(crate) use memory_graph_reconciliation::MemoryGraphReconciliationTaskScheduleV1;
pub use memory_graph_reconciliation::{
    MemoryGraphReconciliationRetirementBlockerV1, MemoryGraphReconciliationRetirementReservationV1,
    MemoryGraphReconciliationRuntimeErrorV1, MemoryGraphReconciliationTaskOwnerV1,
    ProjectMemoryReconciliationTelemetryObserverV1, ProjectMemoryReconciliationTelemetrySnapshotV1,
};
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

#[derive(Clone)]
pub(crate) struct WeakDatabase {
    inner: Weak<DatabaseInner>,
}

impl WeakDatabase {
    pub(crate) fn upgrade(&self) -> Option<Database> {
        self.inner.upgrade().map(|inner| Database { inner })
    }
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

    fn try_begin_commit(&self) -> bool {
        false
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DatabaseHealth {
    Healthy,
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

pub struct DatabaseEngineReadSnapshot {
    snapshot: ReadSnapshot,
}

/// Driver-neutral transaction used by the canonical memory store during the
/// physical database cutover.
pub enum DatabaseMemoryTransaction<'a> {
    Read(DatabaseEngineReadSnapshot),
    Write(DatabaseWriteTransaction<'a>),
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
    let problem = if results.is_empty() {
        "PRAGMA quick_check returned no rows".to_string()
    } else {
        results.join("; ")
    };
    Ok(DatabaseHealth::Corrupt(problem))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub use test_runtime::{TestDatabaseRuntimeMode, TestDatabaseRuntimeScope};

#[cfg(test)]
mod tests;
