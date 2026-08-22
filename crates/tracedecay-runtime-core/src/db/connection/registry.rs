use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::db::{DatabaseAuthority, engine::Connection};
use crate::errors::TraceDecayError;
// The store-runtime registry moved into this kernel, so the facade retains the
// concrete handle rather than an erased port.
use super::memory_graph_reconciliation::{
    MemoryGraphReconciliationCoordinatorV1, ProjectMemoryReconciliationTelemetryV1,
};
use crate::store_runtime::registry::{
    CanonicalGraphStoreOwnerRetirementTargetV1, DatabaseRuntimeAttachment,
    DatabaseRuntimeOwnerAttachmentReservationIdentityV1, DatabaseRuntimeOwnerIdentityV1,
    StoreRuntimeClientLease, StoreRuntimeOwnerAttachmentRetirementReservationV1,
    StoreRuntimeRegistryFailure, StoreRuntimeRetirementTarget,
};

use super::{Database, DatabaseAccessMode};

#[derive(Clone)]
pub(super) struct DatabaseClientLeaseV1 {
    _runtime: StoreRuntimeClientLease,
    access: DatabaseAccessMode,
}

impl DatabaseClientLeaseV1 {
    pub(super) fn runtime(&self) -> &StoreRuntimeClientLease {
        &self._runtime
    }

    pub(in crate::db::connection) fn is_writable(&self) -> bool {
        self.access.is_writable()
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

    pub(super) fn is_writable(&self) -> bool {
        self.client.is_writable()
    }
}

/// Runtime operations over one exact counted database client.
///
/// This opaque capability exposes only typed runtime requests. It retains the
/// client token that minted it and, for a read-write client, the exact
/// database authority required for a runtime write. Neither the registry
/// client nor its physical attachment can escape this boundary.
#[derive(Clone)]
pub struct DatabaseRuntimeClientV1 {
    guard: DatabaseClientGuardV1,
    authority: Option<DatabaseAuthority>,
}

impl DatabaseRuntimeClientV1 {
    /// Returns a value copy of the exact Store publication selected for this
    /// guarded client. The publication can be used for identity CAS checks,
    /// but carries neither a client lease nor a raw runtime capability.
    #[must_use]
    pub fn publication(&self) -> tracedecay_store::StoreRuntimeRegistryPublicationV1 {
        self.guard.runtime().publication().clone()
    }

    #[must_use]
    pub fn binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.guard.runtime().binding()
    }

    #[must_use]
    pub fn verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.guard.runtime().verified_locator()
    }

    pub async fn dispatch_submit(
        &self,
        request: tracedecay_store::RuntimeSubmitRequestV1,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
    ) -> Result<tracedecay_store::RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure> {
        if !self.guard.is_writable() {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "submit through database client",
                message: "read-only database client cannot submit a runtime write".to_owned(),
            });
        }
        let authority = self.authority.clone().ok_or_else(|| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "submit through database client",
                message: "read-write database client has no retained write authority".to_owned(),
            }
        })?;
        tracing::trace!(
            target: "tracedecay::observation_admission_work",
            work = "runtime_command",
            "dispatch database runtime submit"
        );
        self.guard
            .runtime()
            .dispatch_submit_authorized(request, probe, authority)
            .await
    }

    pub fn dispatch_read(
        &self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        tracing::trace!(
            target: "tracedecay::observation_admission_work",
            work = "runtime_command",
            "dispatch database runtime read"
        );
        self.guard.runtime().dispatch_read(request, probe)
    }
}

/// Non-cloneable map authority for one stable database publication. It owns
/// the `DatabaseInner` allocation and only yields independently counted client
/// facades; callers cannot reach the owner through a lease.
pub struct DatabaseOwnerV1 {
    state: Arc<DatabaseOwnerStateV1>,
}

/// Cloneable, weak issuance route for one exact database-map owner.
///
/// The issuer retains neither the owner nor a database client token. Each
/// successful call mints one new independently counted [`Database`] client
/// while the original owner remains ready. This lets long-lived routing maps
/// retain an exact issuance route without pinning the Store runtime across
/// owner retirement.
#[derive(Clone)]
pub struct DatabaseOwnerWeakLeaseIssuerV1 {
    state: Weak<DatabaseOwnerStateV1>,
    binding: tracedecay_store::StoreRuntimeBindingV1,
    verified_locator: tracedecay_store::VerifiedStoreLocatorV1,
}

struct DatabaseOwnerStateV1 {
    inner: Arc<DatabaseInner>,
    access: DatabaseAccessMode,
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

/// A weak owner issuer can only mint leases while its exact owner is ready.
///
/// The error deliberately describes lifecycle availability rather than
/// exposing a raw runtime or owner reservation to long-lived consumers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseOwnerWeakLeaseIssuerErrorV1 {
    Retiring,
    Terminal,
    Unavailable,
}

/// RAII owner reservation transferred into the exact Store target. Dropping
/// before Store linearization restores the original attachment and issuance
/// state; committing it is irreversible.
pub struct DatabaseOwnerRetirementReservationV1 {
    state: Arc<DatabaseOwnerStateV1>,
    attachment: DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
    armed: bool,
}

/// Recoverable refusal while composing exact database and graph owner targets
/// before Store's native-close linearization point.
///
/// Both reservations remain move-only and are returned together, so callers
/// can restore their exact owner slot or retry without remounting a graph or
/// minting a new database owner identity.
pub struct DatabaseGraphOwnerRetirementCompositionRefusalV1 {
    error: DatabaseOwnerErrorV1,
    database_owner_reservation: Box<DatabaseOwnerRetirementReservationV1>,
    graph_owner_target: Box<CanonicalGraphStoreOwnerRetirementTargetV1>,
}

impl DatabaseGraphOwnerRetirementCompositionRefusalV1 {
    #[must_use]
    pub fn error(&self) -> &DatabaseOwnerErrorV1 {
        &self.error
    }

    /// Returns the exact pre-linearization failure and both original inputs.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        DatabaseOwnerErrorV1,
        DatabaseOwnerRetirementReservationV1,
        CanonicalGraphStoreOwnerRetirementTargetV1,
    ) {
        (
            self.error,
            *self.database_owner_reservation,
            *self.graph_owner_target,
        )
    }
}

impl std::fmt::Debug for DatabaseGraphOwnerRetirementCompositionRefusalV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseGraphOwnerRetirementCompositionRefusalV1")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

pub(super) struct DatabaseInner {
    /// Reader-only channel exposed through the retained database facade.
    pub(super) conn: Connection,
    /// Writer-authorized channel retained by a read-write map owner. A
    /// read-only issued facade may share this inner allocation, but cannot
    /// access the channel because its client token carries no write mode.
    pub(super) write_conn: Option<Connection>,
    /// Retains the registry-owned physical runtime. The registry remains the
    /// sole lifecycle owner; this facade never extracts or reopens its
    /// attachment.
    runtime: DatabaseRuntimeAttachment,
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
    /// published writable. Read-only issued facades cannot retrieve it.
    pub(super) _authority: Option<DatabaseAuthority>,
    /// Rebuildable memory topology mounted from the same registered shard as
    /// this relational fact authority. Content never enters this graph.
    /// The daemon map owns the graph runtime attachment. The database keeps
    /// only this weak binding so mounting a derived graph cannot contribute a
    /// counted Store/Graph client that blocks retirement of its own owner.
    pub(super) memory_graph_runtime:
        OnceLock<Weak<dyn crate::store_runtime::VerifiedGraphRuntimePortV1>>,
}

impl DatabaseInner {
    /// Publishes an already-open canonical registry runtime without reopening
    /// the `SQLite` path.
    pub(super) fn publish(
        runtime: StoreRuntimeClientLease,
        access: DatabaseAccessMode,
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

        let write_conn = if access.is_writable() {
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

    /// Retains the exact client token for closed store-runtime operations.
    ///
    /// A read-write client also retains its exact database authority for
    /// [`DatabaseRuntimeClientV1::dispatch_submit`]. A read-only client can
    /// dispatch reads, but submit is denied before touching the runtime.
    #[must_use]
    pub fn runtime_client(&self) -> DatabaseRuntimeClientV1 {
        DatabaseRuntimeClientV1 {
            guard: self.client_guard(),
            authority: self
                .client
                .is_writable()
                .then(|| self.inner._authority.clone())
                .flatten(),
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
        if !self.client.is_writable() {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "authorize exact SQL through database client",
                message: "read-only database client cannot authorize exact SQL writes".to_owned(),
            });
        }
        self.client.runtime().authorized_exact_sql_handle(authority)
    }
}

impl DatabaseOwnerV1 {
    pub(super) fn from_published_inner(
        inner: Arc<DatabaseInner>,
        access: DatabaseAccessMode,
    ) -> Result<Self, DatabaseOwnerErrorV1> {
        let owner_id = inner
            .allocate_owner_identity()
            .map_err(DatabaseOwnerErrorV1::Runtime)?;
        Ok(Self {
            state: Arc::new(DatabaseOwnerStateV1 {
                inner,
                access,
                owner_id,
                lifecycle: Mutex::new(DatabaseOwnerLifecycleV1::Ready),
            }),
        })
    }

    /// Issues one independently counted client facade with this owner's
    /// original access policy. Cloning the returned `Database` shares both
    /// its issuance token and access mode.
    pub fn issue_lease(&self) -> Result<Database, DatabaseOwnerErrorV1> {
        self.issue_client_lease(self.state.access)
    }

    /// Issues one independently counted read-only client facade.
    ///
    /// A read-write owner may reduce an issued client to read-only, while an
    /// owner published read-only remains read-only. No client can elevate its
    /// access mode after issuance.
    pub fn issue_read_only_lease(&self) -> Result<Database, DatabaseOwnerErrorV1> {
        self.issue_client_lease(DatabaseAccessMode::ReadOnly)
    }

    /// Creates a cloneable, non-retaining route that can later issue a fresh
    /// database client only while this exact owner remains ready.
    #[must_use]
    pub fn weak_lease_issuer(&self) -> DatabaseOwnerWeakLeaseIssuerV1 {
        DatabaseOwnerWeakLeaseIssuerV1 {
            state: Arc::downgrade(&self.state),
            binding: self.state.inner.registered_binding().clone(),
            verified_locator: self.state.inner.registered_verified_locator().clone(),
        }
    }

    fn issue_client_lease(
        &self,
        access: DatabaseAccessMode,
    ) -> Result<Database, DatabaseOwnerErrorV1> {
        Self::issue_client_lease_from_state(&self.state, access)
    }

    fn issue_client_lease_from_state(
        state: &Arc<DatabaseOwnerStateV1>,
        access: DatabaseAccessMode,
    ) -> Result<Database, DatabaseOwnerErrorV1> {
        let lifecycle = state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*lifecycle {
            DatabaseOwnerLifecycleV1::Ready => {
                let access = if state.access.is_writable() {
                    access
                } else {
                    DatabaseAccessMode::ReadOnly
                };
                let runtime = state
                    .inner
                    .issue_runtime_client_lease()
                    .map_err(DatabaseOwnerErrorV1::Runtime)?;
                Ok(Database {
                    inner: Arc::clone(&state.inner),
                    client: Arc::new(DatabaseClientLeaseV1 {
                        _runtime: runtime,
                        access,
                    }),
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

impl DatabaseOwnerWeakLeaseIssuerV1 {
    /// Issues a fresh independently counted client only while the exact owner
    /// is ready. The lifecycle lock covers both readiness validation and
    /// client-token issuance, so retirement fencing and issuance linearize.
    pub fn issue_lease(&self) -> Result<Database, DatabaseOwnerWeakLeaseIssuerErrorV1> {
        let state = self
            .state
            .upgrade()
            .ok_or(DatabaseOwnerWeakLeaseIssuerErrorV1::Unavailable)?;
        DatabaseOwnerV1::issue_client_lease_from_state(&state, state.access).map_err(|error| {
            match error {
                DatabaseOwnerErrorV1::RetirementFenced => {
                    DatabaseOwnerWeakLeaseIssuerErrorV1::Retiring
                }
                DatabaseOwnerErrorV1::RetirementTerminal => {
                    DatabaseOwnerWeakLeaseIssuerErrorV1::Terminal
                }
                DatabaseOwnerErrorV1::RetirementFaulted(_)
                | DatabaseOwnerErrorV1::MissingWriteAuthority
                | DatabaseOwnerErrorV1::Runtime(_) => {
                    DatabaseOwnerWeakLeaseIssuerErrorV1::Unavailable
                }
            }
        })
    }

    /// Exact non-retaining Store identity selected when this issuer was
    /// minted. This cannot reopen or retain a physical runtime.
    #[must_use]
    pub fn registered_binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        &self.binding
    }

    /// Exact non-retaining locator identity selected when this issuer was
    /// minted. This cannot reopen or retain a physical runtime.
    #[must_use]
    pub fn registered_verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        &self.verified_locator
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

    /// Consumes the exact database-owner reservation together with the one
    /// graph-map owner target selected from the same Store authority.
    ///
    /// Store validates both attachments under its reservation fence. A
    /// refusal drops this database reservation and restores its original
    /// owner attachment; no replacement graph mount or authority epoch is
    /// created for a retry.
    pub fn into_store_retirement_target_with_graph(
        self,
        graph_owner_attachment: CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> Result<StoreRuntimeRetirementTarget, DatabaseGraphOwnerRetirementCompositionRefusalV1>
    {
        let authority = match self.state.inner._authority.clone() {
            Some(authority) => authority,
            None => {
                return Err(DatabaseGraphOwnerRetirementCompositionRefusalV1 {
                    error: DatabaseOwnerErrorV1::MissingWriteAuthority,
                    database_owner_reservation: Box::new(self),
                    graph_owner_target: Box::new(graph_owner_attachment),
                });
            }
        };
        Ok(StoreRuntimeRetirementTarget::with_owner_attachments(
            self.attachment.binding().clone(),
            authority,
            Box::new(self),
            graph_owner_attachment,
        ))
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

    fn try_into_database_owner_retirement_reservation(
        self: Box<Self>,
    ) -> Result<
        DatabaseOwnerRetirementReservationV1,
        Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>,
    > {
        Ok(*self)
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
