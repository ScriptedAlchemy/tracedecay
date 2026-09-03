use std::future::Future;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock, Weak};

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::{
    db::{
        Database, DatabaseAuthority, DatabaseEngineReadConnection, DatabaseEngineReadSnapshot,
        DatabaseOwnerErrorV1, DatabaseOwnerRetirementReservationV1, DatabaseOwnerV1,
        DatabaseOwnerWeakLeaseIssuerErrorV1, DatabaseOwnerWeakLeaseIssuerV1,
        DatabaseRuntimeClientV1, DatabaseStorageTelemetryHandle, DatabaseWriteTransaction,
        engine::{Executor, IntoParams, QueryExecutor, Rows},
    },
    store_runtime::{VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1},
};
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1};

mod delivery_settlement;
mod session_relation_graph;
pub use delivery_settlement::{
    DeliveryAttemptClaimV1, DeliverySourceReceiptReadV1, DurableDeliverySettlementReceiptV1,
    MAX_PENDING_RECEIPTED_DELIVERIES_V1, MAX_WORK_ATTEMPT_DELIVERY_FANOUTS_V1,
    PendingDeliverySourceReceiptV1, WorkAttemptDeliveryCensusReadV1,
};

type SessionRelationGraphStateV1 = RwLock<
    Option<(
        tracedecay_session_temporal_store::relations::SessionRelationScope,
        tracedecay_graph_db::GraphDbLeaseV1,
        StoreRuntimeBindingV1,
        VerifiedStoreLocatorV1,
    )>,
>;

/// The sole map owner for one registered global-database publication.
///
/// It can issue independently counted client leases, but cannot be cloned or
/// recovered from one. Daemon maps retain this owner; all request and worker
/// paths retain only [`RegisteredGlobalDbLeaseV1`].
pub struct RegisteredGlobalDbOwnerV1 {
    database: DatabaseOwnerV1,
    project_graph: Arc<OnceLock<VerifiedGraphRuntimeWeakProxyV1>>,
    session_relation_graph: Arc<SessionRelationGraphStateV1>,
}

/// Cloneable, weak issuance route for one registered global-database owner.
///
/// This route retains no database client, raw runtime, or SQL authority. Each
/// command must issue its own [`RegisteredGlobalDbLeaseV1`], which keeps the
/// exact owner lifecycle and Store retirement fence authoritative.
#[derive(Clone)]
pub struct RegisteredGlobalDbWeakLeaseIssuerV1 {
    database: DatabaseOwnerWeakLeaseIssuerV1,
    project_graph: Arc<OnceLock<VerifiedGraphRuntimeWeakProxyV1>>,
    session_relation_graph: Weak<SessionRelationGraphStateV1>,
}

impl RegisteredGlobalDbOwnerV1 {
    /// Validates the final schema installed during physical Store open before
    /// the owner becomes visible to any caller. The temporary issuance is
    /// dropped before the owner is returned, so it never becomes a hidden
    /// retirement blocker.
    ///
    /// Only initialization runs the sealed registered-schema installer, so
    /// the attach boundary re-runs schema admission itself: a legacy,
    /// version-skewed, or drifted store fails the attach with each
    /// authority's exact typed reset identity instead of opening on schema it
    /// cannot honor, and an admissibly-fresh existing store receives the full
    /// install. Nothing steps an incompatible store forward in place; the
    /// only in-place changes admission performs are the additive columns for
    /// shapes released binaries actually shipped.
    ///
    /// Short-lived attaches have no background maintenance task, so the
    /// authority-invariant convergence runs synchronously here: a store whose
    /// tamper-invalidation triggers deleted the trusted audit checkpoint (or
    /// whose guard triggers were altered) fails the attach instead of opening
    /// on unaudited authority rows.
    #[hotpath::measure(future = true, label = "global_db.registered.admit")]
    pub async fn admit_and_attach(
        database: DatabaseOwnerV1,
    ) -> tracedecay_domain::errors::Result<Self> {
        let temporary = database.issue_lease().map_err(registered_owner_error)?;
        let registered = RegisteredGlobalDb::from_database(temporary);
        super::schema_stages::ensure_attached_registered_schema(&registered.database).await?;
        registered.rearm_queued_projection_retries().await?;
        super::schema_stages::converge_attached_registered_schema(&registered.database).await?;
        drop(registered);
        Ok(Self {
            database,
            project_graph: Arc::new(OnceLock::new()),
            session_relation_graph: Arc::new(RwLock::new(None)),
        })
    }

    /// Returns the resumable convergence plan for an already admitted schema
    /// without retaining an unowned client lease.
    #[hotpath::measure(future = true, label = "global_db.registered.admit_daemon")]
    pub async fn admit_and_attach_for_daemon(
        database: DatabaseOwnerV1,
    ) -> tracedecay_domain::errors::Result<(Self, super::schema_stages::RegisteredSchemaConvergence)>
    {
        let temporary = database.issue_lease().map_err(registered_owner_error)?;
        let registered = RegisteredGlobalDb::from_database(temporary);
        let convergence =
            super::schema_stages::ensure_attached_registered_schema(&registered.database).await?;
        registered.rearm_queued_projection_retries().await?;
        drop(registered);
        Ok((
            Self {
                database,
                project_graph: Arc::new(OnceLock::new()),
                session_relation_graph: Arc::new(RwLock::new(None)),
            },
            convergence,
        ))
    }

    /// Issues a read-write client when the underlying map owner is writable.
    /// Each call owns one fresh Store client token; clones of the result share
    /// only that issuance.
    pub fn issue_lease(&self) -> Result<RegisteredGlobalDbLeaseV1, DatabaseOwnerErrorV1> {
        Ok(RegisteredGlobalDbLeaseV1::from_database(
            RegisteredGlobalDb::from_database_with_project_graph(
                self.database.issue_lease()?,
                Arc::clone(&self.project_graph),
                Arc::clone(&self.session_relation_graph),
            ),
        ))
    }

    /// Issues a mode-reduced client that can never regain write authority.
    pub fn issue_read_only_lease(&self) -> Result<RegisteredGlobalDbLeaseV1, DatabaseOwnerErrorV1> {
        Ok(RegisteredGlobalDbLeaseV1::from_database(
            RegisteredGlobalDb::from_database_with_project_graph(
                self.database.issue_read_only_lease()?,
                Arc::clone(&self.project_graph),
                Arc::clone(&self.session_relation_graph),
            ),
        ))
    }

    /// Creates a cloneable route for command-scoped registered leases without
    /// retaining this owner or a counted Store client.
    #[must_use]
    pub fn weak_lease_issuer(&self) -> RegisteredGlobalDbWeakLeaseIssuerV1 {
        RegisteredGlobalDbWeakLeaseIssuerV1 {
            database: self.database.weak_lease_issuer(),
            project_graph: Arc::clone(&self.project_graph),
            session_relation_graph: Arc::downgrade(&self.session_relation_graph),
        }
    }

    /// Releases the graph client shared by every live lease from this owner.
    ///
    /// Session-runtime retirement calls this before closing the native graph
    /// owner. Existing database leases remain valid for SQL access but can no
    /// longer keep the retired graph writer locked.
    pub fn detach_session_relation_graph(&self) {
        *self
            .session_relation_graph
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Starts the exact database-owner reservation used by daemon map
    /// retirement. The daemon alone composes it with the graph owner target.
    pub fn reserve_retirement(
        &self,
    ) -> Result<DatabaseOwnerRetirementReservationV1, DatabaseOwnerErrorV1> {
        self.database.reserve_retirement()
    }

    pub fn registered_binding(&self) -> &StoreRuntimeBindingV1 {
        self.database.registered_binding()
    }

    pub fn registered_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.database.registered_verified_locator()
    }
}

impl RegisteredGlobalDbWeakLeaseIssuerV1 {
    /// Issues one fresh registered-database lease while the exact map owner
    /// remains ready. The returned lease retains schema authority only through
    /// the guarded database facade; no raw authority escapes this route.
    pub fn issue_lease(
        &self,
    ) -> Result<RegisteredGlobalDbLeaseV1, DatabaseOwnerWeakLeaseIssuerErrorV1> {
        let session_relation_graph = self
            .session_relation_graph
            .upgrade()
            .ok_or(DatabaseOwnerWeakLeaseIssuerErrorV1::Unavailable)?;
        Ok(RegisteredGlobalDbLeaseV1::from_database(
            RegisteredGlobalDb::from_database_with_project_graph(
                self.database.issue_lease()?,
                Arc::clone(&self.project_graph),
                session_relation_graph,
            ),
        ))
    }

    /// Exact non-retaining Store identity for target registration and removal.
    #[must_use]
    pub fn registered_binding(&self) -> &StoreRuntimeBindingV1 {
        self.database.registered_binding()
    }

    /// Exact non-retaining locator identity for target validation.
    #[must_use]
    pub fn registered_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.database.registered_verified_locator()
    }
}

/// Cloneable client authority for a registered global database.
///
/// The token keeps exactly one owner-issued guarded [`Database`] client alive
/// until every clone is dropped. It never exposes the owner or raw runtime.
#[derive(Clone)]
pub struct RegisteredGlobalDbLeaseV1 {
    token: Arc<RegisteredGlobalDbLeaseToken>,
}

struct RegisteredGlobalDbLeaseToken {
    database: RegisteredGlobalDb,
}

impl std::ops::Deref for RegisteredGlobalDbLeaseV1 {
    type Target = RegisteredGlobalDb;

    fn deref(&self) -> &Self::Target {
        &self.token.database
    }
}

impl AsRef<RegisteredGlobalDb> for RegisteredGlobalDbLeaseV1 {
    fn as_ref(&self) -> &RegisteredGlobalDb {
        self
    }
}

impl std::borrow::Borrow<RegisteredGlobalDb> for RegisteredGlobalDbLeaseV1 {
    fn borrow(&self) -> &RegisteredGlobalDb {
        self
    }
}

impl RegisteredGlobalDbLeaseV1 {
    fn from_database(database: RegisteredGlobalDb) -> Self {
        Self {
            token: Arc::new(RegisteredGlobalDbLeaseToken { database }),
        }
    }

    /// Whether both leases retain the same registered-database client token.
    pub fn shares_client_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}

pub struct RegisteredGlobalDb {
    database: Database,
    project_graph: Arc<OnceLock<VerifiedGraphRuntimeWeakProxyV1>>,
    session_relation_graph: Arc<SessionRelationGraphStateV1>,
}

impl RegisteredGlobalDb {
    #[hotpath::measure(future = true, label = "global_db.registered.schema")]
    pub async fn converge_schema(
        &self,
        convergence: super::schema_stages::RegisteredSchemaConvergence,
    ) -> tracedecay_domain::errors::Result<()> {
        super::schema_stages::converge_registered_schema(&self.database, convergence).await
    }

    pub async fn release_connection_memory(&self) -> tracedecay_domain::errors::Result<()> {
        self.database.release_connection_memory().await
    }

    #[hotpath::skip]
    pub(crate) async fn checkpoint_database(&self) -> tracedecay_domain::errors::Result<()> {
        self.database.checkpoint().await
    }

    /// The write-authority role retained by this client's guarded database.
    /// WAL file truncation is authorized by the runtime only for the
    /// exclusive maintenance role.
    pub(crate) fn write_authority_role(
        &self,
    ) -> tracedecay_domain::errors::Result<tracedecay_runtime_core::db::DatabaseAuthorityRole> {
        Ok(self.database.write_authority()?.role())
    }

    /// Truncates the drained WAL file through the runtime's exclusive
    /// maintenance facade.
    #[hotpath::skip]
    pub(crate) async fn truncate_database_wal(&self) -> tracedecay_domain::errors::Result<()> {
        self.database.truncate_wal_for_offline_maintenance().await
    }

    fn from_database(database: Database) -> Self {
        Self::from_database_with_project_graph(
            database,
            Arc::new(OnceLock::new()),
            Arc::new(RwLock::new(None)),
        )
    }

    fn from_database_with_project_graph(
        database: Database,
        project_graph: Arc<OnceLock<VerifiedGraphRuntimeWeakProxyV1>>,
        session_relation_graph: Arc<SessionRelationGraphStateV1>,
    ) -> Self {
        Self {
            database,
            project_graph,
            session_relation_graph,
        }
    }

    /// Wraps an already-published guarded database for WAL maintenance tests.
    ///
    /// The exclusive-maintenance truncation lane cannot be exercised through
    /// the ordinary registered harness because the registered fixture
    /// publisher mints only Test-role authority; this constructor lets a test
    /// drive [`RegisteredGlobalDb::checkpoint_result`] over a
    /// maintenance-scoped publication without bypassing the database facade.
    #[cfg(test)]
    pub(crate) fn from_database_for_wal_maintenance_test(database: Database) -> Self {
        Self::from_database(database)
    }

    pub fn read_connection(&self) -> DatabaseEngineReadConnection {
        self.database.read_connection()
    }

    /// Creates an observation adapter bound to this exact guarded client.
    /// The adapter retains the client token independently and cannot recover a
    /// raw registry runtime or write authority.
    pub fn observation_store(&self) -> crate::GlobalDbObservationStore {
        crate::GlobalDbObservationStore::new(self.database.clone())
    }

    /// Retains this exact client for closed runtime read/submit requests.
    ///
    /// The returned capability has no raw Store runtime, connection, or
    /// authority escape. Read-only registered leases retain the corresponding
    /// mode reduction, so runtime submission remains denied for them.
    pub fn runtime_client(&self) -> DatabaseRuntimeClientV1 {
        self.database.runtime_client()
    }

    pub fn bind_project_graph_runtime(
        &self,
        runtime: VerifiedGraphRuntimeWeakProxyV1,
    ) -> Result<(), Box<VerifiedGraphRuntimeWeakProxyV1>> {
        let session_shard = &self.binding().shard_id;
        let graph_binding = runtime.relational_binding();
        let graph_locator = runtime.relational_verified_locator();
        let exact = matches!(
            (&session_shard.scope, &graph_binding.shard_id.scope),
            (
                StoreShardScopeV1::ProjectSessions { project_id: expected },
                StoreShardScopeV1::Project { project_id: actual },
            ) if expected == actual
                && session_shard.brain_id == graph_binding.shard_id.brain_id
                && session_shard.profile_id == graph_binding.shard_id.profile_id
        ) && graph_locator.shard_id == graph_binding.shard_id
            && graph_locator.incarnation == graph_binding.incarnation;
        if !exact {
            return Err(Box::new(runtime));
        }
        if let Some(bound) = self.project_graph.get() {
            return if bound.shares_runtime_with(&runtime) {
                Ok(())
            } else {
                Err(Box::new(runtime))
            };
        }
        match self.project_graph.set(runtime) {
            Ok(()) => Ok(()),
            Err(runtime) => {
                if self
                    .project_graph
                    .get()
                    .is_some_and(|bound| bound.shares_runtime_with(&runtime))
                {
                    Ok(())
                } else {
                    Err(Box::new(runtime))
                }
            }
        }
    }

    pub fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1> {
        self.project_graph.get()
    }

    #[hotpath::measure(future = true, label = "global_db.registered.txn.snapshot")]
    pub async fn read_snapshot(
        &self,
    ) -> tracedecay_domain::errors::Result<DatabaseEngineReadSnapshot> {
        self.database
            .begin_engine_read_snapshot("open registered database read snapshot")
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.snapshot_to")]
    pub async fn snapshot_to(&self, destination: &Path) -> tracedecay_domain::errors::Result<()> {
        self.prepare_snapshot_destination(destination)?;
        self.database.snapshot_to(destination).await
    }

    /// Produces an interruption-aware snapshot over this exact guarded
    /// registered database. The request probe cannot acquire a raw runtime or
    /// authority; writer authorization remains inside the database facade.
    #[hotpath::measure(
        future = true,
        label = "global_db.registered.snapshot_to_interruptible"
    )]
    pub async fn snapshot_to_interruptible(
        &self,
        destination: &Path,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
    ) -> tracedecay_domain::errors::Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt> {
        self.prepare_snapshot_destination(destination)?;
        self.database
            .snapshot_to_interruptible(destination, probe)
            .await
    }

    fn prepare_snapshot_destination(
        &self,
        destination: &Path,
    ) -> tracedecay_domain::errors::Result<()> {
        if destination == self.database.canonical_database_path() {
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
        if self.database.canonical_database_path().parent() == Some(parent) {
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
        Ok(())
    }

    #[hotpath::skip]
    async fn rearm_queued_projection_retries(&self) -> tracedecay_domain::errors::Result<()> {
        let transaction = self
            .database
            .begin_write_transaction("rearm queued projection retries")
            .await?;
        crate::observation_projection::rearm_queued_projection_retries(&transaction)
            .await
            .map_err(|error| {
                registered_error("rearm queued projection retries", error.durable_detail())
            })?;
        transaction.commit().await
    }

    /// Rebuilds the registered observation projection through this client's
    /// guarded database capability.
    #[hotpath::skip]
    pub async fn rebuild_observation_projection(
        &self,
        frontier_sequence: u64,
    ) -> tracedecay_store::ProjectionStoreResult<tracedecay_store::ProjectionRebuildOutcome> {
        crate::observation_projection::rebuild_projection(&self.database, frontier_sequence).await
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn validate_registry_schema_contract_for_test(
        &self,
    ) -> tracedecay_domain::errors::Result<()> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| registered_error("open registered profile schema snapshot", error))?;
        super::schema_contract::validate_registry_schema_contract(&snapshot).await
    }

    pub fn writer_connection(
        &self,
    ) -> tracedecay_domain::errors::Result<RegisteredGlobalDbWriterConnection<'_>> {
        if !self.database.is_writable() {
            return Err(registered_error(
                "acquire registered global database writer",
                "registered database client is read-only",
            ));
        }
        self.database
            .write_authority()?
            .require_active_write_scope("open registered global database writer")?;
        Ok(RegisteredGlobalDbWriterConnection {
            database: &self.database,
        })
    }

    #[hotpath::measure(future = true, label = "global_db.registered.txn.begin")]
    pub async fn begin_write_transaction(
        &self,
    ) -> tracedecay_domain::errors::Result<RegisteredGlobalDbWriteTransaction<'_>> {
        let authority = self.database.write_authority()?;
        authority.require_active_write_scope("begin registered global database transaction")?;
        let transaction = self
            .database
            .begin_write_transaction("begin registered global database transaction")
            .await?;
        Ok(RegisteredGlobalDbWriteTransaction {
            transaction,
            authority,
        })
    }

    pub fn binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.database.registered_binding()
    }

    /// Exact non-retaining locator identity for this guarded database client.
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.database.registered_verified_locator()
    }

    pub fn work_storage(
        &self,
    ) -> tracedecay_domain::errors::Result<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>
    {
        self.database.work_storage()
    }

    pub fn authorized_scope_set_storage(
        &self,
    ) -> tracedecay_domain::errors::Result<
        tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage,
    > {
        self.database.authorized_scope_set_storage()
    }

    /// Attaches the workflow source and journal authority over the registered
    /// exact-SQL handle.
    pub fn workflow_storage(
        &self,
    ) -> tracedecay_domain::errors::Result<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        self.database.workflow_storage()
    }

    pub fn handoff_open_storage(
        &self,
    ) -> tracedecay_domain::errors::Result<
        tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority,
    > {
        self.database.handoff_open_storage()
    }

    pub fn storage_telemetry_handle(
        &self,
    ) -> tracedecay_domain::errors::Result<DatabaseStorageTelemetryHandle> {
        self.database.storage_telemetry_handle()
    }

    #[hotpath::skip]
    pub async fn storage_page_counts(&self) -> tracedecay_domain::errors::Result<(u64, u64, u64)> {
        self.database.storage_page_counts().await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.compact")]
    pub async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
    ) -> tracedecay_domain::errors::Result<()> {
        self.database.run_incremental_vacuum(max_pages).await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.retention.sessions")]
    pub async fn run_session_lcm_retention(
        &self,
        provider: &str,
        session_id: Option<&str>,
        config: &tracedecay_lcm::retention::LcmRetentionConfig,
        mode: tracedecay_lcm::retention::RetentionMode,
        now: i64,
    ) -> tracedecay_domain::errors::Result<tracedecay_lcm::retention::LcmRetentionReport> {
        let storage_root = self.db_path().parent().ok_or_else(|| {
            registered_error(
                "run registered session retention",
                "registered sessions database has no storage root",
            )
        })?;
        tracedecay_lcm::retention::run_session_retention(
            &self.database,
            storage_root,
            provider,
            session_id,
            config,
            mode,
            now,
        )
        .await
        .map_err(|error| registered_error("run registered session retention", error))
    }

    #[hotpath::measure(future = true, label = "global_db.registered.retention.observations")]
    pub async fn run_observation_retention(
        &self,
        generation: Option<&str>,
        config: &super::observation::retention::ObservationRetentionConfig,
        mode: super::observation::retention::RetentionMode,
        now: i64,
    ) -> tracedecay_domain::errors::Result<super::observation::retention::ObservationRetentionReport>
    {
        if matches!(mode, super::observation::retention::RetentionMode::Apply) {
            self.database
                .write_authority()?
                .require_active_write_scope("run registered observation retention")?;
        }
        super::observation::retention::run_observation_retention(
            &self.database,
            generation,
            config,
            mode,
            now,
        )
        .await
    }

    pub fn db_path(&self) -> &Path {
        self.database.canonical_database_path()
    }

    pub fn git_index_transaction_store(
        &self,
    ) -> super::git_index_transactions::GlobalDbGitIndexTransactionStore<'_> {
        super::git_index_transactions::GlobalDbGitIndexTransactionStore::new(self)
    }
}

pub struct RegisteredGlobalDbWriterConnection<'a> {
    database: &'a Database,
}

impl RegisteredGlobalDbWriterConnection<'_> {
    #[hotpath::skip]
    pub async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        self.database
            .execute_write_engine("execute registered global database statement", sql, params)
            .await
            .map_err(engine_error)
    }

    #[hotpath::skip]
    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        self.database.read_connection().query(sql, params).await
    }

    #[hotpath::skip]
    pub async fn execute_batch(
        &self,
        sql: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.database
            .execute_write_batch("execute registered global database batch", sql)
            .await
            .map_err(engine_error)
    }
}

pub struct RegisteredGlobalDbWriteTransaction<'a> {
    transaction: DatabaseWriteTransaction<'a>,
    authority: DatabaseAuthority,
}

impl QueryExecutor for RegisteredGlobalDbWriteTransaction<'_> {
    #[hotpath::skip]
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
    #[hotpath::skip]
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

    #[hotpath::skip]
    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        RegisteredGlobalDbWriteTransaction::execute_batch(self, sql).await
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
    #[hotpath::measure(future = true, label = "global_db.registered.txn.attach")]
    async fn attach_database(
        &self,
        path: &Path,
        database_name: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        tracedecay_runtime_core::db::engine::DatabaseAttachmentExecutor::attach_database(
            &self.transaction,
            path,
            database_name,
        )
        .await
    }
}

impl RegisteredGlobalDbWriteTransaction<'_> {
    #[hotpath::skip]
    pub async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        self.transaction.execute(sql, params).await
    }

    #[hotpath::skip]
    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        self.transaction.query(sql, params).await
    }

    #[hotpath::skip]
    pub async fn execute_batch(
        &self,
        sql: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.txn.commit")]
    pub async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        if let Err(error) = self
            .authority
            .require_active_write_scope("commit registered global database transaction")
        {
            let rollback = self.transaction.rollback().await;
            return match rollback {
                Ok(()) => Err(engine_error(error)),
                Err(rollback_error) => Err(
                    tracedecay_runtime_core::db::engine::Error::invalid_operation(format!(
                        "{error}; rollback after authority loss failed: {rollback_error}"
                    )),
                ),
            };
        }
        self.transaction.commit().await.map_err(engine_error)
    }

    #[hotpath::measure(future = true, label = "global_db.registered.txn.rollback")]
    pub async fn rollback(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.transaction.rollback().await.map_err(engine_error)
    }
}

fn registered_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

fn registered_owner_error(error: DatabaseOwnerErrorV1) -> TraceDecayError {
    registered_error(
        "issue registered global database client",
        format!("{error:?}"),
    )
}

fn engine_error(error: TraceDecayError) -> tracedecay_runtime_core::db::engine::Error {
    tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
}

#[cfg(test)]
#[path = "registered/workflow_schema_tests.rs"]
mod workflow_schema_tests;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, atomic::AtomicBool};

    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
        VerifiedGraphSnapshot,
    };
    use tracedecay_runtime_core::db::{
        Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
        TestRuntimeProfileIdentityV1,
    };
    use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
    use tracedecay_runtime_core::store_runtime::registry::{
        StoreRuntimeRetirementBlocker, StoreRuntimeRetirementOutcome, StoreRuntimeRetirementResult,
    };
    use tracedecay_store::{
        FactReadControl, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
        RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1,
        StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
    };

    use super::RegisteredGlobalDb;

    struct TestRegisteredGraphRuntime {
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
    }

    impl VerifiedGraphRuntimePortV1 for TestRegisteredGraphRuntime {
        fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.locator
        }

        fn cancel_reconciliation(&self) {}

        fn publish_verified_manifest(
            &self,
            _manifest: &GraphGenerationManifest,
            _idempotency_key: GraphIdempotencyKey,
            _cancelled: Arc<AtomicBool>,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            Err(GraphDbError::unavailable("test publication is unavailable"))
        }

        fn reconcile_verified_manifest(
            &self,
            _manifest: &GraphGenerationManifest,
            _idempotency_key: GraphIdempotencyKey,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            Err(GraphDbError::unavailable(
                "test reconciliation is unavailable",
            ))
        }

        fn verified_snapshot(
            &self,
            _projection: &GraphProjectionIdentity,
            _read_control: FactReadControl,
        ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
            Ok(None)
        }
    }

    struct ActiveSnapshotProbe {
        cancellation: RuntimeCancellationIdentityV1,
        deadline: RuntimeDeadlineV1,
    }

    impl RuntimeRequestProbeV1 for ActiveSnapshotProbe {
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

    fn active_snapshot_probe() -> Arc<dyn RuntimeRequestProbeV1> {
        Arc::new(ActiveSnapshotProbe {
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new("cancellation.global-snapshot")
                    .expect("valid global snapshot cancellation identity"),
                generation: 1,
            },
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new("deadline.global-snapshot")
                    .expect("valid global snapshot deadline identity"),
            },
        })
    }

    #[tokio::test]
    async fn registered_database_lease_keeps_runtime_alive_after_map_owner_drops() {
        let fixture = crate::tests::harness::RegisteredGlobalDbRetirementHarnessV1::open(
            "registered-global-db-lease-foreign-survival",
        )
        .await;
        let (map_lease, database, retirement, _directory, scope) = fixture.into_parts();
        let foreign = map_lease.clone();
        assert!(foreign.shares_client_with(&map_lease));
        let mut owners = BTreeMap::from([("profile", database)]);
        let independent = owners
            .get("profile")
            .expect("map owner contains the registered database")
            .issue_lease()
            .expect("owner issues independently counted client");
        assert!(!foreign.shares_client_with(&independent));
        drop(independent);
        drop(map_lease);

        owners.clear();
        drop(owners);
        drop(scope);

        let targets = match retirement
            .registry()
            .reserve_retirement_batch(vec![retirement.retirement_target()])
        {
            StoreRuntimeRetirementResult::Blocked(refusal) => {
                let blockers = refusal.blockers();
                assert_eq!(blockers.len(), 2, "unexpected blockers: {blockers:#?}");
                assert!(
                    blockers.iter().any(|blocker| matches!(
                        blocker,
                        StoreRuntimeRetirementBlocker::DatabaseAttachments { binding, count }
                            if binding.as_ref() == retirement.binding() && *count == 1
                    )),
                    "the foreign facade must retain its shared database attachment: {blockers:#?}"
                );
                assert!(
                    blockers.iter().any(|blocker| matches!(
                        blocker,
                        StoreRuntimeRetirementBlocker::ClientLeases { binding, count }
                            if binding.as_ref() == retirement.binding() && *count == 1
                    )),
                    "the foreign facade must retain its independently counted client: {blockers:#?}"
                );
                assert!(matches!(
                    refusal.targets(),
                    [target] if target.binding() == retirement.binding()
                ));
                refusal.into_parts().1
            }
            StoreRuntimeRetirementResult::Reserved(_) => {
                panic!("foreign registered database lease must refuse retirement")
            }
        };

        drop(foreign);

        let mut reservation = match retirement.registry().reserve_retirement_batch(targets) {
            StoreRuntimeRetirementResult::Reserved(reservation) => reservation,
            StoreRuntimeRetirementResult::Blocked(refusal) => {
                panic!("dropped registered database lease must permit retirement: {refusal:?}")
            }
        };
        let committed = reservation
            .commit()
            .expect("retire released registered runtime");
        assert!(matches!(
            committed.outcomes(),
            [StoreRuntimeRetirementOutcome::Closed { target }]
                if target.binding() == retirement.binding()
        ));

        let reopened = retirement
            .reopen()
            .await
            .expect("reopen retired registered runtime");
        assert_ne!(reopened.binding(), retirement.binding());
        assert_eq!(reopened.binding().shard_id, retirement.binding().shard_id);
        assert_eq!(
            reopened.binding().incarnation,
            retirement.binding().incarnation
        );
        assert!(
            reopened.binding().authority_epoch > retirement.binding().authority_epoch,
            "a reopened runtime must mint a newer authority epoch"
        );
        assert_eq!(reopened.locator().verified(), retirement.locator());
    }

    #[tokio::test]
    async fn weak_registered_owner_issuer_mints_fresh_guarded_command_leases() {
        let fixture = crate::tests::harness::RegisteredGlobalDbRetirementHarnessV1::open(
            "weak-registered-global-db-owner-issuer",
        )
        .await;
        let (map_lease, database, _retirement, _directory, _scope) = fixture.into_parts();
        let issuer = database.weak_lease_issuer();
        assert_eq!(issuer.registered_binding(), map_lease.binding());
        assert_eq!(
            issuer.registered_verified_locator(),
            map_lease.verified_locator()
        );

        let first = issuer
            .issue_lease()
            .expect("ready registered owner issues a guarded command lease");
        let first_clone = first.clone();
        let second = issuer
            .issue_lease()
            .expect("each command receives a fresh guarded lease");
        assert!(first.shares_client_with(&first_clone));
        assert!(!first.shares_client_with(&second));
    }

    #[tokio::test]
    async fn registered_project_graph_binding_retains_only_the_database_weak_proxy() {
        crate::register_test_schema_installer();
        let directory = tempfile::tempdir().expect("registered weak graph proxy directory");
        let project_id = ProjectId::new("project.registered-weak-graph")
            .expect("valid registered weak graph project identity");
        let profile_identity = TestRuntimeProfileIdentityV1::new(
            BrainId::new("brain.registered-weak-graph").expect("valid test brain identity"),
            UserProfileId::new("profile.registered-weak-graph")
                .expect("valid test profile identity"),
        );
        let graph_path = directory.path().join("project/memory.db");
        let sessions_path = directory.path().join("project/sessions.db");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(
            graph_path.parent().expect("graph database parent"),
        )
        .expect("create registered weak graph project directory");
        let graph_authority = DatabaseAuthority::acquire_test(
            &graph_path,
            "open registered weak graph project runtime",
        )
        .expect("project graph database authority");
        let (graph_database, _) = Database::publish_registered_test_runtime_for_profile_identity(
            &graph_path,
            &graph_authority,
            TestDatabaseRuntimeMode::Initialize,
            profile_identity.clone(),
            TestDatabaseRuntimeScope::Project {
                project_id: project_id.clone(),
            },
        )
        .await
        .expect("publish project graph database");
        let runtime: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(TestRegisteredGraphRuntime {
            binding: graph_database.registered_binding().clone(),
            locator: graph_database.registered_verified_locator().clone(),
        });
        let weak_runtime = Arc::downgrade(&runtime);
        graph_database
            .bind_memory_graph_runtime(Arc::clone(&runtime))
            .expect("bind exact project graph runtime");
        let proxy = graph_database
            .memory_graph_runtime()
            .expect("database issues the exact weak graph proxy");

        let sessions_authority = DatabaseAuthority::acquire_test(
            &sessions_path,
            "open registered weak graph sessions runtime",
        )
        .expect("project sessions database authority");
        let (sessions_database, _) =
            Database::publish_registered_test_runtime_for_profile_identity(
                &sessions_path,
                &sessions_authority,
                TestDatabaseRuntimeMode::Initialize,
                profile_identity,
                TestDatabaseRuntimeScope::ProjectSessions { project_id },
            )
            .await
            .expect("publish project sessions database");
        let registered = RegisteredGlobalDb::from_database(sessions_database);
        assert!(
            registered.bind_project_graph_runtime(proxy.clone()).is_ok(),
            "database-issued weak graph proxy must bind"
        );
        assert!(
            registered.bind_project_graph_runtime(proxy).is_ok(),
            "binding the same weak runtime must be idempotent"
        );

        drop(runtime);
        assert!(weak_runtime.upgrade().is_none());
        let projection = GraphProjectionIdentity::new(
            tracedecay_graph_db::GraphNamespace::new("registered-weak-proxy")
                .expect("valid registered weak proxy namespace"),
            tracedecay_graph_db::GraphProjectionId::new("availability")
                .expect("valid registered weak proxy projection"),
        );
        assert!(matches!(
            registered
                .project_graph_runtime()
                .expect("registered graph proxy remains bound")
                .verified_snapshot(&projection, FactReadControl::new(Arc::new(|| false)),),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[tokio::test]
    async fn registered_database_interruptible_snapshot_returns_the_canonical_receipt() {
        let fixture = crate::tests::harness::RegisteredGlobalDbRetirementHarnessV1::open(
            "registered-global-db-interruptible-snapshot",
        )
        .await;
        let (database, _owner, _retirement, directory, _scope) = fixture.into_parts();
        let destination = directory.path().join("backup/registered-snapshot.db");

        let receipt = database
            .snapshot_to_interruptible(&destination, active_snapshot_probe())
            .await
            .expect("guarded registered database produces an interruptible snapshot");
        assert!(destination.is_file());
        assert!(receipt.destination_bytes > 0);
    }
}
