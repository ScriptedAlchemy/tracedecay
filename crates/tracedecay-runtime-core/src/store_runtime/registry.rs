//! Canonical daemon registry for store runtimes.
//!
//! Entries are keyed only by typed shard identity and incarnation. Locator
//! resolution starts after an opening entry wins singleflight, and publication
//! retains exactly one concrete [`ShardRuntime`] for that binding.
//!
//! Dead-code allowance lives on the parent `store_runtime` module until every
//! live open routes through this registry.
// The typed failure stays by-value at this boundary; `resolver.rs` documents
// the boxed alternative if the variant set grows further.
#![allow(clippy::result_large_err)]
#![allow(unused_imports)] // Re-exports remain the registry's crate-visible API surface.

mod attachment;
mod capacity;
mod close;
mod destructive;
mod graph;
mod leases;
mod open;
mod ports;
mod retirement;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
pub use attachment::{
    PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot, PhysicalWriterRuntimeSnapshot,
    PublishedShardRuntime,
};
pub use capacity::StoreRuntimeRegistryConfig;
pub(crate) use capacity::{DEFAULT_PROJECT_CODE_OPEN_RUNTIMES, MAX_PROJECT_CODE_OPEN_RUNTIMES};
pub use close::ClosedStoreRuntime;
pub use destructive::{DestructiveMaintenanceReservation, DestructiveMaintenanceTarget};
pub use graph::{CanonicalCodeGraphStoreLeaseV1, CanonicalGraphStoreLeaseV1};
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
pub use retirement::{
    StoreRuntimeRetirementBlocker, StoreRuntimeRetirementCommit, StoreRuntimeRetirementOutcome,
    StoreRuntimeRetirementReservation, StoreRuntimeRetirementResult, StoreRuntimeRetirementTarget,
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
pub struct StoreRuntimeClientLease {
    inner: Arc<StoreRuntimeClientLeaseToken>,
}

/// Exact identity of one database facade attachment within one registered
/// runtime publication. It is never derived from a lease count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DatabaseRuntimeAttachmentIdV1(u64);

/// Opaque identity of the daemon map owner that reserved a database facade
/// attachment for retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DatabaseRuntimeOwnerIdentityV1(u64);

/// Exact retirement reservation allocated for one database owner attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DatabaseRuntimeAttachmentReservationIdV1(u64);

/// Store-side identity carried by a database owner reservation. The database
/// layer owns rollback; Store uses this only to validate that the one map-owned
/// attachment was reclassified before it permits physical retirement.
#[derive(Clone)]
pub(crate) struct DatabaseRuntimeOwnerAttachmentReservationIdentityV1 {
    source: Arc<StoreRuntimeLeaseSource>,
    attachment_id: DatabaseRuntimeAttachmentIdV1,
    owner_id: DatabaseRuntimeOwnerIdentityV1,
    reservation_id: DatabaseRuntimeAttachmentReservationIdV1,
}

/// Opaque RAII bridge supplied by a database owner to Store retirement. Store
/// never reaches through it to the owner; dropping the bridge restores the
/// owner attachment unless `commit` has crossed the irreversible fence.
pub(crate) trait StoreRuntimeOwnerAttachmentRetirementReservationV1: Send {
    fn identity(&self) -> &DatabaseRuntimeOwnerAttachmentReservationIdentityV1;

    /// Runs every fallible reservation check before Store makes any target
    /// globally committing. A successful preflight makes `commit` exact.
    fn preflight_commit(&self) -> Result<(), StoreRuntimeRegistryFailure>;

    fn commit(&mut self) -> Result<(), StoreRuntimeRegistryFailure>;

    /// Once Store has crossed its commit fence, a failing exact commit remains
    /// terminal; it must never restore an owner attachment to `Ready`.
    fn terminalize_after_commit_failure(&mut self);
}

/// Opaque identity for the lifecycle allocation behind one client lease.
///
/// It can compare runtime allocations without retaining or exposing the
/// registry's `Arc<ShardRuntime>` ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StoreRuntimeLeaseIdentity(u64);

struct StoreRuntimeClientLeaseToken {
    source: Arc<StoreRuntimeLeaseSource>,
    _lifetime: super::shard::ShardRuntimeClientLifetimeLease,
}

/// One cloneable in-flight operation token issued beneath a client lease.
#[derive(Clone, Debug)]
pub struct StoreRuntimeOperationLease {
    _lifetime: super::shard::ShardRuntimeOperationLifetimeLease,
}

impl std::ops::Deref for StoreRuntimeClientLeaseToken {
    type Target = StoreRuntimeLeaseSource;

    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

struct StoreRuntimeLeaseSource {
    publication: StoreRuntimeRegistryPublicationV1,
    runtime: Arc<ShardRuntime>,
    attachment: Arc<dyn PhysicalRuntimeAttachment>,
    locator: RuntimeLocatorRecord,
    opened_file_identity: u64,
    database_authority: Option<crate::db::DatabaseAuthority>,
    database_attachments: Mutex<BTreeMap<DatabaseRuntimeAttachmentIdV1, DatabaseAttachmentState>>,
    next_database_attachment_id: AtomicU64,
    next_database_owner_id: AtomicU64,
    next_database_attachment_reservation_id: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseAttachmentState {
    Active {
        client_token: u64,
    },
    OwnerReserved {
        client_token: u64,
        owner_id: DatabaseRuntimeOwnerIdentityV1,
        reservation_id: DatabaseRuntimeAttachmentReservationIdV1,
    },
    Committed {
        client_token: u64,
        owner_id: DatabaseRuntimeOwnerIdentityV1,
        reservation_id: DatabaseRuntimeAttachmentReservationIdV1,
    },
}

impl StoreRuntimeLeaseSource {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.publication.binding
    }

    fn runtime(&self) -> &Arc<ShardRuntime> {
        &self.runtime
    }

    fn locator(&self) -> &RuntimeLocatorRecord {
        &self.locator
    }

    fn canonical_path(&self) -> &std::path::Path {
        self.locator.path()
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.locator.verified()
    }

    fn opened_file_identity(&self) -> Option<u64> {
        Some(self.opened_file_identity)
    }

    fn physical_snapshot(&self) -> PhysicalRuntimeSnapshot {
        self.attachment.snapshot()
    }

    fn writer_present(&self) -> bool {
        self.physical_snapshot().writer_present
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
        let current_file_identity = crate::db::sqlite_generation_identity(self.locator().path())
            .map_err(|_| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: "could not verify the registered SQLite file identity".to_owned(),
            })?;
        if current_file_identity != self.opened_file_identity {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation,
                message: "database file identity changed after registry attachment".to_owned(),
            });
        }
        Ok(self.opened_file_identity)
    }

    fn register_database_attachment(
        &self,
        client_token: u64,
    ) -> Result<DatabaseRuntimeAttachmentIdV1, StoreRuntimeRegistryFailure> {
        let id = DatabaseRuntimeAttachmentIdV1(allocate_database_attachment_counter(
            &self.next_database_attachment_id,
        )?);
        let previous = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, DatabaseAttachmentState::Active { client_token });
        if previous.is_some() {
            return Err(StoreRuntimeRegistryFailure::DatabaseAttachmentIdentityConflict);
        }
        Ok(id)
    }

    fn allocate_database_owner_identity(
        &self,
    ) -> Result<DatabaseRuntimeOwnerIdentityV1, StoreRuntimeRegistryFailure> {
        Ok(DatabaseRuntimeOwnerIdentityV1(
            allocate_database_attachment_counter(&self.next_database_owner_id)?,
        ))
    }

    fn reserve_database_attachment_for_owner(
        self: &Arc<Self>,
        attachment_id: DatabaseRuntimeAttachmentIdV1,
        owner_id: DatabaseRuntimeOwnerIdentityV1,
    ) -> Result<DatabaseRuntimeOwnerAttachmentReservationIdentityV1, StoreRuntimeRegistryFailure>
    {
        let reservation_id = DatabaseRuntimeAttachmentReservationIdV1(
            allocate_database_attachment_counter(&self.next_database_attachment_reservation_id)?,
        );
        let mut attachments = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = attachments.get_mut(&attachment_id) else {
            return Err(StoreRuntimeRegistryFailure::DatabaseAttachmentMissing {
                attachment_id: attachment_id.0,
            });
        };
        match state {
            DatabaseAttachmentState::Active { client_token } => {
                let client_token = *client_token;
                *state = DatabaseAttachmentState::OwnerReserved {
                    client_token,
                    owner_id,
                    reservation_id,
                };
            }
            DatabaseAttachmentState::OwnerReserved { .. }
            | DatabaseAttachmentState::Committed { .. } => {
                return Err(
                    StoreRuntimeRegistryFailure::DatabaseAttachmentAlreadyReserved {
                        attachment_id: attachment_id.0,
                    },
                );
            }
        }
        Ok(DatabaseRuntimeOwnerAttachmentReservationIdentityV1 {
            source: Arc::clone(self),
            attachment_id,
            owner_id,
            reservation_id,
        })
    }

    fn release_database_attachment(&self, attachment_id: DatabaseRuntimeAttachmentIdV1) {
        let _ = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&attachment_id);
    }

    fn restore_database_owner_attachment_reservation(
        &self,
        reservation: &DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        if !std::ptr::eq(self, Arc::as_ptr(&reservation.source)) {
            return Err(StoreRuntimeRegistryFailure::DatabaseAttachmentOwnerMismatch);
        }
        let mut attachments = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = attachments.get_mut(&reservation.attachment_id) else {
            return Err(
                StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost {
                    attachment_id: reservation.attachment_id.0,
                },
            );
        };
        if let DatabaseAttachmentState::OwnerReserved {
            client_token,
            owner_id,
            reservation_id,
        } = state
            && *owner_id == reservation.owner_id
            && *reservation_id == reservation.reservation_id
        {
            let client_token = *client_token;
            *state = DatabaseAttachmentState::Active { client_token };
            return match attachments.get(&reservation.attachment_id) {
                Some(DatabaseAttachmentState::Active { .. }) => Ok(()),
                _ => Err(
                    StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost {
                        attachment_id: reservation.attachment_id.0,
                    },
                ),
            };
        }
        Err(
            StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost {
                attachment_id: reservation.attachment_id.0,
            },
        )
    }

    fn commit_database_owner_attachment_reservation(
        &self,
        reservation: &DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let mut attachments = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = attachments.get_mut(&reservation.attachment_id) else {
            return Err(
                StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost {
                    attachment_id: reservation.attachment_id.0,
                },
            );
        };
        match state {
            DatabaseAttachmentState::OwnerReserved {
                client_token,
                owner_id,
                reservation_id,
            } if *owner_id == reservation.owner_id
                && *reservation_id == reservation.reservation_id =>
            {
                let client_token = *client_token;
                *state = DatabaseAttachmentState::Committed {
                    client_token,
                    owner_id: reservation.owner_id,
                    reservation_id: reservation.reservation_id,
                };
                Ok(())
            }
            _ => Err(
                StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost {
                    attachment_id: reservation.attachment_id.0,
                },
            ),
        }
    }

    fn validate_database_owner_attachment_reservation(
        &self,
        reservation: &DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        if !std::ptr::eq(self, Arc::as_ptr(&reservation.source)) {
            return Err(StoreRuntimeRegistryFailure::DatabaseAttachmentOwnerMismatch);
        }
        let attachments = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match attachments.get(&reservation.attachment_id) {
            Some(DatabaseAttachmentState::OwnerReserved {
                client_token: _,
                owner_id,
                reservation_id,
            }) if *owner_id == reservation.owner_id
                && *reservation_id == reservation.reservation_id =>
            {
                Ok(())
            }
            _ => Err(
                StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost {
                    attachment_id: reservation.attachment_id.0,
                },
            ),
        }
    }

    fn retirement_database_attachment_blockers(
        &self,
        owner_reservation: Option<&DatabaseRuntimeOwnerAttachmentReservationIdentityV1>,
    ) -> usize {
        self.database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(attachment_id, state)| {
                !matches!(
                    (owner_reservation, state),
                    (
                        Some(reservation),
                        DatabaseAttachmentState::OwnerReserved {
                            owner_id,
                            reservation_id,
                            ..
                        },
                    ) if *attachment_id == reservation.attachment_id
                        && *owner_id == reservation.owner_id
                        && *reservation_id == reservation.reservation_id
                )
            })
            .count()
    }

    fn retirement_client_lease_blockers(&self, client_tokens: &BTreeSet<u64>) -> usize {
        let attachment_tokens = self
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, state)| match state {
                DatabaseAttachmentState::Active { client_token }
                | DatabaseAttachmentState::OwnerReserved { client_token, .. }
                | DatabaseAttachmentState::Committed { client_token, .. } => *client_token,
            })
            .collect::<BTreeSet<_>>();
        client_tokens
            .iter()
            .filter(|token| !attachment_tokens.contains(token))
            .count()
    }
}

fn allocate_database_attachment_counter(
    counter: &AtomicU64,
) -> Result<u64, StoreRuntimeRegistryFailure> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .map_err(|_| StoreRuntimeRegistryFailure::DatabaseAttachmentIdentityExhausted)
}

impl DatabaseRuntimeOwnerAttachmentReservationIdentityV1 {
    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.source.binding()
    }

    pub(crate) fn locator(&self) -> &RuntimeLocatorRecord {
        self.source.locator()
    }

    pub(crate) fn restore(&self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.source
            .restore_database_owner_attachment_reservation(self)
    }

    pub(crate) fn commit(&self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.source
            .commit_database_owner_attachment_reservation(self)
    }

    pub(crate) fn validate(&self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.source
            .validate_database_owner_attachment_reservation(self)
    }

    #[cfg(test)]
    pub(crate) fn remove_for_test(&self) {
        self.source.release_database_attachment(self.attachment_id);
    }

    #[cfg(test)]
    pub(crate) fn make_stale_for_test(&self) {
        let mut attachments = self
            .source
            .database_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(state) = attachments.get_mut(&self.attachment_id) else {
            return;
        };
        if let DatabaseAttachmentState::OwnerReserved {
            client_token,
            owner_id,
            reservation_id,
        } = state
            && *owner_id == self.owner_id
            && *reservation_id == self.reservation_id
        {
            let client_token = *client_token;
            *state = DatabaseAttachmentState::Active { client_token };
        }
    }
}

/// The registry's non-cloneable physical attachment. Callers receive only
/// `StoreRuntimeClientLease`; the registry retains this owner for the full
/// publication lifetime.
struct StoreRuntimeOwnerAttachment {
    source: Arc<StoreRuntimeLeaseSource>,
}

/// Unique database-facade attachment. Cloning a `Database` shares its
/// `DatabaseInner` and therefore this one attachment; a separately published
/// facade must consume an independently issued client lease.
pub(crate) struct DatabaseRuntimeAttachment {
    client: StoreRuntimeClientLease,
    id: DatabaseRuntimeAttachmentIdV1,
}

impl StoreRuntimeOwnerAttachment {
    fn issue_client_lease(&self) -> Result<StoreRuntimeClientLease, StoreRuntimeRegistryFailure> {
        let lifetime = ShardRuntime::issue_client_lifetime_lease(Arc::clone(&self.source.runtime))
            .map_err(|error| StoreRuntimeRegistryFailure::LeaseRejected {
                message: error.to_string(),
            })?;
        Ok(StoreRuntimeClientLease {
            inner: Arc::new(StoreRuntimeClientLeaseToken {
                source: Arc::clone(&self.source),
                _lifetime: lifetime,
            }),
        })
    }
}

impl StoreRuntimeClientLease {
    #[must_use]
    pub fn shares_runtime_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner.runtime, &other.inner.runtime)
    }

    #[must_use]
    pub fn runtime_identity(&self) -> StoreRuntimeLeaseIdentity {
        StoreRuntimeLeaseIdentity(self.inner.runtime.instance_id())
    }

    pub fn begin_operation(
        &self,
    ) -> Result<StoreRuntimeOperationLease, StoreRuntimeRegistryFailure> {
        self.inner
            ._lifetime
            .begin_operation()
            .map(|_lifetime| StoreRuntimeOperationLease { _lifetime })
            .map_err(|error| StoreRuntimeRegistryFailure::LeaseRejected {
                message: error.to_string(),
            })
    }

    pub fn health_snapshot(&self) -> super::shard::ShardRuntimeHealthSnapshot {
        self.inner.runtime.health_snapshot()
    }

    pub(crate) fn into_database_attachment(
        self,
    ) -> Result<DatabaseRuntimeAttachment, StoreRuntimeRegistryFailure> {
        let id = self
            .inner
            .register_database_attachment(self.inner._lifetime.token())?;
        Ok(DatabaseRuntimeAttachment { client: self, id })
    }
}

impl DatabaseRuntimeAttachment {
    pub(crate) fn issue_client_lease(
        &self,
    ) -> Result<StoreRuntimeClientLease, StoreRuntimeRegistryFailure> {
        let lifetime =
            ShardRuntime::issue_client_lifetime_lease(Arc::clone(&self.client.inner.runtime))
                .map_err(|error| StoreRuntimeRegistryFailure::LeaseRejected {
                    message: error.to_string(),
                })?;
        Ok(StoreRuntimeClientLease {
            inner: Arc::new(StoreRuntimeClientLeaseToken {
                source: Arc::clone(&self.client.inner.source),
                _lifetime: lifetime,
            }),
        })
    }

    pub(crate) fn client(&self) -> &StoreRuntimeClientLease {
        &self.client
    }

    pub(crate) fn allocate_owner_identity(
        &self,
    ) -> Result<DatabaseRuntimeOwnerIdentityV1, StoreRuntimeRegistryFailure> {
        self.client.inner.allocate_database_owner_identity()
    }

    pub(crate) fn reserve_for_owner(
        &self,
        owner_id: DatabaseRuntimeOwnerIdentityV1,
    ) -> Result<DatabaseRuntimeOwnerAttachmentReservationIdentityV1, StoreRuntimeRegistryFailure>
    {
        self.client
            .inner
            .reserve_database_attachment_for_owner(self.id, owner_id)
    }
}

impl Drop for DatabaseRuntimeAttachment {
    fn drop(&mut self) {
        self.client.inner.release_database_attachment(self.id);
    }
}

impl std::ops::Deref for StoreRuntimeOwnerAttachment {
    type Target = StoreRuntimeLeaseSource;

    fn deref(&self) -> &Self::Target {
        &self.source
    }
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

impl StoreRuntimeClientLease {
    pub fn publication(&self) -> &StoreRuntimeRegistryPublicationV1 {
        &self.inner.publication
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.inner.publication.binding
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
        self.inner.source.verified_locator()
    }

    /// Whether the physical attachment currently holds a writer.
    pub fn writer_present(&self) -> bool {
        self.physical_snapshot().writer_present
    }

    pub fn opened_file_identity(&self) -> Option<u64> {
        Some(self.inner.opened_file_identity)
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

    pub async fn snapshot_to_interruptible(
        &self,
        destination: PathBuf,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
        authority: crate::db::DatabaseAuthority,
    ) -> Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, StoreRuntimeRegistryFailure> {
        let opened_file_identity = self.validate_database_write_authority(
            &authority,
            "authorize registered interruptible online backup",
        )?;
        let authority = Arc::new(RuntimeDatabaseWriteAuthority {
            canonical_path: authority.canonical_database_path().to_path_buf(),
            authority,
            opened_file_identity,
        });
        let receipt = self
            .inner
            .attachment
            .snapshot_to_interruptible(destination, probe, authority)
            .await?;
        self.validate_opened_file_identity("complete registered interruptible online backup")?;
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
}

impl tracedecay_store::StorageRuntimeReadPort for StoreRuntimeClientLease {
    fn dispatch_read<'a>(
        &'a self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &'a dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> tracedecay_store::StorageRuntimePortFutureV1<'a, tracedecay_store::RuntimeReadOutcomeV1>
    {
        Box::pin(async move {
            StoreRuntimeClientLease::dispatch_read(self, request, probe).map_err(|_| {
                tracedecay_store::StorageRuntimeErrorV1::Infrastructure {
                    operation: "dispatch registered runtime read".to_owned(),
                }
                .into()
            })
        })
    }
}

impl fmt::Debug for StoreRuntimeClientLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntimeClientLease")
            .field("publication", &self.inner.publication)
            .field("locator_key", self.inner.locator.key())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRuntimeRegistryFailure {
    ResetRequired {
        authority: String,
        reason: String,
    },
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
        operation_leases: u32,
    },
    AuthorityEpochExhausted,
    OpenAttemptExhausted,
    EvictionAttemptExhausted,
    EvictionReservationLost {
        key: Box<StoreRuntimeKey>,
    },
    PublicationIdExhausted,
    DatabaseAttachmentIdentityExhausted,
    DatabaseAttachmentIdentityConflict,
    DatabaseAttachmentAlreadyReserved {
        attachment_id: u64,
    },
    DatabaseAttachmentMissing {
        attachment_id: u64,
    },
    DatabaseAttachmentOwnerMismatch,
    DatabaseAttachmentReservationLost {
        attachment_id: u64,
    },
    OwnerRetirementReservationLost,
    OwnerRetirementCommitFailed {
        message: String,
    },
    GraphLeaseCountExhausted {
        binding: Box<StoreRuntimeBindingV1>,
    },
    GraphLeaseTokenExhausted {
        key: Box<StoreRuntimeKey>,
    },
    ProfilePinTokenExhausted {
        key: Box<StoreRuntimeKey>,
    },
    RuntimeRetirementInProgress {
        key: Box<StoreRuntimeKey>,
    },
    RuntimeRetirementCommitting {
        key: Box<StoreRuntimeKey>,
    },
    RuntimeRetirementFaulted {
        key: Box<StoreRuntimeKey>,
    },
    RuntimeRetirementDurabilityUncertain {
        key: Box<StoreRuntimeKey>,
    },
    RetirementReservationLost {
        key: Box<StoreRuntimeKey>,
    },
    RetirementReservationConsumed,
    GraphLocatorConflict {
        key: Box<StoreRuntimeKey>,
        retained_path: PathBuf,
        resolved_path: PathBuf,
    },
    GraphIncarnationConflict {
        requested: Box<StoreRuntimeKey>,
        retained: Box<StoreRuntimeBindingV1>,
    },
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
    Ready(StoreRuntimeClientLease),
    Opening {
        key: Box<StoreRuntimeKey>,
    },
    Evicting {
        key: Box<StoreRuntimeKey>,
    },
    Retiring {
        key: Box<StoreRuntimeKey>,
    },
    Committing {
        key: Box<StoreRuntimeKey>,
    },
    Faulted {
        key: Box<StoreRuntimeKey>,
    },
    DurabilityUncertain {
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
    owner: Arc<StoreRuntimeOwnerAttachment>,
}

struct EvictingRuntime {
    attempt: u64,
    owner: Arc<StoreRuntimeOwnerAttachment>,
}

struct RetiringRuntime {
    owner: Arc<StoreRuntimeOwnerAttachment>,
}

struct CommittingRuntime {
    owner: Arc<StoreRuntimeOwnerAttachment>,
}

struct FaultedRuntime {
    owner: Arc<StoreRuntimeOwnerAttachment>,
}

struct DestructivePathReservation {
    root: PathBuf,
    database_paths: Vec<PathBuf>,
    released: tokio::sync::watch::Sender<bool>,
}

struct RetainedGraphPublication {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
    /// One entry per independently issued graph lease. Clones share the
    /// returned lease object and therefore release one exact token at last
    /// drop rather than being inferred from reference counts.
    lease_tokens: BTreeSet<u64>,
}

enum RegistryEntry {
    Opening(open::OpeningRuntime),
    Ready(ReadyRuntime),
    Retiring(RetiringRuntime),
    Committing(CommittingRuntime),
    Faulted(FaultedRuntime),
    DurabilityUncertain(FaultedRuntime),
    Evicting(EvictingRuntime),
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreRuntimeKey, RegistryEntry>,
    graph_publications: BTreeMap<StoreRuntimeKey, RetainedGraphPublication>,
    destructive_paths: BTreeMap<u64, DestructivePathReservation>,
    profile_authorities: BTreeMap<StoreShardIdV1, StoreRuntimeBindingV1>,
    profile_pin_tokens: BTreeMap<StoreRuntimeKey, BTreeSet<u64>>,
    next_destructive_attempt: u64,
    next_open_attempt: u64,
    next_eviction_attempt: u64,
    next_publication: u64,
    next_graph_lease_token: u64,
    next_profile_pin_token: u64,
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
                let actual = ready.owner.binding();
                if actual.authority_epoch == expected.authority_epoch {
                    match ready.owner.issue_client_lease() {
                        Ok(lease) => StoreRuntimeLookup::Ready(lease),
                        Err(_) => StoreRuntimeLookup::Missing { key: Box::new(key) },
                    }
                } else {
                    StoreRuntimeLookup::Fenced {
                        expected: Box::new(expected.clone()),
                        actual: Box::new(actual.clone()),
                    }
                }
            }
            Some(RegistryEntry::Opening(_)) => StoreRuntimeLookup::Opening { key: Box::new(key) },
            Some(RegistryEntry::Retiring(_)) => StoreRuntimeLookup::Retiring { key: Box::new(key) },
            Some(RegistryEntry::Committing(_)) => {
                StoreRuntimeLookup::Committing { key: Box::new(key) }
            }
            Some(RegistryEntry::Faulted(_)) => StoreRuntimeLookup::Faulted { key: Box::new(key) },
            Some(RegistryEntry::DurabilityUncertain(_)) => {
                StoreRuntimeLookup::DurabilityUncertain { key: Box::new(key) }
            }
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
                                    actual: Box::new(ready.owner.binding().clone()),
                                })
                            }
                            RegistryEntry::Opening(_) => None,
                            RegistryEntry::Retiring(retiring) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(retiring.owner.binding().clone()),
                                })
                            }
                            RegistryEntry::Committing(committing) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(committing.owner.binding().clone()),
                                })
                            }
                            RegistryEntry::Faulted(faulted)
                            | RegistryEntry::DurabilityUncertain(faulted) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(faulted.owner.binding().clone()),
                                })
                            }
                            RegistryEntry::Evicting(evicting) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(evicting.owner.binding().clone()),
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
    pub fn retained_runtime_for_read(
        &self,
        key: &StoreRuntimeKey,
    ) -> Option<StoreRuntimeClientLease> {
        let state = self.lock_state();
        match state.entries.get(key) {
            Some(RegistryEntry::Ready(ready)) => ready.owner.issue_client_lease().ok(),
            Some(
                RegistryEntry::Opening(_)
                | RegistryEntry::Retiring(_)
                | RegistryEntry::Committing(_)
                | RegistryEntry::Faulted(_)
                | RegistryEntry::DurabilityUncertain(_)
                | RegistryEntry::Evicting(_),
            )
            | None => None,
        }
    }

    pub(super) fn issue_client_lease_for_open(
        &self,
        key: &StoreRuntimeKey,
    ) -> Result<StoreRuntimeClientLease, StoreRuntimeRegistryFailure> {
        let state = self.lock_state();
        match state.entries.get(key) {
            Some(RegistryEntry::Ready(ready)) => ready.owner.issue_client_lease(),
            Some(RegistryEntry::Opening(_)) => {
                Err(StoreRuntimeRegistryFailure::OpenTaskAbandoned {
                    key: Box::new(key.clone()),
                })
            }
            Some(RegistryEntry::Evicting(_)) => {
                Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                    key: Box::new(key.clone()),
                })
            }
            Some(RegistryEntry::Retiring(_)) => {
                Err(StoreRuntimeRegistryFailure::RuntimeRetirementInProgress {
                    key: Box::new(key.clone()),
                })
            }
            Some(RegistryEntry::Committing(_)) => {
                Err(StoreRuntimeRegistryFailure::RuntimeRetirementCommitting {
                    key: Box::new(key.clone()),
                })
            }
            Some(RegistryEntry::Faulted(_)) => {
                Err(StoreRuntimeRegistryFailure::RuntimeRetirementFaulted {
                    key: Box::new(key.clone()),
                })
            }
            Some(RegistryEntry::DurabilityUncertain(_)) => Err(
                StoreRuntimeRegistryFailure::RuntimeRetirementDurabilityUncertain {
                    key: Box::new(key.clone()),
                },
            ),
            None => Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "issue registered client lease",
                message: "published runtime disappeared before its opener received a lease"
                    .to_owned(),
            }),
        }
    }

    pub fn inventory(
        &self,
        admission: AdmissionConfigV1,
        global_queued_bytes: Option<u64>,
    ) -> RuntimeRegistryInventory {
        let (opening_shards, sources) = {
            let state = self.lock_state();
            let opening_shards = state
                .entries
                .values()
                .filter(|entry| matches!(entry, RegistryEntry::Opening(_)))
                .count();
            let sources = state
                .entries
                .values()
                .filter_map(|entry| match entry {
                    RegistryEntry::Ready(ready) => Some(Arc::clone(&ready.owner.source)),
                    RegistryEntry::Opening(_) => None,
                    RegistryEntry::Retiring(retiring) => Some(Arc::clone(&retiring.owner.source)),
                    RegistryEntry::Committing(committing) => {
                        Some(Arc::clone(&committing.owner.source))
                    }
                    RegistryEntry::Faulted(faulted)
                    | RegistryEntry::DurabilityUncertain(faulted) => {
                        Some(Arc::clone(&faulted.owner.source))
                    }
                    RegistryEntry::Evicting(evicting) => Some(Arc::clone(&evicting.owner.source)),
                })
                .collect::<Vec<_>>();
            let opening_shards = u32::try_from(opening_shards).unwrap_or(u32::MAX);
            (opening_shards, sources)
        };
        let entries = sources
            .into_iter()
            .map(|source| {
                let mut observation = source.runtime().observe(self.inner.config.eviction_idle());
                let physical = source.physical_snapshot();
                observation.health.writer_present |= physical.writer_present;
                observation.health.queued_operations = observation
                    .health
                    .queued_operations
                    .saturating_add(physical.queued_operations);
                observation.health.queued_bytes = observation
                    .health
                    .queued_bytes
                    .saturating_add(physical.queued_bytes);
                if let Some(wal_bytes) = physical.wal_bytes {
                    observation.health.wal_bytes = wal_bytes;
                }
                if let Some(memory_estimate_bytes) = physical.memory_estimate_bytes {
                    observation.health.memory_estimate_bytes = memory_estimate_bytes;
                }
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
            opening_shards,
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
