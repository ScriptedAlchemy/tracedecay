use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_rusqlite_runtime::repository::{
    RepositoryPhysicalAttachmentFactory, RepositoryRuntimePhysicalAttachment,
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
            let attachment =
                LifecycleShardRuntimeAttachment::new(RepositoryPhysicalAttachmentFactory);
            let physical = attachment.attach(&request, admission)?;
            publish_lifecycle_runtime(request, runtime, physical).await
        })
    }
}

async fn publish_lifecycle_runtime(
    request: ShardRuntimeBuildRequest,
    runtime: Arc<ShardRuntime>,
    attachment: LifecyclePhysicalAttachment,
) -> Result<PublishedShardRuntime, StoreRuntimeRegistryFailure> {
    let migrated = if request.mode == StoreRuntimeOpenMode::Initialize {
        match migrate_before_publication(&request, attachment.as_physical()).await {
            Ok(migrated) => migrated,
            Err(error) => {
                attachment.abort(request.locator.is_prospective());
                return Err(error);
            }
        }
    } else {
        false
    };
    if let Err(error) = runtime.transition(RuntimeMaintenanceStateV1::Ready) {
        attachment.abort(request.locator.is_prospective());
        return Err(runtime_lifecycle_failure(error));
    }
    if request.locator.is_prospective()
        && let Err(error) = attachment.commit_initialization()
    {
        attachment.abort(true);
        return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "commit initialized SQLite runtime",
            message: error,
        });
    }
    Ok(PublishedShardRuntime::new_with_schema_migration(
        runtime,
        attachment.into_arc(),
        migrated,
    ))
}

struct LifecyclePhysicalAttachment(RepositoryRuntimePhysicalAttachment);

struct LifecycleShardRuntimeAttachment {
    repository: RepositoryPhysicalAttachmentFactory,
}

impl LifecycleShardRuntimeAttachment {
    const fn new(repository: RepositoryPhysicalAttachmentFactory) -> Self {
        Self { repository }
    }

    fn attach(
        &self,
        request: &ShardRuntimeBuildRequest,
        admission: AdmissionConfigV1,
    ) -> Result<LifecyclePhysicalAttachment, StoreRuntimeRegistryFailure> {
        if request.locator.is_prospective() && request.mode != StoreRuntimeOpenMode::Initialize {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "open prospective SQLite runtime",
                message: "prospective locators require explicit initialization".to_owned(),
            });
        }
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
        Ok(LifecyclePhysicalAttachment(attachment))
    }
}

impl LifecyclePhysicalAttachment {
    fn as_physical(&self) -> &dyn PhysicalRuntimeAttachment {
        &self.0
    }

    fn commit_initialization(&self) -> Result<(), String> {
        self.0.commit_initialization()
    }

    fn abort(&self, created: bool) {
        if created {
            let _ = self.0.abort_initialization();
        } else {
            let _ = self.0.drain();
            let _ = self.0.close_and_join();
        }
    }

    fn into_arc(self) -> Arc<dyn PhysicalRuntimeAttachment> {
        Arc::new(self.0)
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
            .require_active_write_scope("migrate initialized SQLite runtime")
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

async fn migrate_before_publication(
    request: &ShardRuntimeBuildRequest,
    attachment: &dyn PhysicalRuntimeAttachment,
) -> Result<bool, StoreRuntimeRegistryFailure> {
    let authority = request.database_authority.clone().ok_or_else(|| {
        StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "migrate initialized SQLite runtime",
            message: "initialization requires originating database authority".to_owned(),
        }
    })?;
    authority
        .require_active_write_scope("migrate initialized SQLite runtime")
        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "migrate initialized SQLite runtime",
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
            operation: "migrate initialized SQLite runtime",
            message,
        }
    })?;
    let handle = attachment.exact_sql_handle().map_err(|message| {
        StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "migrate initialized SQLite runtime",
            message,
        }
    })?;
    if handle.binding() != &request.binding
        || handle.verified_locator() != request.locator.verified()
    {
        return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "migrate initialized SQLite runtime",
            message: "initialized migration handle identity does not match build request"
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
            operation: "authorize initialized SQLite migration",
            message: error.to_string(),
        })?;
    let connection = crate::db::engine::Connection::attach(handle);
    match &request.binding.shard_id.scope {
        StoreShardScopeV1::Code { .. }
        | StoreShardScopeV1::ProfileMemory
        | StoreShardScopeV1::Project { .. } => {
            crate::db::migrations::create_schema_connection(&connection)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "create initialized graph schema",
                    message: error.to_string(),
                })?;
        }
        StoreShardScopeV1::Profile
        | StoreShardScopeV1::ProfileSessions
        | StoreShardScopeV1::ProjectSessions { .. } => {
            crate::ports::registered_schema::ensure_registered_schema(&connection)
                .await
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "create initialized global/session schema",
                    message: error.to_string(),
                })?;
        }
    }
    Ok(true)
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
