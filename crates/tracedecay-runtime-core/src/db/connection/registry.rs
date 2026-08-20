use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::db::{DatabaseAuthority, engine::Connection};
use crate::errors::TraceDecayError;
// The store-runtime registry moved into this kernel, so the facade retains the
// concrete handle rather than an erased port.
use super::memory_graph_reconciliation::{
    MemoryGraphReconciliationCoordinatorV1, ProjectMemoryReconciliationTelemetryV1,
};
use crate::store_runtime::registry::{
    DatabaseRuntimeAttachment, DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    DatabaseRuntimeOwnerIdentityV1, StoreRuntimeClientLease,
    StoreRuntimeOwnerAttachmentRetirementReservationV1, StoreRuntimeRegistryFailure,
    StoreRuntimeRetirementTarget,
};

use super::Database;

#[derive(Clone)]
pub(super) struct DatabaseClientLeaseV1 {
    _runtime: StoreRuntimeClientLease,
}

impl DatabaseClientLeaseV1 {
    pub(super) fn runtime(&self) -> &StoreRuntimeClientLease {
        &self._runtime
    }
}

/// Opaque retention guard for every derived database capability. Cloning this
/// guard shares the issuing database client's one counted token; it never
/// mints a token or exposes the registry client.
#[derive(Clone)]
pub struct DatabaseClientGuardV1 {
    client: Arc<DatabaseClientLeaseV1>,
}

impl DatabaseClientGuardV1 {
    pub(super) fn runtime(&self) -> &StoreRuntimeClientLease {
        self.client.runtime()
    }
}

/// A derived runtime capability that retains the exact counted database client
/// token that produced it. This adapter intentionally exposes only runtime
/// operations; it never exposes the registry client or attachment itself.
#[derive(Clone)]
pub(crate) struct DatabaseRuntimeClientLeaseV1 {
    guard: DatabaseClientGuardV1,
}

impl DatabaseRuntimeClientLeaseV1 {
    pub(crate) fn binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.guard.runtime().binding()
    }

    pub(crate) fn verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.guard.runtime().verified_locator()
    }

    pub(crate) fn canonical_path(&self) -> &std::path::Path {
        self.guard.runtime().canonical_path()
    }

    pub(crate) fn opened_file_identity(&self) -> Option<u64> {
        self.guard.runtime().opened_file_identity()
    }

    pub(crate) async fn dispatch_submit_authorized(
        &self,
        request: tracedecay_store::RuntimeSubmitRequestV1,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
        authority: DatabaseAuthority,
    ) -> Result<tracedecay_store::RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure> {
        self.guard
            .runtime()
            .dispatch_submit_authorized(request, probe, authority)
            .await
    }

    pub(crate) fn dispatch_read(
        &self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        self.guard.runtime().dispatch_read(request, probe)
    }
}

/// Non-cloneable map authority for one stable database publication. It owns
/// the `DatabaseInner` allocation and only yields independently counted client
/// facades; callers cannot reach the owner through a lease.
pub struct DatabaseOwnerV1 {
    state: Arc<DatabaseOwnerStateV1>,
}

struct DatabaseOwnerStateV1 {
    inner: Arc<DatabaseInner>,
    owner_id: DatabaseRuntimeOwnerIdentityV1,
    lifecycle: Mutex<DatabaseOwnerLifecycleV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DatabaseOwnerLifecycleV1 {
    Ready,
    RetirementFenced,
    Terminal,
    Faulted(StoreRuntimeRegistryFailure),
}

/// Typed owner failure. A fenced owner never fabricates a client facade.
#[derive(Debug)]
pub enum DatabaseOwnerErrorV1 {
    RetirementFenced,
    RetirementTerminal,
    RetirementFaulted(StoreRuntimeRegistryFailure),
    MissingWriteAuthority,
    Runtime(StoreRuntimeRegistryFailure),
}

/// RAII owner reservation transferred into the exact Store target. Dropping
/// before Store linearization restores the original attachment and issuance
/// state; committing it is irreversible.
pub struct DatabaseOwnerRetirementReservationV1 {
    state: Arc<DatabaseOwnerStateV1>,
    attachment: DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    armed: bool,
}

pub(super) struct DatabaseInner {
    /// Reader-only channel exposed through the retained database facade.
    pub(super) conn: Connection,
    /// Writer-authorized channel cloned only while the logical writer lane is
    /// held. Read-only facades never retain one.
    pub(super) write_conn: Option<Connection>,
    /// Retains the registry-owned physical runtime. The registry remains the
    /// sole lifecycle owner; this facade never extracts or reopens its
    /// attachment.
    runtime: DatabaseRuntimeAttachment,
    pub(super) writable: bool,
    /// Descriptor-derived identity reported by the physical attachment.
    pub(super) opened_file_identity: u64,
    /// Serializes logical writers sharing this canonical database slot.
    pub(super) writer: tokio::sync::Mutex<()>,
    /// Coalesces and retains background memory-graph catch-up for this exact
    /// relational attachment.
    pub(super) memory_graph_reconciliation: MemoryGraphReconciliationCoordinatorV1,
    /// Monotonic reconciliation work observed for this exact database attachment.
    pub(super) memory_graph_reconciliation_telemetry: Arc<ProjectMemoryReconciliationTelemetryV1>,
    /// Canonical path from the runtime's verified locator.
    pub(super) canonical_path: PathBuf,
    /// The exact capability retained when this physical attachment was
    /// published writable. Read-only facades never retain write authority.
    pub(super) _authority: Option<DatabaseAuthority>,
    /// Rebuildable memory topology mounted from the same registered shard as
    /// this relational fact authority. Content never enters this graph.
    pub(super) memory_graph_runtime:
        OnceLock<Arc<dyn crate::store_runtime::VerifiedGraphRuntimePortV1>>,
}

impl DatabaseInner {
    /// Publishes an already-open canonical registry runtime without reopening
    /// the `SQLite` path.
    pub(super) fn publish(
        runtime: StoreRuntimeClientLease,
        writable: bool,
        authority: Option<DatabaseAuthority>,
    ) -> crate::errors::Result<Self> {
        let runtime = runtime.into_database_attachment().map_err(|error| {
            database_registry_error("publish canonical database runtime", format!("{error:?}"))
        })?;
        let opened_file_identity = runtime.client().opened_file_identity().ok_or_else(|| {
            database_registry_error(
                "publish canonical database runtime",
                "registered runtime did not report its opened SQLite file identity",
            )
        })?;
        if let Some(authority) = authority.as_ref()
            && runtime.client().canonical_path() != authority.canonical_database_path()
        {
            return Err(database_registry_error(
                "publish canonical database runtime",
                format!(
                    "registered locator {} does not match retained database authority {}",
                    runtime.client().canonical_path().display(),
                    authority.canonical_database_path().display()
                ),
            ));
        }
        runtime
            .client()
            .validate_registered_read("publish canonical database runtime")
            .map_err(|error| {
                database_registry_error("publish canonical database runtime", format!("{error:?}"))
            })?;

        let write_conn = if writable {
            let authority = authority.clone().ok_or_else(|| {
                database_registry_error(
                    "authorize canonical database engine",
                    "writable database publication requires originating authority",
                )
            })?;
            Some(Connection::attach(
                runtime
                    .client()
                    .authorized_exact_sql_handle(authority)
                    .map_err(|error| {
                        database_registry_error(
                            "authorize canonical database engine",
                            format!("{error:?}"),
                        )
                    })?,
            ))
        } else {
            None
        };
        let read_conn =
            Connection::attach(runtime.client().telemetry_read_handle().map_err(|error| {
                database_registry_error("attach canonical database reader", format!("{error:?}"))
            })?);

        Ok(Self {
            conn: read_conn,
            write_conn,
            canonical_path: runtime.client().canonical_path().to_path_buf(),
            runtime,
            writable,
            opened_file_identity,
            writer: tokio::sync::Mutex::new(()),
            memory_graph_reconciliation: MemoryGraphReconciliationCoordinatorV1::default(),
            memory_graph_reconciliation_telemetry: Arc::new(
                ProjectMemoryReconciliationTelemetryV1::default(),
            ),
            _authority: authority,
            memory_graph_runtime: OnceLock::new(),
        })
    }

    fn issue_runtime_client_lease(
        &self,
    ) -> Result<StoreRuntimeClientLease, StoreRuntimeRegistryFailure> {
        self.runtime.issue_client_lease()
    }

    fn registered_binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.runtime.client().binding()
    }

    fn registered_verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.runtime.client().verified_locator()
    }

    fn allocate_owner_identity(
        &self,
    ) -> Result<DatabaseRuntimeOwnerIdentityV1, StoreRuntimeRegistryFailure> {
        self.runtime.allocate_owner_identity()
    }

    fn reserve_owner_attachment(
        &self,
        owner_id: DatabaseRuntimeOwnerIdentityV1,
    ) -> Result<DatabaseRuntimeOwnerAttachmentReservationIdentityV1, StoreRuntimeRegistryFailure>
    {
        self.runtime.reserve_for_owner(owner_id)
    }

    fn validate_owner_attachment_reservation(
        &self,
        attachment: &DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        attachment.validate()
    }
}

impl Database {
    pub(crate) fn client_guard(&self) -> DatabaseClientGuardV1 {
        DatabaseClientGuardV1 {
            client: Arc::clone(&self.client),
        }
    }

    /// Retains the same counted client token for a derived store operation.
    /// This cannot mint a new token or reveal the registry attachment.
    pub(crate) fn runtime_client_lease(&self) -> DatabaseRuntimeClientLeaseV1 {
        DatabaseRuntimeClientLeaseV1 {
            guard: self.client_guard(),
        }
    }

    /// Exact registry binding for daemon-side identity validation.
    pub fn registered_binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.inner.registered_binding()
    }

    /// Exact verified locator for daemon-side identity validation.
    pub fn registered_verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.inner.registered_verified_locator()
    }

    pub(crate) fn authorized_exact_sql_handle(
        &self,
        authority: DatabaseAuthority,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, StoreRuntimeRegistryFailure>
    {
        self.client.runtime().authorized_exact_sql_handle(authority)
    }
}

impl DatabaseOwnerV1 {
    pub(super) fn from_published_inner(
        inner: Arc<DatabaseInner>,
    ) -> Result<Self, DatabaseOwnerErrorV1> {
        let owner_id = inner
            .allocate_owner_identity()
            .map_err(DatabaseOwnerErrorV1::Runtime)?;
        Ok(Self {
            state: Arc::new(DatabaseOwnerStateV1 {
                inner,
                owner_id,
                lifecycle: Mutex::new(DatabaseOwnerLifecycleV1::Ready),
            }),
        })
    }

    /// Issues one independently counted client facade over this owner’s stable
    /// database publication. Cloning the returned `Database` shares only this
    /// issuance token.
    pub fn issue_lease(&self) -> Result<Database, DatabaseOwnerErrorV1> {
        let lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*lifecycle {
            DatabaseOwnerLifecycleV1::Ready => {
                let runtime = self
                    .state
                    .inner
                    .issue_runtime_client_lease()
                    .map_err(DatabaseOwnerErrorV1::Runtime)?;
                Ok(Database {
                    inner: Arc::clone(&self.state.inner),
                    client: Arc::new(DatabaseClientLeaseV1 { _runtime: runtime }),
                })
            }
            DatabaseOwnerLifecycleV1::RetirementFenced => {
                Err(DatabaseOwnerErrorV1::RetirementFenced)
            }
            DatabaseOwnerLifecycleV1::Terminal => Err(DatabaseOwnerErrorV1::RetirementTerminal),
            DatabaseOwnerLifecycleV1::Faulted(error) => {
                Err(DatabaseOwnerErrorV1::RetirementFaulted(error.clone()))
            }
        }
    }

    /// Exact registry binding for consumers that own the daemon map entry.
    /// This is identity-only: it cannot issue an uncounted database client.
    pub fn registered_binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.state.inner.registered_binding()
    }

    /// Exact verified locator for consumers that own the daemon map entry.
    /// This is identity-only: it cannot issue an uncounted database client.
    pub fn registered_verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.state.inner.registered_verified_locator()
    }

    pub fn reserve_retirement(
        &self,
    ) -> Result<DatabaseOwnerRetirementReservationV1, DatabaseOwnerErrorV1> {
        let mut lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*lifecycle {
            DatabaseOwnerLifecycleV1::Ready => {
                let attachment = self
                    .state
                    .inner
                    .reserve_owner_attachment(self.state.owner_id)
                    .map_err(DatabaseOwnerErrorV1::Runtime)?;
                *lifecycle = DatabaseOwnerLifecycleV1::RetirementFenced;
                Ok(DatabaseOwnerRetirementReservationV1 {
                    state: Arc::clone(&self.state),
                    attachment,
                    armed: true,
                })
            }
            DatabaseOwnerLifecycleV1::RetirementFenced => {
                Err(DatabaseOwnerErrorV1::RetirementFenced)
            }
            DatabaseOwnerLifecycleV1::Terminal => Err(DatabaseOwnerErrorV1::RetirementTerminal),
            DatabaseOwnerLifecycleV1::Faulted(error) => {
                Err(DatabaseOwnerErrorV1::RetirementFaulted(error.clone()))
            }
        }
    }
}

impl DatabaseOwnerRetirementReservationV1 {
    /// Consumes this exact owner reservation into the only Store retirement
    /// target authorized to reclassify the canonical attachment.
    pub fn into_store_retirement_target(
        self,
    ) -> Result<StoreRuntimeRetirementTarget, DatabaseOwnerErrorV1> {
        let authority = self
            .state
            .inner
            ._authority
            .clone()
            .ok_or(DatabaseOwnerErrorV1::MissingWriteAuthority)?;
        Ok(
            StoreRuntimeRetirementTarget::with_database_owner_attachment(
                self.attachment.binding().clone(),
                authority,
                Box::new(self),
            ),
        )
    }

    fn commit_owner_attachment(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::OwnerRetirementReservationLost);
        }
        {
            let lifecycle = self
                .state
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*lifecycle, DatabaseOwnerLifecycleV1::RetirementFenced) {
                return Err(StoreRuntimeRegistryFailure::OwnerRetirementReservationLost);
            }
        }
        self.attachment.commit()?;
        let mut lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(&*lifecycle, DatabaseOwnerLifecycleV1::RetirementFenced) {
            return Err(StoreRuntimeRegistryFailure::OwnerRetirementReservationLost);
        }
        *lifecycle = DatabaseOwnerLifecycleV1::Terminal;
        self.armed = false;
        Ok(())
    }

    fn preflight_owner_attachment_commit(&self) -> Result<(), StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::OwnerRetirementReservationLost);
        }
        let lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(&*lifecycle, DatabaseOwnerLifecycleV1::RetirementFenced) {
            return Err(StoreRuntimeRegistryFailure::OwnerRetirementReservationLost);
        }
        self.state
            .inner
            .validate_owner_attachment_reservation(&self.attachment)
    }

    fn seal_after_commit_failure(&mut self) {
        if !self.armed {
            return;
        }
        let mut lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *lifecycle = DatabaseOwnerLifecycleV1::Terminal;
        self.armed = false;
    }

    #[cfg(test)]
    pub(super) fn remove_attachment_for_test(&self) {
        self.attachment.remove_for_test();
    }

    #[cfg(test)]
    pub(super) fn make_attachment_stale_for_test(&self) {
        self.attachment.make_stale_for_test();
    }
}

impl StoreRuntimeOwnerAttachmentRetirementReservationV1 for DatabaseOwnerRetirementReservationV1 {
    fn identity(&self) -> &DatabaseRuntimeOwnerAttachmentReservationIdentityV1 {
        &self.attachment
    }

    fn preflight_commit(&self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.preflight_owner_attachment_commit()
    }

    fn commit(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.commit_owner_attachment()
    }

    fn terminalize_after_commit_failure(&mut self) {
        self.seal_after_commit_failure();
    }
}

impl Drop for DatabaseOwnerRetirementReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let restoration = self.attachment.restore();
        let mut lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match restoration {
            Ok(()) if matches!(&*lifecycle, DatabaseOwnerLifecycleV1::RetirementFenced) => {
                *lifecycle = DatabaseOwnerLifecycleV1::Ready;
            }
            Ok(()) => {
                *lifecycle = DatabaseOwnerLifecycleV1::Faulted(
                    StoreRuntimeRegistryFailure::OwnerRetirementReservationLost,
                );
            }
            Err(error) => {
                *lifecycle = DatabaseOwnerLifecycleV1::Faulted(error);
            }
        }
        self.armed = false;
    }
}

fn database_registry_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}
