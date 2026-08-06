use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_rusqlite_runtime::{
    graph::{
        CodeShardPhysicalLocator, GraphPhysicalAttachmentFactory, GraphRuntimePhysicalAttachment,
    },
    repository::{RepositoryPhysicalAttachmentFactory, RepositoryRuntimePhysicalAttachment},
};
use tracedecay_store::{
    AdmissionConfigV1, RuntimeMaintenanceStateV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, StoreRuntimeBindingV1,
    StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::{
    PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot, PublishedShardRuntime, StoreRuntimeKey,
    StoreRuntimeOpenMode, StoreRuntimeRegistryFailure,
};
use crate::store_runtime::shard::{ShardRuntime, ShardRuntimeError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedStoreLocator {
    verified: VerifiedStoreLocatorV1,
    path: PathBuf,
    prospective: bool,
}

impl ResolvedStoreLocator {
    pub fn new(verified: VerifiedStoreLocatorV1, path: PathBuf) -> Self {
        Self {
            verified,
            path,
            prospective: false,
        }
    }

    pub fn prospective(verified: VerifiedStoreLocatorV1, path: PathBuf) -> Self {
        Self {
            verified,
            path,
            prospective: true,
        }
    }

    pub fn verified(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) const fn is_prospective(&self) -> bool {
        self.prospective
    }

    pub(super) fn matches(&self, key: &StoreRuntimeKey) -> bool {
        self.verified.shard_id == *key.shard_id() && self.verified.incarnation == key.incarnation()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeLocatorRecord {
    key: StoreRuntimeKey,
    locator: ResolvedStoreLocator,
}

impl RuntimeLocatorRecord {
    pub(super) fn new(key: StoreRuntimeKey, locator: ResolvedStoreLocator) -> Self {
        Self { key, locator }
    }

    pub fn key(&self) -> &StoreRuntimeKey {
        &self.key
    }

    pub fn verified(&self) -> &VerifiedStoreLocatorV1 {
        self.locator.verified()
    }

    pub fn path(&self) -> &std::path::Path {
        self.locator.path()
    }

    pub(crate) const fn is_prospective(&self) -> bool {
        self.locator.is_prospective()
    }
}

pub type StoreRuntimeRegistryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait StoreRuntimeResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        mode: StoreRuntimeOpenMode,
        database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>;
}

pub trait ShardRuntimePublisher: Send + Sync {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LifecycleShardRuntimePublisher;

impl ShardRuntimePublisher for LifecycleShardRuntimePublisher {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            let admission = AdmissionConfigV1::default();
            let pinned_profile =
                matches!(request.binding.shard_id.scope, StoreShardScopeV1::Profile);
            let runtime = Arc::new(ShardRuntime::new(request.binding.clone(), pinned_profile));
            runtime
                .transition(RuntimeMaintenanceStateV1::Opening)
                .map_err(runtime_lifecycle_failure)?;
            let mut attachment =
                LifecycleShardRuntimeAttachment::new(RepositoryPhysicalAttachmentFactory);
            publish_lifecycle_runtime(request, runtime, &mut attachment, admission).await
        })
    }
}

async fn publish_lifecycle_runtime(
    request: ShardRuntimeBuildRequest,
    runtime: Arc<ShardRuntime>,
    factory: &mut LifecycleShardRuntimeAttachment,
    admission: AdmissionConfigV1,
) -> Result<PublishedShardRuntime, StoreRuntimeRegistryFailure> {
    let contract = final_schema_contract(&request.binding.shard_id.scope)?;
    let (attachment, exact_schema, initialized) =
        if request.locator.is_prospective() {
            let staging = factory.attach(&request, admission.clone())?;
            let staging_schema =
                match install_staging_schema(&request, staging.as_physical(), &contract).await {
                    Ok(exact_schema) => exact_schema,
                    Err(error) => {
                        staging.abort(true);
                        return Err(error);
                    }
                };
            if let Err(error) = staging.close_for_publication() {
                staging.abort(true);
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "close initialized SQLite staging runtime",
                    message: error,
                });
            }
            if let Err(error) = staging.commit_initialization() {
                staging.abort(true);
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "publish initialized SQLite staging runtime",
                    message: error,
                });
            }
            drop(staging);
            let exact_schema = validate_existing_schema(request.locator.path(), &contract)
                .map_err(|error| StoreRuntimeRegistryFailure::ResetRequired {
                    reset: Box::new(error),
                })?;
            if exact_schema != staging_schema {
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "verify published SQLite schema identity",
                    message: "published SQLite schema proof changed after atomic publication"
                        .to_owned(),
                });
            }
            let attachment = factory.attach_existing(&request, admission)?;
            (attachment, exact_schema, true)
        } else {
            let exact_schema = validate_existing_schema(request.locator.path(), &contract)
                .map_err(|error| StoreRuntimeRegistryFailure::ResetRequired {
                    reset: Box::new(error),
                })?;
            let attachment = factory.attach_existing(&request, admission)?;
            (attachment, exact_schema, false)
        };
    if let Err(error) = runtime.transition(RuntimeMaintenanceStateV1::Ready) {
        attachment.abort(false);
        return Err(runtime_lifecycle_failure(error));
    }
    Ok(PublishedShardRuntime::new_with_schema_migration(
        runtime,
        attachment.into_arc(),
        initialized,
        exact_schema,
    ))
}

enum LifecyclePhysicalAttachment {
    Graph(GraphRuntimePhysicalAttachment),
    Repository(RepositoryRuntimePhysicalAttachment),
}

struct LifecycleShardRuntimeAttachment {
    repository: RepositoryPhysicalAttachmentFactory,
}

impl LifecycleShardRuntimeAttachment {
    const fn new(repository: RepositoryPhysicalAttachmentFactory) -> Self {
        Self { repository }
    }

    fn attach(
        &mut self,
        request: &ShardRuntimeBuildRequest,
        admission: AdmissionConfigV1,
    ) -> Result<LifecyclePhysicalAttachment, StoreRuntimeRegistryFailure> {
        if request.locator.is_prospective() && request.mode != StoreRuntimeOpenMode::Initialize {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "open prospective SQLite runtime",
                message: "prospective locators require explicit initialization".to_owned(),
            });
        }
        if matches!(
            request.binding.shard_id.scope,
            StoreShardScopeV1::Code { .. }
        ) {
            let attachment = if request.locator.is_prospective() {
                GraphPhysicalAttachmentFactory.initialize(
                    request.binding.clone(),
                    request.locator.verified().clone(),
                    request.locator.path().to_path_buf(),
                    admission,
                )
            } else {
                let locator = CodeShardPhysicalLocator::from_verified_existing(
                    request.binding.clone(),
                    request.locator.verified().clone(),
                    request.locator.path().to_path_buf(),
                )
                .map_err(|error| {
                    StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                        operation: "prepare rusqlite graph locator",
                        message: error.to_string(),
                    }
                })?;
                GraphPhysicalAttachmentFactory.attach(&locator, admission)
            }
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "attach rusqlite graph runtime",
                message: error.to_string(),
            })?;
            Ok(LifecyclePhysicalAttachment::Graph(attachment))
        } else {
            let attachment = if request.locator.is_prospective() {
                self.repository.initialize(
                    request.binding.clone(),
                    request.locator.verified().clone(),
                    request.locator.path().to_path_buf(),
                    admission,
                )
            } else {
                self.repository.attach(
                    request.binding.clone(),
                    request.locator.verified().clone(),
                    request.locator.path().to_path_buf(),
                    admission,
                )
            }
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "attach rusqlite repository runtime",
                message: error.to_string(),
            })?;
            Ok(LifecyclePhysicalAttachment::Repository(attachment))
        }
    }

    fn attach_existing(
        &mut self,
        request: &ShardRuntimeBuildRequest,
        admission: AdmissionConfigV1,
    ) -> Result<LifecyclePhysicalAttachment, StoreRuntimeRegistryFailure> {
        if matches!(
            request.binding.shard_id.scope,
            StoreShardScopeV1::Code { .. }
        ) {
            let locator = CodeShardPhysicalLocator::from_verified_existing(
                request.binding.clone(),
                request.locator.verified().clone(),
                request.locator.path().to_path_buf(),
            )
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "prepare rusqlite graph locator",
                message: error.to_string(),
            })?;
            GraphPhysicalAttachmentFactory
                .attach(&locator, admission)
                .map(LifecyclePhysicalAttachment::Graph)
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "attach rusqlite graph runtime",
                    message: error.to_string(),
                })
        } else {
            self.repository
                .attach(
                    request.binding.clone(),
                    request.locator.verified().clone(),
                    request.locator.path().to_path_buf(),
                    admission,
                )
                .map(LifecyclePhysicalAttachment::Repository)
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "attach rusqlite repository runtime",
                    message: error.to_string(),
                })
        }
    }
}

impl LifecyclePhysicalAttachment {
    fn as_physical(&self) -> &dyn PhysicalRuntimeAttachment {
        match self {
            Self::Graph(attachment) => attachment,
            Self::Repository(attachment) => attachment,
        }
    }

    fn commit_initialization(&self) -> Result<(), String> {
        match self {
            Self::Graph(attachment) => attachment.commit_initialization(),
            Self::Repository(attachment) => attachment.commit_initialization(),
        }
    }

    fn close_for_publication(&self) -> Result<(), String> {
        match self {
            Self::Graph(attachment) => {
                attachment.drain()?;
                attachment.close_and_join()
            }
            Self::Repository(attachment) => {
                attachment.drain()?;
                attachment.close_and_join()
            }
        }
    }

    fn abort(&self, created: bool) {
        match self {
            Self::Graph(attachment) if created => {
                let _ = attachment.abort_initialization();
            }
            Self::Repository(attachment) if created => {
                let _ = attachment.abort_initialization();
            }
            Self::Graph(attachment) => {
                let _ = attachment.drain();
                let _ = attachment.close_and_join();
            }
            Self::Repository(attachment) => {
                let _ = attachment.drain();
                let _ = attachment.close_and_join();
            }
        }
    }

    fn into_arc(self) -> Arc<dyn PhysicalRuntimeAttachment> {
        match self {
            Self::Graph(attachment) => Arc::new(attachment),
            Self::Repository(attachment) => Arc::new(attachment),
        }
    }
}

struct InitializingMigrationAuthority {
    authority: crate::db::DatabaseAuthority,
    canonical_path: PathBuf,
    opened_file_identity: u64,
}

impl tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteAuthority
    for InitializingMigrationAuthority
{
    fn verify(
        &self,
        _intent: tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent,
    ) -> Result<(), tracedecay_rusqlite_runtime::exact_sql::ExactSqlError> {
        self.authority
            .require_active_write_scope("install exact final SQLite schema")
            .map_err(|error| {
                tracedecay_rusqlite_runtime::exact_sql::ExactSqlError::AuthorityDenied(
                    error.to_string(),
                )
            })?;
        let identity =
            crate::db::sqlite_generation_identity(&self.canonical_path).map_err(|_| {
                tracedecay_rusqlite_runtime::exact_sql::ExactSqlError::AuthorityDenied(
                    "could not verify initialized SQLite file identity".to_owned(),
                )
            })?;
        if identity != self.opened_file_identity {
            return Err(
                tracedecay_rusqlite_runtime::exact_sql::ExactSqlError::AuthorityDenied(
                    "initialized SQLite file identity changed".to_owned(),
                ),
            );
        }
        Ok(())
    }
}

async fn install_staging_schema(
    request: &ShardRuntimeBuildRequest,
    attachment: &dyn PhysicalRuntimeAttachment,
    contract: &crate::store_runtime::schema::StoreSchemaContractV2,
) -> Result<crate::store_runtime::schema::ExactStoreSchemaV2, StoreRuntimeRegistryFailure> {
    let authority = request.database_authority.clone().ok_or_else(|| {
        StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "install exact final SQLite schema",
            message: "initialization requires originating database authority".to_owned(),
        }
    })?;
    authority
        .require_active_write_scope("install exact final SQLite schema")
        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "install exact final SQLite schema",
            message: error.to_string(),
        })?;
    if authority.canonical_database_path() != request.locator.path() {
        return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "migrate initialized SQLite runtime",
            message: "originating database authority does not match initialized locator".to_owned(),
        });
    }
    let opened_file_identity = attachment.opened_file_identity().map_err(|message| {
        StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "install initialized SQLite runtime",
            message,
        }
    })?;
    let handle = attachment.exact_sql_handle().map_err(|message| {
        StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "install initialized SQLite runtime",
            message,
        }
    })?;
    if handle.binding() != &request.binding
        || handle.verified_locator() != request.locator.verified()
    {
        return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "install initialized SQLite runtime",
            message: "initialized exact-SQL handle identity does not match build request"
                .to_owned(),
        });
    }
    let authority = InitializingMigrationAuthority {
        canonical_path: authority.canonical_database_path().to_path_buf(),
        authority,
        opened_file_identity,
    };
    let handle = handle
        .with_write_authority(Arc::new(authority))
        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "authorize initialized SQLite schema installation",
            message: error.to_string(),
        })?;
    let connection = crate::db::engine::Connection::attach(handle);
    match &request.binding.shard_id.scope {
        StoreShardScopeV1::Code { .. }
        | StoreShardScopeV1::ProfileMemory
        | StoreShardScopeV1::Project { .. } => {
            crate::store_runtime::schema::install_final_graph_memory_schema(&connection)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "create initialized graph schema",
                    message: error.to_string(),
                })?;
        }
        StoreShardScopeV1::Profile
        | StoreShardScopeV1::ProfileSessions
        | StoreShardScopeV1::ProjectSessions { .. } => {
            crate::ports::registered_schema::install_final_registered_schema(&connection)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "create initialized global/session schema",
                    message: error.to_string(),
                })?;
        }
    }
    let exact_schema = crate::store_runtime::schema::validate_store_schema(
        &connection,
        request.locator.path(),
        contract,
    )
    .await
    .map_err(|error| match error {
        crate::store_runtime::schema::StoreSchemaAdmissionErrorV2::ResetRequired(reset) => {
            StoreRuntimeRegistryFailure::ResetRequired {
                reset: Box::new(reset),
            }
        }
    })?;
    checkpoint_staging_wal(&connection).await?;
    Ok(exact_schema)
}

fn final_schema_contract(
    scope: &StoreShardScopeV1,
) -> Result<crate::store_runtime::schema::StoreSchemaContractV2, StoreRuntimeRegistryFailure> {
    match crate::store_runtime::schema::StoreSchemaKindV2::for_scope(scope) {
        crate::store_runtime::schema::StoreSchemaKindV2::GraphMemory => {
            crate::store_runtime::schema::final_graph_memory_schema_contract().map_err(|_| {
                StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "load final graph/memory schema contract",
                    message: "compiled graph/memory schema fingerprint is invalid".to_owned(),
                }
            })
        }
        crate::store_runtime::schema::StoreSchemaKindV2::Registered => {
            crate::ports::registered_schema::final_registered_schema_contract().map_err(|error| {
                StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "load final registered schema contract",
                    message: error.to_string(),
                }
            })
        }
    }
}

fn validate_existing_schema(
    path: &std::path::Path,
    contract: &crate::store_runtime::schema::StoreSchemaContractV2,
) -> Result<
    crate::store_runtime::schema::ExactStoreSchemaV2,
    crate::store_runtime::schema::ResetRequiredV2,
> {
    crate::store_runtime::schema::validate_existing_store_schema(path, contract).map_err(|error| {
        match error {
            crate::store_runtime::schema::StoreSchemaAdmissionErrorV2::ResetRequired(reset) => {
                reset
            }
        }
    })
}

async fn checkpoint_staging_wal(
    connection: &crate::db::engine::Connection,
) -> Result<(), StoreRuntimeRegistryFailure> {
    let mut rows = connection
        .checkpoint_wal_truncate()
        .await
        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "checkpoint initialized SQLite staging runtime",
            message: error.to_string(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "read initialized SQLite staging checkpoint",
            message: error.to_string(),
        })?
        .ok_or_else(|| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "read initialized SQLite staging checkpoint",
            message: "SQLite checkpoint returned no result".to_owned(),
        })?;
    let busy =
        row.get::<i64>(0)
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "read initialized SQLite staging checkpoint",
                message: error.to_string(),
            })?;
    let log =
        row.get::<i64>(1)
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "read initialized SQLite staging checkpoint",
                message: error.to_string(),
            })?;
    let checkpointed =
        row.get::<i64>(2)
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "read initialized SQLite staging checkpoint",
                message: error.to_string(),
            })?;
    if busy != 0 || log != 0 || checkpointed != 0 {
        return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "checkpoint initialized SQLite staging runtime",
            message: format!(
                "SQLite staging WAL did not fully truncate (busy={busy}, log={log}, checkpointed={checkpointed})"
            ),
        });
    }
    Ok(())
}

impl PhysicalRuntimeAttachment for GraphRuntimePhysicalAttachment {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        let snapshot = GraphRuntimePhysicalAttachment::snapshot(self);
        PhysicalRuntimeSnapshot {
            healthy: snapshot.healthy,
            writer_present: snapshot.writer_present,
            reader_handles: snapshot.reader_handles,
            queued_operations: snapshot.queued_operations,
            queued_bytes: snapshot.queued_bytes,
            wal_bytes: snapshot.wal_bytes,
            memory_estimate_bytes: 0,
        }
    }

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(GraphRuntimePhysicalAttachment::opened_file_identity(self))
    }

    fn drain(&self) -> Result<(), String> {
        GraphRuntimePhysicalAttachment::drain(self)
    }

    fn close_and_join(&self) -> Result<(), String> {
        GraphRuntimePhysicalAttachment::close_and_join(self)
    }

    fn exact_sql_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, String> {
        GraphRuntimePhysicalAttachment::exact_sql_handle(self).map_err(|error| error.to_string())
    }

    fn storage_page_counts(&self, reader_wait: Duration) -> Result<(u64, u64, u64), String> {
        retained_storage_page_counts(
            GraphRuntimePhysicalAttachment::exact_sql_handle(self)
                .map_err(|error| error.to_string())?,
            reader_wait,
        )
    }

    fn storage_table_bytes(&self, reader_wait: Duration) -> Result<Vec<(String, u64)>, String> {
        retained_storage_table_bytes(
            GraphRuntimePhysicalAttachment::exact_sql_handle(self)
                .map_err(|error| error.to_string())?,
            reader_wait,
        )
    }

    fn run_bounded_incremental_compaction(
        &self,
        max_pages: u32,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<'_, Result<(), StoreRuntimeRegistryFailure>> {
        Box::pin(async move {
            GraphRuntimePhysicalAttachment::run_bounded_incremental_compaction(
                self, max_pages, authority,
            )
            .await
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "run bounded graph compaction",
                message: error.to_string(),
            })
        })
    }

    fn run_checkpoint(
        &self,
        request: tracedecay_rusqlite_runtime::CheckpointRequest,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        '_,
        Result<tracedecay_rusqlite_runtime::CheckpointOutcome, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            GraphRuntimePhysicalAttachment::run_checkpoint(self, request, authority)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "run graph checkpoint",
                    message: error.to_string(),
                })
        })
    }

    fn snapshot_to(
        &self,
        destination: PathBuf,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        '_,
        Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            GraphRuntimePhysicalAttachment::snapshot_to(self, destination, authority)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "snapshot graph database",
                    message: error.to_string(),
                })
        })
    }

    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<'_, Result<RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            GraphRuntimePhysicalAttachment::dispatch_submit(self, request, probe, authority)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "dispatch graph submit",
                    message: error.to_string(),
                })
        })
    }

    fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        GraphRuntimePhysicalAttachment::dispatch_read(self, request, probe).map_err(|error| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "dispatch graph read",
                message: error.to_string(),
            }
        })
    }
}

impl PhysicalRuntimeAttachment for RepositoryRuntimePhysicalAttachment {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        let snapshot = RepositoryRuntimePhysicalAttachment::snapshot(self);
        PhysicalRuntimeSnapshot {
            healthy: snapshot.healthy,
            writer_present: snapshot.writer_present,
            reader_handles: snapshot.reader_handles,
            queued_operations: snapshot.queued_operations,
            queued_bytes: snapshot.queued_bytes,
            wal_bytes: snapshot.wal_bytes,
            memory_estimate_bytes: 0,
        }
    }

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(RepositoryRuntimePhysicalAttachment::opened_file_identity(
            self,
        ))
    }

    fn drain(&self) -> Result<(), String> {
        RepositoryRuntimePhysicalAttachment::drain(self)
    }

    fn close_and_join(&self) -> Result<(), String> {
        RepositoryRuntimePhysicalAttachment::close_and_join(self)
    }

    fn exact_sql_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, String> {
        RepositoryRuntimePhysicalAttachment::exact_sql_handle(self)
            .map_err(|error| error.to_string())
    }

    fn storage_page_counts(&self, reader_wait: Duration) -> Result<(u64, u64, u64), String> {
        retained_storage_page_counts(
            RepositoryRuntimePhysicalAttachment::exact_sql_handle(self)
                .map_err(|error| error.to_string())?,
            reader_wait,
        )
    }

    fn storage_table_bytes(&self, reader_wait: Duration) -> Result<Vec<(String, u64)>, String> {
        retained_storage_table_bytes(
            RepositoryRuntimePhysicalAttachment::exact_sql_handle(self)
                .map_err(|error| error.to_string())?,
            reader_wait,
        )
    }

    fn run_bounded_incremental_compaction(
        &self,
        max_pages: u32,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<'_, Result<(), StoreRuntimeRegistryFailure>> {
        Box::pin(async move {
            RepositoryRuntimePhysicalAttachment::run_bounded_incremental_compaction(
                self, max_pages, authority,
            )
            .await
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "run bounded repository compaction",
                message: error.to_string(),
            })
        })
    }

    fn run_checkpoint(
        &self,
        request: tracedecay_rusqlite_runtime::CheckpointRequest,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        '_,
        Result<tracedecay_rusqlite_runtime::CheckpointOutcome, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            RepositoryRuntimePhysicalAttachment::run_checkpoint(self, request, authority)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "run repository checkpoint",
                    message: error.to_string(),
                })
        })
    }

    fn snapshot_to(
        &self,
        destination: PathBuf,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        '_,
        Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            RepositoryRuntimePhysicalAttachment::snapshot_to(self, destination, authority)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "snapshot repository database",
                    message: error.to_string(),
                })
        })
    }

    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> StoreRuntimeRegistryFuture<'_, Result<RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            RepositoryRuntimePhysicalAttachment::dispatch_submit(self, request, probe, authority)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "dispatch repository submit",
                    message: error.to_string(),
                })
        })
    }

    fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        RepositoryRuntimePhysicalAttachment::dispatch_read(self, request, probe).map_err(|error| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "dispatch repository read",
                message: error.to_string(),
            }
        })
    }
}

fn retained_storage_page_counts(
    handle: tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle,
    reader_wait: Duration,
) -> Result<(u64, u64, u64), String> {
    let sample = handle
        .read_only_clone()
        .store_size_telemetry(reader_wait, || None)
        .map_err(|error| error.to_string())?;
    Ok((
        u64::from(sample.page_size_bytes),
        sample.page_count,
        sample.freelist_pages,
    ))
}

fn retained_storage_table_bytes(
    handle: tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle,
    reader_wait: Duration,
) -> Result<Vec<(String, u64)>, String> {
    let samples = handle
        .read_only_clone()
        .table_size_telemetry(reader_wait, || None)
        .map_err(|error| error.to_string())?;
    Ok(samples
        .into_iter()
        .map(|sample| (sample.table_name, sample.bytes))
        .collect())
}

#[derive(Clone, Debug)]
pub struct ShardRuntimeBuildRequest {
    pub(super) binding: StoreRuntimeBindingV1,
    locator: RuntimeLocatorRecord,
    mode: StoreRuntimeOpenMode,
    database_authority: Option<crate::db::DatabaseAuthority>,
}

impl ShardRuntimeBuildRequest {
    pub(super) fn new(
        binding: StoreRuntimeBindingV1,
        locator: RuntimeLocatorRecord,
        mode: StoreRuntimeOpenMode,
        database_authority: Option<crate::db::DatabaseAuthority>,
    ) -> Self {
        Self {
            binding,
            locator,
            mode,
            database_authority,
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn locator(&self) -> &RuntimeLocatorRecord {
        &self.locator
    }

    pub const fn mode(&self) -> StoreRuntimeOpenMode {
        self.mode
    }

    pub fn database_authority(&self) -> Option<&crate::db::DatabaseAuthority> {
        self.database_authority.as_ref()
    }
}

fn runtime_lifecycle_failure(error: ShardRuntimeError) -> StoreRuntimeRegistryFailure {
    StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
        message: error.to_string(),
    }
}
