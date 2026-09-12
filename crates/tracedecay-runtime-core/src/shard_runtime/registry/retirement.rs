use std::fmt;
use std::sync::Arc;

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
    #[hotpath::measure(label = "runtime_core.registry.retirement_reserve")]
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
            hotpath::gauge!("runtime_core.registry.retirement_blocks").inc(1.0);
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
                    hotpath::gauge!("runtime_core.registry.retirement_blocks").inc(1.0);
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
                hotpath::gauge!("runtime_core.registry.retirement_blocks").inc(1.0);
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
        if !pending.is_empty() {
            hotpath::gauge!("runtime_core.registry.retirement_pending").inc(pending.len() as f64);
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
            hotpath::gauge!("runtime_core.registry.runtimes_ready").dec(1.0);
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
    #[hotpath::measure(label = "runtime_core.registry.retirement_cancel")]
    pub fn cancel(
        &mut self,
    ) -> Result<Vec<StoreRuntimeRetirementTarget>, StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed);
        }
        let count = self.pending.len();
        let targets = self.registry.restore_retiring_batch(&mut self.pending);
        self.armed = false;
        if count > 0 {
            hotpath::gauge!("runtime_core.registry.retirement_pending").dec(count as f64);
        }
        Ok(targets)
    }

    /// Irreversibly commits the reservation before any physical close begins.
    /// All post-reservation failures are retained as typed terminal states;
    /// they are never restored to `Ready`.
    #[hotpath::measure(label = "runtime_core.registry.retirement_commit")]
    pub fn commit(&mut self) -> Result<StoreRuntimeRetirementCommit, StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed);
        }
        if let Some(message) = self.registry.begin_retirement_commit(&mut self.pending)? {
            let pending = std::mem::take(&mut self.pending);
            self.armed = false;
            if !pending.is_empty() {
                let count = pending.len() as f64;
                hotpath::gauge!("runtime_core.registry.retirement_pending").dec(count);
                hotpath::gauge!("runtime_core.registry.retirement_commits").inc(count);
            }
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
        if !pending.is_empty() {
            let count = pending.len() as f64;
            hotpath::gauge!("runtime_core.registry.retirement_pending").dec(count);
            hotpath::gauge!("runtime_core.registry.retirement_commits").inc(count);
        }

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
            let count = self.pending.len();
            let _ = self.registry.restore_retiring_batch(&mut self.pending);
            self.armed = false;
            if count > 0 {
                hotpath::gauge!("runtime_core.registry.retirement_pending").dec(count as f64);
            }
        }
    }
}

fn authorities_match(left: &DatabaseAuthority, right: &DatabaseAuthority) -> bool {
    left.token() == right.token()
        && left.role() == right.role()
        && left.database_identity_key() == right.database_identity_key()
}

#[cfg(test)]
mod tests;
