use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

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
    StoreRuntimeRegistryFailure,
};
use crate::daemon::store_runtime::shard::{ShardRuntime, ShardRuntimeError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedStoreLocator {
    verified: VerifiedStoreLocatorV1,
    path: PathBuf,
}

impl ResolvedStoreLocator {
    pub(in crate::daemon::store_runtime) fn new(
        verified: VerifiedStoreLocatorV1,
        path: PathBuf,
    ) -> Self {
        Self { verified, path }
    }

    pub(crate) fn verified(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(super) fn matches(&self, key: &StoreRuntimeKey) -> bool {
        self.verified.shard_id == *key.shard_id() && self.verified.incarnation == key.incarnation()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeLocatorRecord {
    key: StoreRuntimeKey,
    locator: ResolvedStoreLocator,
}

impl RuntimeLocatorRecord {
    pub(super) fn new(key: StoreRuntimeKey, locator: ResolvedStoreLocator) -> Self {
        Self { key, locator }
    }

    pub(crate) fn key(&self) -> &StoreRuntimeKey {
        &self.key
    }

    pub(crate) fn verified(&self) -> &VerifiedStoreLocatorV1 {
        self.locator.verified()
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.locator.path()
    }
}

pub(crate) type StoreRuntimeRegistryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait StoreRuntimeResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>;
}

pub(crate) trait ShardRuntimePublisher: Send + Sync {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LifecycleShardRuntimePublisher;

impl ShardRuntimePublisher for LifecycleShardRuntimePublisher {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            let admission = AdmissionConfigV1::default();
            let physical: Arc<dyn PhysicalRuntimeAttachment> =
                if let StoreShardScopeV1::Code { .. } = &request.binding.shard_id.scope {
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
                    let attachment = GraphPhysicalAttachmentFactory
                        .attach(&locator, admission)
                        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                            operation: "attach rusqlite graph runtime",
                            message: error.to_string(),
                        })?;
                    Arc::new(attachment)
                } else {
                    let attachment = RepositoryPhysicalAttachmentFactory
                        .attach(
                            request.binding.clone(),
                            request.locator.verified().clone(),
                            request.locator.path().to_path_buf(),
                            admission,
                        )
                        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                            operation: "attach rusqlite repository runtime",
                            message: error.to_string(),
                        })?;
                    Arc::new(attachment)
                };
            let pinned_profile =
                matches!(request.binding.shard_id.scope, StoreShardScopeV1::Profile);
            let runtime = Arc::new(ShardRuntime::new(request.binding, pinned_profile));
            runtime
                .transition(RuntimeMaintenanceStateV1::Opening)
                .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
                .map_err(runtime_lifecycle_failure)?;
            Ok(PublishedShardRuntime::new(runtime, physical))
        })
    }
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

    fn drain(&self) -> Result<(), String> {
        GraphRuntimePhysicalAttachment::drain(self)
    }

    fn close_and_join(&self) -> Result<(), String> {
        GraphRuntimePhysicalAttachment::close_and_join(self)
    }

    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> StoreRuntimeRegistryFuture<'_, Result<RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            GraphRuntimePhysicalAttachment::dispatch_submit(self, request, probe)
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

    fn drain(&self) -> Result<(), String> {
        RepositoryRuntimePhysicalAttachment::drain(self)
    }

    fn close_and_join(&self) -> Result<(), String> {
        RepositoryRuntimePhysicalAttachment::close_and_join(self)
    }

    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> StoreRuntimeRegistryFuture<'_, Result<RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            RepositoryRuntimePhysicalAttachment::dispatch_submit(self, request, probe)
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

#[derive(Clone, Debug)]
pub(crate) struct ShardRuntimeBuildRequest {
    pub(super) binding: StoreRuntimeBindingV1,
    locator: RuntimeLocatorRecord,
}

impl ShardRuntimeBuildRequest {
    pub(super) fn new(binding: StoreRuntimeBindingV1, locator: RuntimeLocatorRecord) -> Self {
        Self { binding, locator }
    }

    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub(crate) fn locator(&self) -> &RuntimeLocatorRecord {
        &self.locator
    }
}

fn runtime_lifecycle_failure(error: ShardRuntimeError) -> StoreRuntimeRegistryFailure {
    StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
        message: error.to_string(),
    }
}
