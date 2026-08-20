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
    GraphBudgetKind, GraphCancellation, GraphDb, GraphDbError, GraphDbOwner, GraphDbRuntimeState,
    GraphFormatVersion, GraphGenerationManifestProvider,
};

use self::identity::{
    binding, entry_binding, require_binding, require_closing, validate_registration,
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
    prior_fault: Option<GraphDbError>,
    last_used: Instant,
}

enum CloseReservation {
    Absent,
    Removed,
    Closing(Box<Eviction>),
}

/// Exact identity admitted into a multi-graph retirement fence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphDbRetirementTargetV1 {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
}

impl GraphDbRetirementTargetV1 {
    pub fn new(binding: StoreRuntimeBindingV1, verified_locator: VerifiedStoreLocatorV1) -> Self {
        Self {
            binding,
            verified_locator,
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphDbRetirementBlockerKindV1 {
    Missing,
    Opening,
    Closing,
    Faulted,
    IdentityMismatch,
    ActiveLease,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphDbRetirementBlockerV1 {
    binding: StoreRuntimeBindingV1,
    kind: GraphDbRetirementBlockerKindV1,
}

impl GraphDbRetirementBlockerV1 {
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub const fn kind(&self) -> GraphDbRetirementBlockerKindV1 {
        self.kind
    }
}

pub enum GraphDbRetirementAdmissionV1 {
    Reserved(GraphDbRetirementReservationV1),
    Blocked(Vec<GraphDbRetirementBlockerV1>),
}

/// Fences all exact graph entries at once. Dropping this guard before commit
/// restores every entry to Ready; committing starts native close and any
/// durability failure is therefore terminal rather than a fabricated rollback.
pub struct GraphDbRetirementReservationV1 {
    registry: GraphDbRegistry,
    evictions: Vec<Eviction>,
    armed: bool,
}

impl GraphDbRetirementReservationV1 {
    pub fn bindings(&self) -> impl Iterator<Item = &StoreRuntimeBindingV1> {
        self.evictions.iter().map(|eviction| &eviction.binding)
    }

    pub fn commit_close(mut self) -> Result<Vec<StoreRuntimeBindingV1>, GraphDbError> {
        self.armed = false;
        let mut closed = Vec::with_capacity(self.evictions.len());
        for eviction in std::mem::take(&mut self.evictions) {
            let binding = eviction.binding.clone();
            let outcome = eviction.owner.close();
            self.registry.complete_close(eviction, outcome.clone())?;
            outcome?;
            closed.push(binding);
        }
        Ok(closed)
    }
}

impl Drop for GraphDbRetirementReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _restored = self.registry.restore_retirement_ready(&self.evictions);
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

    /// Opens (or joins) the registered runtime and returns the live handle.
    ///
    /// Replayed code graphs must read through [`crate::VerifiedGraphSnapshot`]
    /// via the publication surface; the raw runtime is for graphs whose native
    /// state is itself the authority (for example daemon-owned session
    /// relation graphs) and for direct storage tests.
    pub fn resolve(&self, registration: GraphDbRegistration) -> Result<Arc<GraphDb>, GraphDbError> {
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
                            return Ok(owner.handle());
                        }
                        GraphDbRuntimeState::Closed => {
                            state.entries.remove(&shard_id);
                            continue;
                        }
                        GraphDbRuntimeState::DurabilityUncertain => {
                            return Err(GraphDbError::DurabilityUncertain {
                                message: "registered graph handle has uncertain durability"
                                    .to_owned(),
                            });
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
                })
                | Some(RegistryEntry::Closing {
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
                let database = owner.handle();
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
    ) -> Result<Arc<GraphDb>, GraphDbError> {
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
            let physically_closed = reservation.owner.is_closed();
            self.complete_close(*reservation, close_result.clone())?;
            if let Err(error) = close_result {
                if !physically_closed {
                    return Err(error);
                }
                self.remove_closed_fault(
                    registration.binding(),
                    registration.verified_locator(),
                    &path,
                    expected_format,
                )?;
            }
        }
        self.resolve(registration)
    }

    /// Reopens a raw runtime only for direct storage tests and developer evals.
    ///
    /// Production recovery returns a verified snapshot through
    /// [`GraphDbRegistry::recover_verified_snapshot`].
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn reopen_raw_for_harness(
        &self,
        registration: GraphDbRegistration,
    ) -> Result<Arc<GraphDb>, GraphDbError> {
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
            CloseReservation::Removed => return Ok(true),
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

    /// Performs one linearizable preflight for all exact retained graphs and
    /// fences the whole set before any native owner is closed.
    pub fn reserve_exact_retirement(
        &self,
        targets: &[GraphDbRetirementTargetV1],
    ) -> GraphDbRetirementAdmissionV1 {
        let mut unique = BTreeMap::new();
        for target in targets {
            unique
                .entry(target.binding.shard_id.clone())
                .or_insert_with(|| target.clone());
        }
        let mut state = match self.state_lock() {
            Ok(state) => state,
            Err(_) => {
                return GraphDbRetirementAdmissionV1::Blocked(
                    unique
                        .into_values()
                        .map(|target| GraphDbRetirementBlockerV1 {
                            binding: target.binding,
                            kind: GraphDbRetirementBlockerKindV1::Faulted,
                        })
                        .collect(),
                );
            }
        };
        let mut blockers = Vec::new();
        for target in unique.values() {
            let Some(entry) = state.entries.get(&target.binding.shard_id) else {
                blockers.push(GraphDbRetirementBlockerV1 {
                    binding: target.binding.clone(),
                    kind: GraphDbRetirementBlockerKindV1::Missing,
                });
                continue;
            };
            let (binding, locator, _, _) = identity::binding(entry);
            if binding != &target.binding || locator != &target.verified_locator {
                blockers.push(GraphDbRetirementBlockerV1 {
                    binding: target.binding.clone(),
                    kind: GraphDbRetirementBlockerKindV1::IdentityMismatch,
                });
                continue;
            }
            match entry {
                RegistryEntry::Opening { .. } => blockers.push(GraphDbRetirementBlockerV1 {
                    binding: target.binding.clone(),
                    kind: GraphDbRetirementBlockerKindV1::Opening,
                }),
                RegistryEntry::Closing { .. } => blockers.push(GraphDbRetirementBlockerV1 {
                    binding: target.binding.clone(),
                    kind: GraphDbRetirementBlockerKindV1::Closing,
                }),
                RegistryEntry::Faulted { .. } => blockers.push(GraphDbRetirementBlockerV1 {
                    binding: target.binding.clone(),
                    kind: GraphDbRetirementBlockerKindV1::Faulted,
                }),
                RegistryEntry::Ready { owner, .. } if !owner.is_unleased() => {
                    blockers.push(GraphDbRetirementBlockerV1 {
                        binding: target.binding.clone(),
                        kind: GraphDbRetirementBlockerKindV1::ActiveLease,
                    });
                }
                RegistryEntry::Ready { .. } => {}
            }
        }
        if !blockers.is_empty() {
            return GraphDbRetirementAdmissionV1::Blocked(blockers);
        }
        let mut evictions = Vec::with_capacity(unique.len());
        for target in unique.values() {
            let Some(RegistryEntry::Ready {
                authority_lease,
                binding,
                verified_locator,
                path,
                expected_format,
                owner,
                last_used,
            }) = state.entries.get(&target.binding.shard_id)
            else {
                return GraphDbRetirementAdmissionV1::Blocked(vec![GraphDbRetirementBlockerV1 {
                    binding: target.binding.clone(),
                    kind: GraphDbRetirementBlockerKindV1::Closing,
                }]);
            };
            let eviction = Eviction {
                authority_lease: Arc::clone(authority_lease),
                binding: binding.clone(),
                verified_locator: verified_locator.clone(),
                path: path.clone(),
                expected_format: *expected_format,
                owner: Arc::clone(owner),
                prior_fault: None,
                last_used: *last_used,
            };
            state.entries.insert(
                target.binding.shard_id.clone(),
                RegistryEntry::Closing {
                    authority_lease: Arc::clone(&eviction.authority_lease),
                    binding: eviction.binding.clone(),
                    verified_locator: eviction.verified_locator.clone(),
                    path: eviction.path.clone(),
                    expected_format: eviction.expected_format,
                    owner: Arc::clone(&eviction.owner),
                },
            );
            evictions.push(eviction);
        }
        GraphDbRetirementAdmissionV1::Reserved(GraphDbRetirementReservationV1 {
            registry: self.clone(),
            evictions,
            armed: true,
        })
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
    /// hold closed-handle `Arc`s. Idle eviction stays fail-closed on a live
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
                CloseReservation::Removed => return Ok(true),
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
                        prior_fault: None,
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
            RegistryEntry::Opening { .. } | RegistryEntry::Closing { .. } => {
                return Err(GraphDbError::Conflict);
            }
            RegistryEntry::Ready { owner, .. } if require_unleased && !owner.is_unleased() => {
                return Err(GraphDbError::Conflict);
            }
            RegistryEntry::Faulted {
                owner: Some(owner), ..
            } if require_unleased && !owner.is_unleased() => {
                return Err(GraphDbError::Conflict);
            }
            RegistryEntry::Faulted { owner: None, .. } => {
                state.entries.remove(&requested_binding.shard_id);
                self.inner.changed.notify_all();
                return Ok(CloseReservation::Removed);
            }
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
                prior_fault: None,
                last_used: *last_used,
            },
            RegistryEntry::Faulted {
                authority_lease,
                binding,
                verified_locator,
                path,
                expected_format,
                owner: Some(owner),
                error,
            } => Eviction {
                authority_lease: Arc::clone(authority_lease),
                binding: binding.clone(),
                verified_locator: verified_locator.clone(),
                path: path.clone(),
                expected_format: *expected_format,
                owner: Arc::clone(owner),
                prior_fault: Some(error.clone()),
                last_used: Instant::now(),
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
        let restored = if let Some(error) = eviction.prior_fault {
            RegistryEntry::Faulted {
                authority_lease: eviction.authority_lease,
                binding: eviction.binding,
                verified_locator: eviction.verified_locator,
                path: eviction.path,
                expected_format: eviction.expected_format,
                owner: Some(eviction.owner),
                error,
            }
        } else {
            RegistryEntry::Ready {
                authority_lease: eviction.authority_lease,
                binding: eviction.binding,
                verified_locator: eviction.verified_locator,
                path: eviction.path,
                expected_format: eviction.expected_format,
                owner: eviction.owner,
                last_used: eviction.last_used,
            }
        };
        state
            .entries
            .insert(entry_binding(&restored).shard_id.clone(), restored);
        self.inner.changed.notify_all();
        Ok(())
    }

    fn restore_retirement_ready(&self, evictions: &[Eviction]) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        for eviction in evictions {
            let entry = state
                .entries
                .get(&eviction.binding.shard_id)
                .ok_or_else(|| {
                    GraphDbError::unavailable("graph retirement reservation disappeared")
                })?;
            require_closing(entry, eviction)?;
        }
        for eviction in evictions {
            state.entries.insert(
                eviction.binding.shard_id.clone(),
                RegistryEntry::Ready {
                    authority_lease: Arc::clone(&eviction.authority_lease),
                    binding: eviction.binding.clone(),
                    verified_locator: eviction.verified_locator.clone(),
                    path: eviction.path.clone(),
                    expected_format: eviction.expected_format,
                    owner: Arc::clone(&eviction.owner),
                    last_used: eviction.last_used,
                },
            );
        }
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

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    fn remove_closed_fault(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        path: &Path,
        expected_format: GraphFormatVersion,
    ) -> Result<(), GraphDbError> {
        let mut state = self.state_lock()?;
        let Some(RegistryEntry::Faulted {
            binding: registered_binding,
            verified_locator: registered_locator,
            path: registered_path,
            expected_format: registered_format,
            owner: Some(owner),
            ..
        }) = state.entries.get(&binding.shard_id)
        else {
            return Err(GraphDbError::unavailable(
                "closed graph fault reservation disappeared",
            ));
        };
        if registered_binding != binding
            || registered_locator != verified_locator
            || registered_path != path
            || *registered_format != expected_format
            || !owner.is_closed()
        {
            return Err(GraphDbError::Conflict);
        }
        state.entries.remove(&binding.shard_id);
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
        prior_fault: None,
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
