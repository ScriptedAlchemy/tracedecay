use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use tracedecay_runtime_core::{
    db::{
        Database, DatabaseEngineReadConnection, DatabaseEngineReadSnapshot, DatabaseOwnerErrorV1,
        DatabaseOwnerRetirementReservationV1, DatabaseOwnerV1, DatabaseOwnerWeakLeaseIssuerErrorV1,
        DatabaseOwnerWeakLeaseIssuerV1, DatabaseRuntimeClientV1, DatabaseStorageTelemetryHandle,
        DatabaseWriteTransaction,
        engine::{Executor, IntoParams, QueryExecutor, Rows},
    },
    errors::TraceDecayError,
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

/// The sole map owner for one registered global-database publication.
///
/// It can issue independently counted client leases, but cannot be cloned or
/// recovered from one. Daemon maps retain this owner; all request and worker
/// paths retain only [`RegisteredGlobalDbLeaseV1`].
pub struct RegisteredGlobalDbOwnerV1 {
    database: DatabaseOwnerV1,
}

/// Cloneable, weak issuance route for one registered global-database owner.
///
/// This route retains no database client, raw runtime, or SQL authority. Each
/// command must issue its own [`RegisteredGlobalDbLeaseV1`], which keeps the
/// exact owner lifecycle and Store retirement fence authoritative.
#[derive(Clone)]
pub struct RegisteredGlobalDbWeakLeaseIssuerV1 {
    database: DatabaseOwnerWeakLeaseIssuerV1,
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
    pub async fn admit_and_attach(
        database: DatabaseOwnerV1,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let temporary = database.issue_lease().map_err(registered_owner_error)?;
        let registered = RegisteredGlobalDb::from_database(temporary);
        super::schema_stages::ensure_attached_registered_schema(&registered.database).await?;
        registered.rearm_queued_projection_retries().await?;
        super::schema_stages::converge_attached_registered_schema(&registered.database).await?;
        drop(registered);
        Ok(Self { database })
    }

    /// Returns the resumable convergence plan for an already admitted schema
    /// without retaining an unowned client lease.
    pub async fn admit_and_attach_for_daemon(
        database: DatabaseOwnerV1,
    ) -> tracedecay_runtime_core::errors::Result<(
        Self,
        super::schema_stages::RegisteredSchemaConvergence,
    )> {
        let temporary = database.issue_lease().map_err(registered_owner_error)?;
        let registered = RegisteredGlobalDb::from_database(temporary);
        super::schema_stages::ensure_attached_registered_schema(&registered.database).await?;
        registered.rearm_queued_projection_retries().await?;
        drop(registered);
        Ok((
            Self { database },
            super::schema_stages::RegisteredSchemaConvergence::for_existing_client(),
        ))
    }

    /// Issues a read-write client when the underlying map owner is writable.
    /// Each call owns one fresh Store client token; clones of the result share
    /// only that issuance.
    pub fn issue_lease(&self) -> Result<RegisteredGlobalDbLeaseV1, DatabaseOwnerErrorV1> {
        Ok(RegisteredGlobalDbLeaseV1::from_database(
            RegisteredGlobalDb::from_database(self.database.issue_lease()?),
        ))
    }

    /// Issues a mode-reduced client that can never regain write authority.
    pub fn issue_read_only_lease(&self) -> Result<RegisteredGlobalDbLeaseV1, DatabaseOwnerErrorV1> {
        Ok(RegisteredGlobalDbLeaseV1::from_database(
            RegisteredGlobalDb::from_database(self.database.issue_read_only_lease()?),
        ))
    }

    /// Creates a cloneable route for command-scoped registered leases without
    /// retaining this owner or a counted Store client.
    #[must_use]
    pub fn weak_lease_issuer(&self) -> RegisteredGlobalDbWeakLeaseIssuerV1 {
        RegisteredGlobalDbWeakLeaseIssuerV1 {
            database: self.database.weak_lease_issuer(),
        }
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
        Ok(RegisteredGlobalDbLeaseV1::from_database(
            RegisteredGlobalDb::from_database(self.database.issue_lease()?),
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
    project_graph: OnceLock<VerifiedGraphRuntimeWeakProxyV1>,
    session_relation_graph: OnceLock<(
        crate::session_temporal::relations::SessionRelationScope,
        tracedecay_graph_db::GraphDbLeaseV1,
        StoreRuntimeBindingV1,
        VerifiedStoreLocatorV1,
    )>,
}

#[derive(Clone)]
pub struct RegisteredWorkTopologyV1 {
    source: tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    runtime: VerifiedGraphRuntimeWeakProxyV1,
}

impl RegisteredWorkTopologyV1 {
    pub fn verified_snapshot(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
        cancelled: Arc<AtomicBool>,
    ) -> Result<
        tracedecay_runtime_core::work_topology::WorkTopologyStore,
        tracedecay_runtime_core::work_topology::WorkTopologyError,
    > {
        let events = self
            .source
            .load_authority_events(authority)
            .map_err(|error| {
                tracedecay_runtime_core::work_topology::WorkTopologyError::Unavailable(
                    error.to_string(),
                )
            })?;
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(tracedecay_graph_db::GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        tracedecay_runtime_core::work_topology::WorkTopologyStore::publish_from_events(
            &events,
            &check,
            |manifest, key| {
                self.runtime
                    .publish_verified_manifest(manifest, key, Arc::clone(&cancelled))
            },
        )
    }
}

#[derive(Clone)]
pub struct RegisteredWorkflowTopologyV1 {
    source: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    runtime: VerifiedGraphRuntimeWeakProxyV1,
}

impl RegisteredWorkflowTopologyV1 {
    pub fn verified_snapshot(
        &self,
        definition_id: &tracedecay_domain::WorkflowDefinitionId,
        definition_version: u64,
        cancelled: Arc<AtomicBool>,
    ) -> Result<
        tracedecay_runtime_core::workflow_topology::WorkflowTopologyStore,
        tracedecay_runtime_core::workflow_topology::WorkflowTopologyError,
    > {
        let definition = self
            .source
            .load_definition_source(definition_id, definition_version)
            .map_err(|error| {
                tracedecay_runtime_core::workflow_topology::WorkflowTopologyError::Unavailable(
                    format!("{error:?}"),
                )
            })?
            .ok_or_else(|| {
                tracedecay_runtime_core::workflow_topology::WorkflowTopologyError::Unavailable(
                    "workflow definition source is missing".to_owned(),
                )
            })?;
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(tracedecay_graph_db::GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        tracedecay_runtime_core::workflow_topology::WorkflowTopologyStore::publish_from_definition(
            &definition,
            &check,
            |manifest, key| {
                self.runtime
                    .publish_verified_manifest(manifest, key, Arc::clone(&cancelled))
            },
        )
    }
}

/// Core Work command and projection services over the registered exact-SQL
/// channel.
pub struct RegisteredWorkApplicationServicesV1 {
    commands:
        tracedecay_application::WorkService<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>,
    projections: tracedecay_application::WorkProjectionReadService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    topology: RegisteredWorkTopologyV1,
    attempts: tracedecay_application::WorkAttemptService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    run_control: tracedecay_application::WorkRunControlService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    placement: tracedecay_application::WorkPlacementService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    artifact_hydration: tracedecay_application::WorkArtifactHydrationService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    duplicate_adjudications: tracedecay_application::WorkDuplicateAdjudicationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
}

/// The Work product graph authority: its verified reads and its journaled
/// mutations, both over the same registered store.
///
/// This is a second Work authority, not a view of the first. The task services
/// above are scoped by `WorkAuthority`; this one is scoped by the registered
/// profile owner, which is also where its owner identity comes from — the
/// store's own binding, never a value a request supplied.
pub struct RegisteredWorkProductServicesV1 {
    reads: tracedecay_application::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    mutations: tracedecay_application::WorkProductMutationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    attempts: tracedecay_application::WorkProductAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    synthesis: tracedecay_application::WorkProductSynthesisAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    retry: tracedecay_application::WorkProductRetryServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_application::RuntimeWorkRetryEvidenceV1,
    >,
}

impl RegisteredWorkProductServicesV1 {
    pub const fn reads(
        &self,
    ) -> &tracedecay_application::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.reads
    }

    pub const fn mutations(
        &self,
    ) -> &tracedecay_application::WorkProductMutationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.mutations
    }

    pub const fn attempts(
        &self,
    ) -> &tracedecay_application::WorkProductAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempts
    }

    pub const fn synthesis(
        &self,
    ) -> &tracedecay_application::WorkProductSynthesisAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.synthesis
    }

    pub const fn retry(
        &self,
    ) -> &tracedecay_application::WorkProductRetryServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_application::RuntimeWorkRetryEvidenceV1,
    > {
        &self.retry
    }
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

    pub fn topology(&self) -> &RegisteredWorkTopologyV1 {
        &self.topology
    }

    pub fn attempts(
        &self,
    ) -> &tracedecay_application::WorkAttemptService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempts
    }

    /// The run-level pause/resume authority.
    ///
    /// It is a separate service from [`Self::attempts`] because the aggregate
    /// it owns is separate: an attempt lease fences one attempt, while the run
    /// control fences every future reservation of the run.
    pub const fn run_control(
        &self,
    ) -> &tracedecay_application::WorkRunControlService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.run_control
    }

    /// The placement preflight/admit/status/release authority.
    pub const fn placement(
        &self,
    ) -> &tracedecay_application::WorkPlacementService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.placement
    }

    /// The artifact and evidence hydration read authority.
    pub const fn artifact_hydration(
        &self,
    ) -> &tracedecay_application::WorkArtifactHydrationService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.artifact_hydration
    }

    /// Explicit revisioned duplicate-effort adjudication authority.
    pub const fn duplicate_adjudications(
        &self,
    ) -> &tracedecay_application::WorkDuplicateAdjudicationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.duplicate_adjudications
    }
}

/// Workflow definition reads and journaled mutation authority over the
/// registered exact-SQL channel.
///
/// [`WorkflowSqliteAuthority`]: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority
pub struct RegisteredWorkflowApplicationServicesV1 {
    definitions: tracedecay_application::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    >,
    effects: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    topology: RegisteredWorkflowTopologyV1,
}

impl RegisteredWorkflowApplicationServicesV1 {
    pub fn definitions(
        &self,
    ) -> &tracedecay_application::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        &self.definitions
    }

    pub fn effects(&self) -> &tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority {
        &self.effects
    }

    pub fn has_pending_effects(
        &self,
        worktree_id: &tracedecay_domain::WorktreeId,
    ) -> Result<bool, tracedecay_application::WorkflowEffectAuthorityErrorV1> {
        tracedecay_application::WorkflowEffectAuthorityPortV1::has_pending_effects(
            &self.effects,
            worktree_id,
        )
    }

    pub fn topology(&self) -> &RegisteredWorkflowTopologyV1 {
        &self.topology
    }
}

impl RegisteredGlobalDb {
    pub async fn converge_schema(
        &self,
        convergence: super::schema_stages::RegisteredSchemaConvergence,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        super::schema_stages::converge_registered_schema(&self.database, convergence).await
    }

    pub async fn release_connection_memory(&self) -> tracedecay_runtime_core::errors::Result<()> {
        self.database.release_connection_memory().await
    }

    pub(crate) async fn checkpoint_database(&self) -> tracedecay_runtime_core::errors::Result<()> {
        self.database.checkpoint().await
    }

    fn from_database(database: Database) -> Self {
        Self {
            database,
            project_graph: OnceLock::new(),
            session_relation_graph: OnceLock::new(),
        }
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

    pub async fn read_snapshot(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<DatabaseEngineReadSnapshot> {
        self.database
            .begin_engine_read_snapshot("open registered database read snapshot")
            .await
    }

    pub async fn snapshot_to(
        &self,
        destination: &Path,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.prepare_snapshot_destination(destination)?;
        self.database.snapshot_to(destination).await
    }

    /// Produces an interruption-aware snapshot over this exact guarded
    /// registered database. The request probe cannot acquire a raw runtime or
    /// authority; writer authorization remains inside the database facade.
    pub async fn snapshot_to_interruptible(
        &self,
        destination: &Path,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
    ) -> tracedecay_runtime_core::errors::Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt>
    {
        self.prepare_snapshot_destination(destination)?;
        self.database
            .snapshot_to_interruptible(destination, probe)
            .await
    }

    fn prepare_snapshot_destination(
        &self,
        destination: &Path,
    ) -> tracedecay_runtime_core::errors::Result<()> {
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

    async fn rearm_queued_projection_retries(&self) -> tracedecay_runtime_core::errors::Result<()> {
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
    pub async fn rebuild_observation_projection(
        &self,
        frontier_sequence: u64,
    ) -> tracedecay_store::ProjectionStoreResult<tracedecay_store::ProjectionRebuildOutcome> {
        crate::observation_projection::rebuild_projection(&self.database, frontier_sequence).await
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
        if !self.database.is_writable() {
            return Err(registered_error(
                "acquire registered global database writer",
                "registered database client is read-only",
            ));
        }
        Ok(RegisteredGlobalDbWriterConnection {
            database: &self.database,
        })
    }

    pub async fn begin_write_transaction(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbWriteTransaction<'_>> {
        let transaction = self
            .database
            .begin_write_transaction("begin registered global database transaction")
            .await?;
        Ok(RegisteredGlobalDbWriteTransaction { transaction })
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
    ) -> tracedecay_runtime_core::errors::Result<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>
    {
        self.database.work_storage()
    }

    pub fn authorized_scope_set_storage(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage,
    > {
        self.database.authorized_scope_set_storage()
    }

    pub fn work_application_services(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredWorkApplicationServicesV1> {
        let storage = self.work_storage()?;
        let runtime = self.project_graph_runtime().cloned().ok_or_else(|| {
            registered_error(
                "attach registered Work topology",
                "project graph runtime is not bound",
            )
        })?;
        Ok(RegisteredWorkApplicationServicesV1 {
            commands: tracedecay_application::WorkService::new(storage.clone()),
            projections: tracedecay_application::WorkProjectionReadService::new(storage.clone()),
            attempts: tracedecay_application::WorkAttemptService::new(storage.clone()),
            run_control: tracedecay_application::WorkRunControlService::new(storage.clone()),
            placement: tracedecay_application::WorkPlacementService::new(storage.clone()),
            artifact_hydration: tracedecay_application::WorkArtifactHydrationService::new(
                storage.clone(),
            ),
            duplicate_adjudications:
                tracedecay_application::WorkDuplicateAdjudicationServiceV1::new(storage.clone()),
            topology: RegisteredWorkTopologyV1 {
                source: storage,
                runtime,
            },
        })
    }

    /// Attaches the Work product graph authority over the registered exact-SQL
    /// handle.
    ///
    /// The catalog binding is supplied by the caller rather than minted here,
    /// because a service composed against a capability the catalog does not
    /// advertise could never authorize a request: it would look wired and
    /// answer nothing. Whichever adapter mounts a Work product operation
    /// passes that operation's own capability and use-case ids.
    ///
    /// The owner identity is NOT a parameter. It is resolved from the store's
    /// own registered binding, so no caller can ask for another profile's Work
    /// product by naming it.
    pub fn work_product_services(
        &self,
        binding: tracedecay_application::WorkProductBindingV1,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredWorkProductServicesV1> {
        let storage = self.work_storage()?;
        Ok(RegisteredWorkProductServicesV1 {
            reads: tracedecay_application::WorkProductReadServiceV1::new(
                storage.clone(),
                storage.clone(),
                binding,
            ),
            mutations: tracedecay_application::WorkProductMutationServiceV1::new(
                storage.clone(),
                storage.clone(),
                storage.clone(),
            ),
            attempts: tracedecay_application::WorkProductAttemptServiceV1::new(storage.clone()),
            synthesis: tracedecay_application::WorkProductSynthesisAttemptServiceV1::new(
                storage.clone(),
            ),
            retry: tracedecay_application::WorkProductRetryServiceV1::new(
                storage,
                tracedecay_application::RuntimeWorkRetryEvidenceV1,
            ),
        })
    }

    /// Attaches product intelligence to the canonical verified Work graph and
    /// rooted-evidence authorities owned by this registered exact-SQL store.
    pub fn work_intelligence_service(
        &self,
        binding: tracedecay_application::WorkProductBindingV1,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_application::WorkIntelligenceServiceV1<
            tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
            tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        >,
    > {
        let storage = self.work_storage()?;
        Ok(tracedecay_application::WorkIntelligenceServiceV1::new(
            storage.clone(),
            storage,
            binding,
        ))
    }

    /// Attaches the workflow source and journal authority over the registered
    /// exact-SQL handle.
    pub fn workflow_storage(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        self.database.workflow_storage()
    }

    pub fn workflow_application_services(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredWorkflowApplicationServicesV1> {
        let authority = self.workflow_storage()?;
        let runtime = self.project_graph_runtime().cloned().ok_or_else(|| {
            registered_error(
                "attach registered workflow topology",
                "project graph runtime is not bound",
            )
        })?;
        Ok(RegisteredWorkflowApplicationServicesV1 {
            definitions: tracedecay_application::WorkflowDefinitionService::new(authority.clone()),
            effects: authority.clone(),
            topology: RegisteredWorkflowTopologyV1 {
                source: authority,
                runtime,
            },
        })
    }

    pub fn handoff_open_storage(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority,
    > {
        self.database.handoff_open_storage()
    }

    pub fn storage_telemetry_handle(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<DatabaseStorageTelemetryHandle> {
        self.database.storage_telemetry_handle()
    }

    pub async fn storage_page_counts(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<(u64, u64, u64)> {
        self.database.storage_page_counts().await
    }

    pub async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.database.run_incremental_vacuum(max_pages).await
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
        tracedecay_sessions::runtime::lcm::retention::run_session_retention(
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

    pub async fn run_observation_retention(
        &self,
        generation: Option<&str>,
        config: &super::observation::retention::ObservationRetentionConfig,
        mode: super::observation::retention::RetentionMode,
        now: i64,
    ) -> tracedecay_runtime_core::errors::Result<
        super::observation::retention::ObservationRetentionReport,
    > {
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
        tracedecay_runtime_core::db::engine::DatabaseAttachmentExecutor::attach_database(
            &self.transaction,
            path,
            database_name,
        )
        .await
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
        self.transaction.query(sql, params).await
    }

    pub async fn execute_batch(
        &self,
        sql: &str,
    ) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }

    pub async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.transaction.commit().await.map_err(engine_error)
    }

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

    use tracedecay_domain::ProjectId;
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
        VerifiedGraphSnapshot,
    };
    use tracedecay_runtime_core::db::{
        Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
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
                assert!(matches!(
                    refusal.blockers(),
                    [StoreRuntimeRetirementBlocker::ClientLeases { binding, count }]
                        if binding.as_ref() == retirement.binding() && *count == 1
                ));
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
        assert_eq!(reopened.binding(), retirement.binding());
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
        let directory = tempfile::tempdir().expect("registered weak graph proxy directory");
        let project_id = ProjectId::new("project.registered-weak-graph")
            .expect("valid registered weak graph project identity");
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
        let (graph_database, _) = Database::publish_registered_test_runtime(
            &graph_path,
            &graph_authority,
            TestDatabaseRuntimeMode::Initialize,
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
        let (sessions_database, _) = Database::publish_registered_test_runtime(
            &sessions_path,
            &sessions_authority,
            TestDatabaseRuntimeMode::Initialize,
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
