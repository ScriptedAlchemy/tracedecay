use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use asupersync::channel::watch;

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use super::capacity::{EvictionReservation, drain_and_close_physical};
use super::{
    EvictingRuntime, RegistryEntry, StoreRuntimeKey, StoreRuntimeOwnerAttachment,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};
use crate::db::DatabaseAuthority;
use crate::shard_runtime::shard::ShardRuntimeEvictionBlocker;

const SHUTDOWN_HOLD_DRAIN_POLL: Duration = Duration::from_millis(10);

/// A pass is the last one when the next poll would land at or past the
/// deadline, so the drain logs and returns before its caller's deadline.
fn is_last_drain_pass(now: std::time::Instant, deadline: std::time::Instant) -> bool {
    now + SHUTDOWN_HOLD_DRAIN_POLL >= deadline
}

/// What still keeps one mounted runtime from closing at shutdown.
struct RuntimeHold {
    blockers: Vec<ShardRuntimeEvictionBlocker>,
    profile_pins: usize,
    graph_leases: usize,
}

impl RuntimeHold {
    fn of(
        state: &super::RegistryState,
        key: &StoreRuntimeKey,
        owner: &StoreRuntimeOwnerAttachment,
    ) -> Self {
        let mut blockers = owner
            .runtime()
            .eviction_eligibility(Duration::ZERO)
            .blockers;
        blockers.retain(|blocker| !matches!(blocker, ShardRuntimeEvictionBlocker::PinnedProfile));
        Self {
            blockers,
            profile_pins: state.profile_pin_tokens.get(key).map_or(0, BTreeSet::len),
            graph_leases: state
                .graph_publications
                .get(key)
                .map_or(0, |retained| retained.lease_tokens.len()),
        }
    }

    fn is_idle(&self) -> bool {
        self.blockers.is_empty() && self.profile_pins == 0 && self.graph_leases == 0
    }
}

/// Proof that one exact runtime reached `Closed` after all physical `SQLite`
/// handles joined and before its registry entry was removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedStoreRuntime {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    path: std::path::PathBuf,
    opened_file_identity: u64,
}

impl ClosedStoreRuntime {
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn opened_file_identity(&self) -> u64 {
        self.opened_file_identity
    }
}

struct CloseReservation {
    key: StoreRuntimeKey,
    attempt: u64,
    owner: Arc<StoreRuntimeOwnerAttachment>,
    completion: Arc<watch::Sender<Option<Result<ClosedStoreRuntime, StoreRuntimeRegistryFailure>>>>,
}

struct CloseAttemptGuard {
    registry: StoreRuntimeRegistry,
    reservation: Option<CloseReservation>,
}

impl CloseAttemptGuard {
    fn complete(mut self, outcome: Result<(), StoreRuntimeRegistryFailure>) {
        if let Some(reservation) = self.reservation.take() {
            let completion = reservation.completion.clone();
            let result = self.registry.finish_exact_close(reservation, outcome);
            if let Err(error) = completion.send(Some(result)) {
                tracing::error!(%error, "failed to publish registered runtime close completion");
            }
        }
    }
}

impl Drop for CloseAttemptGuard {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            let completion = reservation.completion.clone();
            let result = self.registry.finish_exact_close(
                reservation,
                Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "settle exact registered runtime close",
                    message: "native close task ended without completing".to_owned(),
                }),
            );
            if let Err(error) = completion.send(Some(result)) {
                tracing::error!(%error, "failed to publish registered runtime close failure");
            }
        }
    }
}

/// Completes one reserved shutdown eviction exactly once, inline when its
/// settlement task never runs.
struct ShutdownEviction {
    registry: StoreRuntimeRegistry,
    reservation: Option<EvictionReservation>,
    outcomes: std::sync::mpsc::Sender<Result<(), StoreRuntimeRegistryFailure>>,
}

impl ShutdownEviction {
    fn complete(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            let outcome = self.registry.complete_eviction(reservation);
            if let Err(std::sync::mpsc::SendError(Err(failure))) = self.outcomes.send(outcome) {
                tracing::warn!(
                    ?failure,
                    "shutdown close failed after its waiter stopped waiting"
                );
            }
        }
    }
}

impl Drop for ShutdownEviction {
    fn drop(&mut self) {
        self.complete();
    }
}

impl StoreRuntimeRegistry {
    /// Closes every mounted runtime that no lease, queued work, profile pin,
    /// or graph lease still holds, so each writer runs its shutdown TRUNCATE
    /// checkpoint. The daemon process exits with this registry reachable, so
    /// no destructor closes these attachments otherwise. In-flight opens are
    /// cancelled and joined first, so none publishes or keeps a physical
    /// handle after the scan. Each reserved runtime owns its own writer and
    /// readers, so they close concurrently and the shutdown waits for the
    /// slowest checkpoint rather than their sum.
    ///
    /// A held runtime is scanned again until its holders let go or `deadline`
    /// passes: owners joined before store close can still be unwinding the
    /// tasks that hold their leases, and under load that tail outlasts the
    /// owner join (#4138). The deadline is the caller's store-close budget, so
    /// the drain spends whatever the phases before it left. The last pass is
    /// the last poll that fits before `deadline`, so the close returns, and
    /// logs each runtime still held with its blockers, before the caller's
    /// own deadline cancels it. A runtime still held then stays mounted.
    /// Returns the number of runtimes closed.
    #[tracing::instrument(
        name = "runtime_core.registry.close_idle_for_shutdown",
        level = "trace",
        skip_all
    )]
    pub async fn close_idle_for_shutdown(
        &self,
        deadline: std::time::Instant,
    ) -> Result<usize, StoreRuntimeRegistryFailure> {
        let cx =
            crate::native_runtime::cx().ok_or(StoreRuntimeRegistryFailure::RuntimeUnavailable {
                operation: "close idle registered runtimes for shutdown",
            })?;
        self.cancel_and_join_opens_for_shutdown(&cx).await?;
        let mut closed = 0;
        loop {
            let final_pass = is_last_drain_pass(std::time::Instant::now(), deadline);
            let (batch, held) = self.close_idle_batch(final_pass).await?;
            closed += batch;
            if !held || final_pass {
                return Ok(closed);
            }
            if crate::native_runtime::sleep_until(
                std::time::Instant::now() + SHUTDOWN_HOLD_DRAIN_POLL,
            )
            .await
            .is_err()
            {
                return self
                    .close_idle_batch(true)
                    .await
                    .map(|(batch, _)| closed + batch);
            }
        }
    }

    /// Closes every runtime idle right now and reports whether any stays
    /// held. Only the final pass logs the held ones.
    async fn close_idle_batch(
        &self,
        final_pass: bool,
    ) -> Result<(usize, bool), StoreRuntimeRegistryFailure> {
        let (reservations, held, reserve_failure) = {
            let mut state = self.lock_state();
            let mut idle = Vec::new();
            let mut held = false;
            for (key, entry) in &state.entries {
                let RegistryEntry::Ready(ready) = entry else {
                    continue;
                };
                let hold = RuntimeHold::of(&state, key, &ready.owner);
                if hold.is_idle() {
                    idle.push(key.clone());
                    continue;
                }
                held = true;
                if final_pass {
                    tracing::warn!(
                        path = %ready.owner.locator().path().display(),
                        blockers = ?hold.blockers,
                        profile_pins = hold.profile_pins,
                        graph_leases = hold.graph_leases,
                        "store runtime still held at shutdown; its WAL is not truncated"
                    );
                }
            }
            let mut reservations = Vec::with_capacity(idle.len());
            let mut reserve_failure = None;
            for key in idle {
                match Self::reserve_eviction(&mut state, key) {
                    Ok(reservation) => reservations.extend(reservation),
                    Err(failure) => {
                        reserve_failure = Some(failure);
                        break;
                    }
                }
            }
            (reservations, held, reserve_failure)
        };
        let closed = reservations.len();
        let mut first_failure = reserve_failure;
        let (outcomes, results) = std::sync::mpsc::channel();
        for reservation in reservations {
            let mut eviction = ShutdownEviction {
                registry: self.clone(),
                reservation: Some(reservation),
                outcomes: outcomes.clone(),
            };
            // A refused closure is dropped unrun, and its eviction completes inline.
            if let Err(error) = self
                .inner
                .operations
                .retain_blocking(move || eviction.complete())
            {
                first_failure.get_or_insert(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "spawn shutdown close of idle registered runtimes",
                    message: error.to_string(),
                });
            }
        }
        drop(outcomes);
        // Exact closes admitted before the idle scan settle on the same owner.
        self.inner.operations.settled().await;
        for outcome in results.try_iter() {
            if let Err(failure) = outcome {
                first_failure.get_or_insert(failure);
            }
        }
        first_failure.map_or(Ok((closed, held)), Err)
    }

    #[tracing::instrument(name = "runtime_core.registry.close_path", level = "trace", skip_all)]
    pub async fn close_path(
        &self,
        path: &Path,
    ) -> Result<Option<ClosedStoreRuntime>, StoreRuntimeRegistryFailure> {
        let selected = {
            let state = self.lock_state();
            let mut selected = state.entries.values().filter_map(|entry| match entry {
                RegistryEntry::Ready(ready) if ready.owner.locator().path() == path => Some(ready),
                _ => None,
            });
            let Some(ready) = selected.next() else {
                return Ok(None);
            };
            if selected.next().is_some() {
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "select exact registered runtime close",
                    message: format!(
                        "multiple registered runtimes resolve to database path '{}'",
                        path.display()
                    ),
                });
            }
            (ready.owner.binding().clone(), Arc::clone(&ready.owner))
        };
        let (binding, owner) = selected;
        let authority = owner.database_authority.as_ref().ok_or_else(|| {
            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "close registered runtime by path",
                message: "registered runtime has no originating database authority".to_owned(),
            }
        })?;
        owner.validate_database_write_authority(authority, "close registered runtime by path")?;
        self.close_exact(&binding, authority).await.map(Some)
    }

    /// Closes one exact runtime for an exclusive maintenance handoff.
    ///
    /// The caller retains only the binding and originating authority. Any
    /// issued database facade, direct runtime reference, or client lease
    /// refuses the close before physical admission is fenced.
    pub async fn close_exact(
        &self,
        expected: &StoreRuntimeBindingV1,
        authority: &DatabaseAuthority,
    ) -> Result<ClosedStoreRuntime, StoreRuntimeRegistryFailure> {
        self.close_exact_with_opened_identity(expected, authority, None)
            .await
    }

    /// Closes an exact retained runtime after the canonical path has already
    /// advanced to another inode under an exclusive recovery authority.
    pub async fn close_exact_stale_attachment(
        &self,
        expected: &StoreRuntimeBindingV1,
        authority: &DatabaseAuthority,
        expected_opened_file_identity: u64,
    ) -> Result<ClosedStoreRuntime, StoreRuntimeRegistryFailure> {
        self.close_exact_with_opened_identity(
            expected,
            authority,
            Some(expected_opened_file_identity),
        )
        .await
    }

    #[tracing::instrument(name = "runtime_core.registry.close_exact", level = "trace", skip_all)]
    async fn close_exact_with_opened_identity(
        &self,
        expected: &StoreRuntimeBindingV1,
        authority: &DatabaseAuthority,
        stale_opened_file_identity: Option<u64>,
    ) -> Result<ClosedStoreRuntime, StoreRuntimeRegistryFailure> {
        let cx =
            crate::native_runtime::cx().ok_or(StoreRuntimeRegistryFailure::RuntimeUnavailable {
                operation: "close exact registered runtime",
            })?;
        let reservation =
            self.reserve_exact_close(expected, authority, stale_opened_file_identity)?;
        let mut completion = reservation.completion.subscribe();
        let owner = reservation.owner.clone();
        let guard = CloseAttemptGuard {
            registry: self.clone(),
            reservation: Some(reservation),
        };
        self.inner
            .operations
            .retain_blocking(move || {
                let mut outcome = drain_and_close_physical(&owner);
                if outcome.is_ok() {
                    outcome = match stale_opened_file_identity {
                        Some(expected_identity)
                            if owner.opened_file_identity() == Some(expected_identity) =>
                        {
                            Ok(())
                        }
                        Some(_) => Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                            operation: "complete stale registered runtime close",
                            message: "retained opened identity changed during stale close"
                                .to_owned(),
                        }),
                        None => owner
                            .validate_opened_file_identity(
                                "complete exact registered runtime close",
                            )
                            .map(|_| ()),
                    };
                }
                if outcome.is_ok() {
                    outcome = owner
                        .runtime()
                        .transition(RuntimeMaintenanceStateV1::Closed)
                        .map_err(
                            |error| StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                                message: error.to_string(),
                            },
                        );
                } else {
                    let _ = owner
                        .runtime()
                        .transition(RuntimeMaintenanceStateV1::Faulted);
                }
                guard.complete(outcome);
            })
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "spawn exact registered runtime close",
                message: error.to_string(),
            })?;
        loop {
            if let Some(result) = completion.borrow().clone() {
                return result;
            }
            completion.changed(&cx).await.map_err(|error| match error {
                watch::RecvError::Cancelled => StoreRuntimeRegistryFailure::WaitCancelled {
                    operation: "wait for exact registered runtime close",
                },
                error => StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "wait for exact registered runtime close",
                    message: error.to_string(),
                },
            })?;
        }
    }

    fn reserve_exact_close(
        &self,
        expected: &StoreRuntimeBindingV1,
        authority: &DatabaseAuthority,
        stale_opened_file_identity: Option<u64>,
    ) -> Result<CloseReservation, StoreRuntimeRegistryFailure> {
        let key = StoreRuntimeKey::from_binding(expected);
        let mut state = self.lock_state();
        let Some(entry) = state.entries.remove(&key) else {
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "reserve exact registered runtime close",
                message: "exact registered runtime is not mounted".to_owned(),
            });
        };
        let ready = match entry {
            RegistryEntry::Ready(ready) => ready,
            RegistryEntry::Opening(opening) => {
                state.entries.insert(key, RegistryEntry::Opening(opening));
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "reserve exact registered runtime close",
                    message: "exact registered runtime is still opening".to_owned(),
                });
            }
            RegistryEntry::Retiring(retiring) => {
                state
                    .entries
                    .insert(key.clone(), RegistryEntry::Retiring(retiring));
                return Err(StoreRuntimeRegistryFailure::RuntimeRetirementInProgress {
                    key: Box::new(key),
                });
            }
            RegistryEntry::Committing(committing) => {
                state
                    .entries
                    .insert(key.clone(), RegistryEntry::Committing(committing));
                return Err(StoreRuntimeRegistryFailure::RuntimeRetirementCommitting {
                    key: Box::new(key),
                });
            }
            RegistryEntry::Faulted(faulted) => {
                state
                    .entries
                    .insert(key.clone(), RegistryEntry::Faulted(faulted));
                return Err(StoreRuntimeRegistryFailure::RuntimeRetirementFaulted {
                    key: Box::new(key),
                });
            }
            RegistryEntry::DurabilityUncertain(faulted) => {
                state
                    .entries
                    .insert(key.clone(), RegistryEntry::DurabilityUncertain(faulted));
                return Err(
                    StoreRuntimeRegistryFailure::RuntimeRetirementDurabilityUncertain {
                        key: Box::new(key),
                    },
                );
            }
            RegistryEntry::Evicting(evicting) => {
                state
                    .entries
                    .insert(key.clone(), RegistryEntry::Evicting(evicting));
                return Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                    key: Box::new(key),
                });
            }
        };
        if ready.owner.binding() != expected {
            let actual = ready.owner.binding().clone();
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(expected.clone()),
                actual: Box::new(actual),
            });
        }
        let retained_authority = ready.owner.database_authority.as_ref();
        let authority_matches = retained_authority.is_some_and(|retained| {
            retained.token() == authority.token()
                && retained.role() == authority.role()
                && retained.database_identity_key() == authority.database_identity_key()
        });
        if !authority_matches {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "reserve exact registered runtime close",
                message: "close authority does not match the originating runtime authority"
                    .to_owned(),
            });
        }
        if let Some(expected_identity) = stale_opened_file_identity {
            if let Err(error) =
                authority.require_active_write_scope("reserve stale registered runtime close")
            {
                state.entries.insert(key, RegistryEntry::Ready(ready));
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "reserve stale registered runtime close",
                    message: error.to_string(),
                });
            }
            if ready.owner.opened_file_identity() != Some(expected_identity) {
                state.entries.insert(key, RegistryEntry::Ready(ready));
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "reserve stale registered runtime close",
                    message: "retained runtime does not match the expected opened identity"
                        .to_owned(),
                });
            }
        } else if let Err(failure) = ready
            .owner
            .validate_database_write_authority(authority, "reserve exact registered runtime close")
        {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(failure);
        }

        // Public leases cannot reveal a raw runtime or a reference count. The
        // canonical client/operation counters are the retirement authority,
        // including database facades that retain a client lease.
        let external_handles = 0;
        let external_runtime_references = 0;
        let leases = ready.owner.runtime().health_snapshot();
        let client_leases = leases.client_leases;
        let operation_leases = leases.operation_leases;
        if external_handles != 0
            || external_runtime_references != 0
            || client_leases != 0
            || operation_leases != 0
        {
            let binding = ready.owner.binding().clone();
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                binding: Box::new(binding),
                external_handles,
                external_runtime_references,
                client_leases,
                operation_leases,
            });
        }
        let Some(attempt) = state.next_eviction_attempt.checked_add(1) else {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::EvictionAttemptExhausted);
        };
        state.next_eviction_attempt = attempt;
        if let Err(error) = ready
            .owner
            .runtime()
            .transition(RuntimeMaintenanceStateV1::Draining)
        {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                message: error.to_string(),
            });
        }
        let owner = ready.owner;
        let completion = Arc::new(watch::channel(None).0);
        state.entries.insert(
            key.clone(),
            RegistryEntry::Evicting(EvictingRuntime {
                attempt,
                owner: owner.clone(),
            }),
        );
        Ok(CloseReservation {
            key,
            attempt,
            owner,
            completion,
        })
    }

    fn finish_exact_close(
        &self,
        reservation: CloseReservation,
        outcome: Result<(), StoreRuntimeRegistryFailure>,
    ) -> Result<ClosedStoreRuntime, StoreRuntimeRegistryFailure> {
        if outcome.is_err()
            && reservation.owner.runtime().maintenance_state() != RuntimeMaintenanceStateV1::Faulted
        {
            let _ = reservation
                .owner
                .runtime()
                .transition(RuntimeMaintenanceStateV1::Faulted);
        }
        let mut state = self.lock_state();
        let entry = state.entries.remove(&reservation.key);
        let evicting = match entry {
            Some(RegistryEntry::Evicting(evicting))
                if evicting.attempt == reservation.attempt
                    && evicting.owner.binding() == reservation.owner.binding() =>
            {
                evicting
            }
            Some(entry) => {
                state.entries.insert(reservation.key.clone(), entry);
                return Err(StoreRuntimeRegistryFailure::EvictionReservationLost {
                    key: Box::new(reservation.key),
                });
            }
            None => {
                return Err(StoreRuntimeRegistryFailure::EvictionReservationLost {
                    key: Box::new(reservation.key),
                });
            }
        };
        if let Err(failure) = outcome {
            state.entries.insert(
                reservation.key,
                RegistryEntry::Evicting(EvictingRuntime {
                    attempt: evicting.attempt,
                    owner: evicting.owner,
                }),
            );
            return Err(failure);
        }
        if reservation.key.is_profile()
            && state
                .profile_authorities
                .get(reservation.key.shard_id())
                .is_some_and(|binding| binding == reservation.owner.binding())
        {
            state.profile_authorities.remove(reservation.key.shard_id());
        }
        let proof = ClosedStoreRuntime {
            binding: reservation.owner.binding().clone(),
            verified_locator: reservation.owner.locator().verified().clone(),
            path: reservation.owner.locator().path().to_path_buf(),
            opened_file_identity: reservation.owner.opened_file_identity().unwrap_or_default(),
        };
        drop(state);
        drop(evicting);
        Ok(proof)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use tracedecay_domain::{
        BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, WorktreeId,
    };
    use tracedecay_store::{
        CodeShardScopeV1, RuntimeMaintenanceStateV1, StoreIncarnationV1, StoreShardIdV1,
        StoreShardScopeV1, VerifiedStoreLocatorV1,
    };

    use super::*;
    use crate::shard_runtime::registry::{
        LifecycleShardRuntimePublisher, PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot,
        ProfileAuthorityPinResult, PublishedShardRuntime, ResolvedStoreLocator,
        ShardRuntimeBuildRequest, ShardRuntimePublisher, StoreRuntimeClientLease,
        StoreRuntimeLookup, StoreRuntimeOpenBegin, StoreRuntimeOpenMode, StoreRuntimeOpenRequest,
        StoreRuntimeOpenResult, StoreRuntimeRegistryConfig, StoreRuntimeRegistryFuture,
        StoreRuntimeResolver,
    };
    use crate::shard_runtime::shard::ShardRuntime;

    use tracedecay_domain::test_fixtures::id;

    fn profile_shard() -> StoreShardIdV1 {
        StoreShardIdV1::profile(
            id::<BrainId>("brain.close-exact"),
            id::<UserProfileId>("profile.close-exact"),
        )
    }

    fn code_shard() -> StoreShardIdV1 {
        code_shard_for("worktree.close-exact")
    }

    fn code_shard_for(worktree_id: &str) -> StoreShardIdV1 {
        StoreShardIdV1::code(
            id::<BrainId>("brain.close-exact"),
            id::<UserProfileId>("profile.close-exact"),
            id::<ProjectId>("project.close-exact"),
            id::<RepositoryId>("repository.close-exact"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>(worktree_id),
            },
        )
    }

    struct FixtureResolver {
        profile_path: PathBuf,
    }

    impl StoreRuntimeResolver for FixtureResolver {
        fn resolve<'a>(
            &'a self,
            key: &'a StoreRuntimeKey,
            _mode: StoreRuntimeOpenMode,
            _database_authority: Option<&'a DatabaseAuthority>,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            let verified = VerifiedStoreLocatorV1::new(
                key.shard_id().clone(),
                key.incarnation(),
                LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            );
            let path = _database_authority.map_or_else(
                || self.profile_path.clone(),
                |authority| authority.canonical_database_path().to_path_buf(),
            );
            Box::pin(async move { Ok(ResolvedStoreLocator::new(verified, path)) })
        }
    }

    async fn seed_final_graph_db(path: PathBuf) -> PathBuf {
        let connection = crate::db::engine::TestConnection::open(&path);
        crate::db::migrations::create_schema_connection(&connection)
            .await
            .expect("seed exact final graph schema");
        drop(connection);
        path.canonicalize().unwrap()
    }

    async fn mount_code_runtime(
        path: PathBuf,
    ) -> (
        StoreRuntimeRegistry,
        StoreRuntimeClientLease,
        StoreRuntimeClientLease,
        DatabaseAuthority,
    ) {
        let path = seed_final_graph_db(path).await;
        let authority =
            DatabaseAuthority::for_runtime(&path, "mount exact-close code runtime").unwrap();
        let profile_path = path.with_file_name("profile.db");
        rusqlite::Connection::open(&profile_path).unwrap();
        let profile_path = profile_path.canonicalize().unwrap();
        let registry = StoreRuntimeRegistry::new(
            asupersync::runtime::Runtime::current_handle().unwrap(),
            Arc::new(FixtureResolver { profile_path }),
            Arc::new(LifecycleShardRuntimePublisher),
        )
        .unwrap();
        let incarnation = StoreIncarnationV1::new(1).unwrap();
        let profile = match registry
            .open(StoreRuntimeOpenRequest::new(
                profile_shard(),
                incarnation,
                None,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("profile publication failed: {failure:?}")
            }
        };
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let code = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                code_shard(),
                incarnation,
                Some(pin),
                authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("code publication failed: {failure:?}")
            }
        };
        (registry, profile, code, authority)
    }

    #[asupersync::test]
    async fn close_path_rejects_single_runtime_without_retained_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, _authority) = mount_code_runtime(path).await;
        let profile_path = profile.canonical_path().to_path_buf();

        assert!(matches!(
            registry.close_path(&profile_path).await,
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "close registered runtime by path",
                message,
            }) if message == "registered runtime has no originating database authority"
        ));
        assert!(matches!(
            registry.lookup(profile.binding()),
            StoreRuntimeLookup::Ready(_)
        ));

        drop(code);
        drop(profile);
    }

    #[asupersync::test]
    async fn close_path_detects_duplicate_path_before_authority_validation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let path = seed_final_graph_db(path).await;
        let authority =
            DatabaseAuthority::for_runtime(&path, "mount duplicate-path code runtime").unwrap();
        let registry = StoreRuntimeRegistry::new(
            asupersync::runtime::Runtime::current_handle().unwrap(),
            Arc::new(FixtureResolver {
                profile_path: path.clone(),
            }),
            Arc::new(LifecycleShardRuntimePublisher),
        )
        .unwrap();
        let incarnation = StoreIncarnationV1::new(1).unwrap();
        let profile = match registry
            .open(StoreRuntimeOpenRequest::new_read_only(
                profile_shard(),
                incarnation,
                None,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("profile publication failed: {failure:?}")
            }
        };
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let code = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                code_shard(),
                incarnation,
                Some(pin),
                authority,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("code publication failed: {failure:?}")
            }
        };
        let shared_path = code.canonical_path().to_path_buf();

        assert!(matches!(
            registry.close_path(&shared_path).await,
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "select exact registered runtime close",
                message,
            }) if message.contains("multiple registered runtimes")
        ));
        assert!(matches!(
            registry.lookup(profile.binding()),
            StoreRuntimeLookup::Ready(_)
        ));
        assert!(matches!(
            registry.lookup(code.binding()),
            StoreRuntimeLookup::Ready(_)
        ));

        drop(code);
        drop(profile);
    }

    #[asupersync::test]
    async fn exact_close_refuses_every_live_client_lease_before_closing() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, authority) = mount_code_runtime(path.clone()).await;
        let binding = code.binding().clone();

        assert!(matches!(
            registry.close_exact(&binding, &authority).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 0,
                external_runtime_references: 0,
                client_leases: 1,
                operation_leases: 0,
                ..
            })
        ));
        assert_eq!(
            code.health_snapshot().state,
            RuntimeMaintenanceStateV1::Ready
        );

        let StoreRuntimeLookup::Ready(independent) = registry.lookup(&binding) else {
            panic!("a ready runtime issues an independent client lease");
        };
        drop(code);
        assert!(matches!(
            registry.close_exact(&binding, &authority).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 0,
                external_runtime_references: 0,
                client_leases: 1,
                operation_leases: 0,
                ..
            })
        ));
        drop(independent);

        let proof = registry.close_exact(&binding, &authority).await.unwrap();
        assert_eq!(proof.binding(), &binding);
        assert_eq!(proof.path(), authority.canonical_database_path());
        assert_eq!(
            proof.opened_file_identity(),
            crate::db::sqlite_generation_identity(proof.path()).unwrap()
        );
        assert_eq!(proof.verified_locator().shard_id, binding.shard_id);
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Missing { .. }
        ));
        drop(profile);
    }

    /// A spent store-close budget makes one final pass: the held runtime stays
    /// mounted instead of the drain waiting on it.
    #[asupersync::test]
    async fn runtime_held_through_shutdown_close_still_closes_exactly_once_released() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, authority) = mount_code_runtime(path.clone()).await;
        let binding = code.binding().clone();

        registry
            .close_idle_for_shutdown(std::time::Instant::now())
            .await
            .unwrap();
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Ready(_)
        ));
        drop(code);

        let proof = registry.close_exact(&binding, &authority).await.unwrap();
        assert_eq!(proof.binding(), &binding);
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Missing { .. }
        ));
        drop(profile);
    }

    /// Leases released seconds after store close begins still have their
    /// runtimes closed, so their WALs are truncated, while the caller's
    /// deadline is far off.
    #[asupersync::test]
    async fn shutdown_close_waits_for_a_lease_released_while_it_runs() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, _authority) = mount_code_runtime(path.clone()).await;
        let binding = code.binding().clone();
        let profile_binding = profile.binding().clone();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(4));
            drop((code, profile));
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        assert_eq!(registry.close_idle_for_shutdown(deadline).await.unwrap(), 2);
        for binding in [binding, profile_binding] {
            assert!(matches!(
                registry.lookup(&binding),
                StoreRuntimeLookup::Missing { .. }
            ));
        }
        release.join().unwrap();
    }

    /// The last pass runs while one poll still fits before the deadline, so
    /// the drain logs its held runtimes and returns before the caller's own
    /// deadline cancels it.
    #[test]
    fn the_last_drain_pass_runs_before_the_deadline() {
        let now = std::time::Instant::now();
        assert!(is_last_drain_pass(now, now + SHUTDOWN_HOLD_DRAIN_POLL / 2));
        assert!(is_last_drain_pass(now, now + SHUTDOWN_HOLD_DRAIN_POLL));
        assert!(!is_last_drain_pass(now, now + SHUTDOWN_HOLD_DRAIN_POLL * 2));
    }

    #[asupersync::test]
    async fn refused_shutdown_close_still_settles_its_reserved_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, _authority) = mount_code_runtime(path.clone()).await;
        let binding = code.binding().clone();
        drop(code);

        registry.inner.operations.begin_shutdown();
        assert!(matches!(
            registry
                .close_idle_for_shutdown(std::time::Instant::now())
                .await,
            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "spawn shutdown close of idle registered runtimes",
                ..
            })
        ));
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Missing { .. }
        ));
        drop(profile);
    }

    #[asupersync::test]
    async fn exact_close_refuses_an_operation_token_after_the_client_drops() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, authority) = mount_code_runtime(path).await;
        let binding = code.binding().clone();
        let operation = code.begin_operation().unwrap();
        drop(code);

        assert!(matches!(
            registry.close_exact(&binding, &authority).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 0,
                external_runtime_references: 0,
                client_leases: 0,
                operation_leases: 1,
                ..
            })
        ));
        let StoreRuntimeLookup::Ready(retained) = registry.lookup(&binding) else {
            panic!("an operation-blocked exact close must leave the runtime ready");
        };
        assert_eq!(
            retained.health_snapshot().state,
            RuntimeMaintenanceStateV1::Ready
        );
        assert_eq!(retained.health_snapshot().operation_leases, 1);
        drop(retained);

        drop(operation);
        let proof = registry.close_exact(&binding, &authority).await.unwrap();
        assert_eq!(proof.binding(), &binding);
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Missing { .. }
        ));
        drop(profile);
    }

    #[cfg(unix)]
    #[asupersync::test]
    async fn stale_exact_close_uses_retained_binding_authority_and_opened_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, authority) = mount_code_runtime(path.clone()).await;
        let binding = code.binding().clone();
        let opened_identity = code.opened_file_identity().unwrap();
        let detached = path.with_extension("detached.db");
        let replacement = path.with_extension("replacement.db");
        rusqlite::Connection::open(&replacement).unwrap();
        std::fs::rename(&path, &detached).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        drop(code);

        let closed = registry
            .close_exact_stale_attachment(&binding, &authority, opened_identity)
            .await
            .unwrap();
        assert_eq!(closed.binding(), &binding);
        assert_eq!(closed.opened_file_identity(), opened_identity);
        drop(profile);
    }

    #[asupersync::test]
    async fn destructive_reservation_fences_open_until_preserved_store_reopens() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, authority) = mount_code_runtime(path.clone()).await;
        let old_runtime_identity = code.runtime_identity();
        let authority_token = authority.token().to_owned();
        drop(code);

        let reservation = registry
            .begin_destructive_maintenance(
                super::super::DestructiveMaintenanceTarget::new(temporary.path(), [path.clone()])
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reservation.closed().len(), 1);

        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let open_registry = registry.clone();
        let open_authority = authority.clone();
        let mut pending = asupersync::Cx::current()
            .unwrap()
            .spawn(move |_| async move {
                open_registry
                    .open(StoreRuntimeOpenRequest::new_authorized(
                        code_shard(),
                        StoreIncarnationV1::new(1).unwrap(),
                        Some(pin),
                        open_authority,
                    ))
                    .await
            })
            .unwrap();
        asupersync::runtime::yield_now().await;
        assert!(
            !pending.is_finished(),
            "ordinary opens must wait for destructive maintenance"
        );

        reservation.abort_preserved().unwrap();
        let reopened = match asupersync::time::timeout(
            asupersync::Cx::current().unwrap().now(),
            Duration::from_secs(2),
            pending.join(&asupersync::Cx::current().unwrap()),
        )
        .await
        .expect("reserved open must wake")
        .unwrap()
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("reserved open failed after release: {failure:?}")
            }
        };
        assert_ne!(reopened.runtime_identity(), old_runtime_identity);
        assert_eq!(
            reopened
                .database_authority("verify reopened destructive store")
                .unwrap()
                .token(),
            authority_token
        );
        drop(reopened);
        drop(profile);
    }

    #[asupersync::test]
    async fn destructive_reservation_atomically_rejects_open_begin() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, authority) = mount_code_runtime(path.clone()).await;
        drop(code);

        let reservation = registry
            .begin_destructive_maintenance(
                super::super::DestructiveMaintenanceTarget::new(temporary.path(), [path]).unwrap(),
            )
            .await
            .unwrap();
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };

        assert!(matches!(
            registry.begin_or_join_open(&StoreRuntimeOpenRequest::new_authorized(
                code_shard(),
                StoreIncarnationV1::new(1).unwrap(),
                Some(pin),
                authority,
            )),
            StoreRuntimeOpenBegin::Rejected(
                StoreRuntimeRegistryFailure::DestructiveMaintenanceInProgress { .. }
            )
        ));

        reservation.abort_preserved().unwrap();
        drop(profile);
    }

    #[asupersync::test]
    async fn failed_destructive_close_releases_reservation_for_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        let (registry, profile, code, _authority) = mount_code_runtime(path.clone()).await;
        let target =
            super::super::DestructiveMaintenanceTarget::new(temporary.path(), [path]).unwrap();

        assert!(matches!(
            registry.begin_destructive_maintenance(target.clone()).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 0,
                external_runtime_references: 0,
                client_leases: 1,
                operation_leases: 0,
                ..
            })
        ));

        drop(code);
        let retry = registry
            .begin_destructive_maintenance(target)
            .await
            .expect("failed close must release destructive reservation");
        retry.abort_preserved().unwrap();
        drop(profile);
    }

    #[asupersync::test]
    async fn destructive_reservation_does_not_block_unrelated_database_under_same_root() {
        let temporary = tempfile::tempdir().unwrap();
        let reserved_path = temporary.path().join("reserved.db");
        let unrelated_path = seed_final_graph_db(temporary.path().join("unrelated.db")).await;
        let (registry, profile, code, _authority) = mount_code_runtime(reserved_path.clone()).await;
        drop(code);
        let reservation = registry
            .begin_destructive_maintenance(
                super::super::DestructiveMaintenanceTarget::new(temporary.path(), [reserved_path])
                    .unwrap(),
            )
            .await
            .unwrap();
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let authority =
            DatabaseAuthority::for_runtime(&unrelated_path, "mount unrelated database").unwrap();

        let unrelated = asupersync::time::timeout(
            asupersync::Cx::current().unwrap().now(),
            Duration::from_secs(2),
            registry.open(StoreRuntimeOpenRequest::new_authorized(
                code_shard_for("worktree.unrelated"),
                StoreIncarnationV1::new(1).unwrap(),
                Some(pin),
                authority,
            )),
        )
        .await
        .expect("an exact-path reservation must not block another database");
        let unrelated = match unrelated {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("unrelated database open failed: {failure:?}")
            }
        };

        drop(unrelated);
        reservation.abort_preserved().unwrap();
        drop(profile);
    }

    struct FailingAttachment {
        opened_file_identity: u64,
    }

    impl PhysicalRuntimeAttachment for FailingAttachment {
        fn snapshot(&self) -> PhysicalRuntimeSnapshot {
            PhysicalRuntimeSnapshot {
                healthy: true,
                writer_present: true,
                ..PhysicalRuntimeSnapshot::default()
            }
        }

        fn opened_file_identity(&self) -> Result<u64, String> {
            Ok(self.opened_file_identity)
        }

        fn drain(&self) -> Result<(), String> {
            Err("injected exact-close drain failure".to_owned())
        }

        fn close_and_join(&self) -> Result<(), String> {
            panic!("close must not follow a failed drain")
        }
    }

    struct FailingPublisher;

    impl ShardRuntimePublisher for FailingPublisher {
        fn publish(
            &self,
            request: ShardRuntimeBuildRequest,
        ) -> StoreRuntimeRegistryFuture<
            '_,
            Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>,
        > {
            Box::pin(async move {
                let runtime = ShardRuntime::new(
                    request.binding().clone(),
                    matches!(request.binding().shard_id.scope, StoreShardScopeV1::Profile),
                );
                runtime
                    .transition(RuntimeMaintenanceStateV1::Opening)
                    .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
                    .unwrap();
                let opened_file_identity =
                    crate::db::sqlite_generation_identity(request.locator().path()).unwrap();
                Ok(PublishedShardRuntime::new(
                    runtime,
                    Box::new(FailingAttachment {
                        opened_file_identity,
                    }),
                ))
            })
        }
    }

    #[asupersync::test]
    async fn failed_exact_close_retains_a_faulted_evicting_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        rusqlite::Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let authority = DatabaseAuthority::for_runtime(&path, "mount failing exact-close").unwrap();
        let profile_path = path.with_file_name("profile.db");
        rusqlite::Connection::open(&profile_path).unwrap();
        let profile_path = profile_path.canonicalize().unwrap();
        let registry = StoreRuntimeRegistry::with_config(
            asupersync::runtime::Runtime::current_handle().unwrap(),
            Arc::new(FixtureResolver { profile_path }),
            Arc::new(FailingPublisher),
            StoreRuntimeRegistryConfig::default(),
        )
        .unwrap();
        let incarnation = StoreIncarnationV1::new(1).unwrap();
        let profile = match registry
            .open(StoreRuntimeOpenRequest::new(
                profile_shard(),
                incarnation,
                None,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("profile publication failed: {failure:?}")
            }
        };
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let code = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                code_shard(),
                incarnation,
                Some(pin),
                authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("code publication failed: {failure:?}")
            }
        };
        let binding = code.binding().clone();
        drop(code);

        assert!(registry.close_exact(&binding, &authority).await.is_err());
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Evicting { .. }
        ));
        let state = registry.lock_state();
        let entry = state.entries.get(&StoreRuntimeKey::from_binding(&binding));
        assert!(matches!(
            entry,
            Some(RegistryEntry::Evicting(evicting))
                if evicting.owner.runtime().maintenance_state()
                    == RuntimeMaintenanceStateV1::Faulted
        ));
        drop(state);
        drop(profile);
    }

    /// This process's descriptors on files under `path` that were unlinked.
    #[cfg(target_os = "linux")]
    fn deleted_descriptors(path: &std::path::Path) -> Vec<String> {
        let prefix = path.display().to_string();
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .map(|target| target.display().to_string())
            .filter(|target| target.starts_with(&prefix) && target.ends_with(" (deleted)"))
            .collect()
    }

    /// The first lookup that is not `Evicting`: a lost runtime stays mapped
    /// until its reader workers have exited.
    #[cfg(target_os = "linux")]
    async fn lookup_after_lost_runtime_closes(
        registry: &StoreRuntimeRegistry,
        binding: &StoreRuntimeBindingV1,
    ) -> StoreRuntimeLookup {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let lookup = registry.lookup(binding);
            if !matches!(lookup, StoreRuntimeLookup::Evicting { .. }) {
                return lookup;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a lost runtime's reader workers must exit"
            );
            let _ = crate::native_runtime::sleep_until(
                std::time::Instant::now() + Duration::from_millis(20),
            )
            .await;
        }
    }

    #[cfg(target_os = "linux")]
    #[asupersync::test]
    async fn a_store_unlinked_under_its_runtime_closes_its_files_and_reopens_fresh() {
        use tracedecay_rusqlite_runtime::exact_sql::{ExactSqlStatement, ExactSqlValue};

        let statement = |sql: &str| ExactSqlStatement::new(sql.to_owned(), Vec::new()).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (registry, profile, code, authority) =
            mount_code_runtime(temporary.path().join("runtime.db")).await;
        let path = authority.canonical_database_path().to_path_buf();
        let handle = code.authorized_exact_sql_handle(authority.clone()).unwrap();
        handle
            .execute(statement("CREATE TABLE unlink_probe (value TEXT)"))
            .unwrap();
        handle
            .execute(statement("INSERT INTO unlink_probe VALUES ('before')"))
            .unwrap();
        drop(handle);

        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(file));
        }

        let deadline = std::time::Instant::now()
            + tracedecay_rusqlite_runtime::FILE_PROBE_INTERVAL
            + Duration::from_secs(2);
        while code.physical_snapshot().healthy || !deleted_descriptors(&path).is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "within one probe interval the store must report itself lost and hold no \
                 descriptor on the unlinked files: {:?}",
                deleted_descriptors(&path)
            );
            let _ = crate::native_runtime::sleep_until(
                std::time::Instant::now() + Duration::from_millis(20),
            )
            .await;
        }
        assert!(!code.physical_snapshot().writer_present);
        assert!(matches!(
            lookup_after_lost_runtime_closes(&registry, code.binding()).await,
            StoreRuntimeLookup::Missing { .. }
        ));

        let path = seed_final_graph_db(path).await;
        let authority = DatabaseAuthority::for_runtime(&path, "reopen a lost store").unwrap();
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let fresh = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                code_shard(),
                StoreIncarnationV1::new(1).unwrap(),
                Some(pin),
                authority.clone(),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            StoreRuntimeOpenResult::Failed(failure) => {
                panic!("the next open must publish a fresh runtime: {failure:?}")
            }
        };
        let handle = fresh.authorized_exact_sql_handle(authority).unwrap();
        handle
            .execute(statement("CREATE TABLE unlink_probe (value TEXT)"))
            .unwrap();
        let rows = handle
            .query(
                statement("SELECT count(*) FROM unlink_probe"),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
        drop((handle, fresh, code, profile));
    }

    #[cfg(target_os = "linux")]
    #[asupersync::test]
    async fn an_evicted_lost_runtime_leaves_no_pin_on_its_key() {
        let temporary = tempfile::tempdir().unwrap();
        let (registry, profile, code, authority) =
            mount_code_runtime(temporary.path().join("runtime.db")).await;
        let key = StoreRuntimeKey::from_binding(code.binding());
        registry
            .lock_state()
            .profile_pin_tokens
            .entry(key.clone())
            .or_default()
            .insert(u64::MAX);
        let path = authority.canonical_database_path().to_path_buf();
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(file));
        }
        let deadline = std::time::Instant::now()
            + tracedecay_rusqlite_runtime::FILE_PROBE_INTERVAL
            + Duration::from_secs(2);
        while code.physical_snapshot().healthy {
            assert!(
                std::time::Instant::now() < deadline,
                "the store must report itself lost"
            );
            let _ = crate::native_runtime::sleep_until(
                std::time::Instant::now() + Duration::from_millis(20),
            )
            .await;
        }

        assert!(matches!(
            lookup_after_lost_runtime_closes(&registry, code.binding()).await,
            StoreRuntimeLookup::Missing { .. }
        ));
        assert!(
            !registry.lock_state().profile_pin_tokens.contains_key(&key),
            "pins held on the evicted runtime must not count against a reopened key"
        );
        drop((code, profile));
    }
}
