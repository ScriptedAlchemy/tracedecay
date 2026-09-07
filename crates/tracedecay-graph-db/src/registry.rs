use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tracedecay_store::{
    RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1, StoreRuntimeBindingV1,
    StoreShardIdV1, VerifiedStoreLocatorV1,
};

use crate::generation::InlineOnlyGraphGenerationManifestProvider;
use crate::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbLeaseV1, GraphDbOwner,
    GraphDbOwnerAttachmentId, GraphDbOwnerAttachmentV1, GraphDbOwnerId,
    GraphDbRetirementReservationId, GraphDbRetirementTarget, GraphDbRuntimeState,
    GraphFormatVersion, GraphGenerationManifestProvider,
};

use self::identity::{
    binding, entry_binding, require_binding, require_closing, require_owner_attachment,
    require_retiring, validate_registration,
};
use self::path::canonical_graph_database_file;
use self::support::{
    check_registration_request, check_request, open_registered_graph, open_registered_graph_lazy,
    reject_path_alias, retains_fault, status,
};

#[path = "registry/code_graph_namespace.rs"]
mod code_graph_namespace;
#[path = "registry/identity.rs"]
mod identity;
#[path = "registry/path.rs"]
mod path;
#[path = "registry/publication.rs"]
mod publication;
#[path = "registry/publication_support.rs"]
mod publication_support;
#[path = "registry/staging.rs"]
mod staging;
#[path = "registry/support.rs"]
mod support;
#[path = "registry/vector_retirement.rs"]
mod vector_retirement;
pub use code_graph_namespace::{
    CODE_GRAPH_SHARD_NAMESPACE_PREFIX, LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX,
    code_graph_shard_namespace, is_code_graph_shard_namespace,
    is_legacy_per_generation_code_graph_namespace,
};
pub use publication::{GraphPublicationPreparationV1, ProvenGraphPublicationV1};
pub use staging::{
    VerifiedGenerationBatchApply, VerifiedGenerationBatchCommit, VerifiedGenerationBeginV1,
};
pub use vector_retirement::{
    SemanticVectorRetentionAction, SemanticVectorRetentionCensus, SemanticVectorRetentionStep,
    SemanticVectorRetirementReservation,
};

const OPEN_WAIT_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
enum OwnerOpenMode {
    Eager,
    Lazy,
}

/// Existing Grafeo stores receive daemon-lifecycle and request cancellation
/// while opening. A newly created database file formats before cancellation
/// can reject its registry publication, so retries never inherit an empty store.
struct RegisteredGraphOpenCancellation {
    request: Arc<dyn GraphCancellation>,
    lifecycle: Arc<dyn GraphCancellation>,
}

impl GraphCancellation for RegisteredGraphOpenCancellation {
    fn is_cancelled(&self) -> bool {
        self.request.is_cancelled() || self.lifecycle.is_cancelled()
    }
}

#[derive(Clone)]
/// A graph-index open approved by the outer daemon store authority.
///
/// This registry serializes handles within one process. Grafeo also holds an
/// exclusive lock on an open single-file database. Callers retain the daemon
/// profile/store authority so a prospective file has one authorized creator
/// before constructing this derived-index registration.
pub struct GraphDbRegistration {
    pub authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
    pub cancellation: Arc<dyn GraphCancellation>,
    pub lifecycle_cancellation: Arc<dyn GraphCancellation>,
    pub deadline: Instant,
}

impl fmt::Debug for GraphDbRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphDbRegistration")
            .field("authority_lease", &self.authority_lease)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl GraphDbRegistration {
    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.authority_lease.binding()
    }

    pub(crate) fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.authority_lease.verified_locator()
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        self.authority_lease.canonical_path()
    }
}

/// Move-only map-owner admission for one graph runtime.
///
/// The retained Store attachment is map ownership, not an ordinary operation
/// lease. It is consumed only when an absent graph entry is mounted. Each
/// later GraphDb operation still arrives through [`GraphDbRegistration`],
/// whose ordinary Store lease becomes part of the issued GraphDb lease token.
pub struct GraphDbOwnerRegistrationV1 {
    pub operation: GraphDbRegistration,
    pub authority_attachment: Box<dyn RetainedGraphStoreOwnerAttachmentV1>,
}

impl fmt::Debug for GraphDbOwnerRegistrationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphDbOwnerRegistrationV1")
            .field("operation", &self.operation)
            .field("authority_attachment", &self.authority_attachment)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphDbRegistryConfig {
    pub max_open: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphDbRegistryCapacity {
    pub max_open: usize,
    pub occupied: usize,
    pub evictable: usize,
}

impl GraphDbRegistryCapacity {
    #[must_use]
    pub fn available_after_eviction(self) -> usize {
        self.max_open
            .saturating_sub(self.occupied.saturating_sub(self.evictable))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphDbRegistryStatus {
    Opening,
    Ready,
    Closing,
    Closed,
    ResetRequired,
    Corrupt,
    DurabilityUncertain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphDbRetirementOutcome {
    Closed(GraphDbRetirementTarget),
    DurabilityUncertain {
        target: GraphDbRetirementTarget,
        message: String,
    },
    Failed {
        target: GraphDbRetirementTarget,
        error: GraphDbError,
    },
}

#[derive(Debug)]
pub struct GraphDbRetirementCommit {
    outcomes: Vec<GraphDbRetirementOutcome>,
}

impl GraphDbRetirementCommit {
    #[must_use]
    pub fn outcomes(&self) -> &[GraphDbRetirementOutcome] {
        &self.outcomes
    }
}

/// A retirement refusal that returns every exact map-owner target before a
/// native graph close starts.
///
/// Targets retain their original attachment identity, so callers can retry
/// after a cancellation, admission race, or pre-close denial without
/// remounting a graph runtime or allocating a new owner epoch.
#[derive(Debug)]
pub struct GraphDbRetirementRefusal {
    error: GraphDbError,
    targets: Vec<GraphDbRetirementTarget>,
}

impl GraphDbRetirementRefusal {
    fn new(error: GraphDbError, targets: Vec<GraphDbRetirementTarget>) -> Self {
        Self { error, targets }
    }

    #[must_use]
    pub fn error(&self) -> &GraphDbError {
        &self.error
    }

    #[must_use]
    pub fn targets(&self) -> &[GraphDbRetirementTarget] {
        &self.targets
    }

    #[must_use]
    pub fn into_parts(self) -> (GraphDbError, Vec<GraphDbRetirementTarget>) {
        (self.error, self.targets)
    }
}

#[derive(Clone)]
pub struct GraphDbRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    config: GraphDbRegistryConfig,
    manifest_provider: Arc<dyn GraphGenerationManifestProvider>,
    state: Mutex<RegistryState>,
    changed: Condvar,
    /// Live direct-sealed readers. `recover_verified_sealed_snapshot` never
    /// resolves the shared database runtime, so its leases are invisible to
    /// the per-database verified-generation state; replay retirement unions
    /// these locators into its retained set so a sealed store is never
    /// deleted under an active direct-sealed reader.
    direct_sealed_readers: Mutex<
        Vec<(
            crate::lease::GenerationLocator,
            std::sync::Weak<crate::lease::VerifiedGenerationLease>,
        )>,
    >,
    #[cfg(test)]
    retirement_completion_failure: Mutex<Option<GraphDbError>>,
    #[cfg(test)]
    close_completion_failure: Mutex<Option<GraphDbError>>,
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreShardIdV1, RegistryEntry>,
}

enum RegistryEntry {
    Opening {
        authority: OpeningAuthority,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
    },
    Ready {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: GraphDbOwner,
        last_used: Instant,
    },
    Closing {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner_id: GraphDbOwnerId,
        owner_attachment_id: Option<GraphDbOwnerAttachmentId>,
        reservation_id: GraphDbRetirementReservationId,
    },
    Retiring {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner_id: GraphDbOwnerId,
        owner_attachment_id: GraphDbOwnerAttachmentId,
        reservation_id: GraphDbRetirementReservationId,
    },
    Faulted {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: Option<GraphDbOwner>,
        error: GraphDbError,
    },
}

/// Store map-owner authority held by an `Opening` slot.
///
/// The `Opening` slot stays in the entry map for the entire native open, so
/// the singleflight for one store identity lives under this registry's own
/// lock: a concurrent same-identity owner attacher observes a typed
/// `Conflict` instead of mounting a second native open, ordinary lease
/// registrations wait for the one in-flight open, and capacity accounting
/// keeps the materializing runtime occupied.
enum OpeningAuthority {
    /// Attachment parked before the native open starts (the capacity
    /// eviction settles while the slot is already claimed).
    Parked(Box<dyn RetainedGraphStoreOwnerAttachmentV1>),
    /// The attachment moved into the in-flight native open of the mounting
    /// call; the slot is settled or released when that open returns.
    NativeOpenInFlight,
}

/// Releases an in-flight `Opening` slot when its mounting call exits without
/// writing terminal truth — plain open failures and unwinds. Terminal
/// settlements (`Ready`/`Faulted`) overwrite the slot first, which makes this
/// guard a no-op: `remove_opening` verifies identity and only ever removes
/// `Opening` entries.
struct OpeningMountGuard<'registry> {
    registry: &'registry GraphDbRegistry,
    shard_id: StoreShardIdV1,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    path: PathBuf,
    expected_format: GraphFormatVersion,
}

impl Drop for OpeningMountGuard<'_> {
    fn drop(&mut self) {
        // Best-effort by design: `Drop` cannot surface errors, and a poisoned
        // registry lock already fails every registry operation, so an
        // `Opening` slot leaked behind it is unreachable anyway.
        let _ = self.registry.remove_opening(
            &self.shard_id,
            &self.binding,
            &self.verified_locator,
            &self.path,
            self.expected_format,
        );
    }
}

struct Eviction {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    path: PathBuf,
    expected_format: GraphFormatVersion,
    owner: GraphDbOwner,
    last_used: Instant,
    close_reservation: OwnerCloseReservation,
}

type EvictionFailure = Box<(Eviction, GraphDbError)>;

enum OwnerCloseReservation {
    Unleased {
        reservation_id: GraphDbRetirementReservationId,
    },
    OwnerAttachment {
        target: Box<GraphDbRetirementTarget>,
        reservation_id: GraphDbRetirementReservationId,
    },
}

impl Eviction {
    fn owner_id(&self) -> GraphDbOwnerId {
        self.owner.owner_id()
    }

    fn owner_attachment_id(&self) -> Option<GraphDbOwnerAttachmentId> {
        match &self.close_reservation {
            OwnerCloseReservation::Unleased { .. } => None,
            OwnerCloseReservation::OwnerAttachment { target, .. } => Some(target.attachment_id()),
        }
    }

    fn reservation_id(&self) -> GraphDbRetirementReservationId {
        match &self.close_reservation {
            OwnerCloseReservation::Unleased { reservation_id }
            | OwnerCloseReservation::OwnerAttachment { reservation_id, .. } => *reservation_id,
        }
    }

    fn restore_owner(&self) -> Result<(), GraphDbError> {
        match &self.close_reservation {
            OwnerCloseReservation::Unleased { reservation_id } => {
                self.owner.restore_unleased_close(*reservation_id)
            }
            OwnerCloseReservation::OwnerAttachment {
                target,
                reservation_id,
            } => self.owner.restore_owner_attachment(target, *reservation_id),
        }
    }

    fn restore_before_native_close(&self) -> Result<(), GraphDbError> {
        match &self.close_reservation {
            OwnerCloseReservation::Unleased { reservation_id } => {
                self.owner.restore_unleased_close(*reservation_id)
            }
            OwnerCloseReservation::OwnerAttachment {
                target,
                reservation_id,
            } => self
                .owner
                .restore_owner_attachment_before_native_close(target, *reservation_id),
        }
    }

    fn begin_close(&self) -> Result<(), GraphDbError> {
        match &self.close_reservation {
            OwnerCloseReservation::Unleased { reservation_id } => {
                self.owner.begin_unleased_close(*reservation_id)
            }
            OwnerCloseReservation::OwnerAttachment {
                target,
                reservation_id,
            } => self
                .owner
                .begin_owner_attachment_close(target, *reservation_id),
        }
    }

    fn finish_close(&self, result: &Result<(), GraphDbError>) -> Result<(), GraphDbError> {
        match &self.close_reservation {
            OwnerCloseReservation::Unleased { reservation_id } => {
                self.owner.finish_unleased_close(*reservation_id, result)
            }
            OwnerCloseReservation::OwnerAttachment { reservation_id, .. } => self
                .owner
                .finish_owner_attachment_close(*reservation_id, result),
        }
    }

    fn force_terminal_after_close(&self, result: &Result<(), GraphDbError>) {
        self.owner.force_terminal_after_close(result);
    }
}

enum CloseReservation {
    Absent,
    Closing(Box<Eviction>),
}

/// An all-or-none reservation of exact ready graph runtimes for retirement.
///
/// It owns every pre-close transition. Dropping an uncommitted reservation
/// restores all of its entries; once close starts, each result is irreversible
/// and is reported by [`GraphDbRetirementCommit`].
pub struct GraphDbRetirementReservation {
    registry: GraphDbRegistry,
    pending: Vec<Eviction>,
    armed: bool,
}

impl GraphDbRetirementReservation {
    pub fn commit(
        &mut self,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> Result<GraphDbRetirementCommit, GraphDbRetirementRefusal> {
        if !self.armed {
            return Err(GraphDbRetirementRefusal::new(
                GraphDbError::conflict("registry.commit"),
                Vec::new(),
            ));
        }
        if let Err(error) = check_request(cancellation.as_ref(), deadline, "registry.retirement") {
            return Err(self.restore_for_retry(error));
        }
        // Establish every exact target's close transition before the first
        // native close. From the boundary below onward, every target receives
        // an irreversible terminal outcome; none can return to ready.
        let mut pending = std::mem::take(&mut self.pending).into_iter();
        let mut closing = Vec::new();
        while let Some(eviction) = pending.next() {
            let target = match &eviction.close_reservation {
                OwnerCloseReservation::OwnerAttachment { target, .. } => target.as_ref().clone(),
                OwnerCloseReservation::Unleased { .. } => {
                    self.pending = closing.into_iter().map(|(eviction, _)| eviction).collect();
                    self.pending.push(eviction);
                    self.pending.extend(pending);
                    return Err(self.restore_for_retry(GraphDbError::unavailable(
                        "graph retirement reservation lost its owner attachment",
                    )));
                }
            };
            if let Err(error) = eviction.begin_close() {
                self.pending = closing.into_iter().map(|(eviction, _)| eviction).collect();
                self.pending.push(eviction);
                self.pending.extend(pending);
                return Err(self.restore_for_retry(error));
            }
            closing.push((eviction, target));
        }

        let mut outcomes = Vec::with_capacity(closing.len());
        for (eviction, target) in closing {
            let close_result = eviction.owner.close();
            let terminalization_failure = match eviction.finish_close(&close_result) {
                Ok(()) => self
                    .registry
                    .complete_retirement_close(eviction, close_result.clone())
                    .err(),
                Err(error) => {
                    eviction.force_terminal_after_close(&close_result);
                    Some(Box::new((eviction, error)))
                }
            };
            if let Some(failure) = terminalization_failure {
                let (eviction, error) = *failure;
                let terminal_error = self.registry.retain_post_close_retirement_fault(
                    eviction,
                    &close_result,
                    error,
                );
                outcomes.push(retirement_outcome(target, Err(terminal_error)));
            } else {
                outcomes.push(retirement_outcome(target, close_result));
            }
        }
        self.armed = false;
        Ok(GraphDbRetirementCommit { outcomes })
    }

    /// Cancels this pre-close reservation and returns its original exact
    /// targets for a later attempt. Once native close starts, `commit` instead
    /// reports terminal outcomes and no reusable target is fabricated.
    pub fn cancel(
        &mut self,
    ) -> std::result::Result<Vec<GraphDbRetirementTarget>, GraphDbRetirementRefusal> {
        if !self.armed {
            return Err(GraphDbRetirementRefusal::new(
                GraphDbError::conflict("registry.cancel"),
                Vec::new(),
            ));
        }
        let targets = self.retry_targets();
        match self.restore_pending() {
            Ok(()) => Ok(targets),
            Err(error) => Err(GraphDbRetirementRefusal::new(error, targets)),
        }
    }

    fn retry_targets(&self) -> Vec<GraphDbRetirementTarget> {
        self.pending
            .iter()
            .filter_map(|eviction| match &eviction.close_reservation {
                OwnerCloseReservation::OwnerAttachment { target, .. } => {
                    Some(target.as_ref().clone())
                }
                OwnerCloseReservation::Unleased { .. } => None,
            })
            .collect()
    }

    fn restore_for_retry(&mut self, refusal: GraphDbError) -> GraphDbRetirementRefusal {
        let targets = self.retry_targets();
        match self.restore_pending() {
            Ok(()) => GraphDbRetirementRefusal::new(refusal, targets),
            Err(error) => GraphDbRetirementRefusal::new(error, targets),
        }
    }

    fn restore_pending(&mut self) -> Result<(), GraphDbError> {
        while let Some(eviction) = self.pending.pop() {
            if let Err(failure) = self.registry.restore_retiring(eviction) {
                let (eviction, error) = *failure;
                self.pending.push(eviction);
                return Err(error);
            }
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for GraphDbRetirementReservation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for eviction in self.pending.drain(..) {
            if let Err(failure) = self.registry.restore_retiring(eviction) {
                let (eviction, error) = *failure;
                self.registry
                    .retain_retirement_restore_fault(eviction, error);
            }
        }
        self.armed = false;
        self.registry.inner.changed.notify_all();
    }
}

impl GraphDbRegistry {
    pub fn new(config: GraphDbRegistryConfig) -> Result<Self, GraphDbError> {
        Self::new_with_manifest_provider(
            config,
            Arc::new(InlineOnlyGraphGenerationManifestProvider),
        )
    }

    pub fn new_with_manifest_provider(
        config: GraphDbRegistryConfig,
        manifest_provider: Arc<dyn GraphGenerationManifestProvider>,
    ) -> Result<Self, GraphDbError> {
        if config.max_open == 0 {
            return Err(GraphDbError::invalid(
                "graph registry max_open must be greater than zero",
            ));
        }
        Ok(Self {
            inner: Arc::new(RegistryInner {
                config,
                manifest_provider,
                state: Mutex::new(RegistryState::default()),
                changed: Condvar::new(),
                direct_sealed_readers: Mutex::new(Vec::new()),
                #[cfg(test)]
                retirement_completion_failure: Mutex::new(None),
                #[cfg(test)]
                close_completion_failure: Mutex::new(None),
            }),
        })
    }

    /// Records a live direct-sealed reader so replay retirement keeps its
    /// generation's sealed store on disk. Dead entries are pruned in place;
    /// liveness is the lease's strong count, so no drop hook is required.
    pub(crate) fn track_direct_sealed_reader(
        &self,
        lease: &Arc<crate::lease::VerifiedGenerationLease>,
    ) -> Result<(), GraphDbError> {
        let mut readers = self.inner.direct_sealed_readers.lock().map_err(|_| {
            GraphDbError::unavailable("direct-sealed reader table lock is poisoned")
        })?;
        readers.retain(|(_, weak)| weak.strong_count() > 0);
        readers.push((lease.locator.clone(), Arc::downgrade(lease)));
        Ok(())
    }

    /// Locators of generations currently held by a live direct-sealed reader.
    pub(crate) fn live_direct_sealed_locators(
        &self,
    ) -> Result<std::collections::BTreeSet<crate::lease::GenerationLocator>, GraphDbError> {
        let mut readers = self.inner.direct_sealed_readers.lock().map_err(|_| {
            GraphDbError::unavailable("direct-sealed reader table lock is poisoned")
        })?;
        readers.retain(|(_, weak)| weak.strong_count() > 0);
        Ok(readers.iter().map(|(locator, _)| locator.clone()).collect())
    }

    #[cfg(test)]
    fn inject_retirement_completion_failure(&self, error: GraphDbError) {
        *self
            .inner
            .retirement_completion_failure
            .lock()
            .expect("retirement completion test fault lock must not be poisoned") = Some(error);
    }

    #[cfg(test)]
    fn inject_close_completion_failure(&self, error: GraphDbError) {
        *self
            .inner
            .close_completion_failure
            .lock()
            .expect("close completion test fault lock must not be poisoned") = Some(error);
    }

    #[cfg(test)]
    fn inject_close_finish_failure(
        &self,
        registration: &GraphDbRegistration,
        error: GraphDbError,
    ) -> Result<(), GraphDbError> {
        let state = self.state_lock()?;
        let Some(RegistryEntry::Ready { owner, .. }) =
            state.entries.get(&registration.binding().shard_id)
        else {
            return Err(GraphDbError::conflict(
                "registry.inject_close_finish_failure",
            ));
        };
        owner.inject_close_finish_failure(error);
        Ok(())
    }

    /// Opens (or joins) the registered runtime and returns one client lease.
    ///
    /// Replayed code graphs must read through [`crate::VerifiedGraphSnapshot`]
    /// via the publication surface; the native runtime lease is for graphs
    /// whose state is itself the authority (for example daemon-owned session
    /// relation graphs) and for direct storage tests.
    #[hotpath::measure(label = "graph_db.registry.resolve", impl_type = "GraphDbRegistry")]
    pub fn resolve(
        &self,
        registration: GraphDbRegistration,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        check_request(
            registration.cancellation.as_ref(),
            registration.deadline,
            "registry.resolve",
        )?;
        validate_registration(&registration)?;
        let path = canonical_graph_database_file(registration.canonical_path())?;
        let expected_format = GraphFormatVersion::current();
        let binding = registration.binding().clone();
        let verified_locator = registration.verified_locator().clone();
        let shard_id = binding.shard_id.clone();

        loop {
            let mut state = self.state_lock()?;
            reject_path_alias(&state, &binding, &verified_locator, &path, expected_format)?;

            let ready_fault = match state.entries.get_mut(&shard_id) {
                Some(RegistryEntry::Ready {
                    binding: registered_binding,
                    verified_locator: registered_locator,
                    path: registered_path,
                    expected_format: registered_format,
                    owner,
                    last_used,
                    ..
                }) => {
                    require_binding(
                        (
                            registered_binding,
                            registered_locator,
                            registered_path,
                            *registered_format,
                        ),
                        (&binding, &verified_locator, &path, expected_format),
                    )?;
                    match owner.runtime_state() {
                        GraphDbRuntimeState::Ready => {
                            *last_used = Instant::now();
                            return owner.issue_registered_lease(&registration);
                        }
                        GraphDbRuntimeState::Closed => Some(GraphDbError::Closed),
                        GraphDbRuntimeState::DurabilityUncertain => {
                            Some(GraphDbError::DurabilityUncertain {
                                message: "registered graph handle has uncertain durability"
                                    .to_owned(),
                            })
                        }
                    }
                }
                Some(RegistryEntry::Faulted {
                    binding: registered_binding,
                    verified_locator: registered_locator,
                    path: registered_path,
                    expected_format: registered_format,
                    error,
                    ..
                }) => {
                    require_binding(
                        (
                            registered_binding,
                            registered_locator,
                            registered_path,
                            *registered_format,
                        ),
                        (&binding, &verified_locator, &path, expected_format),
                    )?;
                    return Err(error.clone());
                }
                Some(RegistryEntry::Opening {
                    binding: registered_binding,
                    verified_locator: registered_locator,
                    path: registered_path,
                    expected_format: registered_format,
                    ..
                }) => {
                    require_binding(
                        (
                            registered_binding,
                            registered_locator,
                            registered_path,
                            *registered_format,
                        ),
                        (&binding, &verified_locator, &path, expected_format),
                    )?;
                    check_request(
                        registration.cancellation.as_ref(),
                        registration.deadline,
                        "registry.resolve.wait_opening",
                    )?;
                    let (next, _) = self
                        .inner
                        .changed
                        .wait_timeout(state, OPEN_WAIT_POLL)
                        .map_err(|_| {
                            GraphDbError::unavailable("graph registry wait lock is poisoned")
                        })?;
                    drop(next);
                    continue;
                }
                // A `Closing` entry is always an in-flight close that settles
                // within its owning call (entry removed, restored ready, or
                // retained as a terminal fault). Wait for that settlement like
                // an in-flight open instead of fabricating a conflict for the
                // same owner's next operation.
                Some(RegistryEntry::Closing {
                    binding: registered_binding,
                    verified_locator: registered_locator,
                    path: registered_path,
                    expected_format: registered_format,
                    ..
                }) => {
                    require_binding(
                        (
                            registered_binding,
                            registered_locator,
                            registered_path,
                            *registered_format,
                        ),
                        (&binding, &verified_locator, &path, expected_format),
                    )?;
                    check_request(
                        registration.cancellation.as_ref(),
                        registration.deadline,
                        "registry.resolve.wait_closing",
                    )?;
                    let (next, _) = self
                        .inner
                        .changed
                        .wait_timeout(state, OPEN_WAIT_POLL)
                        .map_err(|_| {
                            GraphDbError::unavailable("graph registry wait lock is poisoned")
                        })?;
                    drop(next);
                    continue;
                }
                // A `Retiring` entry may be an armed pre-close retirement
                // reservation held across calls; denying new resolution is its
                // all-or-none contract, so it stays a typed conflict.
                Some(RegistryEntry::Retiring {
                    binding: registered_binding,
                    verified_locator: registered_locator,
                    path: registered_path,
                    expected_format: registered_format,
                    ..
                }) => {
                    require_binding(
                        (
                            registered_binding,
                            registered_locator,
                            registered_path,
                            *registered_format,
                        ),
                        (&binding, &verified_locator, &path, expected_format),
                    )?;
                    return Err(GraphDbError::conflict("registry.resolve"));
                }
                None => {
                    return Err(GraphDbError::unavailable(
                        "graph runtime is not mounted by its owner attachment",
                    ));
                }
            };

            if let Some(error) = ready_fault {
                let Some(RegistryEntry::Ready {
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    ..
                }) = state.entries.remove(&shard_id)
                else {
                    return Err(GraphDbError::unavailable(
                        "ready graph runtime disappeared before terminal fault retention",
                    ));
                };
                state.entries.insert(
                    shard_id.clone(),
                    RegistryEntry::Faulted {
                        binding,
                        verified_locator,
                        path,
                        expected_format,
                        owner: Some(owner),
                        error: error.clone(),
                    },
                );
                self.inner.changed.notify_all();
                return Err(error);
            }
        }
    }

    /// Reports whether any entry — `Ready`, `Opening`, `Closing`, `Retiring`,
    /// or `Faulted` — currently occupies this shard's slot.
    ///
    /// A lease-only consumer of a shared physical shard (for example the code
    /// graph, which never attaches its own map owner; see
    /// [`Self::resolve_owner_attachment`]) can use this to detect that the
    /// shard's owning attachment was retired out from under it and that
    /// re-attaching is needed before the next lease attempt, instead of
    /// discovering the gap only as an `unavailable` error deep inside a
    /// publish call.
    pub fn shard_is_registered(&self, shard_id: &StoreShardIdV1) -> Result<bool, GraphDbError> {
        let state = self.state_lock()?;
        Ok(state.entries.contains_key(shard_id))
    }

    /// Number of native Grafeo engines currently resident behind registered
    /// owners. Lazy authority handles do not contribute until first use.
    pub fn resident_engine_count(&self) -> Result<usize, GraphDbError> {
        let state = self.state_lock()?;
        let mut count = 0_usize;
        for entry in state.entries.values() {
            if let RegistryEntry::Ready { owner, .. } = entry
                && owner.engine_is_open()?
            {
                count = count.checked_add(1).ok_or_else(|| {
                    GraphDbError::budget_exhausted_count(GraphBudgetKind::Capacity, usize::MAX)
                })?;
            }
        }
        Ok(count)
    }

    /// Mounts an absent graph runtime through its exact Store map-owner
    /// attachment and returns the matching native graph map-owner attachment.
    ///
    /// This is deliberately the only entry-creation path. Ordinary
    /// [`GraphDbRegistration`] values can resolve an already mounted entry,
    /// but cannot turn an operation lease into map ownership.
    ///
    /// The `Opening` slot is checked and held under this registry's own lock
    /// for the entire native open (one singleflight per store identity): a
    /// concurrent same-identity attacher observes a typed
    /// [`GraphDbError::Conflict`] instead of running a second identical open,
    /// ordinary lease registrations wait for the one in-flight open, and
    /// capacity accounting keeps the materializing runtime occupied. A plain
    /// open failure releases the slot for remount; `ResetRequired`,
    /// `Corrupt`, and `DurabilityUncertain` retain a terminal `Faulted` slot.
    #[hotpath::measure(label = "graph_db.registry.attach", impl_type = "GraphDbRegistry")]
    pub fn resolve_owner_attachment(
        &self,
        registration: GraphDbOwnerRegistrationV1,
    ) -> Result<GraphDbOwnerAttachmentV1, GraphDbError> {
        self.resolve_owner_attachment_with_mode(registration, OwnerOpenMode::Eager)
    }

    /// Mounts the exact map owner without opening its native engine.
    ///
    /// The first graph operation materializes the persistent engine through
    /// the same validated open and deterministic corruption-quarantine path.
    /// Durable relational/session owners use this route so merely retaining
    /// their authority does not replay a corpus-sized graph at daemon startup.
    pub fn resolve_lazy_owner_attachment(
        &self,
        registration: GraphDbOwnerRegistrationV1,
    ) -> Result<GraphDbOwnerAttachmentV1, GraphDbError> {
        self.resolve_owner_attachment_with_mode(registration, OwnerOpenMode::Lazy)
    }

    fn resolve_owner_attachment_with_mode(
        &self,
        registration: GraphDbOwnerRegistrationV1,
        open_mode: OwnerOpenMode,
    ) -> Result<GraphDbOwnerAttachmentV1, GraphDbError> {
        let GraphDbOwnerRegistrationV1 {
            operation,
            authority_attachment,
        } = registration;
        check_request(
            operation.cancellation.as_ref(),
            operation.deadline,
            "registry.attach",
        )?;
        validate_registration(&operation)?;
        require_owner_attachment(&operation, authority_attachment.as_ref())?;
        let path = canonical_graph_database_file(operation.canonical_path())?;
        let expected_format = GraphFormatVersion::current();
        let binding = operation.binding().clone();
        let verified_locator = operation.verified_locator().clone();
        let shard_id = binding.shard_id.clone();

        {
            let mut state = loop {
                let state = self.state_lock()?;
                reject_path_alias(&state, &binding, &verified_locator, &path, expected_format)?;
                match state.entries.get(&shard_id) {
                    None => break state,
                    // An in-flight close settles within its owning call; wait
                    // for it and remount instead of fabricating a conflict for
                    // the same owner's next attach.
                    Some(RegistryEntry::Closing {
                        binding: registered_binding,
                        verified_locator: registered_locator,
                        path: registered_path,
                        expected_format: registered_format,
                        ..
                    }) => {
                        require_binding(
                            (
                                registered_binding,
                                registered_locator,
                                registered_path,
                                *registered_format,
                            ),
                            (&binding, &verified_locator, &path, expected_format),
                        )?;
                        check_request(
                            operation.cancellation.as_ref(),
                            operation.deadline,
                            "registry.attach.wait_closing",
                        )?;
                        let (next, _) = self
                            .inner
                            .changed
                            .wait_timeout(state, OPEN_WAIT_POLL)
                            .map_err(|_| {
                                GraphDbError::unavailable("graph registry wait lock is poisoned")
                            })?;
                        drop(next);
                    }
                    Some(RegistryEntry::Ready {
                        binding: registered_binding,
                        verified_locator: registered_locator,
                        path: registered_path,
                        expected_format: registered_format,
                        ..
                    })
                    | Some(RegistryEntry::Opening {
                        binding: registered_binding,
                        verified_locator: registered_locator,
                        path: registered_path,
                        expected_format: registered_format,
                        ..
                    })
                    | Some(RegistryEntry::Retiring {
                        binding: registered_binding,
                        verified_locator: registered_locator,
                        path: registered_path,
                        expected_format: registered_format,
                        ..
                    }) => {
                        require_binding(
                            (
                                registered_binding,
                                registered_locator,
                                registered_path,
                                *registered_format,
                            ),
                            (&binding, &verified_locator, &path, expected_format),
                        )?;
                        // Same-identity attach against a mounted (or still
                        // materializing) runtime: no native open runs, so a
                        // profile can separate these hits from full opens.
                        hotpath::gauge!("graph_db.registry.attach.already_mounted").inc(1.0);
                        return Err(GraphDbError::conflict("registry.resolve_owner_attachment"));
                    }
                    Some(RegistryEntry::Faulted {
                        binding: registered_binding,
                        verified_locator: registered_locator,
                        path: registered_path,
                        expected_format: registered_format,
                        error,
                        ..
                    }) => {
                        require_binding(
                            (
                                registered_binding,
                                registered_locator,
                                registered_path,
                                *registered_format,
                            ),
                            (&binding, &verified_locator, &path, expected_format),
                        )?;
                        return Err(error.clone());
                    }
                }
            };
            let eviction =
                reserve_capacity_eviction(&mut state, self.inner.config.max_open, &shard_id)?;
            state.entries.insert(
                shard_id.clone(),
                RegistryEntry::Opening {
                    authority: OpeningAuthority::Parked(authority_attachment),
                    binding: binding.clone(),
                    verified_locator: verified_locator.clone(),
                    path: path.clone(),
                    expected_format,
                },
            );
            drop(state);
            if let Some(eviction) = eviction
                && let Err(error) = self.finish_eviction(eviction)
            {
                self.remove_opening(
                    &shard_id,
                    &binding,
                    &verified_locator,
                    &path,
                    expected_format,
                )?;
                return Err(error);
            }
        }

        // Move the parked Store attachment into the native open while the
        // `Opening` slot itself stays mounted: the in-flight open must remain
        // visible under this registry's own lock so same-identity attachers
        // conflict, lease waiters wait, and capacity stays claimed.
        let authority_attachment = {
            let mut state = self.state_lock()?;
            let Some(RegistryEntry::Opening { authority, .. }) = state.entries.get_mut(&shard_id)
            else {
                return Err(GraphDbError::unavailable(
                    "graph owner attachment mount disappeared before completion",
                ));
            };
            match std::mem::replace(authority, OpeningAuthority::NativeOpenInFlight) {
                OpeningAuthority::Parked(authority_attachment) => authority_attachment,
                OpeningAuthority::NativeOpenInFlight => {
                    return Err(GraphDbError::unavailable(
                        "graph owner attachment mount is already opening natively",
                    ));
                }
            }
        };
        if matches!(open_mode, OwnerOpenMode::Eager) {
            hotpath::gauge!("graph_db.registry.attach.full_open").inc(1.0);
        } else {
            hotpath::gauge!("graph_db.registry.attach.lazy").inc(1.0);
        }
        // Dropped on every exit below: releases the in-flight slot after a
        // plain open failure or unwind; a no-op once `Ready`/`Faulted` truth
        // has overwritten it.
        let _opening_slot_guard = OpeningMountGuard {
            registry: self,
            shard_id: shard_id.clone(),
            binding: binding.clone(),
            verified_locator: verified_locator.clone(),
            path: path.clone(),
            expected_format,
        };
        let opened = match open_mode {
            OwnerOpenMode::Eager => {
                open_registered_graph(&path, expected_format, &operation, authority_attachment)
            }
            OwnerOpenMode::Lazy => {
                open_registered_graph_lazy(&path, expected_format, &operation, authority_attachment)
            }
        };
        let mut state = self.state_lock()?;
        let slot_is_this_mount = state.entries.get(&shard_id).is_some_and(|entry| {
            matches!(
                entry,
                RegistryEntry::Opening {
                    authority: OpeningAuthority::NativeOpenInFlight,
                    ..
                }
            ) && require_binding(
                identity::binding(entry),
                (&binding, &verified_locator, &path, expected_format),
            )
            .is_ok()
        });
        if !slot_is_this_mount {
            drop(state);
            let vanished = GraphDbError::unavailable(
                "graph owner attachment mount disappeared before completion",
            );
            return match opened {
                // The freshly opened native handle has no registry slot left
                // to publish into; close it instead of leaking the exclusive
                // native lock until process exit.
                Ok(owner) => match owner.close() {
                    Ok(()) => Err(vanished),
                    Err(close_error) => Err(crate::error::rollback_failure(
                        "close unpublishable graph owner mount",
                        vanished,
                        close_error,
                    )),
                },
                Err(_) => Err(vanished),
            };
        }
        match opened {
            Ok(owner) => {
                let attachment = match owner.issue_owner_attachment(
                    binding.clone(),
                    verified_locator.clone(),
                    path.clone(),
                ) {
                    Ok(attachment) => attachment,
                    Err(error) => {
                        // The slot guard removes the in-flight `Opening`
                        // entry and wakes waiters once `state` unlocks.
                        return Err(error);
                    }
                };
                state.entries.insert(
                    shard_id,
                    RegistryEntry::Ready {
                        binding,
                        verified_locator,
                        path,
                        expected_format,
                        owner,
                        last_used: Instant::now(),
                    },
                );
                self.inner.changed.notify_all();
                Ok(attachment)
            }
            Err(error) => {
                if retains_fault(&error) {
                    state.entries.insert(
                        shard_id,
                        RegistryEntry::Faulted {
                            binding,
                            verified_locator,
                            path,
                            expected_format,
                            owner: None,
                            error: error.clone(),
                        },
                    );
                    self.inner.changed.notify_all();
                    Err(error)
                } else {
                    // The slot guard releases the `Opening` entry for remount
                    // and wakes waiters once `state` unlocks.
                    Err(error)
                }
            }
        }
    }

    fn retain_verification_fault_for_lease(
        &self,
        database: &GraphDbLeaseV1,
        error: &GraphDbError,
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let owner_id = database.owner_id();
        let mut shard_id = None;
        for (candidate, entry) in &state.entries {
            match entry {
                RegistryEntry::Ready { owner, .. } if owner.owns_lease(database) => {
                    shard_id = Some(candidate.clone());
                    break;
                }
                RegistryEntry::Closing {
                    owner_id: current, ..
                }
                | RegistryEntry::Retiring {
                    owner_id: current, ..
                } if *current == owner_id => {
                    return Err(GraphDbError::conflict(
                        "registry.retain_verification_fault_for_lease",
                    ));
                }
                RegistryEntry::Faulted {
                    owner: Some(owner),
                    error: retained_error,
                    ..
                } if owner.owns_lease(database) => {
                    return Err(retained_error.clone());
                }
                _ => {}
            }
        }
        let Some(shard_id) = shard_id else {
            return Err(GraphDbError::unavailable(
                "graph lease disappeared before verification fault retention",
            ));
        };
        let Some(RegistryEntry::Ready {
            binding,
            verified_locator,
            path,
            expected_format,
            owner,
            ..
        }) = state.entries.remove(&shard_id)
        else {
            return Err(GraphDbError::unavailable(
                "ready graph verification entry disappeared before fault retention",
            ));
        };
        state.entries.insert(
            shard_id,
            RegistryEntry::Faulted {
                binding,
                verified_locator,
                path,
                expected_format,
                owner: Some(owner),
                error: error.clone(),
            },
        );
        self.inner.changed.notify_all();
        Ok(())
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn reopen(
        &self,
        registration: GraphDbOwnerRegistrationV1,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        check_request(
            registration.operation.cancellation.as_ref(),
            registration.operation.deadline,
            "registry.reopen",
        )?;
        validate_registration(&registration.operation)?;
        let path = canonical_graph_database_file(registration.operation.canonical_path())?;
        let expected_format = GraphFormatVersion::current();
        if let CloseReservation::Closing(reservation) = self.reserve_close(
            registration.operation.binding(),
            registration.operation.verified_locator(),
            Some((&path, expected_format)),
            true,
        )? {
            if let Err(error) = check_request(
                registration.operation.cancellation.as_ref(),
                registration.operation.deadline,
                "registry.reopen.evict",
            ) {
                self.restore_ready(*reservation)?;
                return Err(error);
            }
            self.finish_eviction(*reservation)?;
        }
        let operation = registration.operation.clone();
        let owner_attachment = self.resolve_owner_attachment(registration)?;
        let lease = self.resolve(operation)?;
        drop(owner_attachment);
        Ok(lease)
    }

    /// Reopens a leased runtime only for direct storage tests and developer evals.
    ///
    /// Production recovery returns a verified snapshot through
    /// [`GraphDbRegistry::recover_verified_snapshot`].
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn reopen_for_harness(
        &self,
        registration: GraphDbOwnerRegistrationV1,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        self.reopen(registration)
    }

    #[hotpath::measure(label = "graph_db.registry.close", impl_type = "GraphDbRegistry")]
    pub fn close(&self, registration: &GraphDbRegistration) -> Result<bool, GraphDbError> {
        check_request(
            registration.cancellation.as_ref(),
            registration.deadline,
            "registry.close",
        )?;
        validate_registration(registration)?;
        let path = canonical_graph_database_file(registration.canonical_path())?;
        let reservation = match self.reserve_close(
            registration.binding(),
            registration.verified_locator(),
            Some((&path, GraphFormatVersion::current())),
            true,
        )? {
            CloseReservation::Absent => return Ok(false),
            CloseReservation::Closing(reservation) => reservation,
        };
        if let Err(error) = check_request(
            registration.cancellation.as_ref(),
            registration.deadline,
            "registry.close.reserved",
        ) {
            self.restore_ready(*reservation)?;
            return Err(error);
        }
        self.finish_eviction(*reservation)?;
        Ok(true)
    }

    /// Closes an already-retained graph by its complete store identity.
    ///
    /// Destructive lifecycle recovery uses this after an external actor has
    /// removed the store root. The registry entry remains the path/format
    /// authority; this operation never reconstructs or canonicalizes a path
    /// from the missing filesystem tree.
    pub fn close_retained(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
    ) -> Result<bool, GraphDbError> {
        self.close_retained_inner(binding, verified_locator, true)
    }

    /// Releases the exclusive Grafeo writer only after every client and exact
    /// map-owner attachment has drained. Retained client leases are an
    /// identity-bearing close blocker even during shutdown.
    pub fn close_retained_for_shutdown(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
    ) -> Result<bool, GraphDbError> {
        self.close_retained_inner(binding, verified_locator, true)
    }

    #[hotpath::measure(
        label = "graph_db.registry.close_retained",
        impl_type = "GraphDbRegistry"
    )]
    fn close_retained_inner(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        require_unleased: bool,
    ) -> Result<bool, GraphDbError> {
        let reservation =
            match self.reserve_close(binding, verified_locator, None, require_unleased)? {
                CloseReservation::Absent => return Ok(false),
                CloseReservation::Closing(reservation) => reservation,
            };
        self.finish_eviction(*reservation)?;
        Ok(true)
    }

    #[hotpath::measure(label = "graph_db.registry.evict", impl_type = "GraphDbRegistry")]
    pub fn evict_idle(
        &self,
        minimum_idle: Duration,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> Result<Vec<StoreRuntimeBindingV1>, GraphDbError> {
        check_request(cancellation.as_ref(), deadline, "registry.evict_idle")?;
        let now = Instant::now();
        let evictions = {
            let mut state = self.state_lock()?;
            let shards = state
                .entries
                .iter()
                .filter_map(|(shard_id, entry)| match entry {
                    RegistryEntry::Ready {
                        owner, last_used, ..
                    } if owner.is_unleased()
                        && now.saturating_duration_since(*last_used) >= minimum_idle =>
                    {
                        Some(shard_id.clone())
                    }
                    RegistryEntry::Opening { .. }
                    | RegistryEntry::Closing { .. }
                    | RegistryEntry::Retiring { .. }
                    | RegistryEntry::Ready { .. }
                    | RegistryEntry::Faulted { .. } => None,
                })
                .collect::<Vec<_>>();
            let mut evictions = Vec::with_capacity(shards.len());
            for shard_id in shards {
                let Some(RegistryEntry::Ready {
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    last_used,
                }) = state.entries.remove(&shard_id)
                else {
                    continue;
                };
                let reservation_id = match owner.reserve_unleased_close() {
                    Ok(reservation_id) => reservation_id,
                    Err(error) => {
                        state.entries.insert(
                            shard_id,
                            RegistryEntry::Ready {
                                binding,
                                verified_locator,
                                path,
                                expected_format,
                                owner,
                                last_used,
                            },
                        );
                        return Err(error);
                    }
                };
                let eviction = Eviction {
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    last_used,
                    close_reservation: OwnerCloseReservation::Unleased { reservation_id },
                };
                state.entries.insert(
                    shard_id,
                    RegistryEntry::Closing {
                        binding: eviction.binding.clone(),
                        verified_locator: eviction.verified_locator.clone(),
                        path: eviction.path.clone(),
                        expected_format: eviction.expected_format,
                        owner_id: eviction.owner_id(),
                        owner_attachment_id: None,
                        reservation_id,
                    },
                );
                evictions.push(eviction);
            }
            evictions
        };

        let mut evicted = Vec::with_capacity(evictions.len());
        let mut first_error = None;
        for eviction in evictions {
            let binding = eviction.binding.clone();
            match self.finish_eviction(eviction) {
                Ok(()) => evicted.push(binding),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        self.inner.changed.notify_all();
        if let Some(error) = first_error {
            Err(error)
        } else {
            evicted.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
            Ok(evicted)
        }
    }

    pub fn status(
        &self,
        registration: &GraphDbRegistration,
    ) -> Result<Option<GraphDbRegistryStatus>, GraphDbError> {
        validate_registration(registration)?;
        let canonical_path = canonical_graph_database_file(registration.canonical_path())?;
        let state = self.state_lock()?;
        let Some(entry) = state.entries.get(&registration.binding().shard_id) else {
            return Ok(None);
        };
        require_binding(
            binding(entry),
            (
                registration.binding(),
                registration.verified_locator(),
                &canonical_path,
                GraphFormatVersion::current(),
            ),
        )?;
        Ok(Some(status(entry)))
    }

    /// Canonical physical-open headroom, including ready owners the registry
    /// can close itself before admitting another shard.
    pub fn capacity(&self) -> Result<GraphDbRegistryCapacity, GraphDbError> {
        let state = self.state_lock()?;
        let occupied = state
            .entries
            .values()
            .filter(|entry| entry_consumes_capacity(entry))
            .count();
        let evictable = state
            .entries
            .values()
            .filter(|entry| entry_is_capacity_evictable(entry))
            .count();
        Ok(GraphDbRegistryCapacity {
            max_open: self.inner.config.max_open,
            occupied,
            evictable,
        })
    }

    /// Reserves every selected ready runtime before any physical close begins.
    ///
    /// Identity and client-lease checks complete under one registry-state lock,
    /// so failure leaves every target ready and success denies new resolution
    /// for the entire selected set until commit or drop.
    #[hotpath::measure(
        label = "graph_db.registry.retire.reserve",
        impl_type = "GraphDbRegistry"
    )]
    pub fn reserve_retirement_batch(
        &self,
        targets: Vec<GraphDbRetirementTarget>,
    ) -> Result<GraphDbRetirementReservation, GraphDbRetirementRefusal> {
        let retry_targets = targets.clone();
        if targets.is_empty() {
            return Err(GraphDbRetirementRefusal::new(
                GraphDbError::invalid("graph retirement batch must select at least one runtime"),
                retry_targets,
            ));
        }
        if targets.iter().enumerate().any(|(index, target)| {
            targets[..index].iter().any(|prior| {
                prior.binding() == target.binding()
                    && prior.verified_locator() == target.verified_locator()
                    && prior.owner_id() == target.owner_id()
                    && prior.attachment_id() == target.attachment_id()
            })
        }) {
            return Err(GraphDbRetirementRefusal::new(
                GraphDbError::conflict("registry.reserve_retirement_batch"),
                retry_targets,
            ));
        }

        let pending = {
            let mut state = self
                .state_lock()
                .map_err(|error| GraphDbRetirementRefusal::new(error, retry_targets.clone()))?;
            let mut pending = Vec::with_capacity(targets.len());
            for target in &targets {
                let entry = state
                    .entries
                    .get(&target.binding().shard_id)
                    .ok_or_else(|| {
                        GraphDbRetirementRefusal::new(
                            GraphDbError::unavailable("graph runtime is not registered"),
                            retry_targets.clone(),
                        )
                    })?;
                let RegistryEntry::Ready {
                    binding,
                    verified_locator,
                    owner,
                    ..
                } = entry
                else {
                    return Err(GraphDbRetirementRefusal::new(
                        GraphDbError::conflict("registry.reserve_retirement_batch"),
                        retry_targets,
                    ));
                };
                if binding != target.binding() || verified_locator != target.verified_locator() {
                    return Err(GraphDbRetirementRefusal::new(
                        GraphDbError::conflict("registry.reserve_retirement_batch"),
                        retry_targets,
                    ));
                }
                owner
                    .can_reserve_owner_attachment(target)
                    .map_err(|error| GraphDbRetirementRefusal::new(error, retry_targets.clone()))?;
            }
            for target in targets {
                let shard_id = target.binding().shard_id.clone();
                let Some(RegistryEntry::Ready {
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    last_used,
                }) = state.entries.remove(&shard_id)
                else {
                    if let Err(error) = rollback_retiring_under_lock(&mut state, &mut pending) {
                        return Err(GraphDbRetirementRefusal::new(error, retry_targets));
                    }
                    self.inner.changed.notify_all();
                    return Err(GraphDbRetirementRefusal::new(
                        GraphDbError::unavailable(
                            "preflighted graph runtime disappeared before retirement fence",
                        ),
                        retry_targets,
                    ));
                };
                let reservation_id = match owner.reserve_owner_attachment(&target) {
                    Ok(reservation_id) => reservation_id,
                    Err(error) => {
                        state.entries.insert(
                            shard_id,
                            RegistryEntry::Ready {
                                binding,
                                verified_locator,
                                path,
                                expected_format,
                                owner,
                                last_used,
                            },
                        );
                        if let Err(rollback_error) =
                            rollback_retiring_under_lock(&mut state, &mut pending)
                        {
                            return Err(GraphDbRetirementRefusal::new(
                                rollback_error,
                                retry_targets,
                            ));
                        }
                        self.inner.changed.notify_all();
                        return Err(GraphDbRetirementRefusal::new(error, retry_targets));
                    }
                };
                let owner_attachment_id = target.attachment_id();
                let eviction = Eviction {
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    last_used,
                    close_reservation: OwnerCloseReservation::OwnerAttachment {
                        target: Box::new(target),
                        reservation_id,
                    },
                };
                state.entries.insert(
                    eviction.binding.shard_id.clone(),
                    RegistryEntry::Retiring {
                        binding: eviction.binding.clone(),
                        verified_locator: eviction.verified_locator.clone(),
                        path: eviction.path.clone(),
                        expected_format: eviction.expected_format,
                        owner_id: eviction.owner_id(),
                        owner_attachment_id,
                        reservation_id,
                    },
                );
                pending.push(eviction);
            }
            self.inner.changed.notify_all();
            pending
        };
        Ok(GraphDbRetirementReservation {
            registry: self.clone(),
            pending,
            armed: true,
        })
    }

    fn reserve_close(
        &self,
        requested_binding: &StoreRuntimeBindingV1,
        requested_locator: &VerifiedStoreLocatorV1,
        requested_location: Option<(&Path, GraphFormatVersion)>,
        require_unleased: bool,
    ) -> Result<CloseReservation, GraphDbError> {
        let mut state = self.state_lock()?;
        let Some(entry) = state.entries.get(&requested_binding.shard_id) else {
            return Ok(CloseReservation::Absent);
        };
        let (registered_binding, registered_locator, registered_path, registered_format) =
            binding(entry);
        if registered_binding != requested_binding
            || registered_locator != requested_locator
            || requested_location.is_some_and(|(path, format)| {
                registered_path != path || registered_format != format
            })
        {
            return Err(GraphDbError::conflict("registry.reserve_close"));
        }
        match entry {
            RegistryEntry::Opening { .. }
            | RegistryEntry::Closing { .. }
            | RegistryEntry::Retiring { .. } => {
                return Err(GraphDbError::conflict("registry.reserve_close"));
            }
            RegistryEntry::Ready { owner, .. } if require_unleased && !owner.is_unleased() => {
                return Err(GraphDbError::conflict("registry.reserve_close"));
            }
            RegistryEntry::Faulted { error, .. } => return Err(error.clone()),
            RegistryEntry::Ready { .. } => {}
        }
        let Some(RegistryEntry::Ready {
            binding,
            verified_locator,
            path,
            expected_format,
            owner,
            last_used,
        }) = state.entries.remove(&requested_binding.shard_id)
        else {
            return Err(GraphDbError::unavailable(
                "ready graph runtime disappeared before close fence",
            ));
        };
        let reservation_id = match owner.reserve_unleased_close() {
            Ok(reservation_id) => reservation_id,
            Err(error) => {
                state.entries.insert(
                    requested_binding.shard_id.clone(),
                    RegistryEntry::Ready {
                        binding,
                        verified_locator,
                        path,
                        expected_format,
                        owner,
                        last_used,
                    },
                );
                return Err(error);
            }
        };
        let reservation = Eviction {
            binding,
            verified_locator,
            path,
            expected_format,
            owner,
            last_used,
            close_reservation: OwnerCloseReservation::Unleased { reservation_id },
        };
        state.entries.insert(
            requested_binding.shard_id.clone(),
            RegistryEntry::Closing {
                binding: reservation.binding.clone(),
                verified_locator: reservation.verified_locator.clone(),
                path: reservation.path.clone(),
                expected_format: reservation.expected_format,
                owner_id: reservation.owner_id(),
                owner_attachment_id: None,
                reservation_id,
            },
        );
        Ok(CloseReservation::Closing(Box::new(reservation)))
    }

    fn finish_eviction(&self, eviction: Eviction) -> Result<(), GraphDbError> {
        if let Err(error) = eviction.begin_close() {
            self.restore_ready(eviction)?;
            return Err(error);
        }
        let close_result = eviction.owner.close();
        let terminalization_failure = match eviction.finish_close(&close_result) {
            Ok(()) => self.complete_close(eviction, close_result.clone()).err(),
            Err(error) => {
                eviction.force_terminal_after_close(&close_result);
                Some(Box::new((eviction, error)))
            }
        };
        if let Some(failure) = terminalization_failure {
            let (eviction, error) = *failure;
            self.retain_post_close_closing_fault(eviction, &close_result, error);
        }
        close_result
    }

    fn restore_ready(&self, eviction: Eviction) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get(&eviction.binding.shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph close reservation disappeared"))?;
        require_closing(entry, &eviction)?;
        eviction.restore_owner()?;
        let restored = RegistryEntry::Ready {
            binding: eviction.binding,
            verified_locator: eviction.verified_locator,
            path: eviction.path,
            expected_format: eviction.expected_format,
            owner: eviction.owner,
            last_used: eviction.last_used,
        };
        state
            .entries
            .insert(entry_binding(&restored).shard_id.clone(), restored);
        self.inner.changed.notify_all();
        Ok(())
    }

    fn complete_close(
        &self,
        reservation: Eviction,
        result: Result<(), GraphDbError>,
    ) -> Result<(), EvictionFailure> {
        let (mut state, lock_poison) = self.post_physical_close_state_lock();
        if let Some(error) = lock_poison {
            return Err(Box::new((reservation, error)));
        }
        let Some(entry) = state.entries.get(&reservation.binding.shard_id) else {
            return Err(Box::new((
                reservation,
                GraphDbError::unavailable("graph close reservation disappeared"),
            )));
        };
        if let Err(error) = require_closing(entry, &reservation) {
            return Err(Box::new((reservation, error)));
        }
        #[cfg(test)]
        if let Some(error) = self
            .inner
            .close_completion_failure
            .lock()
            .expect("close completion test fault lock must not be poisoned")
            .take()
        {
            return Err(Box::new((reservation, error)));
        }
        match result {
            Ok(()) => {
                state.entries.remove(&reservation.binding.shard_id);
            }
            Err(error) => {
                state.entries.insert(
                    reservation.binding.shard_id.clone(),
                    RegistryEntry::Faulted {
                        binding: reservation.binding,
                        verified_locator: reservation.verified_locator,
                        path: reservation.path,
                        expected_format: reservation.expected_format,
                        owner: Some(reservation.owner),
                        error,
                    },
                );
            }
        }
        self.inner.changed.notify_all();
        Ok(())
    }

    fn retain_post_close_closing_fault(
        &self,
        reservation: Eviction,
        close_result: &Result<(), GraphDbError>,
        terminalization_error: GraphDbError,
    ) {
        let (mut state, lock_poison) = self.post_physical_close_state_lock();
        let error = terminal_recording_error(close_result, terminalization_error, lock_poison);
        // Native close is irreversible. Even a poisoned registry lock or a
        // corrupted in-flight entry must not lose this exact owner: replace
        // the key with its terminal truth instead of leaving Closing/Retiring.
        state.entries.insert(
            reservation.binding.shard_id.clone(),
            RegistryEntry::Faulted {
                binding: reservation.binding,
                verified_locator: reservation.verified_locator,
                path: reservation.path,
                expected_format: reservation.expected_format,
                owner: Some(reservation.owner),
                error,
            },
        );
        self.inner.changed.notify_all();
    }

    fn restore_retiring(&self, eviction: Eviction) -> Result<(), EvictionFailure> {
        let mut state = match self.state_lock() {
            Ok(state) => state,
            Err(error) => return Err(Box::new((eviction, error))),
        };
        let Some(entry) = state.entries.get(&eviction.binding.shard_id) else {
            return Err(Box::new((
                eviction,
                GraphDbError::unavailable("graph retirement reservation disappeared"),
            )));
        };
        if let Err(error) = require_retiring(entry, &eviction) {
            return Err(Box::new((eviction, error)));
        }
        if let Err(error) = eviction.restore_before_native_close() {
            return Err(Box::new((eviction, error)));
        }
        state.entries.insert(
            eviction.binding.shard_id.clone(),
            RegistryEntry::Ready {
                binding: eviction.binding,
                verified_locator: eviction.verified_locator,
                path: eviction.path,
                expected_format: eviction.expected_format,
                owner: eviction.owner,
                last_used: eviction.last_used,
            },
        );
        self.inner.changed.notify_all();
        Ok(())
    }

    fn retain_retirement_restore_fault(&self, eviction: Eviction, error: GraphDbError) {
        let mut state = match self.state_lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if state
            .entries
            .get(&eviction.binding.shard_id)
            .is_some_and(|entry| require_retiring(entry, &eviction).is_ok())
        {
            state.entries.insert(
                eviction.binding.shard_id.clone(),
                RegistryEntry::Faulted {
                    binding: eviction.binding,
                    verified_locator: eviction.verified_locator,
                    path: eviction.path,
                    expected_format: eviction.expected_format,
                    owner: Some(eviction.owner),
                    error,
                },
            );
            self.inner.changed.notify_all();
        }
    }

    fn complete_retirement_close(
        &self,
        reservation: Eviction,
        result: Result<(), GraphDbError>,
    ) -> Result<(), EvictionFailure> {
        let (mut state, lock_poison) = self.post_physical_close_state_lock();
        if let Some(error) = lock_poison {
            return Err(Box::new((reservation, error)));
        }
        let Some(entry) = state.entries.get(&reservation.binding.shard_id) else {
            return Err(Box::new((
                reservation,
                GraphDbError::unavailable("graph retirement reservation disappeared"),
            )));
        };
        if let Err(error) = require_retiring(entry, &reservation) {
            return Err(Box::new((reservation, error)));
        }
        #[cfg(test)]
        if let Some(error) = self
            .inner
            .retirement_completion_failure
            .lock()
            .expect("retirement completion test fault lock must not be poisoned")
            .take()
        {
            return Err(Box::new((reservation, error)));
        }
        match result {
            Ok(()) => {
                state.entries.remove(&reservation.binding.shard_id);
            }
            Err(error) => {
                state.entries.insert(
                    reservation.binding.shard_id.clone(),
                    RegistryEntry::Faulted {
                        binding: reservation.binding,
                        verified_locator: reservation.verified_locator,
                        path: reservation.path,
                        expected_format: reservation.expected_format,
                        owner: Some(reservation.owner),
                        error,
                    },
                );
            }
        }
        self.inner.changed.notify_all();
        Ok(())
    }

    fn retain_post_close_retirement_fault(
        &self,
        reservation: Eviction,
        close_result: &Result<(), GraphDbError>,
        terminalization_error: GraphDbError,
    ) -> GraphDbError {
        let (mut state, lock_poison) = self.post_physical_close_state_lock();
        let error = terminal_recording_error(close_result, terminalization_error, lock_poison);
        // Native close is irreversible. Even a poisoned registry lock or a
        // corrupted in-flight entry must not lose this exact owner: replace
        // the key with its terminal truth instead of leaving Closing/Retiring.
        state.entries.insert(
            reservation.binding.shard_id.clone(),
            RegistryEntry::Faulted {
                binding: reservation.binding,
                verified_locator: reservation.verified_locator,
                path: reservation.path,
                expected_format: reservation.expected_format,
                owner: Some(reservation.owner),
                error: error.clone(),
            },
        );
        self.inner.changed.notify_all();
        error
    }

    fn remove_opening(
        &self,
        shard_id: &StoreShardIdV1,
        requested_binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        path: &Path,
        expected_format: GraphFormatVersion,
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        if state.entries.get(shard_id).is_some_and(|entry| {
            matches!(entry, RegistryEntry::Opening { .. })
                && require_binding(
                    binding(entry),
                    (requested_binding, verified_locator, path, expected_format),
                )
                .is_ok()
        }) {
            state.entries.remove(shard_id);
        }
        self.inner.changed.notify_all();
        Ok(())
    }

    fn state_lock(&self) -> Result<MutexGuard<'_, RegistryState>, GraphDbError> {
        crate::hotpath_observe::wait_lock(crate::hotpath_observe::LOCK_WAIT_REGISTRY, || {
            self.inner.state.lock()
        })
        .map_err(|_| GraphDbError::unavailable("graph registry state lock is poisoned"))
    }

    /// Recovers a poisoned registry mutex only after native close has crossed
    /// its irreversible boundary. Ordinary operations must still fail closed.
    fn post_physical_close_state_lock(
        &self,
    ) -> (MutexGuard<'_, RegistryState>, Option<GraphDbError>) {
        match self.inner.state.lock() {
            Ok(state) => (state, None),
            Err(poisoned) => (
                poisoned.into_inner(),
                Some(GraphDbError::unavailable(
                    "graph registry state lock is poisoned after native close",
                )),
            ),
        }
    }
}

fn retirement_outcome(
    target: GraphDbRetirementTarget,
    result: Result<(), GraphDbError>,
) -> GraphDbRetirementOutcome {
    match result {
        Ok(()) => GraphDbRetirementOutcome::Closed(target),
        Err(GraphDbError::DurabilityUncertain { message }) => {
            GraphDbRetirementOutcome::DurabilityUncertain { target, message }
        }
        Err(error) => GraphDbRetirementOutcome::Failed { target, error },
    }
}

fn post_close_terminal_error(
    close_result: &Result<(), GraphDbError>,
    terminalization_error: GraphDbError,
) -> GraphDbError {
    match close_result {
        Ok(()) => GraphDbError::unavailable(format!(
            "native graph close completed but terminal registry recording failed: {terminalization_error}"
        )),
        Err(GraphDbError::DurabilityUncertain { message }) => GraphDbError::DurabilityUncertain {
            message: format!(
                "{message}; terminal registry recording also failed: {terminalization_error}"
            ),
        },
        Err(error) => GraphDbError::unavailable(format!(
            "native graph close returned {error}; terminal registry recording failed: {terminalization_error}"
        )),
    }
}

fn terminal_recording_error(
    close_result: &Result<(), GraphDbError>,
    terminalization_error: GraphDbError,
    lock_poison: Option<GraphDbError>,
) -> GraphDbError {
    let terminalization_error = match lock_poison {
        Some(lock_poison) => GraphDbError::unavailable(format!(
            "{terminalization_error}; terminal registry lock recovery cause: {lock_poison}"
        )),
        None => terminalization_error,
    };
    post_close_terminal_error(close_result, terminalization_error)
}

fn rollback_retiring_under_lock(
    state: &mut RegistryState,
    pending: &mut Vec<Eviction>,
) -> Result<(), GraphDbError> {
    while let Some(eviction) = pending.pop() {
        let entry = state
            .entries
            .get(&eviction.binding.shard_id)
            .ok_or_else(|| {
                GraphDbError::unavailable(
                    "graph retirement reservation disappeared during rollback",
                )
            })?;
        require_retiring(entry, &eviction)?;
        eviction.restore_before_native_close()?;
        state.entries.insert(
            eviction.binding.shard_id.clone(),
            RegistryEntry::Ready {
                binding: eviction.binding,
                verified_locator: eviction.verified_locator,
                path: eviction.path,
                expected_format: eviction.expected_format,
                owner: eviction.owner,
                last_used: eviction.last_used,
            },
        );
    }
    Ok(())
}

fn reserve_capacity_eviction(
    state: &mut RegistryState,
    max_open: usize,
    opening: &StoreShardIdV1,
) -> Result<Option<Eviction>, GraphDbError> {
    let open_count = state
        .entries
        .values()
        .filter(|entry| entry_consumes_capacity(entry))
        .count();
    if open_count < max_open {
        return Ok(None);
    }
    let candidate = state
        .entries
        .iter()
        .filter_map(|(shard_id, entry)| match entry {
            RegistryEntry::Ready {
                owner, last_used, ..
            } if shard_id != opening
                && owner.is_unleased()
                && owner.runtime_state() != GraphDbRuntimeState::DurabilityUncertain =>
            {
                Some((shard_id.clone(), *last_used))
            }
            RegistryEntry::Opening { .. }
            | RegistryEntry::Closing { .. }
            | RegistryEntry::Retiring { .. }
            | RegistryEntry::Ready { .. }
            | RegistryEntry::Faulted { .. } => None,
        })
        .min_by(|(left_shard, left_used), (right_shard, right_used)| {
            left_used
                .cmp(right_used)
                .then_with(|| left_shard.cmp(right_shard))
        })
        .map(|(shard_id, _)| shard_id)
        .ok_or_else(|| GraphDbError::budget_exhausted_count(GraphBudgetKind::Capacity, max_open))?;
    let Some(RegistryEntry::Ready {
        binding,
        verified_locator,
        path,
        expected_format,
        owner,
        last_used,
        ..
    }) = state.entries.remove(&candidate)
    else {
        return Err(GraphDbError::unavailable(
            "reserved graph eviction is not ready",
        ));
    };
    let reservation_id = match owner.reserve_unleased_close() {
        Ok(reservation_id) => reservation_id,
        Err(error) => {
            state.entries.insert(
                candidate,
                RegistryEntry::Ready {
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    last_used,
                },
            );
            return Err(error);
        }
    };
    let eviction = Eviction {
        binding,
        verified_locator,
        path,
        expected_format,
        owner,
        last_used,
        close_reservation: OwnerCloseReservation::Unleased { reservation_id },
    };
    state.entries.insert(
        candidate,
        RegistryEntry::Closing {
            binding: eviction.binding.clone(),
            verified_locator: eviction.verified_locator.clone(),
            path: eviction.path.clone(),
            expected_format: eviction.expected_format,
            owner_id: eviction.owner_id(),
            owner_attachment_id: None,
            reservation_id,
        },
    );
    Ok(Some(eviction))
}

fn entry_consumes_capacity(entry: &RegistryEntry) -> bool {
    match entry {
        RegistryEntry::Opening { .. }
        | RegistryEntry::Ready { .. }
        | RegistryEntry::Closing { .. }
        | RegistryEntry::Retiring { .. } => true,
        // A durable uncertainty can still retain native Grafeo state. A
        // confirmed Closed owner remains recorded for identity truth but must
        // not consume physical-open capacity.
        RegistryEntry::Faulted {
            owner: Some(owner), ..
        } => owner.runtime_state() != GraphDbRuntimeState::Closed,
        RegistryEntry::Faulted { owner: None, .. } => false,
    }
}

fn entry_is_capacity_evictable(entry: &RegistryEntry) -> bool {
    matches!(
        entry,
        RegistryEntry::Ready { owner, .. }
            if owner.is_unleased()
                && owner.runtime_state() != GraphDbRuntimeState::DurabilityUncertain
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Condvar, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_store::{
        BrainId, ProjectId, RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
        RetainedGraphStoreOwnerOperationLeaseErrorV1, StoreAuthorityEpochV1, StoreIncarnationV1,
        StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
        canonical_store_locator_digest,
    };

    use super::{
        CloseReservation, GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry,
        GraphDbRegistryConfig, GraphDbRegistryStatus, GraphDbRetirementOutcome, RegistryEntry,
        reserve_capacity_eviction,
    };
    use crate::{
        GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbLocation, GraphDbOpenOptions,
        GraphDbOwner, GraphDbRuntimeState, GraphDurability, GraphFormatVersion, NeverCancelled,
    };

    #[derive(Debug)]
    struct TestLease {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
    }

    impl RetainedGraphStoreLeaseV1 for TestLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl RetainedGraphStoreOwnerAttachmentV1 for TestLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.path
        }

        fn issue_operation_lease(
            &self,
        ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
        {
            Ok(Arc::new(Self {
                binding: self.binding.clone(),
                verified_locator: self.verified_locator.clone(),
                path: self.path.clone(),
            }))
        }
    }

    #[derive(Debug)]
    struct FailingOwnerAttachment {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        error: RetainedGraphStoreOwnerOperationLeaseErrorV1,
    }

    impl RetainedGraphStoreOwnerAttachmentV1 for FailingOwnerAttachment {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.path
        }

        fn issue_operation_lease(
            &self,
        ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
        {
            Err(self.error)
        }
    }

    fn registration(root: &std::path::Path) -> GraphDbRegistration {
        registration_for(root, "project.registry-retirement")
    }

    fn registration_for(root: &std::path::Path, project: &str) -> GraphDbRegistration {
        let binding = StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                BrainId::try_from("brain.registry-retirement".to_owned()).unwrap(),
                UserProfileId::try_from("profile.registry-retirement".to_owned()).unwrap(),
                ProjectId::try_from(project.to_owned()).unwrap(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        );
        let path = root.join("graph.grafeo");
        let verified_locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&path).unwrap(),
        );
        GraphDbRegistration {
            authority_lease: Arc::new(TestLease {
                binding,
                verified_locator,
                path,
            }),
            cancellation: Arc::new(NeverCancelled),
            lifecycle_cancellation: Arc::new(NeverCancelled),
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn owner_registration(registration: GraphDbRegistration) -> GraphDbOwnerRegistrationV1 {
        let authority_attachment = Box::new(TestLease {
            binding: registration.binding().clone(),
            verified_locator: registration.verified_locator().clone(),
            path: registration.canonical_path().to_path_buf(),
        });
        GraphDbOwnerRegistrationV1 {
            operation: registration,
            authority_attachment,
        }
    }

    #[test]
    fn capacity_reports_only_ready_unleased_owners_as_evictable_headroom() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration(temporary.path())))
            .unwrap();
        let leased = registry.capacity().unwrap();
        assert_eq!(leased.max_open, 1);
        assert_eq!(leased.occupied, 1);
        assert_eq!(leased.evictable, 0);
        assert_eq!(leased.available_after_eviction(), 0);

        drop(owner);
        let idle = registry.capacity().unwrap();
        assert_eq!(idle.occupied, 1);
        assert_eq!(idle.evictable, 1);
        assert_eq!(idle.available_after_eviction(), 1);
    }

    #[test]
    fn resolve_without_an_owner_attachment_is_refused() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

        assert!(matches!(
            registry.resolve(registration(temporary.path())),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[test]
    fn lazy_owner_attachment_opens_the_engine_only_on_first_graph_read() {
        let temporary = TempDir::new().unwrap();
        let graph_path = temporary.path().join("graph.grafeo");
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let attachment = registry
            .resolve_lazy_owner_attachment(owner_registration(registration(temporary.path())))
            .unwrap();

        assert!(
            !graph_path.exists(),
            "publishing a retained graph owner must not eagerly create or replay its engine"
        );

        {
            let lease = attachment.issue_lease().unwrap();
            let _snapshot = lease.snapshot().unwrap();
            assert_eq!(registry.resident_engine_count().unwrap(), 1);
        }
        assert!(
            graph_path.exists(),
            "the first graph read must materialize the lazy engine"
        );
        assert_eq!(
            registry.resident_engine_count().unwrap(),
            0,
            "the lazy engine must hibernate after its final operation lease drops"
        );
    }

    #[test]
    fn owner_attachment_issuance_maps_store_authority_fences_to_typed_graph_failures() {
        for (error, expected) in [
            (
                RetainedGraphStoreOwnerOperationLeaseErrorV1::Retiring,
                GraphDbError::conflict("owner.issue_lease"),
            ),
            (
                RetainedGraphStoreOwnerOperationLeaseErrorV1::Unavailable,
                GraphDbError::unavailable("graph map owner cannot issue a Store operation lease"),
            ),
            (
                RetainedGraphStoreOwnerOperationLeaseErrorV1::TokenExhausted,
                GraphDbError::unavailable("graph map owner cannot issue a Store operation lease"),
            ),
        ] {
            let temporary = TempDir::new().unwrap();
            let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
            let operation = registration(temporary.path());
            let attachment = registry
                .resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
                    authority_attachment: Box::new(FailingOwnerAttachment {
                        binding: operation.binding().clone(),
                        verified_locator: operation.verified_locator().clone(),
                        path: operation.canonical_path().to_path_buf(),
                        error,
                    }),
                    operation,
                })
                .unwrap();

            assert_eq!(attachment.issue_lease().unwrap_err(), expected);
        }
    }

    #[test]
    fn foreign_store_owner_attachment_is_refused_without_mounting() {
        let mounted_root = TempDir::new().unwrap();
        let foreign_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let operation = registration_for(mounted_root.path(), "project.owner-identity");
        let foreign_operation = registration_for(foreign_root.path(), "project.owner-identity");
        let foreign_owner = GraphDbOwnerRegistrationV1 {
            authority_attachment: Box::new(TestLease {
                binding: foreign_operation.binding().clone(),
                verified_locator: foreign_operation.verified_locator().clone(),
                path: foreign_operation.canonical_path().to_path_buf(),
            }),
            operation: operation.clone(),
        };

        assert!(matches!(
            registry
                .resolve_owner_attachment(foreign_owner)
                .unwrap_err(),
            GraphDbError::Conflict { .. }
        ));
        assert!(matches!(
            registry.resolve(operation),
            Err(GraphDbError::Unavailable { .. })
        ));
        assert!(!mounted_root.path().join("graph.grafeo").exists());
    }

    struct Cancelled;

    impl GraphCancellation for Cancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    struct FlagCancellation(Arc<AtomicBool>);

    impl GraphCancellation for FlagCancellation {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
    }

    struct NativeOpenGate {
        released: Mutex<bool>,
        changed: Condvar,
        cancel_on_release: bool,
    }

    impl NativeOpenGate {
        fn new(cancel_on_release: bool) -> Arc<Self> {
            Arc::new(Self {
                released: Mutex::new(false),
                changed: Condvar::new(),
                cancel_on_release,
            })
        }

        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    /// Blocks the mounting call inside `open_registered_graph` until the test
    /// releases the gate: the second cancellation poll runs after the
    /// `Opening` slot switched to its in-flight native open, so concurrent
    /// attachers and lease waiters deterministically observe the held slot.
    struct NativeOpenGateCancellation {
        polls: AtomicUsize,
        entered: mpsc::Sender<()>,
        gate: Arc<NativeOpenGate>,
    }

    impl NativeOpenGateCancellation {
        fn new(entered: mpsc::Sender<()>, gate: Arc<NativeOpenGate>) -> Self {
            Self {
                polls: AtomicUsize::new(0),
                entered,
                gate,
            }
        }
    }

    impl GraphCancellation for NativeOpenGateCancellation {
        fn is_cancelled(&self) -> bool {
            let poll = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
            if poll == 2 {
                self.entered.send(()).unwrap();
                let mut released = self.gate.released.lock().unwrap();
                while !*released {
                    released = self.gate.changed.wait(released).unwrap();
                }
            }
            if poll >= 2 {
                return self.gate.cancel_on_release;
            }
            false
        }
    }

    #[test]
    fn concurrent_same_identity_owner_attachments_share_one_native_open() {
        let root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let (entered, entered_probe) = mpsc::channel();
        let gate = NativeOpenGate::new(false);
        let mut winner_operation = registration_for(root.path(), "project.shared-open");
        winner_operation.cancellation =
            Arc::new(NativeOpenGateCancellation::new(entered, Arc::clone(&gate)));
        let winner_registration = owner_registration(winner_operation);
        let winner_registry = registry.clone();
        let winner =
            thread::spawn(move || winner_registry.resolve_owner_attachment(winner_registration));
        entered_probe
            .recv_timeout(Duration::from_secs(10))
            .expect("winner must reach its native open");

        // The winner is inside its native open. A same-identity attacher must
        // observe the held Opening slot as a typed Conflict instead of
        // running a second identical open over the same store.
        assert!(matches!(
            registry
                .resolve_owner_attachment(owner_registration(registration_for(
                    root.path(),
                    "project.shared-open",
                )))
                .unwrap_err(),
            GraphDbError::Conflict { .. }
        ));

        gate.release();
        let attachment = winner.join().unwrap().unwrap();
        let lease = registry
            .resolve(registration_for(root.path(), "project.shared-open"))
            .unwrap();
        drop(lease);
        drop(attachment);
        assert_eq!(registry.capacity().unwrap().occupied, 1);
    }

    #[test]
    fn distinct_identity_owner_attachments_open_while_another_identity_is_in_flight() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
        let (entered, entered_probe) = mpsc::channel();
        let gate = NativeOpenGate::new(false);
        let mut first_operation = registration_for(first_root.path(), "project.first-shard");
        first_operation.cancellation =
            Arc::new(NativeOpenGateCancellation::new(entered, Arc::clone(&gate)));
        let first_registration = owner_registration(first_operation);
        let first_registry = registry.clone();
        let first =
            thread::spawn(move || first_registry.resolve_owner_attachment(first_registration));
        entered_probe
            .recv_timeout(Duration::from_secs(10))
            .expect("first attacher must reach its native open");

        // The in-flight open holds its capacity slot while it materializes.
        assert_eq!(registry.capacity().unwrap().occupied, 1);
        // A distinct store identity is not serialized behind that open.
        let second = registry
            .resolve_owner_attachment(owner_registration(registration_for(
                second_root.path(),
                "project.second-shard",
            )))
            .unwrap();

        gate.release();
        let first = first.join().unwrap().unwrap();
        assert!(first_root.path().join("graph.grafeo").exists());
        assert!(second_root.path().join("graph.grafeo").exists());
        drop(first);
        drop(second);
        assert_eq!(registry.capacity().unwrap().occupied, 2);
    }

    #[test]
    fn lease_waiter_cancellation_during_a_shared_open_is_a_typed_cancellation() {
        let root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let (entered, entered_probe) = mpsc::channel();
        let gate = NativeOpenGate::new(false);
        let mut winner_operation = registration_for(root.path(), "project.waited-open");
        winner_operation.cancellation =
            Arc::new(NativeOpenGateCancellation::new(entered, Arc::clone(&gate)));
        let winner_registration = owner_registration(winner_operation);
        let winner_registry = registry.clone();
        let winner =
            thread::spawn(move || winner_registry.resolve_owner_attachment(winner_registration));
        entered_probe
            .recv_timeout(Duration::from_secs(10))
            .expect("winner must reach its native open");

        let waiter_cancelled = Arc::new(AtomicBool::new(false));
        let mut waiter_registration = registration_for(root.path(), "project.waited-open");
        waiter_registration.cancellation =
            Arc::new(FlagCancellation(Arc::clone(&waiter_cancelled)));
        let waiter_registry = registry.clone();
        let waiter = thread::spawn(move || waiter_registry.resolve(waiter_registration));

        // Let the waiter observe the in-flight Opening slot, then cancel it
        // while the winner's native open is still gated: the waiter must fail
        // with its own typed cancellation — not a reconstructed registration
        // and not an unmounted-runtime error.
        thread::sleep(Duration::from_millis(100));
        waiter_cancelled.store(true, Ordering::Release);
        assert_eq!(waiter.join().unwrap().unwrap_err(), GraphDbError::Cancelled);

        // The one shared open is unaffected by the waiter's cancellation, and
        // later lease registrations resolve the mounted runtime.
        gate.release();
        let attachment = winner.join().unwrap().unwrap();
        let lease = registry
            .resolve(registration_for(root.path(), "project.waited-open"))
            .unwrap();
        drop(lease);
        drop(attachment);
    }

    #[test]
    fn cancelled_shared_open_releases_the_opening_slot_for_remount() {
        let root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let (entered, entered_probe) = mpsc::channel();
        let gate = NativeOpenGate::new(true);
        let mut cancelled_operation = registration_for(root.path(), "project.cancelled-open");
        cancelled_operation.cancellation =
            Arc::new(NativeOpenGateCancellation::new(entered, Arc::clone(&gate)));
        let cancelled_registration = owner_registration(cancelled_operation);
        let cancelled_registry = registry.clone();
        let mounting = thread::spawn(move || {
            cancelled_registry.resolve_owner_attachment(cancelled_registration)
        });
        entered_probe
            .recv_timeout(Duration::from_secs(10))
            .expect("mount must reach its native open");

        // While the open is in flight its slot stays held...
        assert_eq!(registry.capacity().unwrap().occupied, 1);
        gate.release();
        assert_eq!(
            mounting.join().unwrap().unwrap_err(),
            GraphDbError::Cancelled
        );

        // ...and a cancelled open releases it: the same identity remounts
        // instead of conflicting against a leaked in-flight slot.
        assert_eq!(registry.capacity().unwrap().occupied, 0);
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration_for(
                root.path(),
                "project.cancelled-open",
            )))
            .unwrap();
        drop(attachment);
    }

    fn poison_registry_state_before_terminal_completion(registry: &GraphDbRegistry) {
        let inner = Arc::clone(&registry.inner);
        let poisoned = thread::spawn(move || {
            let _state = inner.state.lock().unwrap();
            panic!("poison registry state before terminal close completion");
        });
        assert!(poisoned.join().is_err());
    }

    #[test]
    fn durability_uncertain_close_is_a_committed_retirement_outcome() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        let lease = attachment.issue_lease().unwrap();
        lease.inner.poisoned.store(true, Ordering::Release);
        drop(lease);

        let mut reservation = registry
            .reserve_retirement_batch(vec![target.clone()])
            .unwrap();
        let commit = reservation
            .commit(
                Arc::new(NeverCancelled),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(commit.outcomes().len(), 1);
        assert!(matches!(
            &commit.outcomes()[0],
            GraphDbRetirementOutcome::DurabilityUncertain {
                target: outcome_target,
                ..
            } if outcome_target == &target
        ));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::DurabilityUncertain)
        );
        assert!(matches!(
            registry.resolve(registration),
            Err(GraphDbError::DurabilityUncertain { .. })
        ));
    }

    #[test]
    fn faulted_owner_consumes_capacity_and_cannot_be_evicted_for_a_second_shard() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let first = registration_for(first_root.path(), "project.faulted-first");
        let second = registration_for(second_root.path(), "project.blocked-second");
        let _first_owner = registry
            .resolve_owner_attachment(owner_registration(first.clone()))
            .unwrap();

        let lease = registry.resolve(first.clone()).unwrap();
        lease.inner.poisoned.store(true, Ordering::Release);
        drop(lease);
        assert!(matches!(
            registry.resolve(first.clone()),
            Err(GraphDbError::DurabilityUncertain { .. })
        ));

        assert_eq!(
            registry
                .resolve_owner_attachment(owner_registration(second.clone()))
                .unwrap_err(),
            GraphDbError::BudgetExhausted {
                kind: GraphBudgetKind::Capacity,
                limit: 1,
            }
        );
        assert_eq!(
            registry.status(&first).unwrap(),
            Some(GraphDbRegistryStatus::DurabilityUncertain)
        );
        assert_eq!(registry.status(&second).unwrap(), None);
        assert!(!second_root.path().join("graph.grafeo").exists());
    }

    #[test]
    fn closed_ready_retirement_is_a_failed_outcome_and_stays_faulted() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        let lease = attachment.issue_lease().unwrap();
        lease.inner.closed.store(true, Ordering::Release);
        drop(lease);

        let mut reservation = registry
            .reserve_retirement_batch(vec![target.clone()])
            .unwrap();
        assert_eq!(
            reservation
                .commit(
                    Arc::new(NeverCancelled),
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap()
                .outcomes(),
            &[GraphDbRetirementOutcome::Failed {
                target,
                error: GraphDbError::Closed,
            }]
        );
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert!(matches!(
            attachment.issue_lease().unwrap_err(),
            GraphDbError::Conflict { .. }
        ));
        assert_eq!(
            registry.resolve(registration).unwrap_err(),
            GraphDbError::Closed
        );
    }

    #[test]
    fn closing_runtime_defers_resolution_until_the_close_settles() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        drop(owner);
        drop(registry.resolve(registration.clone()).unwrap());

        let CloseReservation::Closing(reservation) = registry
            .reserve_close(
                registration.authority_lease.binding(),
                registration.authority_lease.verified_locator(),
                None,
                true,
            )
            .unwrap()
        else {
            panic!("ready runtime must enter closing for this test");
        };
        // While the close is in flight, resolution waits and observes its own
        // typed deadline, never a fabricated conflict for the same owner.
        let mut bounded = registration.clone();
        bounded.deadline = Instant::now() + Duration::from_millis(100);
        assert_eq!(
            registry.resolve(bounded).unwrap_err(),
            GraphDbError::DeadlineExceeded
        );
        registry.restore_ready(*reservation).unwrap();
        drop(registry.resolve(registration).unwrap());
    }

    #[test]
    fn close_finish_failure_after_native_close_retains_a_terminal_fault() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        drop(owner);
        drop(registry.resolve(registration.clone()).unwrap());
        registry
            .inject_close_finish_failure(
                &registration,
                GraphDbError::unavailable("injected close finish failure"),
            )
            .unwrap();

        assert!(registry.close(&registration).unwrap());
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert!(matches!(
            registry.resolve(registration),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[test]
    fn close_completion_failure_after_native_close_retains_a_terminal_fault() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        drop(owner);
        drop(registry.resolve(registration.clone()).unwrap());
        registry.inject_close_completion_failure(GraphDbError::unavailable(
            "injected close completion failure",
        ));

        assert!(registry.close(&registration).unwrap());
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert!(matches!(
            registry.resolve(registration),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[test]
    fn poisoned_registry_lock_after_native_close_retains_the_exact_closing_owner() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let first = registration_for(first_root.path(), "project.poison-closing-first");
        let second = registration_for(second_root.path(), "project.poison-closing-second");
        let first_owner = registry
            .resolve_owner_attachment(owner_registration(first.clone()))
            .unwrap();
        drop(first_owner);
        drop(registry.resolve(first.clone()).unwrap());
        let CloseReservation::Closing(reservation) = registry
            .reserve_close(
                first.authority_lease.binding(),
                first.authority_lease.verified_locator(),
                None,
                true,
            )
            .unwrap()
        else {
            panic!("ready runtime must enter closing before native close");
        };
        let expected_owner_id = reservation.owner_id();
        poison_registry_state_before_terminal_completion(&registry);

        assert!(registry.finish_eviction(*reservation).is_ok());

        let mut state = registry
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(RegistryEntry::Faulted {
            owner: Some(owner),
            error: GraphDbError::Unavailable { message },
            ..
        }) = state.entries.get(&first.authority_lease.binding().shard_id)
        else {
            panic!("native-close poison must retain the exact owner as faulted");
        };
        assert_eq!(owner.owner_id(), expected_owner_id);
        assert_eq!(owner.runtime_state(), GraphDbRuntimeState::Closed);
        assert!(message.contains("poisoned"));
        assert!(
            reserve_capacity_eviction(&mut state, 1, &second.authority_lease.binding().shard_id,)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shutdown_close_finish_failure_leaves_a_terminal_fault_not_closing() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        drop(owner);
        drop(registry.resolve(registration.clone()).unwrap());
        registry
            .inject_close_finish_failure(
                &registration,
                GraphDbError::unavailable("injected shutdown close finish failure"),
            )
            .unwrap();

        assert!(
            registry
                .close_retained_for_shutdown(
                    registration.authority_lease.binding(),
                    registration.authority_lease.verified_locator(),
                )
                .unwrap()
        );
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
    }

    #[test]
    fn idle_eviction_completion_failure_leaves_a_terminal_fault_not_closing() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        drop(owner);
        drop(registry.resolve(registration.clone()).unwrap());
        registry.inject_close_completion_failure(GraphDbError::unavailable(
            "injected idle close completion failure",
        ));

        assert_eq!(
            registry
                .evict_idle(
                    Duration::ZERO,
                    Arc::new(NeverCancelled),
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap(),
            vec![registration.authority_lease.binding().clone()]
        );
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
    }

    #[test]
    fn capacity_eviction_completion_failure_preserves_terminal_truth_without_capacity_leak() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let first = registration_for(first_root.path(), "project.capacity-first");
        let second = registration_for(second_root.path(), "project.capacity-second");
        let first_owner = registry
            .resolve_owner_attachment(owner_registration(first.clone()))
            .unwrap();
        drop(first_owner);
        drop(registry.resolve(first.clone()).unwrap());
        registry.inject_close_completion_failure(GraphDbError::unavailable(
            "injected capacity close completion failure",
        ));

        let second_owner = registry
            .resolve_owner_attachment(owner_registration(second.clone()))
            .unwrap();
        drop(second_owner);
        assert_eq!(
            registry.status(&first).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert_eq!(
            registry.status(&second).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    #[test]
    fn reopen_after_close_completion_fault_reports_terminal_registry_truth() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let owner = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        drop(owner);
        drop(registry.resolve(registration.clone()).unwrap());
        registry.inject_close_completion_failure(GraphDbError::unavailable(
            "injected reopen close completion failure",
        ));

        assert!(matches!(
            registry.reopen(owner_registration(registration.clone())),
            Err(GraphDbError::Unavailable { .. })
        ));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
    }

    #[test]
    fn retirement_race_either_fences_resolution_or_observes_the_external_lease() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        let barrier = Arc::new(Barrier::new(2));
        let (resolved_tx, resolved_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let resolving_registry = registry.clone();
        let resolving_registration = registration.clone();
        let resolving_barrier = Arc::clone(&barrier);
        let resolver = thread::spawn(move || {
            resolving_barrier.wait();
            let lease = resolving_registry.resolve(resolving_registration);
            let issued = lease.is_ok();
            resolved_tx.send(issued).unwrap();
            release_rx.recv().unwrap();
            drop(lease);
        });

        barrier.wait();
        let reservation = registry.reserve_retirement_batch(vec![target]);
        let issued = resolved_rx.recv().unwrap();
        match (reservation, issued) {
            (Ok(reservation), false) => drop(reservation),
            (Err(refusal), true) if matches!(refusal.error(), GraphDbError::Conflict { .. }) => {}
            (_, issued) => panic!(
                "retirement and resolution must linearize to one typed winner, issued={issued}"
            ),
        }
        release_tx.send(()).unwrap();
        resolver.join().unwrap();
    }

    #[test]
    fn external_client_lease_and_snapshot_refuse_owner_attachment_retirement() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        let lease = registry.resolve(registration.clone()).unwrap();
        let snapshot = lease.snapshot().unwrap();
        drop(lease);

        let refusal = match registry.reserve_retirement_batch(vec![target.clone()]) {
            Err(refusal) => refusal,
            Ok(_reservation) => {
                panic!("an external graph snapshot must refuse owner attachment retirement")
            }
        };
        assert!(matches!(refusal.error(), GraphDbError::Conflict { .. }));
        let (_, retry_targets) = refusal.into_parts();
        assert_eq!(retry_targets, vec![target.clone()]);
        drop(snapshot);

        drop(registry.reserve_retirement_batch(retry_targets).unwrap());
    }

    #[test]
    fn attachment_issued_client_lease_refuses_owner_attachment_retirement() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        let lease = attachment.issue_lease().unwrap();

        assert!(matches!(
            registry.reserve_retirement_batch(vec![target.clone()]),
            Err(refusal) if matches!(refusal.error(), GraphDbError::Conflict { .. })
        ));
        drop(lease);

        drop(registry.reserve_retirement_batch(vec![target]).unwrap());
    }

    #[test]
    fn owner_attachment_accepts_only_its_exact_operation_registration() {
        let mounted_root = TempDir::new().unwrap();
        let foreign_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration_for(mounted_root.path(), "project.operation-identity");
        let foreign_registration =
            registration_for(foreign_root.path(), "project.operation-identity");
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();

        assert!(matches!(
            registry.resolve(foreign_registration).unwrap_err(),
            GraphDbError::Conflict { .. }
        ));
        let lease = registry.resolve(registration).unwrap();
        assert!(attachment.shares_runtime_with(&lease));
    }

    #[test]
    fn lease_operation_capability_refuses_a_foreign_graph_runtime() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let lease = registry.resolve(registration).unwrap();

        assert!(registry.registered_operation_with_lease(&lease).is_ok());

        let foreign_owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let foreign = foreign_owner.issue_lease().unwrap();
        assert!(matches!(
            registry.registered_operation_with_lease(&foreign),
            Err(GraphDbError::Unavailable { .. })
        ));

        drop(attachment);
    }

    #[test]
    fn owner_attachment_issued_lease_blocks_retirement_through_clone_and_snapshot() {
        let mounted_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration_for(mounted_root.path(), "project.operation-identity");
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();

        let lease = attachment.issue_lease().unwrap();
        let clone = lease.clone();
        let snapshot = clone.snapshot().unwrap();
        drop(lease);
        assert!(matches!(
            registry.reserve_retirement_batch(vec![attachment.retirement_target()]),
            Err(refusal) if matches!(refusal.error(), GraphDbError::Conflict { .. })
        ));
        drop(snapshot);
        drop(clone);
        drop(
            registry
                .reserve_retirement_batch(vec![attachment.retirement_target()])
                .unwrap(),
        );
    }

    #[test]
    fn foreign_owner_attachment_target_is_rejected_without_fencing_the_ready_runtime() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let foreign_owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let foreign_target = foreign_owner
            .issue_owner_attachment(
                attachment.binding().clone(),
                attachment.verified_locator().clone(),
                attachment.canonical_path().to_path_buf(),
            )
            .unwrap()
            .retirement_target();

        assert!(matches!(
            registry.reserve_retirement_batch(vec![foreign_target]),
            Err(refusal) if matches!(refusal.error(), GraphDbError::Conflict { .. })
        ));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
    }

    #[test]
    fn stale_owner_attachment_id_is_rejected_without_fencing_the_ready_runtime() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        attachment.inject_stale_owner_attachment_id().unwrap();

        assert!(matches!(
            registry.reserve_retirement_batch(vec![target]),
            Err(refusal) if matches!(refusal.error(), GraphDbError::Conflict { .. })
        ));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
    }

    #[test]
    fn multi_target_reservation_failure_restores_each_exact_owner_attachment() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
        let first_registration = registration_for(first_root.path(), "project.rollback-first");
        let second_registration = registration_for(second_root.path(), "project.rollback-second");
        let first_attachment = registry
            .resolve_owner_attachment(owner_registration(first_registration.clone()))
            .unwrap();
        let second_attachment = registry
            .resolve_owner_attachment(owner_registration(second_registration.clone()))
            .unwrap();
        second_attachment.inject_retirement_reservation_failure(GraphDbError::unavailable(
            "injected second owner-attachment reservation failure",
        ));

        assert!(matches!(
            registry.reserve_retirement_batch(vec![
                first_attachment.retirement_target(),
                second_attachment.retirement_target(),
            ]),
            Err(refusal) if matches!(refusal.error(), GraphDbError::Unavailable { .. })
        ));
        assert_eq!(
            registry.status(&first_registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
        assert_eq!(
            registry.status(&second_registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
        let first_lease = first_attachment.issue_lease().unwrap();
        let second_lease = second_attachment.issue_lease().unwrap();
        drop(first_lease);
        drop(second_lease);

        drop(
            registry
                .reserve_retirement_batch(vec![
                    first_attachment.retirement_target(),
                    second_attachment.retirement_target(),
                ])
                .unwrap(),
        );
    }

    #[test]
    fn second_begin_failure_restores_every_target_before_any_native_close() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
        let first_registration = registration_for(first_root.path(), "project.begin-first");
        let second_registration = registration_for(second_root.path(), "project.begin-second");
        let first_attachment = registry
            .resolve_owner_attachment(owner_registration(first_registration.clone()))
            .unwrap();
        let second_attachment = registry
            .resolve_owner_attachment(owner_registration(second_registration.clone()))
            .unwrap();
        second_attachment.inject_retirement_begin_failure(GraphDbError::unavailable(
            "injected second owner begin failure",
        ));
        let mut reservation = registry
            .reserve_retirement_batch(vec![
                first_attachment.retirement_target(),
                second_attachment.retirement_target(),
            ])
            .unwrap();

        assert!(matches!(
            reservation.commit(
                Arc::new(NeverCancelled),
                Instant::now() + Duration::from_secs(30),
            ),
            Err(refusal) if matches!(refusal.error(), GraphDbError::Unavailable { .. })
        ));
        assert_eq!(
            registry.status(&first_registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
        assert_eq!(
            registry.status(&second_registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
        drop(first_attachment.issue_lease().unwrap());
        drop(second_attachment.issue_lease().unwrap());
    }

    #[test]
    fn second_terminalization_failure_after_first_close_reports_every_target() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
        let first_registration = registration_for(first_root.path(), "project.boundary-first");
        let second_registration = registration_for(second_root.path(), "project.boundary-second");
        let first_attachment = registry
            .resolve_owner_attachment(owner_registration(first_registration.clone()))
            .unwrap();
        let second_attachment = registry
            .resolve_owner_attachment(owner_registration(second_registration.clone()))
            .unwrap();
        let first_target = first_attachment.retirement_target();
        let second_target = second_attachment.retirement_target();
        second_attachment.inject_retirement_finish_failure(GraphDbError::unavailable(
            "injected second terminal finish failure",
        ));
        let mut reservation = registry
            .reserve_retirement_batch(vec![first_target.clone(), second_target.clone()])
            .unwrap();

        assert!(matches!(
            reservation
                .commit(
                    Arc::new(NeverCancelled),
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap()
                .outcomes(),
            [
                GraphDbRetirementOutcome::Closed(first),
                GraphDbRetirementOutcome::Failed {
                    target: second,
                    error: GraphDbError::Unavailable { .. },
                },
            ] if first == &first_target && second == &second_target
        ));
        assert_eq!(registry.status(&first_registration).unwrap(), None);
        assert_eq!(
            registry.status(&second_registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert!(matches!(
            registry.resolve(second_registration),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[test]
    fn poisoned_registry_lock_after_retirement_close_records_every_terminal_owner() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let third_root = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
        let first = registration_for(first_root.path(), "project.poison-retirement-first");
        let second = registration_for(second_root.path(), "project.poison-retirement-second");
        let third = registration_for(third_root.path(), "project.poison-retirement-third");
        let first_attachment = registry
            .resolve_owner_attachment(owner_registration(first.clone()))
            .unwrap();
        let second_attachment = registry
            .resolve_owner_attachment(owner_registration(second.clone()))
            .unwrap();
        let first_target = first_attachment.retirement_target();
        let second_target = second_attachment.retirement_target();
        let mut reservation = registry
            .reserve_retirement_batch(vec![first_target.clone(), second_target.clone()])
            .unwrap();
        poison_registry_state_before_terminal_completion(&registry);

        let commit = reservation
            .commit(
                Arc::new(NeverCancelled),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert!(matches!(
            commit.outcomes(),
            [
                GraphDbRetirementOutcome::Failed {
                    target: first_outcome,
                    error: GraphDbError::Unavailable { message: first_message },
                },
                GraphDbRetirementOutcome::Failed {
                    target: second_outcome,
                    error: GraphDbError::Unavailable { message: second_message },
                },
            ] if first_outcome == &first_target
                && second_outcome == &second_target
                && first_message.contains("poisoned")
                && second_message.contains("poisoned")
        ));

        let mut state = registry
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (registration, target) in [(&first, &first_target), (&second, &second_target)] {
            let Some(RegistryEntry::Faulted {
                owner: Some(owner),
                error: GraphDbError::Unavailable { message },
                ..
            }) = state
                .entries
                .get(&registration.authority_lease.binding().shard_id)
            else {
                panic!("every native-close poison target must retain its terminal owner");
            };
            assert_eq!(owner.owner_id(), target.owner_id());
            assert_eq!(owner.runtime_state(), GraphDbRuntimeState::Closed);
            assert!(message.contains("poisoned"));
        }
        assert!(
            reserve_capacity_eviction(&mut state, 2, &third.authority_lease.binding().shard_id,)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn post_close_completion_failure_is_reported_and_retained_as_terminal_fault() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        registry.inject_retirement_completion_failure(GraphDbError::unavailable(
            "injected terminal retirement recording failure",
        ));
        let mut reservation = registry
            .reserve_retirement_batch(vec![target.clone()])
            .unwrap();

        let commit = reservation
            .commit(
                Arc::new(NeverCancelled),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert!(matches!(
            commit.outcomes(),
            [GraphDbRetirementOutcome::Failed {
                target: outcome_target,
                error: GraphDbError::Unavailable { message },
            }] if outcome_target == &target
                && message.contains("native graph close completed")
        ));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert!(matches!(
            registry.resolve(registration.clone()),
            Err(GraphDbError::Unavailable { .. })
        ));
        assert!(matches!(
            attachment.issue_lease().unwrap_err(),
            GraphDbError::Conflict { .. }
        ));
    }

    #[test]
    fn post_close_finish_failure_is_reported_and_retained_as_terminal_fault() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        attachment.inject_retirement_finish_failure(GraphDbError::unavailable(
            "injected terminal owner finish failure",
        ));
        let mut reservation = registry
            .reserve_retirement_batch(vec![target.clone()])
            .unwrap();

        let commit = reservation
            .commit(
                Arc::new(NeverCancelled),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert!(matches!(
            commit.outcomes(),
            [GraphDbRetirementOutcome::Failed {
                target: outcome_target,
                error: GraphDbError::Unavailable { message },
            }] if outcome_target == &target
                && message.contains("native graph close completed")
        ));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closed)
        );
        assert!(matches!(
            registry.resolve(registration),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[test]
    fn dropped_retirement_reservation_restores_the_exact_owner_runtime() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();

        let reservation = registry.reserve_retirement_batch(vec![target]).unwrap();
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Closing)
        );
        drop(reservation);

        let lease = registry.resolve(registration.clone()).unwrap();
        assert!(attachment.shares_runtime_with(&lease));
        assert_eq!(
            registry.status(&registration).unwrap(),
            Some(GraphDbRegistryStatus::Ready)
        );
    }

    #[test]
    fn cancellation_before_commit_restores_the_exact_owner_attachment() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let target = attachment.retirement_target();
        let identity = attachment.runtime_identity();
        let mut reservation = registry
            .reserve_retirement_batch(vec![target.clone()])
            .unwrap();

        let refusal = reservation
            .commit(
                Arc::new(Cancelled),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err();
        assert_eq!(refusal.error(), &GraphDbError::Cancelled);
        let (_, retry_targets) = refusal.into_parts();
        assert_eq!(retry_targets, vec![target]);
        let lease = registry.resolve(registration.clone()).unwrap();
        assert!(attachment.shares_runtime_with(&lease));
        assert_eq!(lease.runtime_identity(), identity);

        drop(lease);
        let retry = registry.reserve_retirement_batch(retry_targets).unwrap();
        drop(retry);
    }

    #[test]
    fn committed_close_is_terminal_and_a_later_owner_mount_opens_a_new_runtime() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let identity = attachment.runtime_identity();
        let target = attachment.retirement_target();
        let mut reservation = registry.reserve_retirement_batch(vec![target]).unwrap();
        let commit = reservation
            .commit(
                Arc::new(NeverCancelled),
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        assert!(matches!(
            commit.outcomes(),
            [GraphDbRetirementOutcome::Closed(_)]
        ));
        drop(commit);
        drop(attachment);

        let reopened_attachment = registry
            .resolve_owner_attachment(owner_registration(registration.clone()))
            .unwrap();
        let reopened = registry.resolve(registration).unwrap();
        assert_ne!(reopened.runtime_identity(), identity);
        assert!(reopened_attachment.shares_runtime_with(&reopened));
    }
}
