use std::path::Path;
use std::sync::Arc;

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use super::capacity::drain_and_close_physical;
use super::{
    EvictingRuntime, RegistryEntry, StoreRuntimeHandle, StoreRuntimeKey, StoreRuntimeRegistry,
    StoreRuntimeRegistryFailure,
};
use crate::db::DatabaseAuthority;

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
    handle: StoreRuntimeHandle,
}

impl StoreRuntimeRegistry {
    pub async fn close_path(
        &self,
        path: &Path,
    ) -> Result<Option<ClosedStoreRuntime>, StoreRuntimeRegistryFailure> {
        let selected = {
            let state = self.lock_state();
            let mut selected = state.entries.values().filter_map(|entry| match entry {
                RegistryEntry::Ready(ready) if ready.handle.locator().path() == path => {
                    Some(ready.handle.clone())
                }
                _ => None,
            });
            let first = selected.next();
            if first.is_some() && selected.next().is_some() {
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "select exact registered runtime close",
                    message: format!(
                        "multiple registered runtimes resolve to database path '{}'",
                        path.display()
                    ),
                });
            }
            first
        };
        let Some(handle) = selected else {
            return Ok(None);
        };
        let binding = handle.binding().clone();
        let authority = handle.database_authority("close registered runtime by path")?;
        drop(handle);
        self.close_exact(&binding, &authority).await.map(Some)
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
        let reservation = self.reserve_exact_close(expected, authority)?;
        let registry = self.clone();
        // The completion task owns the reservation. Dropping this caller's
        // join handle detaches the task, so physical close still reaches the
        // matching lifecycle transition and `finish_exact_close`.
        tokio::spawn(async move {
            let physical = reservation.handle.clone();
            let mut outcome =
                tokio::task::spawn_blocking(move || drain_and_close_physical(&physical))
                    .await
                    .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                        operation: "join exact registered runtime close",
                        message: error.to_string(),
                    })
                    .and_then(|result| result);

            if outcome.is_ok() {
                outcome = reservation
                    .handle
                    .validate_opened_file_identity("complete exact registered runtime close")
                    .map(|_| ());
            }
            if outcome.is_ok() {
                outcome = reservation
                    .handle
                    .runtime()
                    .transition(RuntimeMaintenanceStateV1::Closed)
                    .map_err(
                        |error| StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                            message: error.to_string(),
                        },
                    );
            } else {
                let _ = reservation
                    .handle
                    .runtime()
                    .transition(RuntimeMaintenanceStateV1::Faulted);
            }
            registry.finish_exact_close(reservation, outcome)
        })
        .await
        .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "join cancellation-safe exact registered runtime close",
            message: error.to_string(),
        })?
    }

    fn reserve_exact_close(
        &self,
        expected: &StoreRuntimeBindingV1,
        authority: &DatabaseAuthority,
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
            RegistryEntry::Evicting(evicting) => {
                state
                    .entries
                    .insert(key.clone(), RegistryEntry::Evicting(evicting));
                return Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                    key: Box::new(key),
                });
            }
        };
        if ready.handle.binding() != expected {
            let actual = ready.handle.binding().clone();
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(expected.clone()),
                actual: Box::new(actual),
            });
        }
        let retained_authority = ready.handle.inner.database_authority.as_ref();
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
        if let Err(failure) = ready
            .handle
            .validate_database_write_authority(authority, "reserve exact registered runtime close")
        {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(failure);
        }

        let external_handles = Arc::strong_count(&ready.handle.inner).saturating_sub(1);
        let external_runtime_references =
            Arc::strong_count(ready.handle.runtime()).saturating_sub(1);
        let client_leases = ready.handle.runtime().health_snapshot().client_leases;
        if external_handles != 0 || external_runtime_references != 0 || client_leases != 0 {
            let binding = ready.handle.binding().clone();
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                binding: Box::new(binding),
                external_handles,
                external_runtime_references,
                client_leases,
            });
        }
        let Some(attempt) = state.next_eviction_attempt.checked_add(1) else {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::EvictionAttemptExhausted);
        };
        state.next_eviction_attempt = attempt;
        if let Err(error) = ready
            .handle
            .runtime()
            .transition(RuntimeMaintenanceStateV1::Draining)
        {
            state.entries.insert(key, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                message: error.to_string(),
            });
        }
        let handle = ready.handle;
        state.entries.insert(
            key.clone(),
            RegistryEntry::Evicting(EvictingRuntime {
                attempt,
                handle: handle.clone(),
            }),
        );
        Ok(CloseReservation {
            key,
            attempt,
            handle,
        })
    }

    fn finish_exact_close(
        &self,
        reservation: CloseReservation,
        outcome: Result<(), StoreRuntimeRegistryFailure>,
    ) -> Result<ClosedStoreRuntime, StoreRuntimeRegistryFailure> {
        if outcome.is_err()
            && reservation.handle.runtime().maintenance_state()
                != RuntimeMaintenanceStateV1::Faulted
        {
            let _ = reservation
                .handle
                .runtime()
                .transition(RuntimeMaintenanceStateV1::Faulted);
        }
        let mut state = self.lock_state();
        let entry = state.entries.remove(&reservation.key);
        let evicting = match entry {
            Some(RegistryEntry::Evicting(evicting))
                if evicting.attempt == reservation.attempt
                    && evicting.handle.binding() == reservation.handle.binding() =>
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
                    handle: evicting.handle,
                }),
            );
            return Err(failure);
        }
        if reservation.key.is_profile()
            && state
                .profile_authorities
                .get(reservation.key.shard_id())
                .is_some_and(|binding| binding == reservation.handle.binding())
        {
            state.profile_authorities.remove(reservation.key.shard_id());
        }
        let proof = ClosedStoreRuntime {
            binding: reservation.handle.binding().clone(),
            verified_locator: reservation.handle.locator().verified().clone(),
            path: reservation.handle.locator().path().to_path_buf(),
            opened_file_identity: reservation.handle.inner.opened_file_identity,
        };
        drop(state);
        drop(evicting);
        Ok(proof)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use tracedecay_domain::{
        BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros, WorktreeId,
    };
    use tracedecay_store::{
        CodeShardScopeV1, RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1,
        StoreClientIdV1, StoreIncarnationV1, StoreShardIdV1, StoreShardScopeV1,
        VerifiedStoreLocatorV1,
    };

    use super::*;
    use crate::store_runtime::registry::{
        LifecycleShardRuntimePublisher, PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot,
        ProfileAuthorityPinResult, PublishedShardRuntime, ResolvedStoreLocator,
        ShardRuntimeBuildRequest, ShardRuntimePublisher, StoreRuntimeLookup, StoreRuntimeOpenBegin,
        StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
        StoreRuntimeRegistryConfig, StoreRuntimeRegistryFuture, StoreRuntimeResolver,
    };
    use crate::store_runtime::shard::ShardRuntime;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

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

    async fn mount_code_runtime(
        path: PathBuf,
    ) -> (
        StoreRuntimeRegistry,
        StoreRuntimeHandle,
        StoreRuntimeHandle,
        DatabaseAuthority,
    ) {
        let authority =
            DatabaseAuthority::for_runtime(&path, "mount exact-close code runtime").unwrap();
        let registry = StoreRuntimeRegistry::new(
            Arc::new(FixtureResolver { profile_path: path }),
            Arc::new(LifecycleShardRuntimePublisher),
        );
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

    fn active_lease(binding: &StoreRuntimeBindingV1) -> RuntimeLeaseV1 {
        let now = super::super::utc_now();
        RuntimeLeaseV1 {
            lease_id: RuntimeLeaseIdV1::new("lease.close-exact").unwrap(),
            binding: binding.clone(),
            holder: StoreClientIdV1::new("client.close-exact").unwrap(),
            acquired_at: UtcMicros(now.0.saturating_sub(1_000_000)),
            expires_at: UtcMicros(now.0.saturating_add(60_000_000)),
        }
    }

    #[tokio::test]
    async fn exact_close_refuses_facades_runtime_references_and_leases_before_closing() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        rusqlite::Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (registry, profile, code, authority) = mount_code_runtime(path.clone()).await;
        let binding = code.binding().clone();

        assert!(matches!(
            registry.close_exact(&binding, &authority).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 1,
                external_runtime_references: 0,
                client_leases: 0,
                ..
            })
        ));
        assert_eq!(
            code.runtime().maintenance_state(),
            RuntimeMaintenanceStateV1::Ready
        );

        let runtime = Arc::clone(code.runtime());
        let lease = active_lease(&binding);
        assert!(matches!(
            registry.acquire_lease(lease.clone()),
            super::super::StoreRuntimeLeaseAcquireResult::Acquired(_)
        ));
        drop(code);
        assert!(matches!(
            registry.close_exact(&binding, &authority).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 0,
                external_runtime_references: 1,
                client_leases: 1,
                ..
            })
        ));
        assert!(registry.release_lease(&binding, &lease.lease_id));
        drop(runtime);

        let proof = registry.close_exact(&binding, &authority).await.unwrap();
        assert_eq!(proof.binding(), &binding);
        assert_eq!(proof.path(), path);
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

    #[tokio::test]
    async fn destructive_reservation_fences_open_until_preserved_store_reopens() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        rusqlite::Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (registry, profile, code, authority) = mount_graph(path.clone()).await;
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
        let pending = tokio::spawn(async move {
            open_registry
                .open(StoreRuntimeOpenRequest::new_authorized(
                    code_shard(),
                    StoreIncarnationV1::new(1).unwrap(),
                    Some(pin),
                    open_authority,
                ))
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !pending.is_finished(),
            "ordinary opens must wait for destructive maintenance"
        );

        reservation.abort_preserved().unwrap();
        let reopened = match tokio::time::timeout(Duration::from_secs(2), pending)
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

    #[tokio::test]
    async fn destructive_reservation_atomically_rejects_open_begin() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (registry, profile, code, authority) = mount_graph(path.clone()).await;
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

    #[tokio::test]
    async fn failed_destructive_close_releases_reservation_for_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (registry, profile, code, _authority) = mount_graph(path.clone()).await;
        let target =
            super::super::DestructiveMaintenanceTarget::new(temporary.path(), [path]).unwrap();

        assert!(matches!(
            registry.begin_destructive_maintenance(target.clone()).await,
            Err(StoreRuntimeRegistryFailure::RuntimeCloseBlocked {
                external_handles: 1,
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

    #[tokio::test]
    async fn destructive_reservation_does_not_block_unrelated_database_under_same_root() {
        let temporary = tempfile::tempdir().unwrap();
        let reserved_path = temporary.path().join("reserved.db");
        let unrelated_path = temporary.path().join("unrelated.db");
        create_graph_fixture_database_v1(&reserved_path).unwrap();
        create_graph_fixture_database_v1(&unrelated_path).unwrap();
        let reserved_path = reserved_path.canonicalize().unwrap();
        let unrelated_path = unrelated_path.canonicalize().unwrap();
        let (registry, profile, code, _authority) = mount_graph(reserved_path.clone()).await;
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

        let unrelated = tokio::time::timeout(
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
                let runtime = Arc::new(ShardRuntime::new(
                    request.binding().clone(),
                    matches!(request.binding().shard_id.scope, StoreShardScopeV1::Profile),
                ));
                runtime
                    .transition(RuntimeMaintenanceStateV1::Opening)
                    .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
                    .unwrap();
                let opened_file_identity =
                    crate::db::sqlite_generation_identity(request.locator().path()).unwrap();
                Ok(PublishedShardRuntime::new(
                    runtime,
                    Arc::new(FailingAttachment {
                        opened_file_identity,
                    }),
                ))
            })
        }
    }

    #[tokio::test]
    async fn failed_exact_close_retains_a_faulted_evicting_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("runtime.db");
        rusqlite::Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let authority = DatabaseAuthority::for_runtime(&path, "mount failing exact-close").unwrap();
        let registry = StoreRuntimeRegistry::with_config(
            Arc::new(FixtureResolver { profile_path: path }),
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
                if evicting.handle.runtime().maintenance_state()
                    == RuntimeMaintenanceStateV1::Faulted
        ));
        drop(state);
        drop(profile);
    }
}
