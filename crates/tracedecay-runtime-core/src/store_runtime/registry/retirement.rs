use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreRuntimeBindingV1};

use super::capacity::drain_and_close_physical;
use super::{
    CommittingRuntime, DatabaseRuntimeOwnerAttachmentReservationIdentityV1, FaultedRuntime,
    ReadyRuntime, RegistryEntry, RetiringRuntime, StoreRuntimeKey, StoreRuntimeOwnerAttachment,
    StoreRuntimeOwnerAttachmentRetirementReservationV1, StoreRuntimeRegistry,
    StoreRuntimeRegistryFailure,
};
use crate::db::DatabaseAuthority;

/// Exact registered runtime and originating authority selected for retirement.
///
/// The authority is part of target identity: a matching shard/incarnation with
/// a foreign authority never reserves the live attachment.
pub struct StoreRuntimeRetirementTarget {
    binding: StoreRuntimeBindingV1,
    authority: DatabaseAuthority,
    owner_attachment: Option<Box<dyn StoreRuntimeOwnerAttachmentRetirementReservationV1>>,
}

impl StoreRuntimeRetirementTarget {
    #[must_use]
    pub fn new(binding: StoreRuntimeBindingV1, authority: DatabaseAuthority) -> Self {
        Self {
            binding,
            authority,
            owner_attachment: None,
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

    fn outcome_target(&self) -> Self {
        Self::new(self.binding.clone(), self.authority.clone())
    }

    fn commit_owner_attachment(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        match self.owner_attachment.as_mut() {
            Some(reservation) => reservation.commit(),
            None => Ok(()),
        }
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
    Blocked(Vec<StoreRuntimeRetirementBlocker>),
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

        let mut pending = Vec::with_capacity(targets.len());
        for target in targets {
            let key = StoreRuntimeKey::from_binding(&target.binding);
            let Some(entry) = state.entries.get(&key) else {
                blockers.push(StoreRuntimeRetirementBlocker::Missing {
                    binding: Box::new(target.binding),
                });
                continue;
            };
            let ready = match entry {
                RegistryEntry::Ready(ready) => ready,
                RegistryEntry::Opening(_) | RegistryEntry::Evicting(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::RuntimeState {
                        binding: Box::new(target.binding),
                        state: RuntimeMaintenanceStateV1::Opening,
                    });
                    continue;
                }
                RegistryEntry::Retiring(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::Retiring {
                        binding: Box::new(target.binding),
                    });
                    continue;
                }
                RegistryEntry::Committing(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::Committing {
                        binding: Box::new(target.binding),
                    });
                    continue;
                }
                RegistryEntry::Faulted(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::Faulted {
                        binding: Box::new(target.binding),
                    });
                    continue;
                }
                RegistryEntry::DurabilityUncertain(_) => {
                    blockers.push(StoreRuntimeRetirementBlocker::DurabilityUncertain {
                        binding: Box::new(target.binding),
                    });
                    continue;
                }
            };

            let binding = ready.owner.binding();
            if binding != &target.binding {
                blockers.push(StoreRuntimeRetirementBlocker::BindingMismatch {
                    expected: Box::new(target.binding),
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
                .map_or(0, |tokens| tokens.len());
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
            pending.push(PendingRetirement {
                target,
                key,
                owner: Arc::clone(&ready.owner),
            });
        }

        if !blockers.is_empty() {
            return StoreRuntimeRetirementResult::Blocked(blockers);
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
        if let Some(message) = commit_failure {
            for terminal in pending.iter_mut() {
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
        retirement: &PendingRetirement,
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
                key: Box::new(retirement.key),
            });
        }
        if success {
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

    fn restore_retiring_batch(&self, pending: &mut Vec<PendingRetirement>) {
        let mut state = self.lock_state();
        for retirement in pending.drain(..) {
            let restore = matches!(
                state.entries.get(&retirement.key),
                Some(RegistryEntry::Retiring(retiring_entry))
                    if Arc::ptr_eq(&retiring_entry.owner, &retirement.owner)
                        && retiring_entry.owner.binding() == retirement.target.binding()
            );
            if restore {
                state.entries.insert(
                    retirement.key,
                    RegistryEntry::Ready(ReadyRuntime {
                        owner: retirement.owner,
                    }),
                );
            }
        }
    }
}

impl StoreRuntimeRetirementReservation {
    /// Cancels this uncommitted reservation and restores its exact ready
    /// entries. A consumed reservation cannot be cancelled or committed again.
    pub fn cancel(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        if !self.armed {
            return Err(StoreRuntimeRegistryFailure::RetirementReservationConsumed);
        }
        self.registry.restore_retiring_batch(&mut self.pending);
        self.armed = false;
        Ok(())
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
            for retirement in pending {
                let target = retirement.target.outcome_target();
                let error = match self
                    .registry
                    .finish_retirement_target(&retirement, false, false)
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
        for retirement in pending {
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
                        .finish_retirement_target(&retirement, true, false)
                    {
                        Ok(()) => outcomes.push(StoreRuntimeRetirementOutcome::Closed { target }),
                        Err(error) => {
                            outcomes.push(StoreRuntimeRetirementOutcome::Faulted { target, error })
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
                        &retirement,
                        false,
                        durability_uncertain,
                    );
                    let error = terminal_error.unwrap_or(error);
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
            self.registry.restore_retiring_batch(&mut self.pending);
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

    use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId, UtcMicros};
    use tracedecay_store::{
        RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1, RuntimePublicationIdV1,
        StoreAuthorityEpochV1, StoreClientIdV1, StoreIncarnationV1, StoreRuntimeBindingV1,
        StoreRuntimeRegistryPublicationV1, StoreShardIdV1, VerifiedStoreLocatorV1,
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
            _key: &'a StoreRuntimeKey,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            Box::pin(async {
                Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "retirement fixture never resolves a graph store".to_owned(),
                })
            })
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
            LocatorDigest::new(format!("sha256:{}", "r".repeat(64))).unwrap(),
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
            StoreRuntimeRetirementResult::Blocked(blockers)
                if blockers.iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::BindingMismatch { expected, actual }
                        if expected.as_ref() == &foreign && actual.as_ref() == &binding
                )) && blockers.iter().any(|blocker| matches!(
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
            StoreRuntimeRetirementResult::Blocked(blockers)
                if blockers.iter().any(|blocker| matches!(
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
            StoreRuntimeRetirementResult::Blocked(blockers)
                if blockers.iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::ClientLeases { count: 2, .. }
                )) && blockers.iter().any(|blocker| matches!(
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
            StoreRuntimeRetirementResult::Blocked(blockers)
                if blockers.iter().any(|blocker| matches!(
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
                    lease_tokens: [1].into_iter().collect(),
                },
            );
        }
        let blocked = registry.reserve_retirement_batch(vec![target(&binding, &owner)]);
        assert!(matches!(
            blocked,
            StoreRuntimeRetirementResult::Blocked(blockers)
                if blockers.iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::OperationLeases { count: 1, .. }
                )) && blockers.iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::ProfilePins { count: 1, .. }
                )) && blockers.iter().any(|blocker| matches!(
                    blocker,
                    StoreRuntimeRetirementBlocker::RetainedGraphLeases { count: 1, .. }
                )) && !blockers.iter().any(|blocker| matches!(
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
