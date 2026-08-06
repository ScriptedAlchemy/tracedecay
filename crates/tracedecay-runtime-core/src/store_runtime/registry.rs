//! Canonical daemon registry for store runtimes.
//!
//! Entries are keyed only by typed shard identity and incarnation. Locator
//! resolution starts after an opening entry wins singleflight, and publication
//! retains exactly one concrete [`ShardRuntime`] for that binding.
//!
//! Dead-code allowance lives on the parent `store_runtime` module until every
//! live open routes through this registry.

#![allow(unused_imports)] // Re-exports remain the registry's crate-visible API surface.

mod attachment;
mod capacity;
mod close;
mod destructive;
mod leases;
mod open;
mod ports;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;
use tracedecay_store::{
    AdmissionConfigV1, RuntimeMaintenanceStateV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreRuntimeRegistryPublicationV1, StoreShardIdV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

use super::shard::ShardRuntime;
use super::telemetry::{RuntimeRegistryInventory, RuntimeRegistryInventoryEntry};

#[cfg(test)]
pub(crate) use attachment::EmptyPhysicalRuntimeAttachment;
pub use attachment::{PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot, PublishedShardRuntime};
pub use capacity::StoreRuntimeRegistryConfig;
pub(crate) use capacity::{DEFAULT_PROJECT_CODE_OPEN_RUNTIMES, MAX_PROJECT_CODE_OPEN_RUNTIMES};
pub use close::ClosedStoreRuntime;
pub use destructive::{DestructiveMaintenanceReservation, DestructiveMaintenanceTarget};
pub use leases::{
    ProfileAuthorityPin, ProfileAuthorityPinResult, StoreRuntimeAccessMode,
    StoreRuntimeLeaseAcquireResult, StoreRuntimeOpenMode, StoreRuntimeOpenRequest,
};
pub use open::StoreRuntimeOpenResult;
pub(crate) use open::{StoreRuntimeOpenBegin, StoreRuntimeOpenJoin};
pub use ports::{
    LifecycleShardRuntimePublisher, ResolvedStoreLocator, RuntimeLocatorRecord,
    ShardRuntimeBuildRequest, ShardRuntimePublisher, StoreRuntimeRegistryFuture,
    StoreRuntimeResolver,
};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreRuntimeKey {
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
}

impl StoreRuntimeKey {
    pub fn new(shard_id: StoreShardIdV1, incarnation: StoreIncarnationV1) -> Self {
        Self {
            shard_id,
            incarnation,
        }
    }

    pub fn shard_id(&self) -> &StoreShardIdV1 {
        &self.shard_id
    }

    pub const fn incarnation(&self) -> StoreIncarnationV1 {
        self.incarnation
    }

    fn from_binding(binding: &StoreRuntimeBindingV1) -> Self {
        Self::new(binding.shard_id.clone(), binding.incarnation)
    }

    fn is_profile(&self) -> bool {
        matches!(self.shard_id.scope, StoreShardScopeV1::Profile)
    }

    fn is_project_code_capacity_exempt(&self) -> bool {
        !matches!(self.shard_id.scope, StoreShardScopeV1::Code { .. })
    }
}

#[derive(Clone)]
pub struct StoreRuntimeHandle {
    inner: Arc<StoreRuntimeHandleInner>,
}

struct StoreRuntimeHandleInner {
    publication: StoreRuntimeRegistryPublicationV1,
    runtime: Arc<ShardRuntime>,
    attachment: Arc<dyn PhysicalRuntimeAttachment>,
    locator: RuntimeLocatorRecord,
    opened_file_identity: u64,
    schema_migrated: bool,
    database_authority: Option<crate::db::DatabaseAuthority>,
}

struct RuntimeDatabaseWriteAuthority {
    authority: crate::db::DatabaseAuthority,
    canonical_path: PathBuf,
    opened_file_identity: u64,
}

impl RuntimeDatabaseWriteAuthority {
    fn verify_database(&self, intent: &str) -> Result<(), String> {
        self.authority
            .require_active_write_scope(intent)
            .map_err(|error| error.to_string())?;
        let current_file_identity = crate::db::sqlite_generation_identity(&self.canonical_path)
            .map_err(|_| "could not verify the registered SQLite file identity".to_owned())?;
        if current_file_identity != self.opened_file_identity {
            return Err("database file identity changed after registry attachment".to_owned());
        }
        Ok(())
    }
}

impl tracedecay_rusqlite_runtime::RuntimeWriteAuthority for RuntimeDatabaseWriteAuthority {
    fn verify(
        &self,
        stage: tracedecay_rusqlite_runtime::RuntimeWriteAuthorityStage,
    ) -> Result<(), tracedecay_rusqlite_runtime::RuntimeWriteAuthorityError> {
        let intent = match stage {
            tracedecay_rusqlite_runtime::RuntimeWriteAuthorityStage::BeforeAdmission => {
                "admit registered runtime write"
            }
            tracedecay_rusqlite_runtime::RuntimeWriteAuthorityStage::Dequeued => {
                "dequeue registered runtime write"
            }
            tracedecay_rusqlite_runtime::RuntimeWriteAuthorityStage::BeforeCommit => {
                "commit registered runtime write"
            }
        };
        self.verify_database(intent)
            .map_err(tracedecay_rusqlite_runtime::RuntimeWriteAuthorityError::denied)
    }
}

impl tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteAuthority
    for RuntimeDatabaseWriteAuthority
{
    fn verify(
        &self,
        intent: tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent,
    ) -> Result<(), tracedecay_rusqlite_runtime::exact_sql::ExactSqlError> {
        let intent = match intent {
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::Validate => {
                "validate registered exact SQL statement"
            }
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::Execute => {
                "execute registered exact SQL statement"
            }
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::Query => {
                "query registered exact SQL writer"
            }
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::ExecuteBatch => {
                "execute registered exact SQL statement batch"
            }
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::Vacuum => {
                if self.authority.role() != crate::db::DatabaseAuthorityRole::Maintenance {
                    return Err(
                        tracedecay_rusqlite_runtime::exact_sql::ExactSqlError::AuthorityDenied(
                            "whole-database vacuum requires exclusive maintenance authority"
                                .to_owned(),
                        ),
                    );
                }
                "vacuum registered database under exclusive maintenance"
            }
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::BeginTransaction => {
                "begin registered exact SQL transaction"
            }
            tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteIntent::Commit => {
                "commit registered exact SQL transaction"
            }
        };
        self.verify_database(intent)
            .map_err(tracedecay_rusqlite_runtime::exact_sql::ExactSqlError::AuthorityDenied)
    }
}

impl StoreRuntimeHandle {
    pub fn publication(&self) -> &StoreRuntimeRegistryPublicationV1 {
        &self.inner.publication
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.inner.publication.binding
    }

    pub fn runtime(&self) -> &Arc<ShardRuntime> {
        &self.inner.runtime
    }

    pub fn locator(&self) -> &RuntimeLocatorRecord {
        &self.inner.locator
    }

    /// Canonical path of the attached database.
    pub fn canonical_path(&self) -> &std::path::Path {
        self.locator().path()
    }

    /// Verified locator this attachment was published against.
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.locator().verified()
    }

    /// Whether the physical attachment currently holds a writer.
    pub fn writer_present(&self) -> bool {
        self.physical_snapshot().writer_present
    }

    /// Stable identity of the underlying physical runtime, so two facades can
    /// be compared for attachment sharing.
    pub fn runtime_identity(&self) -> usize {
        Arc::as_ptr(self.runtime()).cast::<()>() as usize
    }

    pub fn opened_file_identity(&self) -> Option<u64> {
        Some(self.inner.opened_file_identity)
    }

    pub fn schema_migrated(&self) -> bool {
        self.inner.schema_migrated
    }

    pub fn database_authority(
        &self,
        operation: &'static str,
    ) -> Result<crate::db::DatabaseAuthority, StoreRuntimeRegistryFailure> {
        let authority = self.inner.database_authority.clone().ok_or_else(|| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: "registered runtime has no originating database authority".to_owned(),
            }
        })?;
        self.validate_database_write_authority(&authority, operation)?;
        Ok(authority)
    }

    pub(crate) fn validate_registered_read(
        &self,
        operation: &'static str,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        self.validate_opened_file_identity(operation).map(|_| ())
    }

    pub(crate) fn physical_snapshot(&self) -> PhysicalRuntimeSnapshot {
        self.inner.attachment.snapshot()
    }

    pub fn storage_page_counts(
        &self,
        reader_wait: Duration,
    ) -> Result<(u64, u64, u64), StoreRuntimeRegistryFailure> {
        self.validate_opened_file_identity("authorize registered store-size telemetry")?;
        let counts = self
            .inner
            .attachment
            .storage_page_counts(reader_wait)
            .map_err(
                |message| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "read registered store-size telemetry",
                    message,
                },
            )?;
        self.validate_opened_file_identity("complete registered store-size telemetry")?;
        Ok(counts)
    }

    pub async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let max_pages = u32::try_from(max_pages).map_err(|_| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "run bounded incremental compaction",
                message: "incremental compaction page limit exceeds u32".to_owned(),
            }
        })?;
        if max_pages == 0 {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "run bounded incremental compaction",
                message: "incremental compaction page limit must be positive".to_owned(),
            });
        }
        let opened_file_identity = self.validate_database_write_authority(
            &authority,
            "authorize registered incremental compaction",
        )?;
        let authority = Arc::new(RuntimeDatabaseWriteAuthority {
            canonical_path: authority.canonical_database_path().to_path_buf(),
            authority,
            opened_file_identity,
        });
        self.inner
            .attachment
            .run_bounded_incremental_compaction(max_pages, authority)
            .await?;
        self.validate_opened_file_identity("complete registered incremental compaction")?;
        Ok(())
    }

    pub async fn run_checkpoint(
        &self,
        request: tracedecay_rusqlite_runtime::CheckpointRequest,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<tracedecay_rusqlite_runtime::CheckpointOutcome, StoreRuntimeRegistryFailure> {
        let opened_file_identity =
            self.validate_database_write_authority(&authority, "authorize registered checkpoint")?;
        let authority = Arc::new(RuntimeDatabaseWriteAuthority {
            canonical_path: authority.canonical_database_path().to_path_buf(),
            authority,
            opened_file_identity,
        });
        let outcome = self
            .inner
            .attachment
            .run_checkpoint(request, authority)
            .await?;
        self.validate_opened_file_identity("complete registered checkpoint")?;
        Ok(outcome)
    }

    pub async fn snapshot_to(
        &self,
        destination: PathBuf,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, StoreRuntimeRegistryFailure> {
        let opened_file_identity = self
            .validate_database_write_authority(&authority, "authorize registered online backup")?;
        let authority = Arc::new(RuntimeDatabaseWriteAuthority {
            canonical_path: authority.canonical_database_path().to_path_buf(),
            authority,
            opened_file_identity,
        });
        let receipt = self
            .inner
            .attachment
            .snapshot_to(destination, authority)
            .await?;
        self.validate_opened_file_identity("complete registered online backup")?;
        Ok(receipt)
    }

    fn exact_sql_handle_unchecked(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, StoreRuntimeRegistryFailure>
    {
        self.inner.attachment.exact_sql_handle().map_err(|message| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "attach exact SQL channel",
                message,
            }
        })
    }

    /// Returns a writerless channel for bounded health and telemetry reads.
    ///
    /// This capability does not accept `DatabaseAuthority` and cannot recover
    /// the attachment's writer sender through `with_write_authority`.
    pub fn telemetry_read_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, StoreRuntimeRegistryFailure>
    {
        self.validate_opened_file_identity("authorize registered telemetry read")?;
        let handle = self.exact_sql_handle_unchecked()?;
        if handle.binding() != self.binding() {
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(self.binding().clone()),
                actual: Box::new(handle.binding().clone()),
            });
        }
        if handle.verified_locator() != self.locator().verified() {
            return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                key: Box::new(self.locator().key().clone()),
                locator: Box::new(handle.verified_locator().clone()),
            });
        }
        Ok(handle.read_only_clone())
    }

    pub fn authorized_exact_sql_handle(
        &self,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, StoreRuntimeRegistryFailure>
    {
        use tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteAuthority;

        authority
            .require_active_write_scope("authorize registered SQLite runtime")
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "authorize exact SQL channel",
                message: error.to_string(),
            })?;
        if authority.canonical_database_path() != self.locator().path() {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "authorize exact SQL channel",
                message: format!(
                    "registered locator {} does not match database authority {}",
                    self.locator().path().display(),
                    authority.canonical_database_path().display()
                ),
            });
        }

        let handle = self.exact_sql_handle_unchecked()?;
        if handle.binding() != self.binding() {
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(self.binding().clone()),
                actual: Box::new(handle.binding().clone()),
            });
        }
        if handle.verified_locator() != self.locator().verified() {
            return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                key: Box::new(self.locator().key().clone()),
                locator: Box::new(handle.verified_locator().clone()),
            });
        }

        let opened_file_identity =
            self.validate_opened_file_identity("authorize exact SQL channel")?;

        let authority = RuntimeDatabaseWriteAuthority {
            canonical_path: authority.canonical_database_path().to_path_buf(),
            authority,
            opened_file_identity,
        };
        handle
            .with_write_authority(Arc::new(authority) as Arc<dyn ExactSqlWriteAuthority>)
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "authorize exact SQL channel",
                message: error.to_string(),
            })
    }

    pub async fn dispatch_submit_authorized(
        &self,
        request: tracedecay_store::RuntimeSubmitRequestV1,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<tracedecay_store::RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure> {
        if request.binding() != self.binding() {
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(self.binding().clone()),
                actual: Box::new(request.binding().clone()),
            });
        }
        let opened_file_identity = self
            .validate_database_write_authority(&authority, "authorize registered runtime write")?;
        let authority = Arc::new(RuntimeDatabaseWriteAuthority {
            canonical_path: authority.canonical_database_path().to_path_buf(),
            authority,
            opened_file_identity,
        });
        self.inner
            .attachment
            .dispatch_submit(request, probe, authority)
            .await
    }

    fn validate_database_write_authority(
        &self,
        authority: &crate::db::DatabaseAuthority,
        operation: &'static str,
    ) -> Result<u64, StoreRuntimeRegistryFailure> {
        authority
            .require_active_write_scope(operation)
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: error.to_string(),
            })?;
        if authority.canonical_database_path() != self.locator().path() {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: format!(
                    "registered locator {} does not match database authority {}",
                    self.locator().path().display(),
                    authority.canonical_database_path().display()
                ),
            });
        }
        self.validate_opened_file_identity(operation)
    }

    fn validate_opened_file_identity(
        &self,
        operation: &'static str,
    ) -> Result<u64, StoreRuntimeRegistryFailure> {
        // File identity is read authority, not write authority. Writable entry
        // points revalidate the retained capability in
        // `validate_database_write_authority`; read-only facades must remain
        // usable after that writer scope is revoked.
        let current_file_identity = crate::db::sqlite_generation_identity(self.locator().path())
            .map_err(|_| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: "could not verify the registered SQLite file identity".to_owned(),
            })?;
        let opened_file_identity = self.inner.opened_file_identity;
        if current_file_identity != opened_file_identity {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: "database file identity changed after registry attachment".to_owned(),
            });
        }
        Ok(opened_file_identity)
    }

    pub fn dispatch_read(
        &self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        if request.binding() != self.binding() {
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(self.binding().clone()),
                actual: Box::new(request.binding().clone()),
            });
        }
        self.validate_opened_file_identity("authorize registered runtime read")?;
        self.inner.attachment.dispatch_read(request, probe)
    }

    fn is_exclusively_held_by_registry(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }
}

impl tracedecay_store::StorageRuntimeReadPort for StoreRuntimeHandle {
    fn dispatch_read<'a>(
        &'a self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &'a dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> tracedecay_store::StorageRuntimePortFutureV1<'a, tracedecay_store::RuntimeReadOutcomeV1>
    {
        Box::pin(async move {
            StoreRuntimeHandle::dispatch_read(self, request, probe).map_err(|_| {
                tracedecay_store::StorageRuntimeErrorV1::Infrastructure {
                    operation: "dispatch registered runtime read".to_owned(),
                }
                .into()
            })
        })
    }
}

impl fmt::Debug for StoreRuntimeHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntimeHandle")
            .field("publication", &self.inner.publication)
            .field("locator_key", self.inner.locator.key())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRuntimeRegistryFailure {
    InvalidProjectCodeBudget {
        requested: usize,
        maximum: usize,
    },
    ProjectCodeBudgetExhausted {
        limit: usize,
    },
    RuntimeEvictionInProgress {
        key: Box<StoreRuntimeKey>,
    },
    DatabaseRuntimeIdentityConflict {
        requested: Box<StoreRuntimeKey>,
        retained: Box<StoreRuntimeKey>,
        path: PathBuf,
    },
    DestructiveMaintenanceInProgress {
        root: PathBuf,
    },
    DestructiveMaintenanceInvalidTarget {
        message: String,
    },
    RuntimeCloseBlocked {
        binding: Box<StoreRuntimeBindingV1>,
        external_handles: usize,
        external_runtime_references: usize,
        client_leases: u32,
    },
    AuthorityEpochExhausted,
    OpenAttemptExhausted,
    EvictionAttemptExhausted,
    EvictionReservationLost {
        key: Box<StoreRuntimeKey>,
    },
    PublicationIdExhausted,
    ResolverFailed {
        message: String,
    },
    UnsupportedShardScope,
    NetworkFilesystemUnavailable {
        filesystem_type: String,
    },
    FilesystemLocalityUnavailable {
        filesystem_type: String,
    },
    LocatorIdentityMismatch {
        key: Box<StoreRuntimeKey>,
        locator: Box<VerifiedStoreLocatorV1>,
    },
    RuntimeBindingMismatch {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    RuntimeLifecycleFailed {
        message: String,
    },
    PhysicalRuntimeFailed {
        operation: &'static str,
        message: String,
    },
    PhysicalRuntimeNotDrained {
        snapshot: PhysicalRuntimeSnapshot,
    },
    ProfileAuthorityRequired {
        key: Box<StoreRuntimeKey>,
    },
    ProfileAuthorityMustNotBeSupplied {
        key: Box<StoreRuntimeKey>,
    },
    ProfileAuthorityShardMismatch {
        key: Box<StoreRuntimeKey>,
        pin: Box<StoreRuntimeBindingV1>,
    },
    ProfileAuthorityNotPinned {
        profile_shard: Box<StoreShardIdV1>,
    },
    ProfileAuthorityFenced {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    ProfileAuthorityUnavailable {
        binding: Box<StoreRuntimeBindingV1>,
        state: RuntimeMaintenanceStateV1,
    },
    ProfileAuthorityShardIsNotProfile {
        shard_id: Box<StoreShardIdV1>,
    },
    InvalidLease {
        message: String,
    },
    LeaseBindingMismatch {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    LeaseRejected {
        message: String,
    },
    OpenTaskAbandoned {
        key: Box<StoreRuntimeKey>,
    },
}

#[derive(Clone, Debug)]
pub enum StoreRuntimeLookup {
    Ready(StoreRuntimeHandle),
    Opening {
        key: Box<StoreRuntimeKey>,
    },
    Evicting {
        key: Box<StoreRuntimeKey>,
    },
    Missing {
        key: Box<StoreRuntimeKey>,
    },
    WrongIncarnation {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    Fenced {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
}

struct ReadyRuntime {
    handle: StoreRuntimeHandle,
}

struct EvictingRuntime {
    attempt: u64,
    handle: StoreRuntimeHandle,
}

struct DestructivePathReservation {
    root: PathBuf,
    database_paths: Vec<PathBuf>,
    released: tokio::sync::watch::Sender<bool>,
}

enum RegistryEntry {
    Opening(open::OpeningRuntime),
    Ready(ReadyRuntime),
    Evicting(EvictingRuntime),
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreRuntimeKey, RegistryEntry>,
    destructive_paths: BTreeMap<u64, DestructivePathReservation>,
    profile_authorities: BTreeMap<StoreShardIdV1, StoreRuntimeBindingV1>,
    next_destructive_attempt: u64,
    next_open_attempt: u64,
    next_eviction_attempt: u64,
    next_publication: u64,
}

struct StoreRuntimeRegistryInner {
    resolver: Arc<dyn StoreRuntimeResolver>,
    publisher: Arc<dyn ShardRuntimePublisher>,
    config: StoreRuntimeRegistryConfig,
    state: Mutex<RegistryState>,
}

#[derive(Clone)]
pub struct StoreRuntimeRegistry {
    inner: Arc<StoreRuntimeRegistryInner>,
}

impl StoreRuntimeRegistry {
    pub fn new(
        resolver: Arc<dyn StoreRuntimeResolver>,
        publisher: Arc<dyn ShardRuntimePublisher>,
    ) -> Self {
        Self {
            inner: Arc::new(StoreRuntimeRegistryInner {
                resolver,
                publisher,
                config: StoreRuntimeRegistryConfig::default(),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    pub fn with_config(
        resolver: Arc<dyn StoreRuntimeResolver>,
        publisher: Arc<dyn ShardRuntimePublisher>,
        config: StoreRuntimeRegistryConfig,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        Self::with_config_and_authority_epoch_floor(resolver, publisher, config, None)
    }

    pub(crate) fn with_config_and_authority_epoch_floor(
        resolver: Arc<dyn StoreRuntimeResolver>,
        publisher: Arc<dyn ShardRuntimePublisher>,
        config: StoreRuntimeRegistryConfig,
        authority_epoch_floor: Option<StoreAuthorityEpochV1>,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        config.validate()?;
        if let Some(floor) = authority_epoch_floor {
            open::retain_authority_epoch_floor(floor);
        }
        Ok(Self {
            inner: Arc::new(StoreRuntimeRegistryInner {
                resolver,
                publisher,
                config,
                state: Mutex::new(RegistryState::default()),
            }),
        })
    }

    pub fn lookup(&self, expected: &StoreRuntimeBindingV1) -> StoreRuntimeLookup {
        let key = StoreRuntimeKey::from_binding(expected);
        let state = self.lock_state();
        match state.entries.get(&key) {
            Some(RegistryEntry::Ready(ready)) => {
                let actual = ready.handle.binding();
                if actual.authority_epoch == expected.authority_epoch {
                    StoreRuntimeLookup::Ready(ready.handle.clone())
                } else {
                    StoreRuntimeLookup::Fenced {
                        expected: Box::new(expected.clone()),
                        actual: Box::new(actual.clone()),
                    }
                }
            }
            Some(RegistryEntry::Opening(_)) => StoreRuntimeLookup::Opening { key: Box::new(key) },
            Some(RegistryEntry::Evicting(_)) => StoreRuntimeLookup::Evicting { key: Box::new(key) },
            None => state
                .entries
                .iter()
                .find_map(|(candidate, entry)| {
                    (candidate.shard_id == expected.shard_id
                        && candidate.incarnation != expected.incarnation)
                        .then(|| match entry {
                            RegistryEntry::Ready(ready) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(ready.handle.binding().clone()),
                                })
                            }
                            RegistryEntry::Opening(_) => None,
                            RegistryEntry::Evicting(evicting) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(evicting.handle.binding().clone()),
                                })
                            }
                        })
                        .flatten()
                })
                .unwrap_or(StoreRuntimeLookup::Missing { key: Box::new(key) }),
        }
    }

    /// Returns an exact already-published runtime for a read-only facade.
    ///
    /// This does not revalidate or expose its retained writer authority. Any
    /// later write still enters the ordinary actor-time authority gates.
    pub fn retained_runtime_for_read(&self, key: &StoreRuntimeKey) -> Option<StoreRuntimeHandle> {
        let state = self.lock_state();
        match state.entries.get(key) {
            Some(RegistryEntry::Ready(ready)) => Some(ready.handle.clone()),
            Some(RegistryEntry::Opening(_) | RegistryEntry::Evicting(_)) | None => None,
        }
    }

    pub fn inventory(
        &self,
        admission: AdmissionConfigV1,
        global_queued_bytes: u64,
    ) -> RuntimeRegistryInventory {
        let handles = {
            let state = self.lock_state();
            state
                .entries
                .values()
                .filter_map(|entry| match entry {
                    RegistryEntry::Ready(ready) => Some(ready.handle.clone()),
                    RegistryEntry::Opening(_) => None,
                    RegistryEntry::Evicting(evicting) => Some(evicting.handle.clone()),
                })
                .collect::<Vec<_>>()
        };
        let entries = handles
            .into_iter()
            .map(|handle| {
                let mut observation = handle.runtime().observe(self.inner.config.eviction_idle());
                let physical = handle.physical_snapshot();
                observation.health.writer_present |= physical.writer_present;
                observation.health.queued_operations = observation
                    .health
                    .queued_operations
                    .saturating_add(physical.queued_operations);
                observation.health.queued_bytes = observation
                    .health
                    .queued_bytes
                    .saturating_add(physical.queued_bytes);
                observation.health.wal_bytes = physical.wal_bytes;
                observation.health.memory_estimate_bytes = physical.memory_estimate_bytes;
                if !physical.healthy
                    && observation.health.health != super::shard::ShardRuntimeHealth::Faulted
                {
                    observation.health.health = super::shard::ShardRuntimeHealth::Degraded;
                }
                let mut entry = RuntimeRegistryInventoryEntry::from(observation);
                entry.physical = physical;
                entry
            })
            .collect();
        RuntimeRegistryInventory {
            admission,
            global_queued_bytes,
            entries,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn utc_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}
