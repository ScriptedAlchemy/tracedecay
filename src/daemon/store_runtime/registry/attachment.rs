//! Driver-neutral ownership seam for one physical shard runtime.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_store::{
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1,
};

use super::StoreRuntimeRegistryFailure;

/// Bounded, path-free facts sampled from the physical writer/read runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PhysicalRuntimeSnapshot {
    pub(crate) healthy: bool,
    pub(crate) writer_present: bool,
    pub(crate) reader_handles: u32,
    pub(crate) queued_operations: u32,
    pub(crate) queued_bytes: u64,
    pub(crate) wal_bytes: u64,
    pub(crate) memory_estimate_bytes: u64,
}

impl PhysicalRuntimeSnapshot {
    pub(crate) const fn is_drained(self) -> bool {
        !self.writer_present
            && self.reader_handles == 0
            && self.queued_operations == 0
            && self.queued_bytes == 0
    }
}

/// Opaque owner of driver resources. Implementations live behind the daemon
/// boundary and must not expose a connection or a driver-specific type.
pub(crate) trait PhysicalRuntimeAttachment: Send + Sync {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot;

    /// Stops admission and drains writer/read work. Returning success promises
    /// that a following snapshot has no writer, readers, or queued work.
    fn drain(&self) -> Result<(), String>;

    /// Closes all physical handles and joins owned workers. Called exactly once
    /// by the registry after a successful drain has been verified.
    fn close_and_join(&self) -> Result<(), String>;

    fn dispatch_submit<'a>(
        &'a self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure>>
                + Send
                + 'a,
        >,
    > {
        let _ = (request, probe);
        Box::pin(async {
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "dispatch submit",
                message: "physical runtime has no write data port".to_owned(),
            })
        })
    }

    fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        let _ = (request, probe);
        Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "dispatch read",
            message: "physical runtime has no read data port".to_owned(),
        })
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct EmptyPhysicalRuntimeAttachment;

#[cfg(test)]
impl PhysicalRuntimeAttachment for EmptyPhysicalRuntimeAttachment {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        PhysicalRuntimeSnapshot {
            healthy: true,
            ..PhysicalRuntimeSnapshot::default()
        }
    }

    fn drain(&self) -> Result<(), String> {
        Ok(())
    }

    fn close_and_join(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The publisher's atomic result: logical lifecycle plus physical lifetime.
pub(crate) struct PublishedShardRuntime {
    runtime: Arc<crate::daemon::store_runtime::shard::ShardRuntime>,
    attachment: Arc<dyn PhysicalRuntimeAttachment>,
}

impl PublishedShardRuntime {
    pub(crate) fn new(
        runtime: Arc<crate::daemon::store_runtime::shard::ShardRuntime>,
        attachment: Arc<dyn PhysicalRuntimeAttachment>,
    ) -> Self {
        Self {
            runtime,
            attachment,
        }
    }

    pub(crate) fn logical(&self) -> &Arc<crate::daemon::store_runtime::shard::ShardRuntime> {
        &self.runtime
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        Arc<crate::daemon::store_runtime::shard::ShardRuntime>,
        Arc<dyn PhysicalRuntimeAttachment>,
    ) {
        (self.runtime, self.attachment)
    }
}

impl fmt::Debug for PublishedShardRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedShardRuntime")
            .field("binding", self.runtime.binding())
            .field("physical", &self.attachment.snapshot())
            .finish()
    }
}

pub(super) fn attachment_failure(
    operation: &'static str,
    message: impl Into<String>,
) -> StoreRuntimeRegistryFailure {
    StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
        operation,
        message: message.into(),
    }
}
