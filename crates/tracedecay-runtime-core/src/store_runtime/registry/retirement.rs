use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreRuntimeBindingV1};

use super::capacity::drain_and_close_physical;
use super::{
    CanonicalGraphStoreOwnerRetirementTargetV1, CommittingRuntime,
    DatabaseRuntimeOwnerAttachmentReservationIdentityV1, FaultedRuntime, ReadyRuntime,
    RegistryEntry, RetiringRuntime, StoreRuntimeKey, StoreRuntimeOwnerAttachment,
    StoreRuntimeOwnerAttachmentRetirementReservationV1, StoreRuntimeRegistry,
    StoreRuntimeRegistryFailure,
};
use crate::db::{DatabaseAuthority, DatabaseOwnerRetirementReservationV1};

/// Exact registered runtime and originating authority selected for retirement.
///
/// The authority is part of target identity: a matching shard/incarnation with
/// a foreign authority never reserves the live attachment.
pub struct StoreRuntimeRetirementTarget {
    binding: StoreRuntimeBindingV1,
    authority: DatabaseAuthority,
    owner_attachment: Option<Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>>,
    graph_owner_attachment: Option<CanonicalGraphStoreOwnerRetirementTargetV1>,
}

impl StoreRuntimeRetirementTarget {
    #[must_use]
    pub fn new(binding: StoreRuntimeBindingV1, authority: DatabaseAuthority) -> Self {
        Self {
            binding,
            authority,
            owner_attachment: None,
            graph_owner_attachment: None,
        }
    }

    pub(crate) fn with_database_owner_attachment(
        binding: StoreRuntimeBindingV1,
        authority: DatabaseAuthority,
        owner_attachment: Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>,
    ) -> Self {
        Self {
            binding,
            authority,
            owner_attachment: Some(owner_attachment),
            graph_owner_attachment: None,
        }
    }

    pub(crate) fn with_owner_attachments(
        binding: StoreRuntimeBindingV1,
        authority: DatabaseAuthority,
        owner_attachment: Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>,
        graph_owner_attachment: CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> Self {
        Self {
            binding,
            authority,
            owner_attachment: Some(owner_attachment),
            graph_owner_attachment: Some(graph_owner_attachment),
        }
    }

    #[must_use]
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    #[must_use]
    pub fn authority(&self) -> &DatabaseAuthority {
        &self.authority
    }

    fn owner_attachment_identity(
        &self,
    ) -> Option<&DatabaseRuntimeOwnerAttachmentReservationIdentityV1> {
        self.owner_attachment
            .as_ref()
            .map(|reservation| reservation.identity())
    }

    fn graph_owner_attachment(&self) -> Option<&CanonicalGraphStoreOwnerRetirementTargetV1> {
        self.graph_owner_attachment.as_ref()
    }

    fn graph_owner_attachment_mut(
        &mut self,
    ) -> Option<&mut CanonicalGraphStoreOwnerRetirementTargetV1> {
        self.graph_owner_attachment.as_mut()
    }

    fn outcome_target(&self) -> Self {
        Self::new(self.binding.clone(), self.authority.clone())
    }

    fn commit_owner_attachment(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        match self.owner_attachment.as_mut() {
            Some(reservation) => reservation.commit(),
            None => Ok(()),
        }
    }

    fn terminalize_graph_owner_attachment(
        &mut self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        match self.graph_owner_attachment_mut() {
            Some(target) => target.terminalize_locked(registry, state),
            None => Ok(()),
        }
    }

    fn remove_graph_owner_attachment_after_store_close(
        &mut self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        match self.graph_owner_attachment_mut() {
            Some(target) => target.remove_after_store_close_locked(registry, state),
            None => Ok(()),
        }
    }

    fn terminalize_graph_owner_attachment_after_commit_failure(
        &mut self,
        registry: &StoreRuntimeRegistry,
    ) {
        let Some(target) = self.graph_owner_attachment_mut() else {
            return;
        };
        let mut state = registry.lock_state();
        target.terminalize_after_commit_failure_locked(registry, &mut state);
    }

    fn terminalize_after_commit_failure(&mut self) {
        if let Some(reservation) = self.owner_attachment.as_mut() {
            reservation.terminalize_after_commit_failure();
        }
    }
}

impl fmt::Debug for StoreRuntimeRetirementTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntimeRetirementTarget")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

/// Exact owner reservations returned from one pre-native-close Store attempt.
///
/// This handoff keeps the database and graph-map reservations paired so a
/// daemon owner slot can return to its retryable state without remounting or
/// minting a new owner identity. It is unavailable after Store's irreversible
/// commit boundary because terminal outcomes intentionally contain no owner
/// reservations.
pub struct DatabaseGraphOwnerRetirementHandoffV1 {
    database_owner_reservation: DatabaseOwnerRetirementReservationV1,
    graph_owner_target: CanonicalGraphStoreOwnerRetirementTargetV1,
}

impl DatabaseGraphOwnerRetirementHandoffV1 {
    /// Returns the exact move-only reservations for one later Store attempt.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        DatabaseOwnerRetirementReservationV1,
        CanonicalGraphStoreOwnerRetirementTargetV1,
    ) {
        (self.database_owner_reservation, self.graph_owner_target)
    }

    /// Abandons one pre-native-close retirement attempt, restoring database
    /// owner issuance while preserving the exact graph-map target for a later
    /// retirement of that same owner slot.
    #[must_use]
    pub fn cancel_to_ready_graph_target(self) -> CanonicalGraphStoreOwnerRetirementTargetV1 {
        let Self {
            database_owner_reservation,
            graph_owner_target,
        } = self;
        drop(database_owner_reservation);
        graph_owner_target
    }
}

impl StoreRuntimeRetirementTarget {
    /// Recovers the paired database and graph owner reservations after an
    /// uncommitted Store refusal or cancellation.
    ///
    /// On a non-paired or foreign bridge target, returns the exact unchanged
    /// Store target so its normal RAII restoration remains authoritative.
    pub fn into_database_graph_owner_handoff(
        mut self,
    ) -> Result<DatabaseGraphOwnerRetirementHandoffV1, Self> {
        let Some(owner_attachment) = self.owner_attachment.take() else {
            return Err(self);
        };
        let Some(graph_owner_target) = self.graph_owner_attachment.take() else {
            self.owner_attachment = Some(owner_attachment);
            return Err(self);
        };
        match owner_attachment.try_into_database_owner_retirement_reservation() {
            Ok(database_owner_reservation) => Ok(DatabaseGraphOwnerRetirementHandoffV1 {
                database_owner_reservation,
                graph_owner_target,
            }),
            Err(owner_attachment) => {
                self.owner_attachment = Some(owner_attachment);
                self.graph_owner_attachment = Some(graph_owner_target);
                Err(self)
            }
        }
    }
}

/// A falsifiable reason a retirement batch did not reserve any target.
#[derive(Debug)]
pub enum StoreRuntimeRetirementBlocker {
    EmptyBatch,
    DuplicateTarget {
        binding: Box<StoreRuntimeBindingV1>,
    },
    Missing {
        binding: Box<StoreRuntimeBindingV1>,
    },
    BindingMismatch {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    RuntimeState {
        binding: Box<StoreRuntimeBindingV1>,
        state: RuntimeMaintenanceStateV1,
    },
    AuthorityMismatch {
        binding: Box<StoreRuntimeBindingV1>,
    },
    IdentityValidation {
        binding: Box<StoreRuntimeBindingV1>,
        message: String,
    },
    OwnerAttachmentReservation {
        binding: Box<StoreRuntimeBindingV1>,
        message: String,
    },
    DatabaseAttachments {
        binding: Box<StoreRuntimeBindingV1>,
        count: usize,
    },
    ClientLeases {
        binding: Box<StoreRuntimeBindingV1>,
        count: usize,
    },
    OperationLeases {
        binding: Box<StoreRuntimeBindingV1>,
        count: usize,
    },
    RuntimeLeases {
        binding: Box<StoreRuntimeBindingV1>,
        count: usize,
    },
    ProfilePins {
        binding: Box<StoreRuntimeBindingV1>,
        count: usize,
    },
    RetainedGraphLeases {
        binding: Box<StoreRuntimeBindingV1>,
        count: usize,
    },
    Retiring {
        binding: Box<StoreRuntimeBindingV1>,
    },
    Committing {
        binding: Box<StoreRuntimeBindingV1>,
    },
    Faulted {
        binding: Box<StoreRuntimeBindingV1>,
    },
    DurabilityUncertain {
        binding: Box<StoreRuntimeBindingV1>,
    },
}

/// Outcome of all-or-none retirement preflight.
#[must_use]
pub enum StoreRuntimeRetirementResult {
    Reserved(StoreRuntimeRetirementReservation),
    Blocked(StoreRuntimeRetirementRefusal),
}

/// A pre-native-close retirement refusal that preserves every exact target.
///
/// Owner-attachment targets are move-only reservations. Returning them on a
/// blocked preflight lets callers retry the same database and graph owner
/// attachments after clearing a lease or cancellation fence; it never mints a
/// new owner identity or remounts a runtime.
#[derive(Debug)]
pub struct StoreRuntimeRetirementRefusal {
    blockers: Vec<StoreRuntimeRetirementBlocker>,
    targets: Vec<StoreRuntimeRetirementTarget>,
}

impl StoreRuntimeRetirementRefusal {
    fn new(
        blockers: Vec<StoreRuntimeRetirementBlocker>,
        targets: Vec<StoreRuntimeRetirementTarget>,
    ) -> Self {
        Self { blockers, targets }
    }

    #[must_use]
    pub fn blockers(&self) -> &[StoreRuntimeRetirementBlocker] {
        &self.blockers
    }

    #[must_use]
    pub fn targets(&self) -> &[StoreRuntimeRetirementTarget] {
        &self.targets
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Vec<StoreRuntimeRetirementBlocker>,
        Vec<StoreRuntimeRetirementTarget>,
    ) {
        (self.blockers, self.targets)
    }
}

/// Per-target terminal result of physical retirement.
#[derive(Debug)]
pub enum StoreRuntimeRetirementOutcome {
    Closed {
        target: StoreRuntimeRetirementTarget,
    },
    Faulted {
        target: StoreRuntimeRetirementTarget,
        error: StoreRuntimeRegistryFailure,
    },
    DurabilityUncertain {
        target: StoreRuntimeRetirementTarget,
        error: StoreRuntimeRegistryFailure,
    },
}

/// Completed retirement batch receipt. A physical-close failure remains an
/// outcome because the registry has already committed the `Committing` fence.
#[derive(Debug)]
pub struct StoreRuntimeRetirementCommit {
    outcomes: Vec<StoreRuntimeRetirementOutcome>,
}

impl StoreRuntimeRetirementCommit {
    #[must_use]
    pub fn outcomes(&self) -> &[StoreRuntimeRetirementOutcome] {
        &self.outcomes
    }
}

struct PendingRetirement {
    target: StoreRuntimeRetirementTarget,
    key: StoreRuntimeKey,
    owner: Arc<StoreRuntimeOwnerAttachment>,
}

/// RAII fence for a fully preflighted batch. Dropping an uncommitted
/// reservation restores only the exact `Retiring` entries it installed.
pub struct StoreRuntimeRetirementReservation {
    registry: StoreRuntimeRegistry,
    pending: Vec<PendingRetirement>,
    armed: bool,
}

impl StoreRuntimeRegistry {
    /// Preflights every target beneath one registry lock and transitions the
    /// whole batch to `Retiring` only if every exact target is clear.
    pub fn reserve_retirement_batch(
        &self,
        targets: Vec<StoreRuntimeRetirementTarget>,
    ) -> StoreRuntimeRetirementResult {
        let mut state = self.lock_state();
        let mut blockers = Vec::new();
        if targets.is_empty() {
            blockers.push(StoreRuntimeRetirementBlocker::EmptyBatch);
        }

        for (index, target) in targets.iter().enumerate() {
            if targets[..index].iter().any(|prior| {
                prior.binding == target.binding
                    && authorities_match(&prior.authority, &target.authority)
            }) {
                blockers.push(StoreRuntimeRetirementBlocker::DuplicateTarget {
                    binding: Box::new(target.binding.clone()),
                });
            }
        }

        for target in &targets {
            let key = StoreRuntimeKey::from_binding(&target.binding);
            let Some(entry) = state.entries.get(&key) else {
                blockers.push(StoreRuntimeRetirementBlocker::Missing {
                    binding: Box::new(target.binding.clone()),
                });
                continue;
            };
            let ready = match entry {
                RegistryEntry::Ready(ready) => ready,
                RegistryEntry::Opening(_) | RegistryEntry::Evicting(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::RuntimeState {
                        binding: Box::new(target.binding.clone()),
                        state: RuntimeMaintenanceStateV1::Opening,
                    });
                    continue;
                }
                RegistryEntry::Retiring(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::Retiring {
                        binding: Box::new(target.binding.clone()),
                    });
                    continue;
                }
                RegistryEntry::Committing(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::Committing {
                        binding: Box::new(target.binding.clone()),
                    });
                    continue;
                }
                RegistryEntry::Faulted(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::Faulted {
                        binding: Box::new(target.binding.clone()),
                    });
                    continue;
                }
                RegistryEntry::DurabilityUncertain(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::DurabilityUncertain {
                        binding: Box::new(target.binding.clone()),
                    });
                    continue;
                }
            };

            let binding = ready.owner.binding();
            if binding != &target.binding {
                blockers.push(StoreRuntimeRetirementBlocker::BindingMismatch {
                    expected: Box::new(target.binding.clone()),
                    actual: Box::new(binding.clone()),
                });
                continue;
            }
            let authority_matches = ready
                .owner
                .database_authority
                .as_ref()
                .is_some_and(|retained| authorities_match(retained, &target.authority));
            if !authority_matches {
                blockers.push(StoreRuntimeRetirementBlocker::AuthorityMismatch {
                    binding: Box::new(binding.clone()),
                });
                continue;
            }
            if let Err(error) = ready.owner.validate_database_write_authority(
                &target.authority,
                "preflight registered runtime retirement",
            ) {
                blockers.push(StoreRuntimeRetirementBlocker::IdentityValidation {
                    binding: Box::new(binding.clone()),
                    message: format!("{error:?}"),
                });
                continue;
            }
            if ready.owner.runtime().maintenance_state() != RuntimeMaintenanceStateV1::Ready {
                blockers.push(StoreRuntimeRetirementBlocker::RuntimeState {
                    binding: Box::new(binding.clone()),
                    state: ready.owner.runtime().maintenance_state(),
                });
                continue;
            }

            let owner_attachment = target.owner_attachment_identity();
            if let Some(owner_attachment) = owner_attachment {
                let reservation_matches_binding = owner_attachment.binding() == binding
                    && owner_attachment.binding() == &target.binding;
                let reservation_matches_locator =
                    owner_attachment.locator().verified() == ready.owner.locator().verified();
                let reservation_is_live = ready
                    .owner
                    .source
                    .validate_database_owner_attachment_reservation(owner_attachment);
                if !reservation_matches_binding
                    || !reservation_matches_locator
                    || reservation_is_live.is_err()
                {
                    blockers.push(StoreRuntimeRetirementBlocker::OwnerAttachmentReservation {
                        binding: Box::new(binding.clone()),
                        message: match reservation_is_live {
                            Ok(()) => {
                                "database owner attachment did not match the exact binding and locator"
                                    .to_owned()
                            }
                            Err(error) => format!("{error:?}"),
                        },
                    });
                    continue;
                }
            }
            let database_attachments = ready
                .owner
                .source
                .retirement_database_attachment_blockers(owner_attachment);
            if database_attachments != 0 {
                blockers.push(StoreRuntimeRetirementBlocker::DatabaseAttachments {
                    binding: Box::new(binding.clone()),
                    count: database_attachments,
                });
            }
            let leases = ready.owner.runtime().retirement_lease_counts();
            let client_leases = ready
                .owner
                .source
                .retirement_client_lease_blockers(&leases.client_tokens);
            if client_leases != 0 {
                blockers.push(StoreRuntimeRetirementBlocker::ClientLeases {
                    binding: Box::new(binding.clone()),
                    count: client_leases,
                });
            }
            if leases.operations != 0 {
                blockers.push(StoreRuntimeRetirementBlocker::OperationLeases {
                    binding: Box::new(binding.clone()),
                    count: leases.operations,
                });
            }
            if leases.runtime_leases != 0 {
                blockers.push(StoreRuntimeRetirementBlocker::RuntimeLeases {
                    binding: Box::new(binding.clone()),
                    count: leases.runtime_leases,
                });
            }
            let profile_pins = state
                .profile_pin_tokens
                .get(&key)
                .map_or(0, std::collections::BTreeSet::len);
            if profile_pins != 0 {
                blockers.push(StoreRuntimeRetirementBlocker::ProfilePins {
                    binding: Box::new(binding.clone()),
                    count: profile_pins,
                });
            }
            let graph_leases = state
                .graph_publications
                .get(&key)
                .map_or(0, |retained| retained.lease_tokens.len());
            if graph_leases != 0 {
                blockers.push(StoreRuntimeRetirementBlocker::RetainedGraphLeases {
                    binding: Box::new(binding.clone()),
                    count: graph_leases,
                });
            }
            let graph_map_owner_present = state
                .graph_publications
                .get(&key)
                .is_some_and(|retained| retained.owner_attachment.is_some());
            match (graph_map_owner_present, target.graph_owner_attachment()) {
                (true, Some(graph_owner_attachment)) => {
                    if !graph_owner_attachment.matches_binding(&target.binding) {
                        blockers.push(StoreRuntimeRetirementBlocker::OwnerAttachmentReservation {
                            binding: Box::new(binding.clone()),
                            message:
                                "graph map owner did not match the exact Store retirement target"
                                    .to_owned(),
                        });
                    } else if let Err(error) =
                        graph_owner_attachment.validate_map_owned_locked(self, &state)
                    {
                        blockers.push(StoreRuntimeRetirementBlocker::OwnerAttachmentReservation {
                            binding: Box::new(binding.clone()),
                            message: format!("{error:?}"),
                        });
                    }
                }
                (true, None) | (false, Some(_)) => {
                    blockers.push(StoreRuntimeRetirementBlocker::OwnerAttachmentReservation {
                        binding: Box::new(binding.clone()),
                        message: "graph map owner did not match an exact Store retirement target"
                            .to_owned(),
                    });
                }
                (false, None) => {}
            }
        }

        if !blockers.is_empty() {
            drop(state);
            return StoreRuntimeRetirementResult::Blocked(StoreRuntimeRetirementRefusal::new(
                blockers, targets,
            ));
        }

        let mut pending: Vec<PendingRetirement> = Vec::with_capacity(targets.len());
        let mut targets = targets.into_iter();
        while let Some(target) = targets.next() {
            let key = StoreRuntimeKey::from_binding(&target.binding);
            let owner = match state.entries.get(&key) {
                Some(RegistryEntry::Ready(ready)) => Ok(Arc::clone(&ready.owner)),
                None => Err(StoreRuntimeRetirementBlocker::Missing {
                    binding: Box::new(target.binding.clone()),
                }),
                Some(RegistryEntry::Opening(_) | RegistryEntry::Evicting(_)) => {
                    Err(StoreRuntimeRetirementBlocker::RuntimeState {
                        binding: Box::new(target.binding.clone()),
                        state: RuntimeMaintenanceStateV1::Opening,
                    })
                }
                Some(RegistryEntry::Retiring(_)) => Err(StoreRuntimeRetirementBlocker::Retiring {
                    binding: Box::new(target.binding.clone()),
                }),
                Some(RegistryEntry::Committing(_)) => {
                    Err(StoreRuntimeRetirementBlocker::Committing {
                        binding: Box::new(target.binding.clone()),
                    })
                }
                Some(RegistryEntry::Faulted(_)) => Err(StoreRuntimeRetirementBlocker::Faulted {
                    binding: Box::new(target.binding.clone()),
                }),
                Some(RegistryEntry::DurabilityUncertain(_)) => {
                    Err(StoreRuntimeRetirementBlocker::DurabilityUncertain {
                        binding: Box::new(target.binding.clone()),
                    })
                }
            };
            let owner = match owner {
                Ok(owner) => owner,
                Err(blocker) => {
                    let mut retry_targets = pending
                        .into_iter()
                        .map(|retirement| retirement.target)
                        .collect::<Vec<_>>();
                    retry_targets.push(target);
                    retry_targets.extend(targets);
                    blockers.push(blocker);
                    drop(state);
                    return StoreRuntimeRetirementResult::Blocked(
                        StoreRuntimeRetirementRefusal::new(blockers, retry_targets),
                    );
                }
            };
            pending.push(PendingRetirement { target, key, owner });
        }

        for position in 0..pending.len() {
            let reservation = pending[position]
                .target
                .graph_owner_attachment_mut()
                .map(|target| target.reserve_locked(self, &mut state));
            if let Some(Err(error)) = reservation {
                for prior in &mut pending[..position] {
                    if let Some(target) = prior.target.graph_owner_attachment_mut() {
                        let _ = target.restore_locked(self, &mut state);
                    }
                }
                blockers.push(StoreRuntimeRetirementBlocker::OwnerAttachmentReservation {
                    binding: Box::new(pending[position].target.binding().clone()),
                    message: format!("{error:?}"),
                });
                drop(state);
                let targets = pending
                    .into_iter()
                    .map(|retirement| retirement.target)
                    .collect();
                return StoreRuntimeRetirementResult::Blocked(StoreRuntimeRetirementRefusal::new(
                    blockers, targets,
                ));
            }
        }

        for retirement in &pending {
            state.entries.insert(
                retirement.key.clone(),
                RegistryEntry::Retiring(RetiringRuntime {
                    owner: Arc::clone(&retirement.owner),
                }),
            );
        }
        StoreRuntimeRetirementResult::Reserved(StoreRuntimeRetirementReservation {
            registry: self.clone(),
            pending,
            armed: true,
        })
    }

    fn begin_retirement_commit(
        &self,
        pending: &mut [PendingRetirement],
    ) -> Result<Option<String>, StoreRuntimeRegistryFailure> {
        let state = self.lock_state();
        for retirement in pending.iter() {
            let Some(RegistryEntry::Retiring(retiring)) = state.entries.get(&retirement.key) else {
                return Err(StoreRuntimeRegistryFailure::RetirementReservationLost {
                    key: Box::new(retirement.key.clone()),
                });
            };
            if !Arc::ptr_eq(&retiring.owner, &retirement.owner)
                || retiring.owner.binding() != retirement.target.binding()
            {
                return Err(StoreRuntimeRegistryFailure::RetirementReservationLost {
                    key: Box::new(retirement.key.clone()),
                });
            }
            if let Some(graph_owner_attachment) = retirement.target.graph_owner_attachment() {
                graph_owner_attachment.validate_reserved_locked(self, &state)?;
            }
        }
        drop(state);

        for retirement in pending.iter() {
            if let Some(owner_attachment) = retirement.target.owner_attachment.as_ref() {
                owner_attachment.preflight_commit()?;
            }
        }

        // The owner attachment is the irreversible authority fence. Keep the
        // registry in `Retiring` until every fallible owner commit has either
        // succeeded or yielded terminal truth; a global `Committing` state
        // must never race ahead of that exact attachment decision.
        let mut commit_failure = None;
        for retirement in pending.iter_mut() {
            if let Err(error) = retirement.target.commit_owner_attachment() {
                commit_failure = Some(format!("{error:?}"));
                break;
            }
        }
        if commit_failure.is_none() {
            let mut state = self.lock_state();
            for retirement in pending.iter_mut() {
                if let Err(error) = retirement
                    .target
                    .terminalize_graph_owner_attachment(self, &mut state)
                {
                    commit_failure = Some(format!("{error:?}"));
                    break;
                }
            }
        }
        if commit_failure.is_some() {
            for terminal in pending.iter_mut() {
                terminal
                    .target
                    .terminalize_graph_owner_attachment_after_commit_failure(self);
                terminal.target.terminalize_after_commit_failure();
            }
        }

        // The reservation exclusively owns the `Retiring` entries it placed.
        // Do not introduce a post-owner-commit fallible path that could leave
        // terminal attachment truth paired with a rollback-capable registry.
        let mut state = self.lock_state();
        for retirement in pending.iter() {
            state.entries.insert(
                retirement.key.clone(),
                RegistryEntry::Committing(CommittingRuntime {
                    owner: Arc::clone(&retirement.owner),
                }),
            );
        }
        Ok(commit_failure)
    }

    fn finish_retirement_target(
        &self,
        retirement: &mut PendingRetirement,
        success: bool,
        durability_uncertain: bool,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let mut state = self.lock_state();
        let Some(RegistryEntry::Committing(committing)) = state.entries.get(&retirement.key) else {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationLost {
                key: Box::new(retirement.key.clone()),
            });
        };
        if !Arc::ptr_eq(&committing.owner, &retirement.owner) {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationLost {
                key: Box::new(retirement.key.clone()),
            });
        }
        if success {
            retirement
                .target
                .remove_graph_owner_attachment_after_store_close(self, &mut state)?;
            state.entries.remove(&retirement.key);
            if retirement.key.is_profile()
                && state
                    .profile_authorities
                    .get(retirement.key.shard_id())
                    .is_some_and(|binding| binding == retirement.owner.binding())
            {
                state.profile_authorities.remove(retirement.key.shard_id());
            }
        } else {
            let faulted = FaultedRuntime {
                owner: Arc::clone(&retirement.owner),
            };
            let entry = if durability_uncertain {
                RegistryEntry::DurabilityUncertain(faulted)
            } else {
                RegistryEntry::Faulted(faulted)
            };
            state.entries.insert(retirement.key.clone(), entry);
        }
        Ok(())
    }

    fn restore_retiring_batch(
        &self,
        pending: &mut Vec<PendingRetirement>,
    ) -> Vec<StoreRuntimeRetirementTarget> {
        let mut restored = std::mem::take(pending);
        let mut state = self.lock_state();
        for retirement in &mut restored {
            if let Some(graph_owner_attachment) = retirement.target.graph_owner_attachment_mut() {
                let _ = graph_owner_attachment.restore_locked(self, &mut state);
            }
            let restore = matches!(
                state.entries.get(&retirement.key),
                Some(RegistryEntry::Retiring(retiring_entry))
                    if Arc::ptr_eq(&retiring_entry.owner, &retirement.owner)
                        && retiring_entry.owner.binding() == retirement.target.binding()
            );
            if restore {
                state.entries.insert(
                    retirement.key.clone(),
                    RegistryEntry::Ready(ReadyRuntime {
                        owner: Arc::clone(&retirement.owner),
                    }),
                );
            }
        }
        drop(state);
        restored
            .into_iter()
            .map(|retirement| retirement.target)
            .collect()
    }
}

impl StoreRuntimeRetirementReservation {
    /// Cancels this uncommitted reservation, restores its exact ready entries,
    /// and returns the original targets for a later attempt.
    ///
    /// Once [`Self::commit`] crosses the owner-attachment fence, terminal
    /// outcomes—not reusable targets—describe the physical-close result.
    pub fn cancel(
        &mut self,
    ) -> Result<Vec<StoreRuntimeRetirementTarget>, StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed);
        }
        let targets = self.registry.restore_retiring_batch(&mut self.pending);
        self.armed = false;
        Ok(targets)
    }

    /// Irreversibly commits the reservation before any physical close begins.
    /// All post-reservation failures are retained as typed terminal states;
    /// they are never restored to `Ready`.
    pub fn commit(&mut self) -> Result<StoreRuntimeRetirementCommit, StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed);
        }
        if let Some(message) = self.registry.begin_retirement_commit(&mut self.pending)? {
            let pending = std::mem::take(&mut self.pending);
            self.armed = false;
            let mut outcomes = Vec::with_capacity(pending.len());
            for mut retirement in pending {
                let target = retirement.target.outcome_target();
                let error =
                    match self
                        .registry
                        .finish_retirement_target(&mut retirement, false, false)
                    {
                        Ok(()) => StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed {
                            message: message.clone(),
                        },
                        Err(error) => error,
                    };
                outcomes.push(StoreRuntimeRetirementOutcome::Faulted { target, error });
            }
            return Ok(StoreRuntimeRetirementCommit { outcomes });
        }
        let pending = std::mem::take(&mut self.pending);
        self.armed = false;

        let mut outcomes = Vec::with_capacity(pending.len());
        for mut retirement in pending {
            let close = match retirement
                .owner
                .runtime()
                .transition(RuntimeMaintenanceStateV1::Draining)
            {
                Err(error) => Err((
                    StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                        message: error.to_string(),
                    },
                    false,
                )),
                Ok(()) => match drain_and_close_physical(&retirement.owner) {
                    Err(error) => {
                        let durability_uncertain = matches!(
                            &error,
                            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                operation: "close_and_join",
                                ..
                            }
                        );
                        Err((error, durability_uncertain))
                    }
                    Ok(()) => retirement
                        .owner
                        .runtime()
                        .transition(RuntimeMaintenanceStateV1::Closed)
                        .map_err(|error| {
                            (
                                StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                                    message: error.to_string(),
                                },
                                // Closing the state machine failed only after the physical
                                // attachment has been closed, so retry cannot safely reopen it.
                                true,
                            )
                        }),
                },
            };
            match close {
                Ok(()) => {
                    let target = retirement.target.outcome_target();
                    match self
                        .registry
                        .finish_retirement_target(&mut retirement, true, false)
                    {
                        Ok(()) => outcomes.push(StoreRuntimeRetirementOutcome::Closed { target }),
                        Err(error) => {
                            outcomes.push(StoreRuntimeRetirementOutcome::Faulted { target, error });
                        }
                    }
                }
                Err((error, durability_uncertain)) => {
                    let _ = retirement
                        .owner
                        .runtime()
                        .transition(RuntimeMaintenanceStateV1::Faulted);
                    let target = retirement.target.outcome_target();
                    let terminal_error = self.registry.finish_retirement_target(
                        &mut retirement,
                        false,
                        durability_uncertain,
                    );
                    let error = terminal_error.err().unwrap_or(error);
                    if durability_uncertain {
                        outcomes.push(StoreRuntimeRetirementOutcome::DurabilityUncertain {
                            target,
                            error,
                        });
                    } else {
                        outcomes.push(StoreRuntimeRetirementOutcome::Faulted { target, error });
                    }
                }
            }
        }
        Ok(StoreRuntimeRetirementCommit { outcomes })
    }
}

impl Drop for StoreRuntimeRetirementReservation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.registry.restore_retiring_batch(&mut self.pending);
            self.armed = false;
        }
    }
}

fn authorities_match(left: &DatabaseAuthority, right: &DatabaseAuthority) -> bool {
    left.token() == right.token()
        && left.role() == right.role()
        && left.database_identity_key() == right.database_identity_key()
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracedecay_domain::{BrainId, ProjectId, UserProfileId, UtcMicros};
    use tracedecay_store::{
        RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1, RuntimePublicationIdV1,
        StoreAuthorityEpochV1, StoreClientIdV1, StoreIncarnationV1, StoreRuntimeBindingV1,
        StoreRuntimeRegistryPublicationV1, StoreShardIdV1, VerifiedStoreLocatorV1,
        canonical_store_locator_digest,
    };

    use super::*;
    use crate::store_runtime::registry::{
        EmptyPhysicalRuntimeAttachment, PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot,
        ResolvedStoreLocator, ShardRuntimeBuildRequest, ShardRuntimePublisher, StoreRuntimeLookup,
        StoreRuntimeOpenBegin, StoreRuntimeOpenMode, StoreRuntimeOpenRequest,
        StoreRuntimeRegistryFuture, StoreRuntimeResolver,
    };
    use crate::store_runtime::shard::ShardRuntime;

    struct UnusedResolver;

    impl StoreRuntimeResolver for UnusedResolver {
        fn resolve<'a>(
            &'a self,
            _key: &'a StoreRuntimeKey,
            _mode: StoreRuntimeOpenMode,
            _database_authority: Option<&'a DatabaseAuthority>,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            Box::pin(async {
                Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "retirement fixture never opens a runtime".to_owned(),
                })
            })
        }

        fn resolve_graph<'a>(
            &'a self,
            key: &'a StoreRuntimeKey,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            let path = std::path::PathBuf::from(format!(
                "/retirement-graph/{:?}/{}",
                key.shard_id.scope,
                key.incarnation.get()
            ));
            let locator = VerifiedStoreLocatorV1::new(
                key.shard_id.clone(),
                key.incarnation,
                canonical_store_locator_digest(&path).unwrap(),
            );
            Box::pin(async move { Ok(ResolvedStoreLocator::new(locator, path)) })
        }
    }

    struct UnusedPublisher;

    impl ShardRuntimePublisher for UnusedPublisher {
        fn publish(
            &self,
            _request: ShardRuntimeBuildRequest,
        ) -> StoreRuntimeRegistryFuture<
            '_,
            Result<super::super::PublishedShardRuntime, StoreRuntimeRegistryFailure>,
        > {
            Box::pin(async {
                Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "publish retirement fixture runtime",
                    message: "retirement fixture installs a ready owner directly".to_owned(),
                })
            })
        }
    }

    struct DrainFailure;

    impl PhysicalRuntimeAttachment for DrainFailure {
        fn snapshot(&self) -> PhysicalRuntimeSnapshot {
            PhysicalRuntimeSnapshot::default()
        }

        fn opened_file_identity(&self) -> Result<u64, String> {
            Ok(0)
        }

        fn drain(&self) -> Result<(), String> {
            Err("injected retirement drain failure".to_owned())
        }

        fn close_and_join(&self) -> Result<(), String> {
            Ok(())
        }
    }

    struct CloseFailure;

    impl PhysicalRuntimeAttachment for CloseFailure {
        fn snapshot(&self) -> PhysicalRuntimeSnapshot {
            PhysicalRuntimeSnapshot::default()
        }

        fn opened_file_identity(&self) -> Result<u64, String> {
            Ok(0)
        }

        fn drain(&self) -> Result<(), String> {
            Ok(())
        }

        fn close_and_join(&self) -> Result<(), String> {
            Err("injected irreversible close failure".to_owned())
        }
    }

    struct FailingOwnerAttachmentReservation {
        identity: super::super::DatabaseRuntimeOwnerAttachmentReservationIdentityV1,
        _attachment: super::super::DatabaseRuntimeAttachment,
        fail_preflight: bool,
        fail_commit: bool,
        terminalized: Arc<AtomicBool>,
    }

    impl Drop for FailingOwnerAttachmentReservation {
        fn drop(&mut self) {
            if !self.terminalized.load(Ordering::SeqCst) {
                let _ = self.identity.restore();
            }
        }
    }

    impl StoreRuntimeOwnerAttachmentRetirementReservationV1 for FailingOwnerAttachmentReservation {
        fn identity(&self) -> &super::super::DatabaseRuntimeOwnerAttachmentReservationIdentityV1 {
            &self.identity
        }

        fn try_into_database_owner_retirement_reservation(
            self: Box<Self>,
        ) -> Result<
            crate::db::DatabaseOwnerRetirementReservationV1,
            Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>,
        > {
            Err(self)
        }

        fn preflight_commit(&self) -> Result<(), StoreRuntimeRegistryFailure> {
            self.identity.validate()?;
            if self.fail_preflight {
                return Err(StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed {
                    message: "injected owner preflight failure".to_owned(),
                });
            }
            Ok(())
        }

        fn commit(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
            self.identity.commit()?;
            if self.fail_commit {
                return Err(StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed {
                    message: "injected owner commit failure".to_owned(),
                });
            }
            Ok(())
        }

        fn terminalize_after_commit_failure(&mut self) {
            self.terminalized.store(true, Ordering::SeqCst);
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn profile_shard(profile: &str) -> StoreShardIdV1 {
        StoreShardIdV1::profile(
            id::<BrainId>("brain.runtime-retirement"),
            id::<UserProfileId>(profile),
        )
    }

    fn project_shard(project: &str) -> StoreShardIdV1 {
        StoreShardIdV1::project(
            id::<BrainId>("brain.runtime-retirement"),
            id::<UserProfileId>("profile.runtime-retirement"),
            id::<ProjectId>(project),
        )
    }

    fn registry() -> StoreRuntimeRegistry {
        StoreRuntimeRegistry::new(Arc::new(UnusedResolver), Arc::new(UnusedPublisher))
    }

    fn authority(root: &std::path::Path, name: &str) -> DatabaseAuthority {
        let path = root.join(format!("{name}.db"));
        std::fs::write(&path, []).unwrap();
        DatabaseAuthority::acquire_test(&path, "retirement fixture authority").unwrap()
    }

    fn install_ready(
        registry: &StoreRuntimeRegistry,
        shard_id: StoreShardIdV1,
        authority: DatabaseAuthority,
        attachment: Box<dyn PhysicalRuntimeAttachment>,
    ) -> (StoreRuntimeBindingV1, Arc<StoreRuntimeOwnerAttachment>) {
        let binding = StoreRuntimeBindingV1::new(
            shard_id,
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        );
        let key = StoreRuntimeKey::from_binding(&binding);
        let runtime = Arc::new(ShardRuntime::new(binding.clone(), key.is_profile()));
        runtime
            .transition(RuntimeMaintenanceStateV1::Opening)
            .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
            .unwrap();
        let verified = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(authority.canonical_database_path()).unwrap(),
        );
        let locator = super::super::RuntimeLocatorRecord::new(
            key.clone(),
            ResolvedStoreLocator::new(verified, authority.canonical_database_path().to_path_buf()),
        );
        let source = Arc::new(super::super::StoreRuntimeLeaseSource {
            publication: StoreRuntimeRegistryPublicationV1 {
                publication_id: RuntimePublicationIdV1::new(format!(
                    "retirement-publication-{}",
                    binding.authority_epoch.get()
                ))
                .unwrap(),
                binding: binding.clone(),
                published_at: super::super::utc_now(),
            },
            runtime,
            attachment: Arc::from(attachment),
            locator,
            opened_file_identity: crate::db::sqlite_generation_identity(
                authority.canonical_database_path(),
            )
            .unwrap(),
            database_authority: Some(authority),
            database_attachments: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            next_database_attachment_id: std::sync::atomic::AtomicU64::new(1),
            next_database_owner_id: std::sync::atomic::AtomicU64::new(1),
            next_database_attachment_reservation_id: std::sync::atomic::AtomicU64::new(1),
        });
        let owner = Arc::new(StoreRuntimeOwnerAttachment { source });
        let mut state = registry.lock_state();
        if key.is_profile() {
            state
                .profile_authorities
                .insert(key.shard_id().clone(), binding.clone());
        }
        state.entries.insert(
            key,
            RegistryEntry::Ready(ReadyRuntime {
                owner: Arc::clone(&owner),
            }),
        );
        (binding, owner)
    }

    fn target(
        binding: &StoreRuntimeBindingV1,
        owner: &StoreRuntimeOwnerAttachment,
    ) -> StoreRuntimeRetirementTarget {
        StoreRuntimeRetirementTarget::new(
            binding.clone(),
            owner
                .database_authority
                .clone()
                .expect("retirement fixture installs an authority"),
        )
    }

    fn failing_owner_target(
        binding: &StoreRuntimeBindingV1,
        owner: &StoreRuntimeOwnerAttachment,
        fail_preflight: bool,
        fail_commit: bool,
        terminalized: Arc<AtomicBool>,
    ) -> StoreRuntimeRetirementTarget {
        let attachment = owner
            .issue_client_lease()
            .unwrap()
            .into_database_attachment()
            .unwrap();
        let owner_id = attachment.allocate_owner_identity().unwrap();
        let identity = attachment.reserve_for_owner(owner_id).unwrap();
        StoreRuntimeRetirementTarget::with_database_owner_attachment(
            binding.clone(),
            owner.database_authority.clone().unwrap(),
            Box::new(FailingOwnerAttachmentReservation {
                identity,
                _attachment: attachment,
                fail_preflight,
                fail_commit,
                terminalized,
            }),
        )
    }

    fn stale_owner_target(
        binding: &StoreRuntimeBindingV1,
        owner: &StoreRuntimeOwnerAttachment,
    ) -> StoreRuntimeRetirementTarget {
        let attachment = owner
            .issue_client_lease()
            .unwrap()
            .into_database_attachment()
            .unwrap();
        let owner_id = attachment.allocate_owner_identity().unwrap();
        let mut identity = attachment.reserve_for_owner(owner_id).unwrap();
        identity.owner_id =
            super::super::DatabaseRuntimeOwnerIdentityV1(owner_id.0.checked_add(1).unwrap());
        StoreRuntimeRetirementTarget::with_database_owner_attachment(
            binding.clone(),
            owner.database_authority.clone().unwrap(),
            Box::new(FailingOwnerAttachmentReservation {
                identity,
                _attachment: attachment,
                fail_preflight: false,
                fail_commit: false,
                terminalized: Arc::new(AtomicBool::new(false)),
            }),
        )
    }

    #[tokio::test]
    async fn paired_graph_owner_target_reserves_and_restores_with_the_database_owner() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "paired-graph-owner");
        let (binding, owner) = install_ready(
            &registry,
            project_shard("project.paired-graph-owner"),
            authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let (graph_owner, graph_target) = registry
            .attach_graph_store_owner(StoreRuntimeKey::from_binding(&binding))
            .await
            .unwrap();
        let database_attachment = owner
            .issue_client_lease()
            .unwrap()
            .into_database_attachment()
            .unwrap();
        let database_owner_id = database_attachment.allocate_owner_identity().unwrap();
        let database_identity = database_attachment
            .reserve_for_owner(database_owner_id)
            .unwrap();
        let target = StoreRuntimeRetirementTarget::with_owner_attachments(
            binding.clone(),
            owner.database_authority.clone().unwrap(),
            Box::new(FailingOwnerAttachmentReservation {
                identity: database_identity,
                _attachment: database_attachment,
                fail_preflight: false,
                fail_commit: false,
                terminalized: Arc::new(AtomicBool::new(false)),
            }),
            graph_target,
        );

        let StoreRuntimeRetirementResult::Reserved(mut reservation) =
            registry.reserve_retirement_batch(vec![target])
        else {
            panic!("the exact paired owner attachments must reserve together");
        };
        {
            let state = registry.lock_state();
            let publication = state
                .graph_publications
                .get(&StoreRuntimeKey::from_binding(&binding))
                .expect("reserved graph owner publication");
            assert!(matches!(
                publication.owner_attachment,
                Some(super::super::GraphStoreOwnerAttachmentState::OwnerReserved { .. })
            ));
        }

        reservation.cancel().unwrap();
        {
            let state = registry.lock_state();
            let publication = state
                .graph_publications
                .get(&StoreRuntimeKey::from_binding(&binding))
                .expect("cancelled graph owner publication");
            assert!(matches!(
                publication.owner_attachment,
                Some(super::super::GraphStoreOwnerAttachmentState::MapOwned { .. })
            ));
        }
        drop(graph_owner);
    }

    #[tokio::test]
    async fn two_exact_owner_targets_survive_blocked_and_cancelled_retries() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let (first_binding, first_owner) = install_ready(
            &registry,
            project_shard("project.retry-owner-first"),
            authority(directory.path(), "retry-owner-first"),
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let (second_binding, second_owner) = install_ready(
            &registry,
            project_shard("project.retry-owner-second"),
            authority(directory.path(), "retry-owner-second"),
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let (first_graph_owner, first_graph_target) = registry
            .attach_graph_store_owner(StoreRuntimeKey::from_binding(&first_binding))
            .await
            .unwrap();
        let (second_graph_owner, second_graph_target) = registry
            .attach_graph_store_owner(StoreRuntimeKey::from_binding(&second_binding))
            .await
            .unwrap();
        let expected_first_graph_owner = {
            let state = registry.lock_state();
            state
                .graph_publications
                .get(&StoreRuntimeKey::from_binding(&first_binding))
                .expect("first graph publication exists")
                .owner_attachment
        };
        let expected_second_graph_owner = {
            let state = registry.lock_state();
            state
                .graph_publications
                .get(&StoreRuntimeKey::from_binding(&second_binding))
                .expect("second graph publication exists")
                .owner_attachment
        };

        let first_database_attachment = first_owner
            .issue_client_lease()
            .unwrap()
            .into_database_attachment()
            .unwrap();
        let first_database_identity = first_database_attachment
            .reserve_for_owner(first_database_attachment.allocate_owner_identity().unwrap())
            .unwrap();
        let expected_first_database_identity = first_database_identity.clone();
        let second_database_attachment = second_owner
            .issue_client_lease()
            .unwrap()
            .into_database_attachment()
            .unwrap();
        let second_database_identity = second_database_attachment
            .reserve_for_owner(
                second_database_attachment
                    .allocate_owner_identity()
                    .unwrap(),
            )
            .unwrap();
        let expected_second_database_identity = second_database_identity.clone();
        let first_target = StoreRuntimeRetirementTarget::with_owner_attachments(
            first_binding.clone(),
            first_owner.database_authority.clone().unwrap(),
            Box::new(FailingOwnerAttachmentReservation {
                identity: first_database_identity,
                _attachment: first_database_attachment,
                fail_preflight: false,
                fail_commit: false,
                terminalized: Arc::new(AtomicBool::new(false)),
            }),
            first_graph_target,
        );
        let second_target = StoreRuntimeRetirementTarget::with_owner_attachments(
            second_binding.clone(),
            second_owner.database_authority.clone().unwrap(),
            Box::new(FailingOwnerAttachmentReservation {
                identity: second_database_identity,
                _attachment: second_database_attachment,
                fail_preflight: false,
                fail_commit: false,
                terminalized: Arc::new(AtomicBool::new(false)),
            }),
            second_graph_target,
        );
        let blocker = first_owner.issue_client_lease().unwrap();

        let StoreRuntimeRetirementResult::Blocked(refusal) =
            registry.reserve_retirement_batch(vec![first_target, second_target])
        else {
            panic!("the live client must block both exact owner targets before reservation");
        };
        assert!(refusal.blockers().iter().any(|blocker| matches!(
            blocker,
            StoreRuntimeRetirementBlocker::ClientLeases { binding, count: 1 }
                if binding.as_ref() == &first_binding
        )));
        let (_, retry_targets) = refusal.into_parts();
        assert_eq!(retry_targets.len(), 2);
        assert_eq!(
            retry_targets[0]
                .owner_attachment_identity()
                .expect("first target retains its database owner attachment")
                .attachment_id,
            expected_first_database_identity.attachment_id
        );
        assert_eq!(
            retry_targets[1]
                .owner_attachment_identity()
                .expect("second target retains its database owner attachment")
                .attachment_id,
            expected_second_database_identity.attachment_id
        );
        drop(blocker);

        let StoreRuntimeRetirementResult::Reserved(mut reservation) =
            registry.reserve_retirement_batch(retry_targets)
        else {
            panic!("the exact targets must reserve once their blocker releases");
        };
        let retry_targets = reservation.cancel().unwrap();
        assert_eq!(retry_targets.len(), 2);
        assert_eq!(
            retry_targets[0]
                .owner_attachment_identity()
                .expect("cancelled first target retains its database owner attachment")
                .attachment_id,
            expected_first_database_identity.attachment_id
        );
        assert_eq!(
            retry_targets[1]
                .owner_attachment_identity()
                .expect("cancelled second target retains its database owner attachment")
                .attachment_id,
            expected_second_database_identity.attachment_id
        );
        {
            let state = registry.lock_state();
            assert_eq!(
                state
                    .graph_publications
                    .get(&StoreRuntimeKey::from_binding(&first_binding))
                    .expect("first graph publication remains after cancellation")
                    .owner_attachment,
                expected_first_graph_owner
            );
            assert_eq!(
                state
                    .graph_publications
                    .get(&StoreRuntimeKey::from_binding(&second_binding))
                    .expect("second graph publication remains after cancellation")
                    .owner_attachment,
                expected_second_graph_owner
            );
        }

        let StoreRuntimeRetirementResult::Reserved(reservation) =
            registry.reserve_retirement_batch(retry_targets)
        else {
            panic!("cancelled exact owner targets must reserve on the second retry");
        };
        drop(reservation);
        drop(first_graph_owner);
        drop(second_graph_owner);
    }

    fn active_lease(binding: &StoreRuntimeBindingV1, lease_id: &str) -> RuntimeLeaseV1 {
        RuntimeLeaseV1 {
            lease_id: RuntimeLeaseIdV1::new(lease_id).unwrap(),
            binding: binding.clone(),
            holder: StoreClientIdV1::new("retirement-fixture-client").unwrap(),
            acquired_at: UtcMicros(0),
            expires_at: UtcMicros(i64::MAX),
        }
    }

    #[test]
    fn preflight_is_all_or_none_and_foreign_identity_never_transitions_the_live_entry() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let initial_authority = authority(directory.path(), "first");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.first"),
            initial_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let foreign = StoreRuntimeBindingV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            StoreAuthorityEpochV1::new(2).unwrap(),
        );
        let foreign_authority = authority(directory.path(), "foreign-authority");

        let result = registry.reserve_retirement_batch(vec![
            target(&binding, &owner),
            target(&foreign, &owner),
            StoreRuntimeRetirementTarget::new(binding.clone(), foreign_authority),
        ]);
        assert!(matches!(
            result,
            StoreRuntimeRetirementResult::Blocked(refusal)
                if refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::BindingMismatch { expected, actual }
                        if expected.as_ref() == &foreign && actual.as_ref() == &binding
                )) && refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::AuthorityMismatch { binding: actual }
                        if actual.as_ref() == &binding
                ))
        ));
        let StoreRuntimeLookup::Ready(lease) = registry.lookup(&binding) else {
            panic!("all-or-none preflight must leave the matching runtime ready");
        };
        drop(lease);
    }

    #[test]
    fn owner_preflight_failure_rolls_back_every_target_before_committing_any() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let first_authority = authority(directory.path(), "preflight-first");
        let second_authority = authority(directory.path(), "preflight-second");
        let (first_binding, first_owner) = install_ready(
            &registry,
            profile_shard("profile.preflight-first"),
            first_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let (second_binding, second_owner) = install_ready(
            &registry,
            profile_shard("profile.preflight-second"),
            second_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let terminalized = Arc::new(AtomicBool::new(false));
        let StoreRuntimeRetirementResult::Reserved(mut reservation) = registry
            .reserve_retirement_batch(vec![
                failing_owner_target(
                    &first_binding,
                    &first_owner,
                    true,
                    false,
                    Arc::clone(&terminalized),
                ),
                target(&second_binding, &second_owner),
            ])
        else {
            panic!("preflight fixture must reserve both targets before commit");
        };

        assert!(matches!(
            reservation.commit(),
            Err(StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed { .. })
        ));
        assert!(!terminalized.load(Ordering::SeqCst));
        drop(reservation);
        assert!(matches!(
            registry.lookup(&first_binding),
            StoreRuntimeLookup::Ready(_)
        ));
        assert!(matches!(
            registry.lookup(&second_binding),
            StoreRuntimeLookup::Ready(_)
        ));
    }

    #[test]
    fn stale_foreign_owner_identity_cannot_reclassify_the_canonical_attachment() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "foreign-owner");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.foreign-owner"),
            authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );

        let result = registry.reserve_retirement_batch(vec![stale_owner_target(&binding, &owner)]);
        assert!(matches!(
            result,
            StoreRuntimeRetirementResult::Blocked(refusal)
                if refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::OwnerAttachmentReservation { .. }
                ))
        ));
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Ready(_)
        ));
    }

    #[test]
    fn owner_commit_failure_is_terminal_truth_for_the_entire_batch() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let first_authority = authority(directory.path(), "commit-first");
        let second_authority = authority(directory.path(), "commit-second");
        let (first_binding, first_owner) = install_ready(
            &registry,
            profile_shard("profile.commit-first"),
            first_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let (second_binding, second_owner) = install_ready(
            &registry,
            profile_shard("profile.commit-second"),
            second_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let terminalized = Arc::new(AtomicBool::new(false));
        let StoreRuntimeRetirementResult::Reserved(mut reservation) = registry
            .reserve_retirement_batch(vec![
                failing_owner_target(
                    &first_binding,
                    &first_owner,
                    false,
                    true,
                    Arc::clone(&terminalized),
                ),
                target(&second_binding, &second_owner),
            ])
        else {
            panic!("commit fixture must reserve both targets");
        };

        let commit = reservation.commit().unwrap();
        assert!(terminalized.load(Ordering::SeqCst));
        assert!(commit.outcomes().iter().all(|outcome| matches!(
            outcome,
            StoreRuntimeRetirementOutcome::Faulted {
                error: StoreRuntimeRegistryFailure::OwnerRetirementCommitFailed { .. },
                ..
            }
        )));
        assert!(matches!(
            registry.lookup(&first_binding),
            StoreRuntimeLookup::Faulted { .. }
        ));
        assert!(matches!(
            registry.lookup(&second_binding),
            StoreRuntimeLookup::Faulted { .. }
        ));
    }

    #[test]
    fn client_clone_and_direct_runtime_lease_block_then_release_for_retry() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "client-and-direct");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.client-and-direct"),
            authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let first = owner.issue_client_lease().unwrap();
        let first_clone = first.clone();
        let second = owner.issue_client_lease().unwrap();
        let direct = active_lease(&binding, "retirement.direct-lease");
        assert!(matches!(
            registry.acquire_lease(direct.clone()),
            super::super::StoreRuntimeLeaseAcquireResult::Acquired(_)
        ));

        let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
        assert!(matches!(
            blocked,
            StoreRuntimeRetirementResult::Blocked(refusal)
                if refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::ClientLeases { count: 2, .. }
                )) && refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::RuntimeLeases { count: 1, .. }
                ))
        ));

        drop(first);
        drop(first_clone);
        drop(second);
        assert!(registry.release_lease(&binding, &direct.lease_id));
        let StoreRuntimeRetirementResult::Reserved(reservation) =
            registry.reserve_retirement_batch(vec![target(&binding, &owner)])
        else {
            panic!("released independent tokens must permit a retry");
        };
        drop(reservation);
    }

    #[test]
    fn independently_attached_database_facade_blocks_retirement() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "database-facade");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.database-facade"),
            authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let facade = owner
            .issue_client_lease()
            .unwrap()
            .into_database_attachment()
            .unwrap();

        let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
        assert!(matches!(
            blocked,
            StoreRuntimeRetirementResult::Blocked(refusal)
                if refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::DatabaseAttachments { count: 1, .. }
                ))
        ));

        drop(facade);
        let StoreRuntimeRetirementResult::Reserved(reservation) =
            registry.reserve_retirement_batch(vec![target(&binding, &owner)])
        else {
            panic!("releasing the facade must permit an exact retry");
        };
        drop(reservation);
    }

    #[test]
    fn operation_profile_and_graph_leases_block_and_dropping_reservation_restores_ready() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "operation-profile-graph");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.operation-profile-graph"),
            authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let pin = match registry.profile_authority_pin(&binding.shard_id) {
            super::super::ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile fixture did not issue a pin: {other:?}"),
        };
        let client = owner.issue_client_lease().unwrap();
        let operation = client.begin_operation().unwrap();
        drop(client);
        {
            let mut state = registry.lock_state();
            state.graph_publications.insert(
                StoreRuntimeKey::from_binding(&binding),
                super::super::RetainedGraphPublication {
                    binding: binding.clone(),
                    verified_locator: owner.verified_locator().clone(),
                    canonical_path: owner.canonical_path().to_path_buf(),
                    owner_attachment: None,
                    lease_tokens: [1].into_iter().collect(),
                },
            );
        }
        let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
        assert!(matches!(
            blocked,
            StoreRuntimeRetirementResult::Blocked(refusal)
                if refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::OperationLeases { count: 1, .. }
                )) && refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::ProfilePins { count: 1, .. }
                )) && refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::RetainedGraphLeases { count: 1, .. }
                )) && !refusal.blockers().iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::ClientLeases { .. }
                ))
        ));

        drop(operation);
        drop(pin);
        registry.lock_state().graph_publications.clear();
        let StoreRuntimeRetirementResult::Reserved(reservation) =
            registry.reserve_retirement_batch(vec![target(&binding, &owner)])
        else {
            panic!("dropping every blocker must reserve the exact entry");
        };
        assert!(matches!(
            registry.acquire_lease(active_lease(&binding, "retirement.retiring")),
            super::super::StoreRuntimeLeaseAcquireResult::Rejected(
                StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
            )
        ));
        assert!(matches!(
            registry.profile_authority_pin(&binding.shard_id),
            super::super::ProfileAuthorityPinResult::Rejected(
                StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
            )
        ));
        let request =
            StoreRuntimeOpenRequest::new(binding.shard_id.clone(), binding.incarnation, None);
        assert!(matches!(
            registry.begin_or_join_open(&request),
            StoreRuntimeOpenBegin::Rejected(
                StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
            )
        ));
        drop(reservation);
        let StoreRuntimeLookup::Ready(lease) = registry.lookup(&binding) else {
            panic!("cancelling a reservation must restore its exact ready entry");
        };
        drop(lease);
    }

    #[test]
    fn postcommit_drain_failure_is_faulted_and_never_restored_to_ready() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "fault");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.fault"),
            authority,
            Box::new(DrainFailure),
        );
        let StoreRuntimeRetirementResult::Reserved(mut reservation) =
            registry.reserve_retirement_batch(vec![target(&binding, &owner)])
        else {
            panic!("clean target must reserve before a physical failure");
        };
        let commit = reservation.commit().unwrap();
        assert!(matches!(
            commit.outcomes(),
            [StoreRuntimeRetirementOutcome::Faulted { .. }]
        ));
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Faulted { .. }
        ));
    }

    #[test]
    fn postcommit_irreversible_close_failure_is_durability_uncertain() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "close-fault");
        let (binding, owner) = install_ready(
            &registry,
            profile_shard("profile.close-fault"),
            authority,
            Box::new(CloseFailure),
        );
        let StoreRuntimeRetirementResult::Reserved(mut reservation) =
            registry.reserve_retirement_batch(vec![target(&binding, &owner)])
        else {
            panic!("clean target must reserve before an irreversible close failure");
        };
        let commit = reservation.commit().unwrap();
        assert!(matches!(
            commit.outcomes(),
            [StoreRuntimeRetirementOutcome::DurabilityUncertain { .. }]
        ));
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::DurabilityUncertain { .. }
        ));
    }

    #[test]
    fn reservations_are_one_shot_after_commit_or_cancellation() {
        let directory = tempfile::tempdir().unwrap();

        let committed_registry = registry();
        let committed_authority = authority(directory.path(), "committed-once");
        let (committed_binding, committed_owner) = install_ready(
            &committed_registry,
            profile_shard("profile.committed-once"),
            committed_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let StoreRuntimeRetirementResult::Reserved(mut committed) = committed_registry
            .reserve_retirement_batch(vec![target(&committed_binding, &committed_owner)])
        else {
            panic!("clean target must reserve for one-shot commit");
        };
        assert!(matches!(
            committed.commit().unwrap().outcomes(),
            [StoreRuntimeRetirementOutcome::Closed { .. }]
        ));
        assert!(matches!(
            committed.commit(),
            Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed)
        ));

        let cancelled_registry = registry();
        let cancelled_authority = authority(directory.path(), "cancelled-once");
        let (cancelled_binding, cancelled_owner) = install_ready(
            &cancelled_registry,
            profile_shard("profile.cancelled-once"),
            cancelled_authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let StoreRuntimeRetirementResult::Reserved(mut cancelled) = cancelled_registry
            .reserve_retirement_batch(vec![target(&cancelled_binding, &cancelled_owner)])
        else {
            panic!("clean target must reserve for cancellation");
        };
        cancelled.cancel().unwrap();
        assert!(matches!(
            cancelled_registry.lookup(&cancelled_binding),
            StoreRuntimeLookup::Ready(_)
        ));
        assert!(matches!(
            cancelled.commit(),
            Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed)
        ));
        assert!(matches!(
            cancelled.cancel(),
            Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed)
        ));
    }

    #[tokio::test]
    async fn retain_rejects_retiring_project_without_waiting_for_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let registry = registry();
        let authority = authority(directory.path(), "retiring-project");
        let (binding, owner) = install_ready(
            &registry,
            project_shard("project.retiring"),
            authority,
            Box::new(EmptyPhysicalRuntimeAttachment),
        );
        let key = StoreRuntimeKey::from_binding(&binding);
        let StoreRuntimeRetirementResult::Reserved(reservation) =
            registry.reserve_retirement_batch(vec![target(&binding, &owner)])
        else {
            panic!("clean project target must reserve");
        };
        assert!(matches!(
            registry.retain_graph_store(key).await,
            Err(StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. })
        ));
        drop(reservation);
    }
}
