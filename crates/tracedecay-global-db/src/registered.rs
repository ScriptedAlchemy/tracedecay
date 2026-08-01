use std::future::Future;
use std::path::Path;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
};

use tracedecay_runtime_core::{
    db::{
        DatabaseAuthority,
        engine::{
            Connection, Executor, IntoParams, QueryExecutor, ReadConnection, ReadSnapshot, Rows,
            Transaction, TransactionBehavior, WalCheckpointExecutor,
        },
    },
    errors::TraceDecayError,
    store_runtime::registry::StoreRuntimeHandle,
};

pub struct RegisteredGlobalDb {
    read_connection: ReadConnection,
    write_connection: Connection,
    runtime: StoreRuntimeHandle,
    authority: DatabaseAuthority,
}

pub struct RegisteredWorkApplicationServicesV1 {
    commands:
        tracedecay_application::WorkService<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>,
    projections: tracedecay_application::WorkProjectionReadService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
}

impl RegisteredWorkApplicationServicesV1 {
    pub fn commands(
        &self,
    ) -> &tracedecay_application::WorkService<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>
    {
        &self.commands
    }

    pub fn projections(
        &self,
    ) -> &tracedecay_application::WorkProjectionReadService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.projections
    }
}

/// PR17 workflow definition/activation and task-handoff-token services over
/// the same registered Work migration-SQL channel as [`RegisteredWorkApplicationServicesV1`].
/// This is not a second Work authority: [`WorkflowSqliteAuthority`] installs its
/// tables through the exact handle `WorkSqliteStorage` owns.
///
/// [`WorkflowSqliteAuthority`]: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority
pub struct RegisteredWorkflowApplicationServicesV1 {
    definitions: tracedecay_application::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    >,
    handoffs: tracedecay_application::TaskHandoffService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    >,
}

impl RegisteredWorkflowApplicationServicesV1 {
    pub fn definitions(
        &self,
    ) -> &tracedecay_application::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        &self.definitions
    }

    pub fn handoffs(
        &self,
    ) -> &tracedecay_application::TaskHandoffService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        &self.handoffs
    }
}

impl RegisteredGlobalDb {
    /// Migrates an already-published runtime before validating and exposing
    /// the registered global database facade. No path is reopened.
    pub async fn migrate_and_attach(
        runtime: StoreRuntimeHandle,
        expected_binding: tracedecay_store::StoreRuntimeBindingV1,
        expected_locator: tracedecay_store::VerifiedStoreLocatorV1,
        authority: DatabaseAuthority,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let write_connection =
            registered_connection(&runtime, &expected_binding, &expected_locator, &authority)?;
        if !runtime.schema_migrated() {
            super::ensure_registered_schema(&write_connection).await?;
        }
        Self::finish_attach(runtime, write_connection, authority).await
    }

    /// Installs only admission-critical schema before publishing a daemon
    /// runtime. The returned plan owns resumable historical convergence.
    pub async fn migrate_and_attach_for_daemon(
        runtime: StoreRuntimeHandle,
        expected_binding: tracedecay_store::StoreRuntimeBindingV1,
        expected_locator: tracedecay_store::VerifiedStoreLocatorV1,
        authority: DatabaseAuthority,
    ) -> tracedecay_runtime_core::errors::Result<(
        Self,
        Option<super::schema_stages::RegisteredSchemaConvergence>,
    )> {
        let write_connection =
            registered_connection(&runtime, &expected_binding, &expected_locator, &authority)?;
        let convergence = if runtime.schema_migrated() {
            None
        } else {
            Some(
                super::schema_stages::ensure_registered_schema_for_admission(&write_connection)
                    .await?,
            )
        };
        let database = Self::finish_attach(runtime, write_connection, authority).await?;
        Ok((database, convergence))
    }

    pub async fn converge_schema(
        &self,
        convergence: super::schema_stages::RegisteredSchemaConvergence,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        super::schema_stages::converge_registered_schema(&self.write_connection, convergence).await
    }

    pub async fn release_connection_memory(&self) -> tracedecay_runtime_core::errors::Result<()> {
        self.write_connection
            .execute_batch("PRAGMA shrink_memory")
            .await
            .map_err(|error| registered_error("release registered database memory", error))
    }

    async fn finish_attach(
        runtime: StoreRuntimeHandle,
        write_connection: Connection,
        authority: DatabaseAuthority,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let read_connection = write_connection.read_only();
        let database = Self {
            read_connection,
            write_connection,
            runtime,
            authority,
        };
        database.validate_authority_schema_contract().await?;
        Ok(database)
    }

    pub fn read_connection(&self) -> &ReadConnection {
        &self.read_connection
    }

    pub async fn read_snapshot(&self) -> tracedecay_runtime_core::db::engine::Result<ReadSnapshot> {
        self.read_connection.read_snapshot().await
    }

    pub async fn snapshot_to(
        &self,
        destination: &Path,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        if destination == self.authority.canonical_database_path() {
            return Err(registered_error(
                "snapshot registered global database",
                "snapshot destination must not be the canonical database",
            ));
        }
        if destination.exists() {
            return Err(registered_error(
                "snapshot registered global database",
                format!(
                    "snapshot destination already exists: {}",
                    destination.display()
                ),
            ));
        }
        let parent = destination.parent().ok_or_else(|| {
            registered_error(
                "snapshot registered global database",
                "snapshot destination has no parent directory",
            )
        })?;
        if self.authority.canonical_database_path().parent() == Some(parent) {
            return Err(registered_error(
                "snapshot registered global database",
                "snapshot destination must be outside the canonical database directory",
            ));
        }
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(parent).map_err(
            |error| {
                registered_error(
                    "prepare private registered database snapshot directory",
                    error,
                )
            },
        )?;
        self.runtime
            .snapshot_to(destination.to_path_buf(), self.authority.clone())
            .await
            .map(|_| ())
            .map_err(|error| {
                registered_error("snapshot registered global database", format!("{error:?}"))
            })
    }

    async fn validate_authority_schema_contract(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let snapshot = self.read_snapshot().await.map_err(|error| {
            registered_error("begin registered authority schema validation", error)
        })?;
        super::schema_contract::validate_authority_schema_contract(&snapshot).await
    }

    #[doc(hidden)]
    pub async fn validate_registry_schema_contract_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| registered_error("open registered profile schema snapshot", error))?;
        super::schema_contract::validate_registry_schema_contract(&snapshot).await
    }

    pub fn writer_connection(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbWriterConnection<'_>> {
        self.authority
            .require_active_write_scope("open registered global database writer")?;
        Ok(RegisteredGlobalDbWriterConnection {
            connection: &self.write_connection,
            authority: &self.authority,
        })
    }

    pub async fn advance_projection_version_migration_until_cancelled(
        &self,
        cancelled: &AtomicBool,
    ) -> tracedecay_runtime_core::errors::Result<bool> {
        self.authority
            .require_active_write_scope("advance observation projection migration")?;
        super::observation_projection::advance_projection_version_migration_until_cancelled_with_engine(
                &self.write_connection,
                cancelled,
            )
            .await
            .map_err(|error| registered_error("advance observation projection migration", error))
    }

    pub async fn begin_write_transaction(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbWriteTransaction<'_>> {
        self.authority
            .require_active_write_scope("begin registered global database transaction")?;
        let transaction = self
            .write_connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|error| {
                registered_error("begin registered global database transaction", error)
            })?;
        Ok(RegisteredGlobalDbWriteTransaction {
            transaction,
            authority: &self.authority,
        })
    }

    pub fn binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.runtime.binding()
    }

    /// The store runtime this registered database is mounted on.
    ///
    /// Exposed so the composition root can build the adapters it owns —
    /// `RuntimeEvidenceAssemblyStore`, `RuntimeExternalSourceStore`,
    /// `GlobalDbObservationStore`, `DaemonWorkRuntimeV1` — without this crate
    /// naming a root type. See the "root-owned adapters" seam note in
    /// `SEAMS.md`.
    pub fn runtime(&self) -> &StoreRuntimeHandle {
        &self.runtime
    }

    /// The write authority guarding every mutation against this database.
    ///
    /// Companion to [`RegisteredGlobalDb::runtime`]; the root adapters take
    /// the `(runtime, authority)` pair by clone.
    pub fn authority(&self) -> &DatabaseAuthority {
        &self.authority
    }

    pub fn work_storage(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>
    {
        let handle = self
            .runtime
            .authorized_migration_sql_handle(self.authority.clone())
            .map_err(|error| {
                registered_error("attach registered Work storage", format!("{error:?}"))
            })?;
        validate_registered_identity(
            handle.binding(),
            handle.verified_locator(),
            self.runtime.binding(),
            self.runtime.locator().verified(),
        )?;
        Ok(tracedecay_rusqlite_runtime::work::WorkSqliteStorage::from_registered(handle))
    }

    pub fn authorized_scope_set_storage(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage,
    > {
        let handle = self
            .runtime
            .authorized_migration_sql_handle(self.authority.clone())
            .map_err(|error| {
                registered_error(
                    "attach registered authorized scope-set storage",
                    format!("{error:?}"),
                )
            })?;
        validate_registered_identity(
            handle.binding(),
            handle.verified_locator(),
            self.runtime.binding(),
            self.runtime.locator().verified(),
        )?;
        Ok(
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage::from_registered(
                handle,
            ),
        )
    }

    pub fn work_application_services(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredWorkApplicationServicesV1> {
        let storage = self.work_storage()?;
        Ok(RegisteredWorkApplicationServicesV1 {
            commands: tracedecay_application::WorkService::new(storage.clone()),
            projections: tracedecay_application::WorkProjectionReadService::new(storage),
        })
    }

    /// Attaches the PR17 workflow-definition/task-handoff authority over the
    /// registered Work migration-SQL handle. Installs `workflow_*` tables
    /// idempotently through the same handle `work_storage` validates.
    pub fn workflow_storage(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        let storage = self.work_storage()?;
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority::from_work_storage(&storage)
            .map_err(|error| {
                registered_error("attach registered workflow storage", format!("{error:?}"))
            })
    }

    pub fn workflow_application_services(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredWorkflowApplicationServicesV1> {
        let authority = self.workflow_storage()?;
        Ok(RegisteredWorkflowApplicationServicesV1 {
            definitions: tracedecay_application::WorkflowDefinitionService::new(authority.clone()),
            handoffs: tracedecay_application::TaskHandoffService::new(authority),
        })
    }

    // Root-owned adapter, deliberately not built here: `work_runtime` returned
    // `crate::daemon::work_runtime::DaemonWorkRuntimeV1<WorkSqliteStorage>`,
    // which lives above this crate. The composition root builds it from
    // `work_storage()` plus `Arc::clone(&registered)`; see `SEAMS.md`.

    pub fn storage_telemetry_handle(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle,
    > {
        self.runtime.telemetry_read_handle().map_err(|error| {
            registered_error(
                "attach registered storage telemetry reader",
                format!("{error:?}"),
            )
        })
    }

    // Root-owned adapter, deliberately not built here: `external_source_store`
    // returned `crate::application::external_source_store::
    // RuntimeExternalSourceStore`. The composition root builds it from
    // `runtime().clone()` and `authority().clone()`; see `SEAMS.md`.

    pub fn storage_page_counts(&self) -> tracedecay_runtime_core::errors::Result<(u64, u64, u64)> {
        self.runtime
            .storage_page_counts(std::time::Duration::from_secs(5))
            .map_err(|error| {
                registered_error(
                    "read registered global database page counts",
                    format!("{error:?}"),
                )
            })
    }

    pub async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.runtime
            .run_bounded_incremental_compaction(max_pages, self.authority.clone())
            .await
            .map_err(|error| {
                registered_error(
                    "run registered global database incremental compaction",
                    format!("{error:?}"),
                )
            })
    }

    pub async fn run_session_lcm_retention(
        &self,
        provider: &str,
        session_id: Option<&str>,
        config: &tracedecay_sessions::runtime::lcm::retention::LcmRetentionConfig,
        mode: tracedecay_sessions::runtime::lcm::retention::RetentionMode,
        now: i64,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_sessions::runtime::lcm::retention::LcmRetentionReport,
    > {
        let storage_root = self.db_path().parent().ok_or_else(|| {
            registered_error(
                "run registered session retention",
                "registered sessions database has no storage root",
            )
        })?;
        let authority = self.authority.clone();
        tracedecay_sessions::runtime::lcm::retention::run_session_retention_authorized(
            &self.write_connection,
            storage_root,
            provider,
            session_id,
            config,
            mode,
            now,
            &move |intent| {
                authority
                    .require_active_write_scope(intent)
                    .map_err(|error| {
                        tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string())
                    })
            },
        )
        .await
        .map_err(|error| registered_error("run registered session retention", error))
    }

    pub async fn run_observation_retention(
        &self,
        generation: Option<&str>,
        config: &super::observation::retention::ObservationRetentionConfig,
        mode: super::observation::retention::RetentionMode,
        now: i64,
    ) -> tracedecay_runtime_core::errors::Result<
        super::observation::retention::ObservationRetentionReport,
    > {
        let authority = self.authority.clone();
        super::observation::retention::run_observation_retention_authorized(
            &self.write_connection,
            generation,
            config,
            mode,
            now,
            &move |intent| authority.require_active_write_scope(intent),
        )
        .await
    }

    // Root-owned adapter, deliberately not built here: `observation_store`
    // returned `crate::store::observation::GlobalDbObservationStore<'_>`, the
    // root implementation of `tracedecay_store::ObservationStore`. The
    // composition root builds it from `runtime()` and `authority()`; see
    // `SEAMS.md`.

    pub fn db_path(&self) -> &Path {
        self.authority.canonical_database_path()
    }

    pub fn git_index_transaction_store(
        &self,
    ) -> super::git_index_transactions::GlobalDbGitIndexTransactionStore<'_> {
        super::git_index_transactions::GlobalDbGitIndexTransactionStore::new(self)
    }
}

// `impl MigrationSqlWriteAuthority for DatabaseAuthority` moved into
// `tracedecay_runtime_core::db::access`: both the trait and the type are
// now foreign to this crate, so the orphan rule forbids it here.

pub struct RegisteredGlobalDbWriterConnection<'a> {
    connection: &'a Connection,
    authority: &'a DatabaseAuthority,
}

impl RegisteredGlobalDbWriterConnection<'_> {
    pub async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        self.require_active("execute registered global database statement")?;
        self.connection.execute(sql, params).await
    }

    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        self.require_active("query registered global database writer")?;
        self.connection.query(sql, params).await
    }

    pub async fn execute_batch(
        &self,
        sql: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.require_active("execute registered global database batch")?;
        self.connection.execute_batch(sql).await
    }

    fn require_active(&self, intent: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.authority
            .require_active_write_scope(intent)
            .map_err(|error| {
                tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
            })
    }
}

impl WalCheckpointExecutor for RegisteredGlobalDbWriterConnection<'_> {
    async fn checkpoint_wal_truncate(&self) -> tracedecay_runtime_core::db::engine::Result<Rows> {
        self.require_active("checkpoint registered global database WAL")?;
        self.connection.checkpoint_wal_truncate().await
    }
}

pub struct RegisteredGlobalDbWriteTransaction<'a> {
    transaction: Transaction,
    authority: &'a DatabaseAuthority,
}

impl QueryExecutor for RegisteredGlobalDbWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        RegisteredGlobalDbWriteTransaction::query(self, sql, params).await
    }
}

impl Executor for RegisteredGlobalDbWriteTransaction<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        RegisteredGlobalDbWriteTransaction::execute(self, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        RegisteredGlobalDbWriteTransaction::execute_batch(self, sql).await
    }
}

impl tracedecay_sessions::runtime::store_port::SessionWriteTxn
    for RegisteredGlobalDbWriteTransaction<'_>
{
    #[allow(clippy::manual_async_fn)]
    fn commit(self) -> impl Future<Output = tracedecay_runtime_core::errors::Result<()>> + Send {
        async move {
            RegisteredGlobalDbWriteTransaction::commit(self)
                .await
                .map_err(|error| registered_error("commit registered session transaction", error))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn rollback(self) -> impl Future<Output = tracedecay_runtime_core::errors::Result<()>> + Send {
        async move {
            RegisteredGlobalDbWriteTransaction::rollback(self)
                .await
                .map_err(|error| {
                    registered_error("roll back registered session transaction", error)
                })
        }
    }
}

impl tracedecay_sessions::runtime::store_port::SessionStoreAuthority for RegisteredGlobalDb {
    type WriteTxn<'txn>
        = RegisteredGlobalDbWriteTransaction<'txn>
    where
        Self: 'txn;

    fn shard_id(&self) -> &tracedecay_store::StoreShardIdV1 {
        &self.binding().shard_id
    }

    fn db_path(&self) -> &Path {
        RegisteredGlobalDb::db_path(self)
    }

    #[allow(clippy::manual_async_fn)]
    fn read_snapshot(
        &self,
    ) -> impl Future<Output = tracedecay_runtime_core::errors::Result<ReadSnapshot>> + Send {
        async move {
            RegisteredGlobalDb::read_snapshot(self)
                .await
                .map_err(|error| registered_error("open registered session read snapshot", error))
        }
    }

    fn begin_write_transaction(
        &self,
    ) -> impl Future<Output = tracedecay_runtime_core::errors::Result<Self::WriteTxn<'_>>> + Send
    {
        RegisteredGlobalDb::begin_write_transaction(self)
    }
}

impl tracedecay_sessions::runtime::git_correlation::GitCorrelationWriteTxn
    for RegisteredGlobalDbWriteTransaction<'_>
{
    #[allow(clippy::manual_async_fn)]
    fn commit(
        self,
    ) -> impl Future<
        Output = Result<(), tracedecay_sessions::runtime::git_correlation::GitCorrelationError>,
    > + Send {
        async move {
            RegisteredGlobalDbWriteTransaction::commit(self)
                .await
                .map_err(|error| {
                    tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Db(
                        error.to_string(),
                    )
                })
        }
    }
}

impl tracedecay_sessions::runtime::workflow_index::WorkflowIngestWriteTxn
    for RegisteredGlobalDbWriteTransaction<'_>
{
    #[allow(clippy::manual_async_fn)]
    fn commit(
        self,
    ) -> impl Future<
        Output = Result<(), tracedecay_sessions::runtime::workflow_index::WorkflowIndexError>,
    > + Send {
        async move {
            RegisteredGlobalDbWriteTransaction::commit(self)
                .await
                .map_err(|error| {
                    tracedecay_sessions::runtime::workflow_index::WorkflowIndexError::Db(
                        error.to_string(),
                    )
                })
        }
    }
}

impl tracedecay_runtime_core::db::engine::DatabaseAttachmentExecutor
    for RegisteredGlobalDbWriteTransaction<'_>
{
    async fn attach_database(
        &self,
        path: &Path,
        database_name: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.require_active("attach registered consolidation input")?;
        self.transaction.attach_database(path, database_name).await
    }
}

impl RegisteredGlobalDbWriteTransaction<'_> {
    pub async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        self.require_active("execute registered global database transaction")?;
        self.transaction.execute(sql, params).await
    }

    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        self.require_active("query registered global database transaction")?;
        self.transaction.query(sql, params).await
    }

    pub async fn execute_batch(
        &self,
        sql: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.require_active("execute registered global database transaction batch")?;
        self.transaction.execute_batch(sql).await
    }

    pub async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        if let Err(error) = self
            .authority
            .require_active_write_scope("commit registered global database transaction")
        {
            let rollback = self.transaction.rollback().await;
            return match rollback {
                Ok(()) => Err(
                    tracedecay_runtime_core::db::engine::Error::invalid_operation(
                        error.to_string(),
                    ),
                ),
                Err(rollback_error) => Err(
                    tracedecay_runtime_core::db::engine::Error::invalid_operation(format!(
                        "{error}; rollback after authority loss failed: {rollback_error}"
                    )),
                ),
            };
        }
        self.transaction.commit().await
    }

    pub async fn rollback(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.transaction.rollback().await
    }

    fn require_active(&self, intent: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.authority
            .require_active_write_scope(intent)
            .map_err(|error| {
                tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
            })
    }
}

fn registered_connection(
    runtime: &StoreRuntimeHandle,
    expected_binding: &tracedecay_store::StoreRuntimeBindingV1,
    expected_locator: &tracedecay_store::VerifiedStoreLocatorV1,
    authority: &DatabaseAuthority,
) -> tracedecay_runtime_core::errors::Result<Connection> {
    validate_registered_locator(runtime, expected_binding, expected_locator, authority)?;
    let handle = runtime
        .authorized_migration_sql_handle(authority.clone())
        .map_err(|error| {
            registered_error(
                "attach registered global database runtime",
                format!("{error:?}"),
            )
        })?;
    validate_registered_identity(
        handle.binding(),
        handle.verified_locator(),
        expected_binding,
        expected_locator,
    )?;
    Ok(Connection::attach(handle))
}

fn validate_registered_locator(
    runtime: &StoreRuntimeHandle,
    expected_binding: &tracedecay_store::StoreRuntimeBindingV1,
    expected_locator: &tracedecay_store::VerifiedStoreLocatorV1,
    authority: &DatabaseAuthority,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_registered_identity(
        runtime.binding(),
        runtime.locator().verified(),
        expected_binding,
        expected_locator,
    )?;
    validate_registered_path(runtime.locator().path(), authority)?;
    let current_file_identity = tracedecay_runtime_core::db::sqlite_generation_identity(
        authority.canonical_database_path(),
    )
    .map_err(|error| {
        registered_error(
            "verify registered global database file identity",
            sqlite_identity_error_message(error),
        )
    })?;
    validate_opened_file_identity(runtime.opened_file_identity(), current_file_identity)
}

fn validate_opened_file_identity(
    opened_file_identity: Option<u64>,
    current_file_identity: u64,
) -> tracedecay_runtime_core::errors::Result<()> {
    let opened_file_identity = opened_file_identity.ok_or_else(|| {
        registered_error(
            "bind registered global database runtime",
            "registry attachment has no opened SQLite file identity",
        )
    })?;
    if current_file_identity != opened_file_identity {
        return Err(registered_error(
            "bind registered global database runtime",
            "database file identity changed after registry attachment",
        ));
    }
    Ok(())
}

fn validate_registered_identity(
    actual_binding: &tracedecay_store::StoreRuntimeBindingV1,
    actual_locator: &tracedecay_store::VerifiedStoreLocatorV1,
    expected_binding: &tracedecay_store::StoreRuntimeBindingV1,
    expected_locator: &tracedecay_store::VerifiedStoreLocatorV1,
) -> tracedecay_runtime_core::errors::Result<()> {
    if actual_binding != expected_binding || actual_locator != expected_locator {
        return Err(registered_error(
            "bind registered global database runtime",
            "registry binding or verified locator does not match expected typed authority",
        ));
    }
    Ok(())
}

fn validate_registered_path(
    runtime_path: &std::path::Path,
    authority: &DatabaseAuthority,
) -> tracedecay_runtime_core::errors::Result<()> {
    if runtime_path != authority.canonical_database_path() {
        return Err(registered_error(
            "bind registered global database runtime",
            format!(
                "registry locator {} does not match database authority {}",
                runtime_path.display(),
                authority.canonical_database_path().display()
            ),
        ));
    }
    Ok(())
}

fn registered_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

fn sqlite_identity_error_message(
    error: tracedecay_runtime_core::db::SqliteFileIdentityError,
) -> &'static str {
    match error {
        tracedecay_runtime_core::db::SqliteFileIdentityError::Open => {
            "could not open SQLite file identity"
        }
        tracedecay_runtime_core::db::SqliteFileIdentityError::Inspect => {
            "could not inspect SQLite file identity"
        }
        tracedecay_runtime_core::db::SqliteFileIdentityError::Identify => {
            "could not identify SQLite file"
        }
        tracedecay_runtime_core::db::SqliteFileIdentityError::Unavailable => {
            "SQLite file identity is unavailable"
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    use tempfile::TempDir;
    use tracedecay_domain::LocatorDigest;
    use tracedecay_store::{StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

    use super::*;

    #[derive(Default)]
    struct AuthorityGate {
        state: Mutex<AuthorityGateState>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct AuthorityGateState {
        armed: bool,
        arrived: bool,
        released: bool,
    }

    impl AuthorityGate {
        fn arm(&self) {
            self.state.lock().unwrap().armed = true;
        }

        fn wait_until_arrived(&self) {
            let state = self.state.lock().unwrap();
            let (state, timeout) = self
                .changed
                .wait_timeout_while(state, Duration::from_secs(5), |state| !state.arrived)
                .unwrap();
            assert!(
                state.arrived,
                "writer actor never reached authority gate (timed_out={})",
                timeout.timed_out()
            );
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            self.changed.notify_all();
        }
    }

    struct GatedAuthority {
        authority: DatabaseAuthority,
        gate: Arc<AuthorityGate>,
    }

    impl MigrationSqlWriteAuthority for GatedAuthority {
        fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            {
                let mut state = self.gate.state.lock().unwrap();
                if state.armed {
                    state.arrived = true;
                    self.gate.changed.notify_all();
                    let (state_after_wait, timeout) = self
                        .gate
                        .changed
                        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.released)
                        .unwrap();
                    if timeout.timed_out() && !state_after_wait.released {
                        return Err(MigrationSqlError::AuthorityDenied(
                            "test authority gate timed out".to_owned(),
                        ));
                    }
                }
            }
            self.authority.verify(intent)
        }
    }

    #[test]
    fn registered_locator_mismatch_is_denied() {
        let directory = TempDir::new().unwrap();
        let authority_path = directory.path().join("authority.db");
        let other_path = directory.path().join("other.db");
        fs::write(&authority_path, []).unwrap();
        fs::write(&other_path, []).unwrap();
        let authority =
            DatabaseAuthority::acquire_test(&authority_path, "test registered locator").unwrap();

        let result = validate_registered_path(&other_path.canonicalize().unwrap(), &authority);

        assert!(result.is_err());
    }

    #[test]
    fn registered_locator_validation_never_creates_missing_path() {
        let directory = TempDir::new().unwrap();
        let authority_path = directory.path().join("authority.db");
        let missing_path = directory.path().join("must-not-be-created.db");
        fs::write(&authority_path, []).unwrap();
        let authority =
            DatabaseAuthority::acquire_test(&authority_path, "test registered no-create").unwrap();

        let result = validate_registered_path(&missing_path, &authority);

        assert!(result.is_err());
        assert!(!missing_path.exists());
    }

    #[test]
    fn same_path_cannot_substitute_a_different_typed_binding() {
        fn binding(project: &str) -> StoreRuntimeBindingV1 {
            serde_json::from_value(serde_json::json!({
                "shard_id": {
                    "brain_id": "brain.registered-test",
                    "profile_id": "profile.registered-test",
                    "scope": { "kind": "project", "project_id": project }
                },
                "incarnation": 1,
                "authority_epoch": 7
            }))
            .unwrap()
        }

        let actual = binding("project.actual");
        let expected = binding("project.expected");
        let actual_locator = VerifiedStoreLocatorV1::new(
            actual.shard_id.clone(),
            StoreIncarnationV1::new(1).unwrap(),
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        let expected_locator = VerifiedStoreLocatorV1::new(
            expected.shard_id.clone(),
            StoreIncarnationV1::new(1).unwrap(),
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );

        assert!(
            validate_registered_identity(&actual, &actual_locator, &expected, &expected_locator,)
                .is_err()
        );
    }

    #[test]
    fn missing_or_replaced_opened_file_identity_is_denied() {
        assert!(validate_opened_file_identity(None, 7).is_err());
        assert!(validate_opened_file_identity(Some(6), 7).is_err());
        assert!(validate_opened_file_identity(Some(7), 7).is_ok());
    }

    #[tokio::test]
    async fn issued_registered_writer_rejects_scope_loss_before_sql_dispatch() {
        let profile = TempDir::new().unwrap();
        let database_path = profile.path().join("projects/project/sessions.db");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile.path(),
            1,
            "registered-issued-writer",
        )
        .unwrap();
        let connection = tracedecay_runtime_core::db::engine::TestConnection::open(&database_path);
        let authority =
            DatabaseAuthority::for_runtime(&database_path, "registered issued writer").unwrap();
        let writer = RegisteredGlobalDbWriterConnection {
            connection: &connection,
            authority: &authority,
        };
        drop(scope);

        let error = writer
            .execute(
                "CREATE TABLE stale_registered_writer_must_not_persist (value INTEGER)",
                (),
            )
            .await
            .expect_err("issued registered writer must not outlive daemon scope");

        assert!(error.to_string().contains("active daemon"));
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                tracedecay_runtime_core::db::engine::params![
                    "stale_registered_writer_must_not_persist"
                ],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_registered_write_rechecks_authority_inside_writer_actor() {
        let profile = TempDir::new().unwrap();
        let database_path = profile.path().join("projects/project/queued.db");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile.path(),
            1,
            "queued-registered-writer",
        )
        .unwrap();
        let authority =
            DatabaseAuthority::for_runtime(&database_path, "queued registered writer").unwrap();
        let gate = Arc::new(AuthorityGate::default());
        let connection =
            tracedecay_runtime_core::db::engine::TestConnection::open_with_write_authority(
                &database_path,
                Arc::new(GatedAuthority {
                    authority: authority.clone(),
                    gate: Arc::clone(&gate),
                }),
            );
        connection
            .execute_batch("CREATE TABLE queued_write (value INTEGER NOT NULL)")
            .await
            .unwrap();
        let holder = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let writer = RegisteredGlobalDbWriterConnection {
            connection: &connection,
            authority: &authority,
        };
        gate.arm();

        let queued_write = writer.execute("INSERT INTO queued_write VALUES (1)", ());
        let release_holder = async {
            holder.rollback().await.unwrap();
            let waiting_gate = Arc::clone(&gate);
            tokio::task::spawn_blocking(move || waiting_gate.wait_until_arrived())
                .await
                .unwrap();
            drop(scope);
            gate.release();
        };
        let (result, ()) = tokio::join!(queued_write, release_holder);

        let error = result.expect_err("queued write must recheck revoked daemon authority");
        assert!(error.to_string().contains("active daemon"));
        let mut rows = connection
            .query("SELECT count(*) FROM queued_write", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }
}
