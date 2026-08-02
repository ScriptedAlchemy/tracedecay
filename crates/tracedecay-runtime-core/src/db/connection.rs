// Rust guideline compliant 2025-10-17
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use std::collections::BTreeMap;
use std::path::Path;
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use sha2::{Digest, Sha256};
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use tracedecay_domain::{BrainId, RepositoryId, UserProfileId, WorktreeId};
use tracedecay_domain::{FactOwnerV1, SourceStoreId};
use tracedecay_rusqlite_runtime::{CheckpointBlockers, CheckpointOutcome, CheckpointRequest};
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use tracedecay_store::{
    CodeShardScopeV1, LocatorDigest, ProjectId, StoreIncarnationV1, StoreShardIdV1,
    VerifiedStoreLocatorV1,
};
use tracedecay_store::{
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestProbeV1,
};

// The store-runtime registry moved into this kernel, so the facade retains the
// concrete handle rather than an erased port.
use crate::db::engine::{Connection, ReadSnapshot, Transaction, TransactionBehavior};
use crate::errors::{Result, TraceDecayError};
use crate::store_runtime::registry::StoreRuntimeHandle;
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use crate::store_runtime::registry::{
    LifecycleShardRuntimePublisher, ProfileAuthorityPinResult, ResolvedStoreLocator,
    StoreRuntimeKey, StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture,
    StoreRuntimeResolver,
};

use super::{
    CapturedMemoryV2Frontiers, DatabaseAuthority, MemoryV2BackfillBatchOutcome, memory_v2,
};

mod integrity;
mod memory_v2_authority;
mod pragmas;
mod registry;

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

#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestDatabaseRuntimeMode {
    Initialize,
    Existing,
    ReadOnly,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
struct ExactTestRuntimeResolver {
    locators: BTreeMap<StoreRuntimeKey, ExactTestRuntimeLocator>,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
struct ExactTestRuntimeLocator {
    verified: VerifiedStoreLocatorV1,
    path: PathBuf,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
impl StoreRuntimeResolver for ExactTestRuntimeResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        mode: StoreRuntimeOpenMode,
        database_authority: Option<&'a DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        'a,
        std::result::Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            let locator = self.locators.get(key).ok_or_else(|| {
                StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test runtime resolver received the wrong typed shard".to_owned(),
                }
            })?;
            let authority =
                database_authority.ok_or_else(|| StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test runtime publication requires exact database authority"
                        .to_owned(),
                })?;
            authority
                .require_active_write_scope("resolve canonical test runtime")
                .map_err(|error| StoreRuntimeRegistryFailure::ResolverFailed {
                    message: error.to_string(),
                })?;
            if authority.canonical_database_path() != locator.path {
                return Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test runtime authority does not match its exact locator".to_owned(),
                });
            }
            match (mode, locator.path.try_exists()) {
                (StoreRuntimeOpenMode::Initialize, Ok(false)) => {
                    Ok(ResolvedStoreLocator::prospective(
                        locator.verified.clone(),
                        locator.path.clone(),
                    ))
                }
                (StoreRuntimeOpenMode::Existing, Ok(true)) => Ok(ResolvedStoreLocator::new(
                    locator.verified.clone(),
                    locator.path.clone(),
                )),
                (StoreRuntimeOpenMode::Initialize, Ok(true)) => {
                    Err(StoreRuntimeRegistryFailure::ResolverFailed {
                        message: "test runtime initialization requires a missing database"
                            .to_owned(),
                    })
                }
                (StoreRuntimeOpenMode::Existing, Ok(false)) => {
                    Err(StoreRuntimeRegistryFailure::ResolverFailed {
                        message: "test runtime database does not exist".to_owned(),
                    })
                }
                (_, Err(error)) => Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: error.to_string(),
                }),
            }
        })
    }
}

/// Logical access granted by a canonical runtime mount.
///
/// This is deliberately independent of the physical runtime's writer
/// presence: one writable runtime can issue both read-only and read-write
/// database facades without opening the `SQLite` path again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseAccessMode {
    ReadOnly,
    ReadWrite,
}

impl DatabaseAccessMode {
    const fn is_writable(self) -> bool {
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

impl DatabaseWriterConnection<'_> {
    pub fn engine_connection(&self) -> &Connection {
        &self.conn
    }

    #[cfg(test)]
    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.conn.execute_batch(sql).await
    }

    #[cfg(test)]
    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.execute(sql, params).await
    }

    pub fn memory_store(&self) -> crate::memory::store::MemoryStore<'_> {
        crate::memory::store::MemoryStore::new_runtime(&self.conn)
    }

    #[cfg(test)]
    pub async fn execute_engine<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.execute(sql, params).await
    }

    #[cfg(test)]
    pub async fn query_engine<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.query(sql, params).await
    }
}

impl DatabaseEngineConnection {
    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.query(sql, params).await
    }
}

impl crate::db::engine::QueryExecutor for DatabaseEngineConnection {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseEngineConnection::query(self, sql, params).await
    }
}

impl DatabaseEngineStatement<'_> {
    pub async fn execute<P>(&self, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        match &self.target {
            DatabaseEngineStatementTarget::Transaction(transaction) => {
                transaction.execute(&self.sql, params).await
            }
        }
    }

    pub fn reset(&self) {}
}

impl DatabaseEngineReadSnapshot {
    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.snapshot.query(sql, params).await
    }

    pub async fn commit(self) -> crate::db::engine::Result<()> {
        drop(self);
        Ok(())
    }

    pub async fn rollback(self) -> crate::db::engine::Result<()> {
        drop(self);
        Ok(())
    }
}

impl crate::db::engine::QueryExecutor for DatabaseEngineReadSnapshot {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseEngineReadSnapshot::query(self, sql, params).await
    }
}

impl<'a> DatabaseMemoryTransaction<'a> {
    pub fn read(snapshot: DatabaseEngineReadSnapshot) -> Self {
        Self::Read(snapshot)
    }

    pub fn write(transaction: DatabaseWriteTransaction<'a>) -> Self {
        Self::Write(transaction)
    }

    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        match self {
            Self::Read(snapshot) => snapshot.query(sql, params).await,
            Self::Write(transaction) => transaction.query_engine(sql, params).await,
        }
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        match self {
            Self::Read(_) => Err(crate::db::engine::Error::Runtime(
                "cannot execute a write in a memory read snapshot".to_owned(),
            )),
            Self::Write(transaction) => transaction.execute_engine(sql, params).await,
        }
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        match self {
            Self::Read(_) => Err(crate::db::engine::Error::Runtime(
                "cannot execute a write in a memory read snapshot".to_owned(),
            )),
            Self::Write(transaction) => transaction.execute_batch_engine(sql).await,
        }
    }

    pub async fn commit(self) -> Result<()> {
        match self {
            Self::Read(snapshot) => {
                snapshot
                    .commit()
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("failed to commit memory read snapshot: {error}"),
                        operation: "commit memory read snapshot".to_owned(),
                    })
            }
            Self::Write(transaction) => transaction.commit().await,
        }
    }

    pub async fn rollback(self) -> Result<()> {
        match self {
            Self::Read(snapshot) => {
                snapshot
                    .rollback()
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("failed to roll back memory read snapshot: {error}"),
                        operation: "rollback memory read snapshot".to_owned(),
                    })
            }
            Self::Write(transaction) => transaction.rollback().await,
        }
    }
}

impl crate::db::engine::QueryExecutor for DatabaseMemoryTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseMemoryTransaction::query(self, sql, params).await
    }
}

impl crate::db::engine::Executor for DatabaseMemoryTransaction<'_> {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseMemoryTransaction::execute(self, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        DatabaseMemoryTransaction::execute_batch(self, sql).await
    }
}

impl crate::db::engine::DatabaseAttachmentExecutor for DatabaseMemoryTransaction<'_> {
    async fn attach_database(
        &self,
        path: &Path,
        database_name: &str,
    ) -> crate::db::engine::Result<()> {
        match self {
            Self::Read(_) => Err(crate::db::engine::Error::invalid_operation(
                "cannot attach a database to a memory read snapshot",
            )),
            Self::Write(transaction) => {
                transaction
                    .transaction
                    .attach_database(path, database_name)
                    .await
            }
        }
    }
}

impl DatabaseMemoryWriter<'_> {
    /// Returns a memory store whose writable connection remains protected by
    /// the canonical database writer lane for this capability's lifetime.
    pub fn store(&self) -> crate::memory::store::MemoryStore<'_> {
        self.writer.memory_store()
    }

    /// Returns a retriever bound to the same serialized memory authority.
    pub fn retriever(&self) -> crate::memory::retrieval::FactRetriever<'_> {
        crate::memory::retrieval::FactRetriever::new_runtime(&self.writer.conn)
    }
}

impl DatabaseWriteTransaction<'_> {
    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.execute(sql, params).await
    }

    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.query(sql, params).await
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }

    pub async fn execute_batch_engine(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }

    pub async fn execute_engine<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.execute(sql, params).await
    }

    pub async fn query_engine<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.query(sql, params).await
    }

    pub(crate) async fn prepare_engine(
        &self,
        sql: &str,
    ) -> crate::db::engine::Result<DatabaseEngineStatement<'_>> {
        self.transaction.validate(sql).await?;
        Ok(DatabaseEngineStatement {
            target: DatabaseEngineStatementTarget::Transaction(&self.transaction),
            sql: sql.to_owned(),
        })
    }

    pub async fn commit(self) -> Result<()> {
        let Self { transaction, guard } = self;
        let transaction = transaction.commit().await;
        drop(guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to commit isolated writer transaction: {error}"),
            operation: "commit write transaction".to_string(),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn rollback(self) -> Result<()> {
        let Self { transaction, guard } = self;
        let transaction = transaction.rollback().await;
        drop(guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to roll back isolated writer transaction: {error}"),
            operation: "rollback write transaction".to_string(),
        })
    }
}

impl crate::db::engine::QueryExecutor for DatabaseWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.query_engine(sql, params).await
    }
}

impl crate::db::engine::Executor for DatabaseWriteTransaction<'_> {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.execute_engine(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.execute_batch_engine(sql).await
    }
}

impl crate::db::engine::DatabaseAttachmentExecutor for DatabaseWriteTransaction<'_> {
    async fn attach_database(
        &self,
        path: &Path,
        database_name: &str,
    ) -> crate::db::engine::Result<()> {
        self.transaction.attach_database(path, database_name).await
    }
}

impl Database {
    pub fn retained_runtime(&self) -> &StoreRuntimeHandle {
        &self.inner._runtime
    }

    /// Canonical path held by this database's verified runtime locator.
    pub fn canonical_database_path(&self) -> &Path {
        &self.inner.canonical_path
    }

    /// Returns the canonical path bound to this already-open database.
    ///
    /// Primarily exposed for read-only inspection and integration fixtures;
    /// callers must not treat the path as a substitute for write authority.
    #[doc(hidden)]
    pub fn database_path(&self) -> &Path {
        self.canonical_database_path()
    }

    /// Physical `SQLite` identity captured when this retained handle was opened.
    pub fn opened_file_identity(&self) -> u64 {
        self.inner.opened_file_identity
    }

    pub fn filesystem_is_read_only(&self) -> bool {
        std::fs::metadata(self.canonical_database_path())
            .is_ok_and(|metadata| metadata.permissions().readonly())
    }

    /// Clones the originating revocable write capability for actor-time checks.
    pub fn write_authority(&self) -> Result<DatabaseAuthority> {
        if !self.inner.writable {
            return Err(integrity::read_only_upgrade_error(
                self.canonical_database_path(),
                "acquire database write authority",
            ));
        }
        self.inner
            ._authority
            .clone()
            .ok_or_else(|| TraceDecayError::Database {
                message: "writable database facade has no originating authority".to_owned(),
                operation: "acquire database write authority".to_owned(),
            })
    }

    /// Publishes one verified registry runtime as the only physical owner of
    /// this database path.
    ///
    /// The runtime already carries its typed binding, verified locator, and
    /// opened file identity. A read-write facade additionally retains the
    /// originating authority; a read-only facade never requests it. Neither
    /// mode derives identity from a path or extracts the physical attachment.
    pub async fn publish_runtime(
        runtime: StoreRuntimeHandle,
        access: DatabaseAccessMode,
    ) -> Result<Self> {
        let writable = access.is_writable();
        let authority = if writable {
            if !runtime.writer_present() {
                return Err(TraceDecayError::Database {
                    message: "registered runtime has no physical writer".to_owned(),
                    operation: "publish database runtime".to_owned(),
                });
            }
            let authority = runtime
                .database_authority("publish database runtime")
                .map_err(|error| TraceDecayError::Database {
                    message: format!("{error:?}"),
                    operation: "publish database runtime".to_owned(),
                })?;
            authority.require_active_write_scope("publish database runtime")?;
            Some(authority)
        } else {
            None
        };
        let slot = authority
            .as_ref()
            .map(|authority| database_slot(authority.database_identity_key()));
        if let Some(slot) = &slot {
            let mut open = slot.lock().await;
            if let Some(inner) = open.upgrade() {
                return Ok(Self { inner });
            }
            let inner = Arc::new(DatabaseInner::publish(
                runtime,
                true,
                authority,
                Some(Arc::clone(slot)),
            )?);
            *open = Arc::downgrade(&inner);
            return Ok(Self { inner });
        }
        DatabaseInner::publish(runtime, false, None, None)
            .map(Arc::new)
            .map(|inner| Self { inner })
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_test_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "canonical test runtime requires explicit test authority".to_owned(),
                operation: "publish test database runtime".to_owned(),
            });
        }
        Self::publish_fixture_runtime(db_path, authority, mode).await
    }

    /// Publishes an isolated integration-test fixture with the retained
    /// exclusive-maintenance authority whose scope the test controls.
    ///
    /// This remains separate from [`Self::publish_test_runtime`]: it accepts
    /// only maintenance authority, rejects production paths, and therefore
    /// preserves actor-time scope revocation without weakening the Test-only
    /// fixture escape hatch.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn publish_maintenance_test_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Maintenance {
            return Err(TraceDecayError::Database {
                message:
                    "maintenance test runtime requires explicit exclusive-maintenance authority"
                        .to_owned(),
                operation: "publish maintenance test database runtime".to_owned(),
            });
        }
        if !super::access::is_isolated_test_path(db_path) {
            return Err(TraceDecayError::Database {
                message: format!(
                    "maintenance test database must be inside an isolated test root at '{}'",
                    db_path.display()
                ),
                operation: "publish maintenance test database runtime".to_owned(),
            });
        }
        Self::publish_fixture_runtime(db_path, authority, mode).await
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    async fn publish_fixture_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
    ) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "publish test database runtime")?;
        authority.require_active_write_scope("publish test database runtime")?;
        let path = authority.canonical_database_path().to_path_buf();
        let existing_slot = database_slot(authority.database_identity_key());
        if let Some(inner) = existing_slot.lock().await.upgrade() {
            if mode == TestDatabaseRuntimeMode::ReadOnly {
                let database =
                    Self::publish_runtime(inner._runtime.clone(), DatabaseAccessMode::ReadOnly)
                        .await?;
                return Ok((database, false));
            }
            return Ok((Self { inner }, false));
        }
        let brain_id = BrainId::try_from("brain.test-runtime".to_owned()).map_err(|error| {
            test_runtime_error("construct test brain identity", error.to_string())
        })?;
        let profile_id =
            UserProfileId::try_from("profile.test-runtime".to_owned()).map_err(|error| {
                test_runtime_error("construct test profile identity", error.to_string())
            })?;
        let profile_shard = StoreShardIdV1::profile(brain_id.clone(), profile_id.clone());
        let code_shard = StoreShardIdV1::code(
            brain_id,
            profile_id,
            ProjectId::try_from("project.test-runtime".to_owned()).map_err(|error| {
                test_runtime_error("construct test project identity", error.to_string())
            })?,
            RepositoryId::try_from("repository.test-runtime".to_owned()).map_err(|error| {
                test_runtime_error("construct test repository identity", error.to_string())
            })?,
            CodeShardScopeV1::Worktree {
                worktree_id: WorktreeId::try_from("worktree.test-runtime".to_owned()).map_err(
                    |error| {
                        test_runtime_error("construct test worktree identity", error.to_string())
                    },
                )?,
            },
        );
        let incarnation = StoreIncarnationV1::new(1)
            .map_err(|error| test_runtime_error("construct test incarnation", error.to_string()))?;
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.test-runtime.profile.v1\0");
        digest.update(path.as_os_str().as_encoded_bytes());
        let profile_name = format!(
            ".tracedecay-test-profile-{}.db",
            &hex::encode(digest.finalize())[..16]
        );
        let profile_path = path.with_file_name(profile_name);
        let (profile_key, profile_locator) =
            exact_test_runtime_locator(profile_shard.clone(), incarnation, profile_path.clone())?;
        let (code_key, code_locator) =
            exact_test_runtime_locator(code_shard.clone(), incarnation, path)?;
        let mut locators = BTreeMap::new();
        locators.insert(profile_key, profile_locator);
        locators.insert(code_key, code_locator);
        let resolver = Arc::new(ExactTestRuntimeResolver { locators });
        let registry =
            StoreRuntimeRegistry::new(resolver, Arc::new(LifecycleShardRuntimePublisher));
        let profile_authority =
            DatabaseAuthority::acquire_test(&profile_path, "publish test profile runtime")?;
        let profile_exists = profile_path.try_exists().map_err(|error| {
            test_runtime_error("inspect test profile runtime", error.to_string())
        })?;
        let profile_request = if profile_exists {
            StoreRuntimeOpenRequest::new_authorized(
                profile_shard.clone(),
                incarnation,
                None,
                profile_authority,
            )
        } else {
            StoreRuntimeOpenRequest::new_initialize_authorized(
                profile_shard.clone(),
                incarnation,
                None,
                profile_authority,
            )
        };
        let _profile_runtime = match registry.open(profile_request).await {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(test_runtime_error(
                    "publish test profile runtime",
                    format!("{failure:?}"),
                ));
            }
        };
        let profile_pin = match registry.profile_authority_pin(&profile_shard) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            outcome => {
                return Err(test_runtime_error(
                    "pin test profile runtime",
                    format!("{outcome:?}"),
                ));
            }
        };
        let open_mode = match mode {
            TestDatabaseRuntimeMode::Initialize => StoreRuntimeOpenMode::Initialize,
            TestDatabaseRuntimeMode::Existing | TestDatabaseRuntimeMode::ReadOnly => {
                StoreRuntimeOpenMode::Existing
            }
        };
        let request = match open_mode {
            StoreRuntimeOpenMode::Initialize => StoreRuntimeOpenRequest::new_initialize_authorized(
                code_shard,
                incarnation,
                Some(profile_pin),
                authority.clone(),
            ),
            StoreRuntimeOpenMode::Existing => StoreRuntimeOpenRequest::new_authorized(
                code_shard,
                incarnation,
                Some(profile_pin),
                authority.clone(),
            ),
        };
        let runtime = match registry.open(request).await {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(test_runtime_error(
                    "publish test database runtime",
                    format!("{failure:?}"),
                ));
            }
        };
        let _schema_initialized = runtime.schema_migrated();
        let access = if mode == TestDatabaseRuntimeMode::ReadOnly {
            DatabaseAccessMode::ReadOnly
        } else {
            DatabaseAccessMode::ReadWrite
        };
        let database = Self::publish_runtime(runtime, access).await?;
        let migrated = match mode {
            TestDatabaseRuntimeMode::Initialize => {
                crate::db::migrations::migrate(&database).await?;
                false
            }
            TestDatabaseRuntimeMode::Existing => {
                crate::db::migrations::migrate(&database).await?.is_some()
            }
            TestDatabaseRuntimeMode::ReadOnly => false,
        };
        Ok((database, migrated))
    }

    /// Legacy compatibility lookup.
    ///
    /// Physical creation and schema bootstrap are owned by the registered
    /// runtime. This method can reuse an attachment already published for the
    /// exact authority, but it never opens a path or invents store identity.
    pub async fn initialize(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "initialize")?;
        authority.require_active_write_scope("initialize")?;
        let slot = database_slot(authority.database_identity_key());
        let open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "initialize"));
            }
            return Ok((Self { inner }, false));
        }
        Err(registered_attachment_required("initialize", db_path))
    }

    /// Reuses an already-published writable attachment for `db_path`.
    pub async fn open(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open")?;
        authority.require_active_write_scope("open")?;
        let slot = database_slot(authority.database_identity_key());
        let open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "open"));
            }
            return Ok((Self { inner }, false));
        }
        Err(registered_attachment_required("open", db_path))
    }

    /// Reuses an already-published attachment for a read-only caller.
    pub async fn open_read_only(
        db_path: &Path,
        authority: &DatabaseAuthority,
    ) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open_read_only")?;
        let slot = database_slot(authority.database_identity_key());
        if let Some(inner) = slot.lock().await.upgrade() {
            let read_only = DatabaseInner::publish(inner._runtime.clone(), false, None, None)?;
            return Ok((
                Self {
                    inner: Arc::new(read_only),
                },
                false,
            ));
        }
        Err(registered_attachment_required("open_read_only", db_path))
    }

    /// Returns the canonical runtime facade.
    ///
    /// Mutations must use [`Self::writer_connection`] or an isolated
    /// transaction while holding [`Self::writer`].
    pub fn conn(&self) -> &Connection {
        &self.inner.conn
    }

    /// Runs a bounded scalar inspection on the retained runtime, projecting the
    /// first column of the first row.
    async fn query_scalar<T, P>(&self, operation: &str, sql: &str, params: P) -> Result<T>
    where
        T: crate::db::engine::FromValue,
        P: crate::db::engine::IntoParams,
    {
        let mut rows = self
            .inner
            .conn
            .query(sql, params)
            .await
            .map_err(|error| database_query_error(operation, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| database_query_error(operation, error))?
            .ok_or_else(|| TraceDecayError::Database {
                message: "scalar query returned no rows".to_owned(),
                operation: operation.to_owned(),
            })?;
        row.get(0)
            .map_err(|error| database_query_error(operation, error))
    }

    /// Runs a bounded scalar integer inspection on the retained runtime.
    #[doc(hidden)]
    pub async fn query_scalar_i64(&self, operation: &str, sql: &str) -> Result<i64> {
        self.query_scalar(operation, sql, ()).await
    }

    /// Runs a bounded scalar blob inspection on the retained runtime.
    #[doc(hidden)]
    pub async fn query_scalar_blob(&self, operation: &str, sql: &str) -> Result<Vec<u8>> {
        self.query_scalar(operation, sql, ()).await
    }

    /// Runs a bounded scalar text inspection on the retained runtime.
    #[doc(hidden)]
    pub async fn query_scalar_text(&self, operation: &str, sql: &str) -> Result<String> {
        self.query_scalar(operation, sql, ()).await
    }

    /// Runs a bounded scalar integer inspection with one text identity bound.
    #[doc(hidden)]
    pub async fn query_scalar_i64_with_text(
        &self,
        operation: &str,
        sql: &str,
        value: &str,
    ) -> Result<i64> {
        self.query_scalar(operation, sql, (value,)).await
    }

    pub fn engine_conn(&self) -> DatabaseEngineConnection {
        DatabaseEngineConnection {
            conn: self.inner.conn.clone(),
        }
    }

    /// Executes one autocommit-style mutation through the canonical writer
    /// broker. Primarily useful for fixtures and maintenance adapters that do
    /// not need to retain a raw writable connection.
    // Visible outside the crate: integration suites in `tests/` are external
    // crates and exercise this fixture path directly.
    #[doc(hidden)]
    pub async fn execute_write<P>(&self, operation: &str, sql: &str, params: P) -> Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.execute_write_engine(operation, sql, params).await
    }

    pub async fn execute_write_engine<P>(
        &self,
        operation: &str,
        sql: &str,
        params: P,
    ) -> Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        let transaction = self.begin_write_transaction(operation).await?;
        let changed = transaction
            .execute_engine(sql, params)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to execute brokered write: {error}"),
                operation: operation.to_owned(),
            })?;
        transaction.commit().await?;
        Ok(changed)
    }

    /// Executes a SQL batch atomically through the canonical writer broker.
    #[doc(hidden)]
    pub async fn execute_write_batch(&self, operation: &str, sql: &str) -> Result<()> {
        let transaction = self.begin_write_transaction(operation).await?;
        transaction
            .execute_batch(sql)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to execute brokered write batch: {error}"),
                operation: operation.to_string(),
            })?;
        transaction.commit().await
    }

    /// Acquires the process-local writer lane for this canonical database.
    ///
    /// Writable handles opened for the same database share one `DatabaseInner`,
    /// so this guard coordinates MCP, dashboard, and automation mutations.
    pub async fn writer(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.writer.lock().await
    }

    fn require_active_write_scope(&self, operation: &str) -> Result<()> {
        if !self.inner.writable {
            return Err(integrity::read_only_upgrade_error(
                self.canonical_database_path(),
                operation,
            ));
        }
        self.inner
            ._authority
            .as_ref()
            .ok_or_else(|| {
                integrity::read_only_upgrade_error(self.canonical_database_path(), operation)
            })?
            .require_active_write_scope(operation)
    }

    pub(super) async fn open_writer_connection_unguarded(
        &self,
        operation: &str,
    ) -> Result<Connection> {
        self.require_active_write_scope(operation)?;
        self.inner.write_conn.clone().ok_or_else(|| {
            integrity::read_only_upgrade_error(self.canonical_database_path(), operation)
        })
    }

    /// Opens an isolated writer while holding the process-local writer lane.
    /// The handle cannot escape the guard, preventing raw DML from bypassing
    /// serialization or joining a transaction on the retained reader.
    pub async fn writer_connection(&self, operation: &str) -> Result<DatabaseWriterConnection<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        Ok(DatabaseWriterConnection {
            _guard: guard,
            conn,
        })
    }

    /// Acquires opaque, serialized access to memory mutations.
    #[doc(hidden)]
    pub async fn memory_writer(&self) -> Result<DatabaseMemoryWriter<'_>> {
        Ok(DatabaseMemoryWriter {
            writer: self
                .writer_connection("memory store writer capability")
                .await?,
        })
    }

    /// Whether this owner has no V1 legacy memory at all, so the cutover
    /// ladder would only manufacture an all-zero backfill row and a receipt
    /// for a migration that never happened.
    pub(crate) async fn memory_v2_cutover_is_vacuous(
        &self,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
    ) -> Result<bool> {
        let writer = self
            .writer_connection("probe memory v2 legacy cutover source")
            .await?;
        memory_v2::memory_v2_cutover_is_vacuous(&writer.conn, owner, source_store_id).await
    }

    pub(crate) async fn load_or_capture_memory_v2_frontiers(
        &self,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
    ) -> Result<CapturedMemoryV2Frontiers> {
        let writer = self
            .writer_connection("capture memory v2 backfill frontiers")
            .await?;
        memory_v2::load_or_capture_memory_v2_frontiers(&writer.conn, owner, source_store_id).await
    }

    pub async fn backfill_memory_v2_batch(
        &self,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        frontiers: CapturedMemoryV2Frontiers,
        batch_size: i64,
    ) -> Result<MemoryV2BackfillBatchOutcome> {
        let writer = self
            .writer_connection("backfill one memory v2 batch")
            .await?;
        memory_v2::backfill_memory_v2_batch(
            &writer.conn,
            owner,
            source_store_id,
            frontiers,
            batch_size,
        )
        .await
    }

    /// Starts a query-only snapshot on a separate connection that cannot join
    /// a transaction running on the retained writable connection.
    pub(crate) async fn begin_isolated_read_snapshot(
        &self,
        operation: &str,
    ) -> Result<ReadSnapshot> {
        self.inner
            .conn
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin isolated read snapshot: {error}"),
                operation: operation.to_string(),
            })
    }

    pub async fn begin_engine_read_snapshot(
        &self,
        operation: &str,
    ) -> Result<DatabaseEngineReadSnapshot> {
        self.begin_isolated_read_snapshot(operation)
            .await
            .map(|snapshot| DatabaseEngineReadSnapshot { snapshot })
    }

    /// Starts a query-only snapshot on the reserved health-reader lane.
    /// Health aggregates use this lane so they remain available when general
    /// readers are saturated by background graph work.
    pub(crate) async fn begin_engine_health_read_snapshot(
        &self,
        operation: &str,
    ) -> Result<DatabaseEngineReadSnapshot> {
        self.inner
            .conn
            .health_read_snapshot()
            .await
            .map(|snapshot| DatabaseEngineReadSnapshot { snapshot })
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin health read snapshot: {error}"),
                operation: operation.to_string(),
            })
    }

    pub async fn begin_memory_read_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseMemoryTransaction<'_>> {
        self.begin_engine_read_snapshot(operation)
            .await
            .map(DatabaseMemoryTransaction::read)
    }

    /// Starts an immediate transaction that owns the canonical writer lane.
    /// Dropping the returned capability rolls back before releasing the lane.
    pub async fn begin_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseWriteTransaction<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin isolated writer transaction: {error}"),
                operation: operation.to_string(),
            })?;
        Ok(DatabaseWriteTransaction { transaction, guard })
    }

    /// Starts an atomic bulk-replacement transaction on the canonical writer.
    ///
    /// A full index can contain more than a million rows. Unlike an ordinary
    /// mutation, it can legitimately remain active beyond the fixed
    /// transaction lease while continuously making progress. The runtime's
    /// migration policy renews that lease only after successful commands;
    /// idle transactions, revoked authority, and shutdown still cancel it.
    pub async fn begin_bulk_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseWriteTransaction<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        let transaction = conn.schema_migration_transaction().await.map_err(|error| {
            TraceDecayError::Database {
                message: format!("failed to begin bulk writer transaction: {error}"),
                operation: operation.to_string(),
            }
        })?;
        Ok(DatabaseWriteTransaction { transaction, guard })
    }

    pub async fn begin_memory_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseMemoryTransaction<'_>> {
        self.begin_write_transaction(operation)
            .await
            .map(DatabaseMemoryTransaction::write)
    }

    /// Releases this database handle.
    ///
    /// The underlying connection remains open until all cloned handles are
    /// released.
    pub fn close(self) {
        drop(self);
    }

    /// Applies the canonical runtime's bounded WAL checkpoint policy.
    pub async fn checkpoint(&self) -> Result<()> {
        self.require_active_write_scope("checkpoint")?;
        let _writer = self.writer().await;
        self.checkpoint_unguarded().await
    }

    pub async fn release_connection_memory(&self) -> Result<()> {
        self.inner
            .conn
            .execute_batch("PRAGMA shrink_memory")
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to release graph reader cache: {error}"),
                operation: "release graph database memory".to_owned(),
            })?;
        if let Some(connection) = &self.inner.write_conn {
            connection
                .execute_batch("PRAGMA shrink_memory")
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to release graph writer cache: {error}"),
                    operation: "release graph database memory".to_owned(),
                })?;
        }
        Ok(())
    }

    /// Forces a complete WAL truncation through the retained writer actor.
    ///
    /// This is narrower than the pressure-based runtime checkpoint policy:
    /// only an exclusive-maintenance authority may use it, and success means
    /// `SQLite` reported no busy readers and no remaining log frames. Offline
    /// migration artifacts need that proof before they can be attached.
    pub async fn truncate_wal_for_offline_maintenance(&self) -> Result<()> {
        self.require_active_write_scope("truncate WAL for offline maintenance")?;
        let authority = self.write_authority()?;
        if authority.role() != super::DatabaseAuthorityRole::Maintenance {
            return Err(TraceDecayError::Database {
                message: "WAL truncation requires exclusive maintenance authority".to_owned(),
                operation: "truncate WAL for offline maintenance".to_owned(),
            });
        }
        let _writer = self.writer().await;
        let connection = self
            .open_writer_connection_unguarded("truncate WAL for offline maintenance")
            .await?;
        let mut rows = connection
            .checkpoint_wal_truncate()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to truncate WAL through the writer actor: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read WAL truncation result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "WAL truncation returned no result".to_owned(),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let busy = row
            .get::<i64>(0)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation busy result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let log_frames = row
            .get::<i64>(1)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation frame result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let checkpointed_frames = row
            .get::<i64>(2)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation checkpoint result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        if busy != 0 || log_frames != 0 || checkpointed_frames != 0 {
            return Err(TraceDecayError::Database {
                message: format!(
                    "WAL truncation incomplete: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
                ),
                operation: "truncate WAL for offline maintenance".to_owned(),
            });
        }
        if rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to finish WAL truncation result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?
            .is_some()
        {
            return Err(TraceDecayError::Database {
                message: "WAL truncation returned multiple results".to_owned(),
                operation: "truncate WAL for offline maintenance".to_owned(),
            });
        }
        Ok(())
    }

    /// Produces a standalone checkpointed fixture artifact.
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub async fn truncate_wal_for_test_artifact(&self) -> Result<()> {
        self.truncate_wal_for_offline_maintenance().await
    }

    pub(crate) async fn checkpoint_unguarded(&self) -> Result<()> {
        let authority = self.write_authority()?;
        let request = CheckpointRequest::new(
            CheckpointBlockers::default(),
            Arc::new(database_checkpoint_probe()?),
        );
        let outcome = self
            .inner
            ._runtime
            .run_checkpoint(request, authority)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("registered checkpoint failed: {error:?}"),
                operation: "checkpoint".to_owned(),
            })?;
        match outcome {
            CheckpointOutcome::BelowSoft { .. } | CheckpointOutcome::Complete { .. } => Ok(()),
            CheckpointOutcome::Pending { .. } => Err(TraceDecayError::Database {
                message: "registered checkpoint remains pending".to_owned(),
                operation: "checkpoint".to_owned(),
            }),
            CheckpointOutcome::Interrupted { reason, .. } => Err(TraceDecayError::Database {
                message: format!("registered checkpoint was interrupted: {reason:?}"),
                operation: "checkpoint".to_owned(),
            }),
        }
    }

    /// Writes a transactionally consistent copy of this database.
    ///
    /// The writer-owned online-backup command copies one consistent `SQLite`
    /// snapshot in bounded steps. The destination must not already exist.
    pub async fn snapshot_to(&self, destination: &Path) -> Result<()> {
        self.require_active_write_scope("snapshot_to")?;
        let _writer = self.writer().await;
        self.snapshot_to_unguarded(destination).await
    }

    pub(crate) async fn snapshot_to_unguarded(&self, destination: &Path) -> Result<()> {
        if destination.to_str().is_none() {
            return Err(TraceDecayError::Database {
                message: format!(
                    "snapshot destination is not valid UTF-8: '{}'",
                    destination.display()
                ),
                operation: "snapshot".to_string(),
            });
        }
        let authority = self.write_authority()?;
        self.inner
            ._runtime
            .snapshot_to(destination.to_path_buf(), authority)
            .await
            .map(|_| ())
            .map_err(|error| TraceDecayError::Database {
                message: format!("registered online backup failed: {error:?}"),
                operation: "snapshot".to_owned(),
            })
    }

    /// Runs VACUUM and ANALYZE to reclaim space and update query planner statistics.
    /// Returns the on-disk size of the database file in bytes.
    pub async fn size(&self) -> Result<u64> {
        let mut rows = self
            .inner
            .conn
            .query(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                (),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to get database size: {e}"),
                operation: "size".to_string(),
            })?;

        let row = rows
            .next()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to read database size row: {e}"),
                operation: "size".to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "no result from page size query".to_string(),
                operation: "size".to_string(),
            })?;

        let size = row.get::<i64>(0).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read size value: {e}"),
            operation: "size".to_string(),
        })?;

        Ok(size as u64)
    }

    /// Runs `PRAGMA quick_check` and returns `true` if the database is intact.
    ///
    /// This is faster than `integrity_check` — it verifies B-tree structure
    /// without cross-checking index contents against table data.
    pub async fn quick_check(&self) -> Result<bool> {
        Ok(self.quick_check_report().await?.is_none())
    }

    /// Runs `PRAGMA quick_check` on a fresh reader and returns its problem
    /// report, if any.
    ///
    /// `None` means the database is intact. A pragma that returns no rows is
    /// reported as a problem rather than silently treated as healthy.
    pub async fn quick_check_report(&self) -> Result<Option<String>> {
        Ok(match self.health_on_fresh_reader("quick_check").await? {
            DatabaseHealth::Healthy => None,
            DatabaseHealth::FtsOnlyCorruption(problem) | DatabaseHealth::Corrupt(problem) => {
                Some(problem)
            }
        })
    }

    /// Rebuilds the FTS5 index from the content table under the canonical
    /// writer lane.
    ///
    /// This fixes FTS-only corruption (e.g. from an interrupted bulk load)
    /// without requiring a full re-index of the codebase. Callers must hold
    /// managed-daemon or exclusive-maintenance authority; read paths must
    /// never invoke this.
    pub async fn rebuild_fts(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("rebuild_fts").await?;
        self.rebuild_fts_unguarded(&transaction).await?;
        transaction.commit().await
    }

    /// Checks the retained post-open connection and repairs only a proven
    /// `nodes_fts`-only failure. The existing rebuild path owns the canonical
    /// writer lane, so concurrent writers complete before repair starts.
    pub async fn repair_fts_after_open(&self) -> Result<Option<String>> {
        let problem = match self.health_on_fresh_reader("post_open_health").await? {
            DatabaseHealth::Healthy => return Ok(None),
            DatabaseHealth::FtsOnlyCorruption(problem) => problem,
            DatabaseHealth::Corrupt(problem) => {
                return Err(TraceDecayError::Database {
                    message: format!("database quick_check failed: {problem}"),
                    operation: "post_open_health".to_string(),
                });
            }
        };

        self.rebuild_fts().await?;
        match self.health_on_fresh_reader("post_repair_health").await? {
            DatabaseHealth::Healthy => Ok(Some(problem)),
            DatabaseHealth::FtsOnlyCorruption(remaining) | DatabaseHealth::Corrupt(remaining) => {
                Err(TraceDecayError::Database {
                    message: format!("FTS repair did not restore database health: {remaining}"),
                    operation: "post_repair_health".to_string(),
                })
            }
        }
    }

    async fn health_on_fresh_reader(&self, operation: &str) -> Result<DatabaseHealth> {
        let queued_at = std::time::Instant::now();
        let _health_guard = DATABASE_HEALTH_GATE
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let wait_ms = u64::try_from(queued_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::debug!(
            event = "database_health_check",
            phase = "start",
            operation,
            wait_ms,
            "database health check started"
        );
        let started_at = std::time::Instant::now();
        let snapshot = self.inner.conn.health_read_snapshot().await.map_err(|e| {
            TraceDecayError::Database {
                message: format!("failed to begin database health snapshot: {e}"),
                operation: operation.to_string(),
            }
        })?;
        let result = database_health(&snapshot, operation).await;
        let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::debug!(
            event = "database_health_check",
            phase = "complete",
            operation,
            elapsed_ms,
            healthy = matches!(&result, Ok(DatabaseHealth::Healthy)),
            "database health check finished"
        );
        result
    }

    pub(crate) async fn rebuild_fts_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
            .execute_batch(
                "DROP TABLE nodes_fts;
                 CREATE VIRTUAL TABLE nodes_fts USING fts5(
                     name, qualified_name, docstring, signature,
                     content='nodes', content_rowid='rowid'
                 );
                 INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');",
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to rebuild FTS index: {e}"),
                operation: "rebuild_fts".to_string(),
            })?;
        Ok(())
    }

    /// Drops secondary indexes, disables fsync/FK, and clears FTS for fast
    /// bulk loading. Callers should insert data sorted by PK so the primary
    /// B-tree gets sequential appends. Call `end_bulk_load` afterwards to
    /// rebuild indexes in one optimized pass.
    pub async fn begin_bulk_load(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("begin_bulk_load").await?;
        self.begin_bulk_load_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub async fn begin_bulk_load_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
            .execute_batch(
                "DROP INDEX IF EXISTS idx_nodes_kind;
             DROP INDEX IF EXISTS idx_nodes_name;
             DROP INDEX IF EXISTS idx_nodes_qualified_name;
             DROP INDEX IF EXISTS idx_nodes_file_path;
             DROP INDEX IF EXISTS idx_nodes_file_path_start_line;
             DROP INDEX IF EXISTS idx_edges_source;
             DROP INDEX IF EXISTS idx_edges_target;
             DROP INDEX IF EXISTS idx_edges_kind;
             DROP INDEX IF EXISTS idx_edges_source_kind;
             DROP INDEX IF EXISTS idx_edges_target_kind;
             DROP INDEX IF EXISTS idx_edges_unique;
             DROP INDEX IF EXISTS idx_unresolved_refs_from_node_id;
             DROP INDEX IF EXISTS idx_unresolved_refs_reference_name;
             DROP INDEX IF EXISTS idx_unresolved_refs_file_path;
             DROP TRIGGER IF EXISTS nodes_fts_insert;
             DROP TRIGGER IF EXISTS nodes_fts_delete;
             DROP TRIGGER IF EXISTS nodes_fts_update;
             -- nodes_fts is an external-content FTS5 table: a plain DELETE
             -- computes the terms to remove from the CURRENT content rows, so
             -- any index/content divergence survives it and the end-of-load
             -- reinsert then duplicates entries (malformed inverted index).
             -- 'delete-all' wipes the index structures unconditionally.
             INSERT INTO nodes_fts(nodes_fts) VALUES('delete-all');",
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to begin bulk load: {e}"),
                operation: "begin_bulk_load".to_string(),
            })?;
        Ok(())
    }

    /// Recreates secondary indexes (benefiting from sorted row order),
    /// restores FTS triggers and content, and re-enables normal durability.
    pub async fn end_bulk_load(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("end_bulk_load").await?;
        self.end_bulk_load_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub async fn end_bulk_load_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
             CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
             CREATE INDEX IF NOT EXISTS idx_nodes_qualified_name ON nodes(qualified_name);
             CREATE INDEX IF NOT EXISTS idx_nodes_file_path ON nodes(file_path);
             CREATE INDEX IF NOT EXISTS idx_nodes_file_path_start_line ON nodes(file_path, start_line);
             CREATE INDEX IF NOT EXISTS idx_edges_source_kind ON edges(source, kind);
             CREATE INDEX IF NOT EXISTS idx_edges_target_kind ON edges(target, kind);
             CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique ON edges(source, target, kind, COALESCE(line, -1));
             CREATE INDEX IF NOT EXISTS idx_unresolved_refs_from_node_id ON unresolved_refs(from_node_id);
             CREATE INDEX IF NOT EXISTS idx_unresolved_refs_reference_name ON unresolved_refs(reference_name);
             CREATE INDEX IF NOT EXISTS idx_unresolved_refs_file_path ON unresolved_refs(file_path);
             CREATE TRIGGER IF NOT EXISTS nodes_fts_insert AFTER INSERT ON nodes BEGIN
                 INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
                 VALUES (NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
             END;
             CREATE TRIGGER IF NOT EXISTS nodes_fts_delete AFTER DELETE ON nodes BEGIN
                 INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
                 VALUES ('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
             END;
             CREATE TRIGGER IF NOT EXISTS nodes_fts_update AFTER UPDATE ON nodes BEGIN
                 INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
                 VALUES ('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
                 INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
                 VALUES (NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
             END;
             -- Canonical external-content resync: 'rebuild' derives the whole
             -- index from the content table, correct even if the index was
             -- not perfectly empty when the bulk load began.
             INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');",
        ).await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to end bulk load: {e}"),
            operation: "end_bulk_load".to_string(),
        })?;
        Ok(())
    }
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn exact_test_runtime_locator(
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    path: PathBuf,
) -> Result<(StoreRuntimeKey, ExactTestRuntimeLocator)> {
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.test-runtime.locator.v1\0");
    digest.update(path.as_os_str().as_encoded_bytes());
    let verified = VerifiedStoreLocatorV1::new(
        shard_id.clone(),
        incarnation,
        LocatorDigest::new(format!("sha256:{}", hex::encode(digest.finalize()))).map_err(
            |error| test_runtime_error("construct test locator digest", error.to_string()),
        )?,
    );
    Ok((
        StoreRuntimeKey::new(shard_id, incarnation),
        ExactTestRuntimeLocator { verified, path },
    ))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn test_runtime_error(operation: &'static str, message: String) -> TraceDecayError {
    TraceDecayError::Database {
        message,
        operation: operation.to_owned(),
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;

    #[test]
    fn adaptive_new_db_gets_minimum() {
        let (cache_kb, mmap) = adaptive_cache_sizes(0);
        assert_eq!(cache_kb, 2 * MB / KB); // 2 MB in KiB = 2048
        assert_eq!(mmap, 0);
    }

    #[test]
    fn adaptive_small_db() {
        // 5 MB DB → cache = 2 MB (floor), mmap = 10 MB
        let (cache_kb, mmap) = adaptive_cache_sizes(5 * MB);
        assert_eq!(cache_kb, 2 * MB / KB);
        assert_eq!(mmap, 10 * MB);
    }

    #[test]
    fn adaptive_medium_db() {
        // 100 MB DB → cache = 25 MB, mmap = 200 MB
        let (cache_kb, mmap) = adaptive_cache_sizes(100 * MB);
        assert_eq!(cache_kb, 25 * MB / KB);
        assert_eq!(mmap, 200 * MB);
    }

    #[test]
    fn adaptive_large_db() {
        // 500 MB DB → cache = 64 MB (cap), mmap = 256 MB (cap)
        let (cache_kb, mmap) = adaptive_cache_sizes(500 * MB);
        assert_eq!(cache_kb, 64 * MB / KB);
        assert_eq!(mmap, 256 * MB);
    }

    #[test]
    fn adaptive_very_large_db() {
        // 2 GB DB → both capped at max
        let (cache_kb, mmap) = adaptive_cache_sizes(2 * 1024 * MB);
        assert_eq!(cache_kb, 64 * MB / KB);
        assert_eq!(mmap, 256 * MB);
    }

    #[test]
    fn mmap_disabled_for_every_graph_database() {
        let raw = 200 * MB;
        let effective = platform_safe_mmap_size(raw);
        assert_eq!(effective, 0);
        assert_eq!(platform_safe_mmap_size(0), 0);
    }

    #[tokio::test]
    async fn repeated_authorized_opens_share_one_physical_connection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "connection reuse").unwrap();
        let (first, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let (second, _) = Database::open(&path, &authority).await.unwrap();
        let mut readers = Vec::new();
        for _ in 0..12 {
            readers.push(Database::open_read_only(&path, &authority).await.unwrap().0);
        }

        assert!(Arc::ptr_eq(&first.inner, &second.inner));
        assert!(
            readers
                .iter()
                .all(|reader| !Arc::ptr_eq(&first.inner, &reader.inner))
        );
        assert!(readers.iter().all(|reader| !reader.inner.writable));
        assert!(first.inner.writable);
    }

    #[tokio::test]
    async fn repeated_authorized_opens_share_one_writer_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "writer reuse").unwrap();
        let (first, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let (second, _) = Database::open(&path, &authority).await.unwrap();

        let first_writer = first.writer().await;
        assert!(second.inner.writer.try_lock().is_err());
        drop(first_writer);
        assert!(second.inner.writer.try_lock().is_ok());
    }

    #[tokio::test]
    async fn read_only_preflight_and_writable_mount_share_one_registered_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "preflight reuse").unwrap();
        let (writer, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let reader = Database::publish_runtime(
            writer.retained_runtime().clone(),
            DatabaseAccessMode::ReadOnly,
        )
        .await
        .unwrap();
        let remounted_writer = Database::publish_runtime(
            reader.retained_runtime().clone(),
            DatabaseAccessMode::ReadWrite,
        )
        .await
        .unwrap();

        assert!(Arc::ptr_eq(
            writer.retained_runtime().runtime(),
            reader.retained_runtime().runtime()
        ));
        assert_eq!(writer.opened_file_identity(), reader.opened_file_identity());
        assert!(Arc::ptr_eq(&writer.inner, &remounted_writer.inner));
    }

    #[tokio::test]
    async fn retained_daemon_database_refuses_writes_after_scope_drops() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("projects/project/tracedecay.db");
        let scope = crate::db::enter_daemon_database_scope(temp.path(), 9, "writer-scope").unwrap();
        let authority = DatabaseAuthority::acquire_daemon(&path, "writer-scope").unwrap();
        let (database, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let retained = database.clone();
        drop(scope);

        match retained
            .begin_write_transaction("write after scope drop")
            .await
        {
            Ok(_) => panic!("retained database began a write after its scope dropped"),
            Err(error) => assert!(error.to_string().contains("active daemon")),
        }
    }

    #[tokio::test]
    async fn twelve_handles_serialize_isolated_writer_connections() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "twelve writers").unwrap();
        let (first, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        first
            .writer_connection("create writer counter")
            .await
            .unwrap()
            .execute_batch(
                "CREATE TABLE writer_counter (value INTEGER NOT NULL);
                 INSERT INTO writer_counter(value) VALUES (0);",
            )
            .await
            .unwrap();

        let mut tasks = Vec::new();
        for _ in 0..12 {
            let handle = Database::open(&path, &authority).await.unwrap().0;
            tasks.push(tokio::spawn(async move {
                handle
                    .writer_connection("increment writer counter")
                    .await
                    .unwrap()
                    .execute("UPDATE writer_counter SET value = value + 1", ())
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let mut rows = first
            .conn()
            .query("SELECT value FROM writer_counter", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            12
        );
    }

    #[tokio::test]
    async fn retained_reader_never_observes_uncommitted_writer_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "paused writer read").unwrap();
        let (db, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        db.writer_connection("seed paused writer")
            .await
            .unwrap()
            .execute_batch(
                "CREATE TABLE paused_writer (value INTEGER NOT NULL);
                 INSERT INTO paused_writer(value) VALUES (0);",
            )
            .await
            .unwrap();

        let transaction = db.begin_write_transaction("pause writer").await.unwrap();
        transaction
            .execute("UPDATE paused_writer SET value = 1", ())
            .await
            .unwrap();

        let mut before = db
            .conn()
            .query("SELECT value FROM paused_writer", ())
            .await
            .unwrap();
        assert_eq!(
            before.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        drop(before);
        transaction.commit().await.unwrap();

        let mut after = db
            .conn()
            .query("SELECT value FROM paused_writer", ())
            .await
            .unwrap();
        assert_eq!(
            after.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn opaque_memory_writer_serializes_and_mutates_without_raw_connection_access() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "memory writer capability").unwrap();
        let (db, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let first = db.memory_writer().await.unwrap();
        let mut second = Box::pin(db.memory_writer());

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut second)
                .await
                .is_err()
        );
        drop(first);
        let second = second.await.unwrap();
        second
            .store()
            .add_fact(
                crate::memory::types::AddFactRequest {
                    content: "opaque writer fixture".to_string(),
                    category: crate::memory::types::MemoryCategory::General,
                    source: Some("test".to_string()),
                    tags: Vec::new(),
                    entities: Vec::new(),
                    trust: None,
                    metadata: serde_json::json!({}),
                },
                crate::memory::trust::DEFAULT_TRUST,
            )
            .await
            .unwrap();
        drop(second);

        let mut rows = db
            .conn()
            .query("SELECT COUNT(*) FROM memory_facts", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_write_transaction_rolls_back_before_releasing_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "cancelled writer").unwrap();
        let (db, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        db.writer_connection("seed cancelled writer")
            .await
            .unwrap()
            .execute_batch(
                "CREATE TABLE cancelled_writer (value INTEGER NOT NULL);
                 INSERT INTO cancelled_writer(value) VALUES (0);",
            )
            .await
            .unwrap();

        let task_db = db.clone();
        let (updated_tx, updated_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let transaction = task_db
                .begin_write_transaction("cancelled update")
                .await
                .unwrap();
            transaction
                .execute("UPDATE cancelled_writer SET value = 1", ())
                .await
                .unwrap();
            let _ = updated_tx.send(());
            std::future::pending::<()>().await;
            drop(transaction);
        });
        updated_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        let transaction = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            db.begin_write_transaction("writer after cancellation"),
        )
        .await
        .expect("writer lane remained locked after cancellation")
        .unwrap();
        let mut rows = transaction
            .query("SELECT value FROM cancelled_writer", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        drop(rows);
        transaction.commit().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writable_symlink_aliases_share_one_database_slot() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "writer alias reuse").unwrap();
        let (direct, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let alias = temp.path().join("graph-alias.db");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let (through_alias, _) = Database::open(&alias, &authority).await.unwrap();

        assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn writable_case_aliases_share_one_database_slot() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("SlotCase");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "writer case reuse").unwrap();
        let (direct, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let alias = directory.join("graph.db");
        let (through_alias, _) = Database::open(&alias, &authority).await.unwrap();

        assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
    }

    #[tokio::test]
    async fn checkpoint_waits_for_shared_writer_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "checkpoint writer lane").unwrap();
        let (first, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let (second, _) = Database::open(&path, &authority).await.unwrap();
        let writer = first.writer().await;
        let mut checkpoint = tokio::spawn(async move { second.checkpoint().await });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut checkpoint)
                .await
                .is_err()
        );
        drop(writer);
        checkpoint.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn retained_database_guard_keeps_authority_alive_for_query_connection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "dashboard guard").unwrap();
        let (db, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let raw = db.conn().clone();
        let guard = Arc::new(db.clone());
        drop(db);
        drop(authority);

        assert!(matches!(
            crate::db::probe_writer_owner(&path).unwrap(),
            crate::db::WriterOwnership::Active(_)
        ));
        raw.query("SELECT 1", ()).await.unwrap();

        drop(guard);
        assert_eq!(
            crate::db::probe_writer_owner(&path).unwrap(),
            crate::db::WriterOwnership::Idle
        );
        drop(raw);
    }

    #[tokio::test]
    async fn read_only_first_open_does_not_block_writable_owner() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "readonly upgrade").unwrap();
        let (seed, _) = Database::publish_fixture_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let runtime = seed.retained_runtime().clone();
        drop(seed);

        let reader = Database::publish_runtime(runtime.clone(), DatabaseAccessMode::ReadOnly)
            .await
            .unwrap();
        let writer = Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite)
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&reader.inner, &writer.inner));
        writer
            .writer_connection("reader isolation test")
            .await
            .unwrap()
            .execute("CREATE TABLE reader_did_not_poison_writer (id INTEGER)", ())
            .await
            .unwrap();
        assert!(
            writer
                .conn()
                .execute("CREATE TABLE forbidden_retained_write (id INTEGER)", ())
                .await
                .is_err()
        );
        assert!(
            reader
                .conn()
                .execute("CREATE TABLE forbidden_reader_write (id INTEGER)", ())
                .await
                .is_err()
        );
    }
}
