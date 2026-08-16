use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tracedecay_store::{
    RetainedGraphStoreLeaseV1, StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
};

use crate::generation::InlineOnlyGraphGenerationManifestProvider;
use crate::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbLeaseV1, GraphDbOwner,
    GraphDbRuntimeState, GraphFormatVersion, GraphGenerationManifestProvider,
};

use self::identity::{
    binding, entry_binding, require_binding, require_closing, require_retiring,
    validate_registration,
};
use self::path::canonical_graph_database_file;
use self::support::{
    check_deadline, check_registration_request, check_request, open_registered_graph,
    reject_path_alias, retains_fault, status,
};

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
pub use staging::{VerifiedGenerationBatchApply, VerifiedGenerationBatchCommit};
pub use vector_retirement::{
    SemanticVectorRetentionAction, SemanticVectorRetentionCensus, SemanticVectorRetentionStep,
    SemanticVectorRetirementReservation,
};

const OPEN_WAIT_POLL: Duration = Duration::from_millis(10);

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
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.authority_lease.binding()
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.authority_lease.verified_locator()
    }

    fn canonical_path(&self) -> &Path {
        self.authority_lease.canonical_path()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphDbRegistryConfig {
    pub max_open: usize,
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

/// Exact runtime identity selected for coordinated graph retirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphDbRetirementTarget {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
}

impl GraphDbRetirementTarget {
    #[must_use]
    pub fn new(binding: StoreRuntimeBindingV1, verified_locator: VerifiedStoreLocatorV1) -> Self {
        Self {
            binding,
            verified_locator,
        }
    }

    #[must_use]
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    #[must_use]
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }
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

#[derive(Clone)]
pub struct GraphDbRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    config: GraphDbRegistryConfig,
    manifest_provider: Arc<dyn GraphGenerationManifestProvider>,
    state: Mutex<RegistryState>,
    changed: Condvar,
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreShardIdV1, RegistryEntry>,
}

enum RegistryEntry {
    Opening {
        authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
    },
    Ready {
        authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: Arc<GraphDbOwner>,
        last_used: Instant,
    },
    Closing {
        authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: Arc<GraphDbOwner>,
    },
    Retiring {
        authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: Arc<GraphDbOwner>,
    },
    Faulted {
        authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: Option<Arc<GraphDbOwner>>,
        error: GraphDbError,
    },
}

#[derive(Clone)]
struct Eviction {
    authority_lease: Arc<dyn RetainedGraphStoreLeaseV1>,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    path: PathBuf,
    expected_format: GraphFormatVersion,
    owner: Arc<GraphDbOwner>,
    last_used: Instant,
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
    ) -> Result<GraphDbRetirementCommit, GraphDbError> {
        if !self.armed {
            return Err(GraphDbError::Conflict);
        }
        if let Err(error) = check_request(cancellation.as_ref(), deadline) {
            self.restore_pending()?;
            return Err(error);
        }
        // This is the commit point. Every subsequent close has a typed,
        // irreversible outcome, so late cancellation cannot fake a rollback.
        let mut outcomes = Vec::with_capacity(self.pending.len());
        while !self.pending.is_empty() {
            let eviction = self.pending.remove(0);
            let target = GraphDbRetirementTarget::new(
                eviction.binding.clone(),
                eviction.verified_locator.clone(),
            );
            let close_result = eviction.owner.close();
            self.registry
                .complete_retirement_close(eviction, close_result.clone())?;
            match close_result {
                Ok(()) => outcomes.push(GraphDbRetirementOutcome::Closed(target)),
                Err(GraphDbError::DurabilityUncertain { message }) => {
                    outcomes
                        .push(GraphDbRetirementOutcome::DurabilityUncertain { target, message });
                }
                Err(error) => outcomes.push(GraphDbRetirementOutcome::Failed { target, error }),
            }
        }
        self.armed = false;
        Ok(GraphDbRetirementCommit { outcomes })
    }

    fn restore_pending(&mut self) -> Result<(), GraphDbError> {
        while let Some(eviction) = self.pending.pop() {
            if let Err(error) = self.registry.restore_retiring(eviction.clone()) {
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
        // A retiring entry cannot be concurrently replaced: resolution,
        // close, eviction, and competing retirement reservations all reject
        // that state. Recover a poisoned mutex only to restore this exact
        // pre-close state; the reservation never fabricates a Ready entry.
        let mut state = match self.registry.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        for eviction in self.pending.drain(..) {
            let should_restore = state
                .entries
                .get(&eviction.binding.shard_id)
                .is_some_and(|entry| require_retiring(entry, &eviction).is_ok());
            if should_restore {
                state.entries.insert(
                    eviction.binding.shard_id.clone(),
                    RegistryEntry::Ready {
                        authority_lease: eviction.authority_lease,
                        binding: eviction.binding,
                        verified_locator: eviction.verified_locator,
                        path: eviction.path,
                        expected_format: eviction.expected_format,
                        owner: eviction.owner,
                        last_used: eviction.last_used,
                    },
                );
            }
        }
        self.armed = false;
        drop(state);
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
            }),
        })
    }

    /// Opens (or joins) the registered runtime and returns one client lease.
    ///
    /// Replayed code graphs must read through [`crate::VerifiedGraphSnapshot`]
    /// via the publication surface; the native runtime lease is for graphs
    /// whose state is itself the authority (for example daemon-owned session
    /// relation graphs) and for direct storage tests.
    pub fn resolve(
        &self,
        registration: GraphDbRegistration,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        check_request(registration.cancellation.as_ref(), registration.deadline)?;
        validate_registration(&registration)?;
        let path = canonical_graph_database_file(registration.canonical_path())?;
        let expected_format = GraphFormatVersion::current();
        let binding = registration.binding().clone();
        let verified_locator = registration.verified_locator().clone();
        let authority_lease = Arc::clone(&registration.authority_lease);
        let shard_id = binding.shard_id.clone();

        loop {
            let mut state = self.state_lock()?;
            reject_path_alias(&state, &binding, &verified_locator, &path, expected_format)?;

            match state.entries.get_mut(&shard_id) {
                Some(RegistryEntry::Ready {
                    authority_lease: registered_authority_lease,
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
                            return Ok(owner.lease());
                        }
                        GraphDbRuntimeState::Closed => {
                            let error = GraphDbError::Closed;
                            let faulted = RegistryEntry::Faulted {
                                authority_lease: Arc::clone(registered_authority_lease),
                                binding: registered_binding.clone(),
                                verified_locator: registered_locator.clone(),
                                path: registered_path.clone(),
                                expected_format: *registered_format,
                                owner: Some(Arc::clone(owner)),
                                error: error.clone(),
                            };
                            state.entries.insert(shard_id.clone(), faulted);
                            self.inner.changed.notify_all();
                            return Err(error);
                        }
                        GraphDbRuntimeState::DurabilityUncertain => {
                            let error = GraphDbError::DurabilityUncertain {
                                message: "registered graph handle has uncertain durability"
                                    .to_owned(),
                            };
                            let faulted = RegistryEntry::Faulted {
                                authority_lease: Arc::clone(registered_authority_lease),
                                binding: registered_binding.clone(),
                                verified_locator: registered_locator.clone(),
                                path: registered_path.clone(),
                                expected_format: *registered_format,
                                owner: Some(Arc::clone(owner)),
                                error: error.clone(),
                            };
                            state.entries.insert(shard_id.clone(), faulted);
                            self.inner.changed.notify_all();
                            return Err(error);
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
                    check_request(registration.cancellation.as_ref(), registration.deadline)?;
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
                    return Err(GraphDbError::Conflict);
                }
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
                    return Err(GraphDbError::Conflict);
                }
                None => {
                    check_request(registration.cancellation.as_ref(), registration.deadline)?;
                    let eviction = reserve_capacity_eviction(
                        &mut state,
                        self.inner.config.max_open,
                        &shard_id,
                    )?;
                    state.entries.insert(
                        shard_id.clone(),
                        RegistryEntry::Opening {
                            authority_lease: Arc::clone(&authority_lease),
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
                            &authority_lease,
                            &binding,
                            &verified_locator,
                            &path,
                            expected_format,
                        )?;
                        return Err(error);
                    }
                    break;
                }
            }
        }

        let opened = open_registered_graph(&path, expected_format, &registration);
        let mut state = self.state_lock()?;
        match opened {
            Ok(owner) => {
                let owner = Arc::new(owner);
                let database = owner.lease();
                state.entries.insert(
                    shard_id,
                    RegistryEntry::Ready {
                        authority_lease,
                        binding,
                        verified_locator,
                        path,
                        expected_format,
                        owner,
                        last_used: Instant::now(),
                    },
                );
                self.inner.changed.notify_all();
                Ok(database)
            }
            Err(error) => {
                if retains_fault(&error) {
                    state.entries.insert(
                        shard_id,
                        RegistryEntry::Faulted {
                            authority_lease,
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
                    state.entries.remove(&shard_id);
                    self.inner.changed.notify_all();
                    Err(error)
                }
            }
        }
    }

    fn retain_verification_fault(
        &self,
        registration: &GraphDbRegistration,
        error: &GraphDbError,
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get(&registration.binding().shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph verification entry disappeared"))?;
        let RegistryEntry::Ready {
            authority_lease,
            binding,
            verified_locator,
            path,
            expected_format,
            owner,
            ..
        } = entry
        else {
            return Err(GraphDbError::Conflict);
        };
        require_binding(
            (binding, verified_locator, path, *expected_format),
            (
                registration.binding(),
                registration.verified_locator(),
                registration.canonical_path(),
                GraphFormatVersion::current(),
            ),
        )?;
        let faulted = RegistryEntry::Faulted {
            authority_lease: Arc::clone(authority_lease),
            binding: binding.clone(),
            verified_locator: verified_locator.clone(),
            path: path.clone(),
            expected_format: *expected_format,
            owner: Some(Arc::clone(owner)),
            error: error.clone(),
        };
        state
            .entries
            .insert(registration.binding().shard_id.clone(), faulted);
        self.inner.changed.notify_all();
        Ok(())
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn reopen(
        &self,
        registration: GraphDbRegistration,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        check_request(registration.cancellation.as_ref(), registration.deadline)?;
        validate_registration(&registration)?;
        let path = canonical_graph_database_file(registration.canonical_path())?;
        let expected_format = GraphFormatVersion::current();
        if let CloseReservation::Closing(reservation) = self.reserve_close(
            registration.binding(),
            registration.verified_locator(),
            Some((&path, expected_format)),
            true,
        )? {
            if let Err(error) =
                check_request(registration.cancellation.as_ref(), registration.deadline)
            {
                self.restore_ready(*reservation)?;
                return Err(error);
            }
            let close_result = reservation.owner.close();
            self.complete_close(*reservation, close_result.clone())?;
            close_result?;
        }
        self.resolve(registration)
    }

    /// Reopens a leased runtime only for direct storage tests and developer evals.
    ///
    /// Production recovery returns a verified snapshot through
    /// [`GraphDbRegistry::recover_verified_snapshot`].
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn reopen_for_harness(
        &self,
        registration: GraphDbRegistration,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        self.reopen(registration)
    }

    pub fn close(&self, registration: &GraphDbRegistration) -> Result<bool, GraphDbError> {
        check_request(registration.cancellation.as_ref(), registration.deadline)?;
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
        if let Err(error) = check_request(registration.cancellation.as_ref(), registration.deadline)
        {
            self.restore_ready(*reservation)?;
            return Err(error);
        }
        let close_result = reservation.owner.close();
        self.complete_close(*reservation, close_result.clone())?;
        close_result?;
        check_deadline(registration.deadline)?;
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

    /// Releases the exclusive Grafeo writer even while session databases still
    /// retain closed client leases. Idle eviction stays fail-closed on a live
    /// lease; daemon and harness shutdown must drain the file lock so the next
    /// in-process open is not blocked by a retired registry.
    pub fn close_retained_for_shutdown(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
    ) -> Result<bool, GraphDbError> {
        self.close_retained_inner(binding, verified_locator, false)
    }

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
        let close_result = reservation.owner.close();
        self.complete_close(*reservation, close_result.clone())?;
        close_result?;
        Ok(true)
    }

    pub fn evict_idle(
        &self,
        minimum_idle: Duration,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> Result<Vec<StoreRuntimeBindingV1>, GraphDbError> {
        check_request(cancellation.as_ref(), deadline)?;
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
            shards
                .into_iter()
                .filter_map(|shard_id| {
                    let RegistryEntry::Ready {
                        authority_lease,
                        binding,
                        verified_locator,
                        path,
                        expected_format,
                        owner,
                        last_used,
                        ..
                    } = state.entries.get(&shard_id)?
                    else {
                        return None;
                    };
                    let eviction = Eviction {
                        authority_lease: Arc::clone(authority_lease),
                        binding: binding.clone(),
                        verified_locator: verified_locator.clone(),
                        path: path.clone(),
                        expected_format: *expected_format,
                        owner: Arc::clone(owner),
                        last_used: *last_used,
                    };
                    state.entries.insert(
                        shard_id,
                        RegistryEntry::Closing {
                            authority_lease: Arc::clone(&eviction.authority_lease),
                            binding: eviction.binding.clone(),
                            verified_locator: eviction.verified_locator.clone(),
                            path: eviction.path.clone(),
                            expected_format: eviction.expected_format,
                            owner: Arc::clone(&eviction.owner),
                        },
                    );
                    Some(eviction)
                })
                .collect::<Vec<_>>()
        };

        let mut evicted = Vec::with_capacity(evictions.len());
        let mut first_error = None;
        for eviction in evictions {
            if let Err(error) = check_request(cancellation.as_ref(), deadline) {
                self.restore_ready(eviction)?;
                first_error.get_or_insert(error);
                continue;
            }
            let close_result = eviction.owner.close();
            self.complete_close(eviction.clone(), close_result.clone())?;
            match close_result {
                Ok(()) => evicted.push(eviction.binding),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        self.inner.changed.notify_all();
        if let Some(error) = first_error {
            Err(error)
        } else {
            check_deadline(deadline)?;
            evicted.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
            Ok(evicted)
        }
    }

    pub fn status(
        &self,
        registration: &GraphDbRegistration,
    ) -> Result<Option<GraphDbRegistryStatus>, GraphDbError> {
        validate_registration(registration)?;
        canonical_graph_database_file(registration.canonical_path())?;
        let state = self.state_lock()?;
        let Some(entry) = state.entries.get(&registration.binding().shard_id) else {
            return Ok(None);
        };
        require_binding(
            binding(entry),
            (
                registration.binding(),
                registration.verified_locator(),
                registration.canonical_path(),
                GraphFormatVersion::current(),
            ),
        )?;
        Ok(Some(status(entry)))
    }

    /// Reserves every selected ready runtime before any physical close begins.
    ///
    /// Identity and client-lease checks complete under one registry-state lock,
    /// so failure leaves every target ready and success denies new resolution
    /// for the entire selected set until commit or drop.
    pub fn reserve_retirement_batch(
        &self,
        targets: Vec<GraphDbRetirementTarget>,
    ) -> Result<GraphDbRetirementReservation, GraphDbError> {
        if targets.is_empty() {
            return Err(GraphDbError::invalid(
                "graph retirement batch must select at least one runtime",
            ));
        }
        if targets
            .iter()
            .enumerate()
            .any(|(index, target)| targets[..index].contains(target))
        {
            return Err(GraphDbError::Conflict);
        }

        let pending = {
            let mut state = self.state_lock()?;
            let mut pending = Vec::with_capacity(targets.len());
            for target in &targets {
                let entry = state
                    .entries
                    .get(&target.binding.shard_id)
                    .ok_or_else(|| GraphDbError::unavailable("graph runtime is not registered"))?;
                let RegistryEntry::Ready {
                    authority_lease,
                    binding,
                    verified_locator,
                    path,
                    expected_format,
                    owner,
                    last_used,
                } = entry
                else {
                    return Err(GraphDbError::Conflict);
                };
                if binding != &target.binding || verified_locator != &target.verified_locator {
                    return Err(GraphDbError::Conflict);
                }
                if !owner.is_unleased() {
                    return Err(GraphDbError::Conflict);
                }
                pending.push(Eviction {
                    authority_lease: Arc::clone(authority_lease),
                    binding: binding.clone(),
                    verified_locator: verified_locator.clone(),
                    path: path.clone(),
                    expected_format: *expected_format,
                    owner: Arc::clone(owner),
                    last_used: *last_used,
                });
            }
            for eviction in &pending {
                state.entries.insert(
                    eviction.binding.shard_id.clone(),
                    RegistryEntry::Retiring {
                        authority_lease: Arc::clone(&eviction.authority_lease),
                        binding: eviction.binding.clone(),
                        verified_locator: eviction.verified_locator.clone(),
                        path: eviction.path.clone(),
                        expected_format: eviction.expected_format,
                        owner: Arc::clone(&eviction.owner),
                    },
                );
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
            return Err(GraphDbError::Conflict);
        }
        let reservation = match entry {
            RegistryEntry::Opening { .. }
            | RegistryEntry::Closing { .. }
            | RegistryEntry::Retiring { .. } => {
                return Err(GraphDbError::Conflict);
            }
            RegistryEntry::Ready { owner, .. } if require_unleased && !owner.is_unleased() => {
                return Err(GraphDbError::Conflict);
            }
            RegistryEntry::Faulted { error, .. } => return Err(error.clone()),
            RegistryEntry::Ready {
                authority_lease,
                binding,
                verified_locator,
                path,
                expected_format,
                owner,
                last_used,
            } => Eviction {
                authority_lease: Arc::clone(authority_lease),
                binding: binding.clone(),
                verified_locator: verified_locator.clone(),
                path: path.clone(),
                expected_format: *expected_format,
                owner: Arc::clone(owner),
                last_used: *last_used,
            },
        };
        state.entries.insert(
            requested_binding.shard_id.clone(),
            RegistryEntry::Closing {
                authority_lease: Arc::clone(&reservation.authority_lease),
                binding: reservation.binding.clone(),
                verified_locator: reservation.verified_locator.clone(),
                path: reservation.path.clone(),
                expected_format: reservation.expected_format,
                owner: Arc::clone(&reservation.owner),
            },
        );
        Ok(CloseReservation::Closing(Box::new(reservation)))
    }

    fn finish_eviction(&self, eviction: Eviction) -> Result<(), GraphDbError> {
        let close_result = eviction.owner.close();
        self.complete_close(eviction, close_result.clone())?;
        close_result
    }

    fn restore_ready(&self, eviction: Eviction) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get(&eviction.binding.shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph close reservation disappeared"))?;
        require_closing(entry, &eviction)?;
        let restored = RegistryEntry::Ready {
            authority_lease: eviction.authority_lease,
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
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get(&reservation.binding.shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph close reservation disappeared"))?;
        require_closing(entry, &reservation)?;
        match result {
            Ok(()) => {
                state.entries.remove(&reservation.binding.shard_id);
            }
            Err(error) => {
                state.entries.insert(
                    reservation.binding.shard_id.clone(),
                    RegistryEntry::Faulted {
                        authority_lease: reservation.authority_lease,
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

    fn restore_retiring(&self, eviction: Eviction) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get(&eviction.binding.shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph retirement reservation disappeared"))?;
        require_retiring(entry, &eviction)?;
        state.entries.insert(
            eviction.binding.shard_id.clone(),
            RegistryEntry::Ready {
                authority_lease: eviction.authority_lease,
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

    fn complete_retirement_close(
        &self,
        reservation: Eviction,
        result: Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get(&reservation.binding.shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph retirement reservation disappeared"))?;
        require_retiring(entry, &reservation)?;
        match result {
            Ok(()) => {
                state.entries.remove(&reservation.binding.shard_id);
            }
            Err(error) => {
                state.entries.insert(
                    reservation.binding.shard_id.clone(),
                    RegistryEntry::Faulted {
                        authority_lease: reservation.authority_lease,
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

    fn remove_opening(
        &self,
        shard_id: &StoreShardIdV1,
        requested_lease: &Arc<dyn RetainedGraphStoreLeaseV1>,
        requested_binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        path: &Path,
        expected_format: GraphFormatVersion,
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        if state.entries.get(shard_id).is_some_and(|entry| {
            matches!(
                entry,
                RegistryEntry::Opening { authority_lease, .. }
                    if Arc::ptr_eq(authority_lease, requested_lease)
            ) && require_binding(
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
        self.inner
            .state
            .lock()
            .map_err(|_| GraphDbError::unavailable("graph registry state lock is poisoned"))
    }
}

fn reserve_capacity_eviction(
    state: &mut RegistryState,
    max_open: usize,
    opening: &StoreShardIdV1,
) -> Result<Option<Eviction>, GraphDbError> {
    let open_count = state
        .entries
        .values()
        .filter(|entry| {
            matches!(
                entry,
                RegistryEntry::Opening { .. }
                    | RegistryEntry::Ready { .. }
                    | RegistryEntry::Closing { .. }
                    | RegistryEntry::Retiring { .. }
            )
        })
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
        authority_lease,
        binding,
        verified_locator,
        path,
        expected_format,
        owner,
        last_used,
        ..
    }) = state.entries.get(&candidate)
    else {
        return Err(GraphDbError::unavailable(
            "reserved graph eviction is not ready",
        ));
    };
    let eviction = Eviction {
        authority_lease: Arc::clone(authority_lease),
        binding: binding.clone(),
        verified_locator: verified_locator.clone(),
        path: path.clone(),
        expected_format: *expected_format,
        owner: Arc::clone(owner),
        last_used: *last_used,
    };
    state.entries.insert(
        candidate,
        RegistryEntry::Closing {
            authority_lease: Arc::clone(&eviction.authority_lease),
            binding: eviction.binding.clone(),
            verified_locator: eviction.verified_locator.clone(),
            path: eviction.path.clone(),
            expected_format: eviction.expected_format,
            owner: Arc::clone(&eviction.owner),
        },
    );
    Ok(Some(eviction))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_store::{
        BrainId, ProjectId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
        StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
        canonical_store_locator_digest,
    };

    use super::{
        CloseReservation, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
        GraphDbRegistryStatus, GraphDbRetirementOutcome, GraphDbRetirementTarget,
    };
    use crate::{GraphDbError, NeverCancelled};

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

    fn registration(root: &std::path::Path) -> GraphDbRegistration {
        let binding = StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                BrainId::try_from("brain.registry-retirement".to_owned()).unwrap(),
                UserProfileId::try_from("profile.registry-retirement".to_owned()).unwrap(),
                ProjectId::try_from("project.registry-retirement".to_owned()).unwrap(),
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

    #[test]
    fn durability_uncertain_close_is_a_committed_retirement_outcome() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let target = GraphDbRetirementTarget::new(
            registration.authority_lease.binding().clone(),
            registration.authority_lease.verified_locator().clone(),
        );
        let lease = registry.resolve(registration.clone()).unwrap();
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
    fn closed_ready_retirement_is_a_failed_outcome_and_stays_faulted() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
        let target = GraphDbRetirementTarget::new(
            registration.authority_lease.binding().clone(),
            registration.authority_lease.verified_locator().clone(),
        );
        let lease = registry.resolve(registration.clone()).unwrap();
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
        assert_eq!(
            registry.resolve(registration).unwrap_err(),
            GraphDbError::Closed
        );
    }

    #[test]
    fn closing_runtime_denies_resolution_without_waiting() {
        let temporary = TempDir::new().unwrap();
        let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
        let registration = registration(temporary.path());
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
        assert_eq!(
            registry.resolve(registration.clone()).unwrap_err(),
            GraphDbError::Conflict
        );
        registry.restore_ready(*reservation).unwrap();
    }
}
