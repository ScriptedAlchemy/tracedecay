//! Registry-owned exclusion for destructive store maintenance.

use std::path::{Path, PathBuf};

use super::{
    DestructivePathReservation, RegistryEntry, StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveMaintenanceTarget {
    root: PathBuf,
    database_paths: Vec<PathBuf>,
    initial_file_identities: Vec<(PathBuf, u64)>,
}

impl DestructiveMaintenanceTarget {
    pub fn new(
        root: impl Into<PathBuf>,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        let root = canonical_existing_directory(root.into())?;
        let mut database_paths = database_paths
            .into_iter()
            .map(|path| canonical_database_under_root(&root, &path))
            .collect::<Result<Vec<_>, _>>()?;
        database_paths.sort();
        database_paths.dedup();
        if database_paths.is_empty() {
            return Err(
                StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                    message: "destructive maintenance requires at least one SQLite database"
                        .to_owned(),
                },
            );
        }
        let initial_file_identities = database_paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                crate::db::sqlite_generation_identity(path)
                    .map(|identity| (path.clone(), identity))
                    .map_err(
                        |_| StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                            message: format!(
                                "database '{}' has no stable file identity",
                                path.display()
                            ),
                        },
                    )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            root,
            database_paths,
            initial_file_identities,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database_paths(&self) -> &[PathBuf] {
        &self.database_paths
    }

    fn includes_database_path(&self, path: &Path) -> bool {
        self.database_paths
            .binary_search_by(|candidate| candidate.as_path().cmp(path))
            .is_ok()
    }
}

struct DestructiveReservationGuard {
    registry: StoreRuntimeRegistry,
    attempt: u64,
    active: bool,
}

impl DestructiveReservationGuard {
    fn new(registry: StoreRuntimeRegistry, attempt: u64) -> Self {
        Self {
            registry,
            attempt,
            active: true,
        }
    }

    fn release(&mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.registry.release_destructive(self.attempt)?;
        self.active = false;
        Ok(())
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for DestructiveReservationGuard {
    fn drop(&mut self) {
        if self.active {
            self.registry
                .release_destructive_after_cancellation(self.attempt);
        }
    }
}

pub struct DestructiveMaintenanceReservation {
    registry: StoreRuntimeRegistry,
    attempt: u64,
    target: DestructiveMaintenanceTarget,
    closed: Vec<super::ClosedStoreRuntime>,
    released: bool,
}

impl DestructiveMaintenanceReservation {
    pub fn target(&self) -> &DestructiveMaintenanceTarget {
        &self.target
    }

    pub fn closed(&self) -> &[super::ClosedStoreRuntime] {
        &self.closed
    }

    pub fn finish_deleted(mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.registry.release_destructive(self.attempt)?;
        self.released = true;
        Ok(())
    }

    pub fn abort_preserved(mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        for (path, initial_identity) in &self.target.initial_file_identities {
            let current = crate::db::sqlite_generation_identity(path).map_err(|_| {
                StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "verify preserved destructive-maintenance database",
                    message: format!("database '{}' is no longer intact", path.display()),
                }
            })?;
            if current != *initial_identity {
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "verify preserved destructive-maintenance database",
                    message: format!(
                        "database '{}' changed during destructive maintenance",
                        path.display()
                    ),
                });
            }
        }
        self.registry.release_destructive(self.attempt)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for DestructiveMaintenanceReservation {
    fn drop(&mut self) {
        if !self.released {
            // Abandonment intentionally remains fail-closed until process exit.
        }
    }
}

impl StoreRuntimeRegistry {
    pub async fn begin_destructive_maintenance(
        &self,
        target: DestructiveMaintenanceTarget,
    ) -> Result<DestructiveMaintenanceReservation, StoreRuntimeRegistryFailure> {
        let (attempt, closes) =
            {
                let mut state = self.lock_state();
                if state.destructive_paths.values().any(|reservation| {
                    paths_overlap(&target.root, &reservation.root)
                        || target
                            .database_paths
                            .iter()
                            .any(|path| reservation.database_paths.binary_search(path).is_ok())
                }) {
                    return Err(
                        StoreRuntimeRegistryFailure::DestructiveMaintenanceInProgress {
                            root: target.root.clone(),
                        },
                    );
                }
                if state.entries.values().any(|entry| match entry {
                    RegistryEntry::Opening(opening) => opening
                        .database_authority
                        .as_ref()
                        .is_some_and(|authority| {
                            target.includes_database_path(authority.canonical_database_path())
                        }),
                    RegistryEntry::Ready(_) | RegistryEntry::Evicting(_) => false,
                }) {
                    return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                        operation: "reserve destructive store maintenance",
                        message: format!(
                            "a runtime under '{}' is still opening",
                            target.root.display()
                        ),
                    });
                }
                if let Some(key) = state.entries.iter().find_map(|(key, entry)| match entry {
                    RegistryEntry::Evicting(evicting)
                        if target.includes_database_path(evicting.handle.locator().path()) =>
                    {
                        Some(key.clone())
                    }
                    RegistryEntry::Opening(_)
                    | RegistryEntry::Ready(_)
                    | RegistryEntry::Evicting(_) => None,
                }) {
                    return Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                        key: Box::new(key),
                    });
                }
                let attempt = state
                    .next_destructive_attempt
                    .checked_add(1)
                    .ok_or(StoreRuntimeRegistryFailure::EvictionAttemptExhausted)?;
                state.next_destructive_attempt = attempt;
                let closes = state
                    .entries
                    .values()
                    .filter_map(|entry| match entry {
                        RegistryEntry::Ready(ready)
                            if target.includes_database_path(ready.handle.locator().path()) =>
                        {
                            ready
                                .handle
                                .inner
                                .database_authority
                                .clone()
                                .map(|authority| (ready.handle.binding().clone(), authority))
                        }
                        RegistryEntry::Opening(_)
                        | RegistryEntry::Ready(_)
                        | RegistryEntry::Evicting(_) => None,
                    })
                    .collect::<Vec<_>>();
                let (released, _) = tokio::sync::watch::channel(false);
                state.destructive_paths.insert(
                    attempt,
                    DestructivePathReservation {
                        root: target.root.clone(),
                        database_paths: target.database_paths.clone(),
                        released,
                    },
                );
                (attempt, closes)
            };

        let mut reservation_guard = DestructiveReservationGuard::new(self.clone(), attempt);
        let mut closed = Vec::with_capacity(closes.len());
        for (binding, authority) in closes {
            match self.close_exact(&binding, &authority).await {
                Ok(proof) => closed.push(proof),
                Err(error) => {
                    reservation_guard.release()?;
                    return Err(error);
                }
            }
        }
        reservation_guard.disarm();
        Ok(DestructiveMaintenanceReservation {
            registry: self.clone(),
            attempt,
            target,
            closed,
            released: false,
        })
    }

    pub(super) fn destructive_wait(
        &self,
        path: &Path,
    ) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.lock_state()
            .destructive_paths
            .values()
            .find(|reservation| reservation_matches(reservation, path))
            .map(|reservation| reservation.released.subscribe())
    }

    fn release_destructive(&self, attempt: u64) -> Result<(), StoreRuntimeRegistryFailure> {
        let reservation = self
            .lock_state()
            .destructive_paths
            .remove(&attempt)
            .ok_or_else(|| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "release destructive store maintenance",
                message: "destructive maintenance reservation was lost".to_owned(),
            })?;
        reservation.released.send_replace(true);
        Ok(())
    }

    fn release_destructive_after_cancellation(&self, attempt: u64) {
        if let Some(reservation) = self.lock_state().destructive_paths.remove(&attempt) {
            reservation.released.send_replace(true);
        }
    }
}

pub(super) fn reservation_matches(reservation: &DestructivePathReservation, path: &Path) -> bool {
    reservation
        .database_paths
        .binary_search_by(|candidate| candidate.as_path().cmp(path))
        .is_ok()
}

fn canonical_existing_directory(root: PathBuf) -> Result<PathBuf, StoreRuntimeRegistryFailure> {
    let canonical = root.canonicalize().map_err(|error| {
        StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
            message: format!("canonicalize store root '{}': {error}", root.display()),
        }
    })?;
    if !canonical.is_dir() {
        return Err(
            StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                message: format!("store root '{}' is not a directory", canonical.display()),
            },
        );
    }
    Ok(canonical)
}

fn canonical_database_under_root(
    root: &Path,
    path: &Path,
) -> Result<PathBuf, StoreRuntimeRegistryFailure> {
    let canonical = crate::path_safety::canonicalize_path_or_existing_parent(path);
    if !canonical.starts_with(root) {
        return Err(
            StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                message: format!(
                    "database '{}' is outside reserved store root '{}'",
                    canonical.display(),
                    root.display()
                ),
            },
        );
    }
    if canonical.exists() && !canonical.is_file() {
        return Err(
            StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                message: format!("database '{}' is not a regular file", canonical.display()),
            },
        );
    }
    Ok(canonical)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use tracedecay_domain::{
        BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, WorktreeId,
    };
    use tracedecay_store::{
        CodeShardScopeV1, RuntimeMaintenanceStateV1, StoreIncarnationV1, StoreShardIdV1,
        StoreShardScopeV1, VerifiedStoreLocatorV1,
    };

    use super::*;
    use crate::db::DatabaseAuthority;
    use crate::store_runtime::registry::{
        PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot, ProfileAuthorityPinResult,
        PublishedShardRuntime, ResolvedStoreLocator, ShardRuntimeBuildRequest,
        ShardRuntimePublisher, StoreRuntimeLookup, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
        StoreRuntimeRegistryFuture, StoreRuntimeResolver,
    };
    use crate::store_runtime::shard::ShardRuntime;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn profile_shard() -> StoreShardIdV1 {
        StoreShardIdV1::profile(
            id::<BrainId>("brain.destructive-evicting"),
            id::<UserProfileId>("profile.destructive-evicting"),
        )
    }

    fn code_shard() -> StoreShardIdV1 {
        StoreShardIdV1::code(
            id::<BrainId>("brain.destructive-evicting"),
            id::<UserProfileId>("profile.destructive-evicting"),
            id::<ProjectId>("project.destructive-evicting"),
            id::<RepositoryId>("repository.destructive-evicting"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>("worktree.destructive-evicting"),
            },
        )
    }

    struct FixtureResolver {
        profile_path: PathBuf,
    }

    impl StoreRuntimeResolver for FixtureResolver {
        fn resolve<'a>(
            &'a self,
            key: &'a super::super::StoreRuntimeKey,
            _mode: super::super::StoreRuntimeOpenMode,
            _database_authority: Option<&'a DatabaseAuthority>,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            let verified = VerifiedStoreLocatorV1::new(
                key.shard_id().clone(),
                key.incarnation(),
                LocatorDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
            );
            let path = _database_authority.map_or_else(
                || self.profile_path.clone(),
                |authority| authority.canonical_database_path().to_path_buf(),
            );
            Box::pin(async move { Ok(ResolvedStoreLocator::new(verified, path)) })
        }
    }

    struct BlockingCloseAttachment {
        opened_file_identity: u64,
        drained: AtomicBool,
        close_started: tokio::sync::Notify,
        close_released: Mutex<bool>,
        close_wake: Condvar,
        closed: AtomicBool,
    }

    impl BlockingCloseAttachment {
        fn release_close(&self) {
            *self.close_released.lock().unwrap() = true;
            self.close_wake.notify_all();
        }
    }

    impl PhysicalRuntimeAttachment for BlockingCloseAttachment {
        fn snapshot(&self) -> PhysicalRuntimeSnapshot {
            PhysicalRuntimeSnapshot {
                healthy: true,
                writer_present: !self.drained.load(Ordering::SeqCst),
                ..PhysicalRuntimeSnapshot::default()
            }
        }

        fn opened_file_identity(&self) -> Result<u64, String> {
            Ok(self.opened_file_identity)
        }

        fn drain(&self) -> Result<(), String> {
            self.drained.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn close_and_join(&self) -> Result<(), String> {
            self.close_started.notify_one();
            let mut released = self.close_released.lock().unwrap();
            while !*released {
                released = self.close_wake.wait(released).unwrap();
            }
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct BlockingClosePublisher {
        attachment: Arc<BlockingCloseAttachment>,
    }

    impl ShardRuntimePublisher for BlockingClosePublisher {
        fn publish(
            &self,
            request: ShardRuntimeBuildRequest,
        ) -> StoreRuntimeRegistryFuture<
            '_,
            Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>,
        > {
            let attachment = Arc::clone(&self.attachment);
            Box::pin(async move {
                let runtime = Arc::new(ShardRuntime::new(
                    request.binding().clone(),
                    matches!(request.binding().shard_id.scope, StoreShardScopeV1::Profile),
                ));
                runtime
                    .transition(RuntimeMaintenanceStateV1::Opening)
                    .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
                    .unwrap();
                Ok(PublishedShardRuntime::new(runtime, attachment))
            })
        }
    }

    #[tokio::test]
    async fn destructive_reservation_rejects_a_matching_evicting_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_path = temporary.path().join("profile.db");
        let code_path = temporary.path().join("code.db");
        rusqlite::Connection::open(&profile_path).unwrap();
        rusqlite::Connection::open(&code_path).unwrap();
        let profile_path = profile_path.canonicalize().unwrap();
        let code_path = code_path.canonicalize().unwrap();
        let opened_file_identity = crate::db::sqlite_generation_identity(&code_path).unwrap();
        let attachment = Arc::new(BlockingCloseAttachment {
            opened_file_identity,
            drained: AtomicBool::new(false),
            close_started: tokio::sync::Notify::new(),
            close_released: Mutex::new(false),
            close_wake: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        let authority =
            DatabaseAuthority::for_runtime(&code_path, "reserve destructive evicting runtime")
                .unwrap();
        let registry = StoreRuntimeRegistry::new(
            Arc::new(FixtureResolver { profile_path }),
            Arc::new(BlockingClosePublisher {
                attachment: Arc::clone(&attachment),
            }),
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
        let binding = code.binding().clone();
        drop(code);
        drop(profile);

        let close_registry = registry.clone();
        let close_binding = binding.clone();
        let close_authority = authority.clone();
        let close = tokio::spawn(async move {
            close_registry
                .close_exact(&close_binding, &close_authority)
                .await
        });
        if tokio::time::timeout(Duration::from_secs(2), attachment.close_started.notified())
            .await
            .is_err()
        {
            attachment.release_close();
            panic!("exact close did not enter the blocking physical close");
        }

        let reservation = registry
            .begin_destructive_maintenance(
                DestructiveMaintenanceTarget::new(temporary.path(), [code_path]).unwrap(),
            )
            .await;

        attachment.release_close();
        close.await.unwrap().unwrap();
        assert!(matches!(
            reservation,
            Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_destructive_reservation_releases_and_reopens_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_path = temporary.path().join("profile.db");
        let code_path = temporary.path().join("code.db");
        rusqlite::Connection::open(&profile_path).unwrap();
        rusqlite::Connection::open(&code_path).unwrap();
        let profile_path = profile_path.canonicalize().unwrap();
        let code_path = code_path.canonicalize().unwrap();
        let opened_file_identity = crate::db::sqlite_generation_identity(&code_path).unwrap();
        let attachment = Arc::new(BlockingCloseAttachment {
            opened_file_identity,
            drained: AtomicBool::new(false),
            close_started: tokio::sync::Notify::new(),
            close_released: Mutex::new(false),
            close_wake: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        let authority = DatabaseAuthority::for_runtime(
            &code_path,
            "mount cancellation-safe destructive reservation",
        )
        .unwrap();
        let registry = StoreRuntimeRegistry::new(
            Arc::new(FixtureResolver { profile_path }),
            Arc::new(BlockingClosePublisher {
                attachment: Arc::clone(&attachment),
            }),
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
                Some(pin.clone()),
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

        let target =
            DestructiveMaintenanceTarget::new(temporary.path(), [code_path.clone()]).unwrap();
        let reservation_registry = registry.clone();
        let reservation = tokio::spawn(async move {
            reservation_registry
                .begin_destructive_maintenance(target)
                .await
        });
        if tokio::time::timeout(Duration::from_secs(2), attachment.close_started.notified())
            .await
            .is_err()
        {
            attachment.release_close();
            panic!("destructive close did not enter the blocking physical close");
        }
        reservation.abort();
        assert!(matches!(reservation.await, Err(error) if error.is_cancelled()));
        assert!(matches!(
            registry.lookup(&binding),
            StoreRuntimeLookup::Evicting { .. }
        ));
        assert!(
            registry.destructive_wait(&code_path).is_none(),
            "cancelling the reservation must release its path fence before close completes"
        );
        assert!(matches!(
            registry
                .begin_destructive_maintenance(
                    DestructiveMaintenanceTarget::new(temporary.path(), [code_path.clone()])
                        .unwrap(),
                )
                .await,
            Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress { .. })
        ));

        attachment.release_close();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !matches!(
                registry.lookup(&binding),
                StoreRuntimeLookup::Missing { .. }
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(attachment.closed.load(Ordering::SeqCst));

        let reopened = match registry
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
                panic!("reopen after cancelled destructive reservation failed: {failure:?}")
            }
        };
        assert_ne!(reopened.binding(), &binding);
        drop(reopened);
        drop(profile);
    }
}
