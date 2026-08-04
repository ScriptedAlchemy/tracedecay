//! Driver-neutral ownership seam for one physical shard runtime.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_store::{
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1,
};

use super::StoreRuntimeRegistryFailure;

/// Bounded, path-free facts sampled from the physical writer/read runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhysicalRuntimeSnapshot {
    pub healthy: bool,
    pub writer_present: bool,
    pub reader_handles: u32,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
}

impl PhysicalRuntimeSnapshot {
    pub const fn is_drained(self) -> bool {
        !self.writer_present
            && self.reader_handles == 0
            && self.queued_operations == 0
            && self.queued_bytes == 0
    }
}

/// Opaque owner of driver resources. Implementations live behind the daemon
/// boundary and must not expose a connection or a driver-specific type.
pub trait PhysicalRuntimeAttachment: Send + Sync {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot;

    /// Physical identity captured from a descriptor held across `SQLite` worker
    /// startup. Implementations must not derive this from a later pathname
    /// stat.
    fn opened_file_identity(&self) -> Result<u64, String> {
        Err("physical runtime has no opened SQLite file identity".to_owned())
    }

    /// Stops admission and drains writer/read work. Returning success promises
    /// that a following snapshot has no writer, readers, or queued work.
    fn drain(&self) -> Result<(), String>;

    /// Closes all physical handles and joins owned workers. Called exactly once
    /// by the registry after a successful drain has been verified.
    fn close_and_join(&self) -> Result<(), String>;

    /// Exact SQL channel bound to this already-verified attachment.
    ///
    /// Implementations must return a handle over their owned writer/readers;
    /// they must never reopen the locator path.
    fn exact_sql_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, String> {
        Err("physical runtime has no exact SQL channel".to_owned())
    }

    /// Reads the retained attachment's bounded reserved-health telemetry.
    ///
    /// Implementations must use their already-open reader pool. Reopening the
    /// locator path or accepting caller-provided SQL is forbidden.
    fn storage_page_counts(&self, reader_wait: Duration) -> Result<(u64, u64, u64), String> {
        let _ = reader_wait;
        Err("physical runtime has no store-size telemetry port".to_owned())
    }

    /// Runs one typed, page-bounded incremental compaction on the retained
    /// writer. The authority is sampled by the writer at admission, dequeue,
    /// and before commit; no SQL or path crosses this port.
    fn run_bounded_incremental_compaction<'a>(
        &'a self,
        max_pages: u32,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreRuntimeRegistryFailure>> + Send + 'a>> {
        let _ = (max_pages, authority);
        Box::pin(async {
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "run bounded incremental compaction",
                message: "physical runtime has no typed compaction port".to_owned(),
            })
        })
    }

    fn run_checkpoint<'a>(
        &'a self,
        request: tracedecay_rusqlite_runtime::CheckpointRequest,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        tracedecay_rusqlite_runtime::CheckpointOutcome,
                        StoreRuntimeRegistryFailure,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let _ = (request, authority);
        Box::pin(async {
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "run checkpoint",
                message: "physical runtime has no typed checkpoint port".to_owned(),
            })
        })
    }

    fn snapshot_to<'a>(
        &'a self,
        destination: PathBuf,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        tracedecay_rusqlite_runtime::OnlineBackupReceipt,
                        StoreRuntimeRegistryFailure,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let _ = (destination, authority);
        Box::pin(async {
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "snapshot database",
                message: "physical runtime has no typed online-backup port".to_owned(),
            })
        })
    }

    fn dispatch_submit<'a>(
        &'a self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn tracedecay_rusqlite_runtime::RuntimeWriteAuthority>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure>>
                + Send
                + 'a,
        >,
    > {
        let _ = (request, probe, authority);
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

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(1)
    }

    fn drain(&self) -> Result<(), String> {
        Ok(())
    }

    fn close_and_join(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The publisher's atomic result: logical lifecycle plus physical lifetime.
pub struct PublishedShardRuntime {
    runtime: Arc<crate::store_runtime::shard::ShardRuntime>,
    attachment: Arc<dyn PhysicalRuntimeAttachment>,
    schema_migrated: bool,
}

impl PublishedShardRuntime {
    pub fn new(
        runtime: Arc<crate::store_runtime::shard::ShardRuntime>,
        attachment: Arc<dyn PhysicalRuntimeAttachment>,
    ) -> Self {
        Self {
            runtime,
            attachment,
            schema_migrated: false,
        }
    }

    pub(crate) fn new_with_schema_migration(
        runtime: Arc<crate::store_runtime::shard::ShardRuntime>,
        attachment: Arc<dyn PhysicalRuntimeAttachment>,
        schema_migrated: bool,
    ) -> Self {
        Self {
            runtime,
            attachment,
            schema_migrated,
        }
    }

    pub fn logical(&self) -> &Arc<crate::store_runtime::shard::ShardRuntime> {
        &self.runtime
    }

    pub fn opened_file_identity(&self) -> Result<u64, String> {
        self.attachment.opened_file_identity()
    }

    pub const fn schema_migrated(&self) -> bool {
        self.schema_migrated
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        Arc<crate::store_runtime::shard::ShardRuntime>,
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
