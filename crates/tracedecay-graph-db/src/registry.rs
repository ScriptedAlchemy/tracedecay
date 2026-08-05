use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tracedecay_store::{StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1};

use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner,
    GraphDbRuntimeState, GraphDurability, GraphFormatVersion,
};

use self::identity::{
    binding, entry_binding, require_binding, require_closing, validate_registration,
};
use self::path::{GraphPathAnchor, validate_managed_graph_path};

#[path = "registry/identity.rs"]
mod identity;
#[path = "registry/path.rs"]
mod path;

const OPEN_WAIT_POLL: Duration = Duration::from_millis(10);

#[derive(Clone)]
pub struct GraphDbRegistration {
    pub binding: StoreRuntimeBindingV1,
    pub verified_locator: VerifiedStoreLocatorV1,
    pub canonical_path: PathBuf,
    pub cancellation: Arc<dyn GraphCancellation>,
    pub lifecycle_cancellation: Arc<dyn GraphCancellation>,
    pub deadline: Instant,
}

impl fmt::Debug for GraphDbRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphDbRegistration")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .field("canonical_path", &self.canonical_path)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
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
    state: Mutex<RegistryState>,
    changed: Condvar,
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreShardIdV1, RegistryEntry>,
}

enum RegistryEntry {
    Opening {
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
        owner: Arc<GraphDbOwner>,
        last_used: Instant,
    },
    Closing {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        expected_format: GraphFormatVersion,
        owner: Arc<GraphDbOwner>,
    },
    Faulted {
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
    Closing(Eviction),
}

impl GraphDbRegistry {
    pub fn new(config: GraphDbRegistryConfig) -> Result<Self, GraphDbError> {
        if config.max_open == 0 {
            return Err(GraphDbError::invalid(
                "graph registry max_open must be greater than zero",
            ));
        }
        Ok(Self {
            inner: Arc::new(RegistryInner {
                config,
                state: Mutex::new(RegistryState::default()),
                changed: Condvar::new(),
            }),
        })
    }

    pub fn resolve(&self, registration: GraphDbRegistration) -> Result<Arc<GraphDb>, GraphDbError> {
        check_request(registration.cancellation.as_ref(), registration.deadline)?;
        validate_registration(&registration)?;
        let path = registration.canonical_path.clone();
        validate_managed_graph_path(&path)?;
        let expected_format = GraphFormatVersion::current();
        let binding = registration.binding.clone();
        let verified_locator = registration.verified_locator.clone();
        let shard_id = binding.shard_id.clone();

        loop {
            check_request(registration.cancellation.as_ref(), registration.deadline)?;
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
                    let eviction = reserve_capacity_eviction(
                        &mut state,
                        self.inner.config.max_open,
                        &shard_id,
                    )?;
                    state.entries.insert(
                        shard_id.clone(),
                        RegistryEntry::Opening {
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
                        binding,
                        verified_locator,
                        path,
                        expected_format,
                        owner,
                        last_used: Instant::now(),
                    },
                );
                self.inner.changed.notify_all();
                check_request(registration.cancellation.as_ref(), registration.deadline)
                    .map(|()| database)
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
                } else {
                    state.entries.remove(&shard_id);
                }
                self.inner.changed.notify_all();
                Err(error)
            }
        }
    }

    pub fn reopen(&self, registration: GraphDbRegistration) -> Result<Arc<GraphDb>, GraphDbError> {
        check_request(registration.cancellation.as_ref(), registration.deadline)?;
        validate_registration(&registration)?;
        let path = registration.canonical_path.clone();
        validate_managed_graph_path(&path)?;
        let expected_format = GraphFormatVersion::current();
        if let CloseReservation::Closing(reservation) = self.reserve_close(
            &registration.binding,
            &registration.verified_locator,
            Some((&path, expected_format)),
        )? {
            if let Err(error) =
                check_request(registration.cancellation.as_ref(), registration.deadline)
            {
                self.restore_ready(reservation)?;
                return Err(error);
            }
            let close_result = reservation.owner.close();
            let physically_closed = reservation.owner.is_closed();
            self.complete_close(reservation, close_result.clone())?;
            if let Err(error) = close_result {
                if !physically_closed {
                    return Err(error);
                }
                self.remove_closed_fault(
                    &registration.binding,
                    &registration.verified_locator,
                    &path,
                    expected_format,
                )?;
            }
        }
        self.resolve(registration)
    }

    pub fn close(&self, registration: &GraphDbRegistration) -> Result<bool, GraphDbError> {
        check_request(registration.cancellation.as_ref(), registration.deadline)?;
        validate_registration(registration)?;
        let path = registration.canonical_path.clone();
        validate_managed_graph_path(&path)?;
        let reservation = match self.reserve_close(
            &registration.binding,
            &registration.verified_locator,
            Some((&path, GraphFormatVersion::current())),
        )? {
            CloseReservation::Absent => return Ok(false),
            CloseReservation::Removed => return Ok(true),
            CloseReservation::Closing(reservation) => reservation,
        };
        if let Err(error) = check_request(registration.cancellation.as_ref(), registration.deadline)
        {
            self.restore_ready(reservation)?;
            return Err(error);
        }
        let close_result = reservation.owner.close();
        self.complete_close(reservation, close_result.clone())?;
        close_result?;
        check_deadline(registration.deadline)?;
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
        validate_managed_graph_path(&registration.canonical_path)?;
        let state = self.state_lock()?;
        let Some(entry) = state.entries.get(&registration.binding.shard_id) else {
            return Ok(None);
        };
        require_binding(
            binding(entry),
            (
                &registration.binding,
                &registration.verified_locator,
                &registration.canonical_path,
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
            RegistryEntry::Ready { owner, .. } if !owner.is_unleased() => {
                return Err(GraphDbError::Conflict);
            }
            RegistryEntry::Faulted {
                owner: Some(owner), ..
            } if !owner.is_unleased() => return Err(GraphDbError::Conflict),
            RegistryEntry::Faulted { owner: None, .. } => {
                state.entries.remove(&requested_binding.shard_id);
                self.inner.changed.notify_all();
                return Ok(CloseReservation::Removed);
            }
            RegistryEntry::Ready {
                binding,
                verified_locator,
                path,
                expected_format,
                owner,
                last_used,
            } => Eviction {
                binding: binding.clone(),
                verified_locator: verified_locator.clone(),
                path: path.clone(),
                expected_format: *expected_format,
                owner: Arc::clone(owner),
                prior_fault: None,
                last_used: *last_used,
            },
            RegistryEntry::Faulted {
                binding,
                verified_locator,
                path,
                expected_format,
                owner: Some(owner),
                error,
            } => Eviction {
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
                binding: reservation.binding.clone(),
                verified_locator: reservation.verified_locator.clone(),
                path: reservation.path.clone(),
                expected_format: reservation.expected_format,
                owner: Arc::clone(&reservation.owner),
            },
        );
        Ok(CloseReservation::Closing(reservation))
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
                binding: eviction.binding,
                verified_locator: eviction.verified_locator,
                path: eviction.path,
                expected_format: eviction.expected_format,
                owner: Some(eviction.owner),
                error,
            }
        } else {
            RegistryEntry::Ready {
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
        .ok_or(GraphDbError::BudgetExhausted)?;
    let Some(RegistryEntry::Ready {
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
            binding: eviction.binding.clone(),
            verified_locator: eviction.verified_locator.clone(),
            path: eviction.path.clone(),
            expected_format: eviction.expected_format,
            owner: Arc::clone(&eviction.owner),
        },
    );
    Ok(Some(eviction))
}

fn reject_path_alias(
    state: &RegistryState,
    requested_binding: &StoreRuntimeBindingV1,
    requested_locator: &VerifiedStoreLocatorV1,
    path: &Path,
    expected_format: GraphFormatVersion,
) -> Result<(), GraphDbError> {
    for entry in state.entries.values() {
        let (registered_binding, registered_locator, registered_path, registered_format) =
            binding(entry);
        if registered_binding.shard_id == requested_binding.shard_id {
            require_binding(
                (
                    registered_binding,
                    registered_locator,
                    registered_path,
                    registered_format,
                ),
                (requested_binding, requested_locator, path, expected_format),
            )?;
        } else if registered_path == path {
            return Err(GraphDbError::Conflict);
        }
    }
    Ok(())
}

fn open_registered_graph(
    path: &Path,
    expected_format: GraphFormatVersion,
    registration: &GraphDbRegistration,
) -> Result<GraphDbOwner, GraphDbError> {
    check_request(
        registration.lifecycle_cancellation.as_ref(),
        registration.deadline,
    )?;
    let path_anchor = GraphPathAnchor::acquire(path)?;
    check_request(
        registration.lifecycle_cancellation.as_ref(),
        registration.deadline,
    )?;
    let owner = GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.to_path_buf()),
        expected_format,
        durability: GraphDurability::Sync,
        cancellation: Arc::clone(&registration.lifecycle_cancellation),
    })?;
    if let Err(error) = check_request(
        registration.lifecycle_cancellation.as_ref(),
        registration.deadline,
    ) {
        owner.close()?;
        return Err(error);
    }
    if let Err(error) = path_anchor.verify(path) {
        owner.close()?;
        return Err(error);
    }
    Ok(owner)
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

fn check_deadline(deadline: Instant) -> Result<(), GraphDbError> {
    if Instant::now() >= deadline {
        Err(GraphDbError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn check_request(
    cancellation: &dyn GraphCancellation,
    deadline: Instant,
) -> Result<(), GraphDbError> {
    check_cancelled(cancellation)?;
    check_deadline(deadline)
}

fn retains_fault(error: &GraphDbError) -> bool {
    matches!(
        error,
        GraphDbError::ResetRequired { .. }
            | GraphDbError::Corrupt { .. }
            | GraphDbError::DurabilityUncertain { .. }
    )
}

fn status(entry: &RegistryEntry) -> GraphDbRegistryStatus {
    match entry {
        RegistryEntry::Opening { .. } => GraphDbRegistryStatus::Opening,
        RegistryEntry::Closing { .. } => GraphDbRegistryStatus::Closing,
        RegistryEntry::Ready { owner, .. } => match owner.runtime_state() {
            GraphDbRuntimeState::Ready => GraphDbRegistryStatus::Ready,
            GraphDbRuntimeState::Closed => GraphDbRegistryStatus::Closed,
            GraphDbRuntimeState::DurabilityUncertain => GraphDbRegistryStatus::DurabilityUncertain,
        },
        RegistryEntry::Faulted { error, .. } => match error {
            GraphDbError::ResetRequired { .. } => GraphDbRegistryStatus::ResetRequired,
            GraphDbError::Corrupt { .. } => GraphDbRegistryStatus::Corrupt,
            GraphDbError::DurabilityUncertain { .. } => GraphDbRegistryStatus::DurabilityUncertain,
            GraphDbError::Cancelled
            | GraphDbError::InvalidRequest { .. }
            | GraphDbError::Conflict
            | GraphDbError::BudgetExhausted
            | GraphDbError::DeadlineExceeded
            | GraphDbError::Unavailable { .. }
            | GraphDbError::Closed => GraphDbRegistryStatus::Closed,
        },
    }
}
