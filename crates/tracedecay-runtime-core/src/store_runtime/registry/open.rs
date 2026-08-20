use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;
use tracedecay_store::{
    RuntimeMaintenanceStateV1, RuntimePublicationIdV1, StoreAuthorityEpochV1,
    StoreRuntimeBindingV1, StoreRuntimeRegistryPublicationV1,
};

use super::capacity::CapacityReservation;
use super::leases::validate_profile_authority;
use super::{
    PublishedShardRuntime, ReadyRuntime, RegistryEntry, RegistryState, RuntimeLocatorRecord,
    ShardRuntimeBuildRequest, StoreRuntimeAccessMode, StoreRuntimeHandle, StoreRuntimeHandleInner,
    StoreRuntimeKey, StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeRegistry,
    StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture, utc_now,
};

static PROCESS_AUTHORITY_EPOCH: AtomicU64 = AtomicU64::new(0);

pub(crate) enum StoreRuntimeOpenBegin {
    Ready(StoreRuntimeHandle),
    Started(StoreRuntimeOpenJoin),
    Joined(StoreRuntimeOpenJoin),
    Rejected(StoreRuntimeRegistryFailure),
}

/// Published shard runtime plus the locator and optional write authority used to
/// open it. Factored out so `build_runtime`'s future return stays under Clippy's
/// type-complexity threshold without changing registry publication semantics.
type BuiltShardRuntimePublication = (
    PublishedShardRuntime,
    RuntimeLocatorRecord,
    Option<crate::db::DatabaseAuthority>,
);

impl StoreRuntimeOpenBegin {
    pub async fn wait(self) -> StoreRuntimeOpenResult {
        match self {
            Self::Ready(handle) => StoreRuntimeOpenResult::Published(handle),
            Self::Started(join) | Self::Joined(join) => join.wait().await,
            Self::Rejected(failure) => StoreRuntimeOpenResult::Failed(failure),
        }
    }
}

impl fmt::Debug for StoreRuntimeOpenBegin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(handle) => formatter.debug_tuple("Ready").field(handle).finish(),
            Self::Started(join) => formatter.debug_tuple("Started").field(join).finish(),
            Self::Joined(join) => formatter.debug_tuple("Joined").field(join).finish(),
            Self::Rejected(failure) => formatter.debug_tuple("Rejected").field(failure).finish(),
        }
    }
}

pub(crate) struct StoreRuntimeOpenJoin {
    key: Box<StoreRuntimeKey>,
    updates: watch::Receiver<OpenState>,
}

impl StoreRuntimeOpenJoin {
    pub(super) async fn wait(mut self) -> StoreRuntimeOpenResult {
        loop {
            let current = self.updates.borrow().clone();
            match current {
                OpenState::Opening => {
                    if self.updates.changed().await.is_err() {
                        return StoreRuntimeOpenResult::Failed(
                            StoreRuntimeRegistryFailure::OpenTaskAbandoned {
                                key: self.key.clone(),
                            },
                        );
                    }
                }
                OpenState::Published(handle) => return StoreRuntimeOpenResult::Published(handle),
                OpenState::Failed(failure) => return StoreRuntimeOpenResult::Failed(failure),
            }
        }
    }
}

impl fmt::Debug for StoreRuntimeOpenJoin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntimeOpenJoin")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub enum StoreRuntimeOpenResult {
    Published(StoreRuntimeHandle),
    Failed(StoreRuntimeRegistryFailure),
}

#[derive(Clone)]
pub(super) enum OpenState {
    Opening,
    Published(StoreRuntimeHandle),
    Failed(StoreRuntimeRegistryFailure),
}

pub(super) struct OpeningRuntime {
    pub(super) binding: StoreRuntimeBindingV1,
    pub(super) attempt: u64,
    pub(super) updates: watch::Sender<OpenState>,
    pub(super) database_authority: Option<crate::db::DatabaseAuthority>,
    pub(super) expected_opened_file_identity: Option<u64>,
    pub(super) mode: StoreRuntimeOpenMode,
    pub(super) access: StoreRuntimeAccessMode,
}

impl StoreRuntimeRegistry {
    pub(crate) fn begin_or_join_open(
        &self,
        request: &StoreRuntimeOpenRequest,
    ) -> StoreRuntimeOpenBegin {
        let key = request.key.clone();
        let (binding, attempt, updates, join, eviction) = {
            let mut state = self.lock_state();
            if state.retiring.contains_key(&key) {
                return StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { key: Box::new(key) },
                );
            }
            if let Some(path) = request
                .database_authority
                .as_ref()
                .map(crate::db::DatabaseAuthority::canonical_database_path)
            {
                if let Some(reservation) = state
                    .destructive_paths
                    .values()
                    .find(|reservation| super::destructive::reservation_matches(reservation, path))
                {
                    return StoreRuntimeOpenBegin::Rejected(
                        StoreRuntimeRegistryFailure::DestructiveMaintenanceInProgress {
                            root: reservation.root.clone(),
                        },
                    );
                }
                if let Some(retained) = retained_database_key(&state, path)
                    && retained != key
                {
                    return StoreRuntimeOpenBegin::Rejected(
                        StoreRuntimeRegistryFailure::DatabaseRuntimeIdentityConflict {
                            requested: Box::new(key),
                            retained: Box::new(retained),
                            path: path.to_path_buf(),
                        },
                    );
                }
            }
            if let Err(failure) = validate_profile_authority(&state, request) {
                return StoreRuntimeOpenBegin::Rejected(failure);
            }
            if let Some((_, graph)) = state
                .graph_publications
                .iter()
                .find(|(graph_key, _)| graph_key.shard_id() == key.shard_id() && *graph_key != &key)
            {
                return StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::GraphIncarnationConflict {
                        requested: Box::new(key),
                        retained: Box::new(graph.binding.clone()),
                    },
                );
            }
            if let Some(entry) = state.entries.get(&key) {
                return match entry {
                    RegistryEntry::Ready(ready)
                        if request.access == StoreRuntimeAccessMode::ReadOnly
                            || (ready.handle.writer_present()
                                && matching_database_authority(
                                    request.database_authority.as_ref(),
                                    ready.handle.inner.database_authority.as_ref(),
                                )) =>
                    {
                        match request.expected_opened_file_identity {
                            Some(expected)
                                if ready.handle.opened_file_identity() != Some(expected) =>
                            {
                                StoreRuntimeOpenBegin::Rejected(
                                    StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                        operation: "join identity-bound registered runtime open",
                                        message: format!(
                                            "opened identity {:?} does not match expected identity {expected}",
                                            ready.handle.opened_file_identity()
                                        ),
                                    },
                                )
                            }
                            Some(_) | None => StoreRuntimeOpenBegin::Ready(ready.handle.clone()),
                        }
                    }
                    RegistryEntry::Opening(opening)
                        if open_access_compatible(request, opening)
                            && request.mode == opening.mode =>
                    {
                        StoreRuntimeOpenBegin::Joined(StoreRuntimeOpenJoin {
                            key: Box::new(key),
                            updates: opening.updates.subscribe(),
                        })
                    }
                    RegistryEntry::Ready(_) | RegistryEntry::Opening(_) => {
                        StoreRuntimeOpenBegin::Rejected(
                            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                operation: "join registered runtime open",
                                message:
                                    "runtime open authority does not match the originating capability"
                                        .to_owned(),
                            },
                        )
                    }
                    RegistryEntry::Evicting(_) => StoreRuntimeOpenBegin::Rejected(
                        StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                            key: Box::new(key),
                        },
                    ),
                };
            }

            let authority_epoch = match state.graph_publications.get(&key) {
                Some(graph) => graph.binding.authority_epoch,
                None => match allocate_authority_epoch() {
                    Ok(epoch) => epoch,
                    Err(failure) => return StoreRuntimeOpenBegin::Rejected(failure),
                },
            };
            let Some(attempt) = allocate_counter(&mut state.next_open_attempt) else {
                return StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::OpenAttemptExhausted,
                );
            };
            let binding =
                StoreRuntimeBindingV1::new(key.shard_id.clone(), key.incarnation, authority_epoch);
            let (updates, receiver) = watch::channel(OpenState::Opening);
            let eviction = if key.is_project_code_capacity_exempt() {
                None
            } else {
                match self.reserve_project_code_capacity(&mut state) {
                    Ok(CapacityReservation::Available) => None,
                    Ok(CapacityReservation::Eviction(reservation)) => Some(reservation),
                    Ok(CapacityReservation::Exhausted) => {
                        return StoreRuntimeOpenBegin::Rejected(
                            StoreRuntimeRegistryFailure::ProjectCodeBudgetExhausted {
                                limit: self.inner.config.project_budget(),
                            },
                        );
                    }
                    Err(failure) => return StoreRuntimeOpenBegin::Rejected(failure),
                }
            };
            state.entries.insert(
                key.clone(),
                RegistryEntry::Opening(OpeningRuntime {
                    binding: binding.clone(),
                    attempt,
                    updates: updates.clone(),
                    database_authority: request.database_authority.clone(),
                    expected_opened_file_identity: request.expected_opened_file_identity,
                    mode: request.mode,
                    access: request.access,
                }),
            );
            let join = StoreRuntimeOpenJoin {
                key: Box::new(key.clone()),
                updates: receiver,
            };
            (binding, attempt, updates, join, eviction)
        };

        if let Some(eviction) = eviction
            && let Err(failure) = self.complete_project_code_eviction(eviction)
        {
            self.fail_reserved_open(&key, attempt, &updates, failure.clone());
            return StoreRuntimeOpenBegin::Rejected(failure);
        }

        let registry = self.clone();
        let database_authority = request.database_authority.clone();
        let mode = request.mode;
        let access = request.access;
        let expected_opened_file_identity = request.expected_opened_file_identity;
        tokio::spawn(async move {
            let guard = OpenAttemptGuard::new(registry.clone(), key.clone(), attempt, updates);
            let outcome = registry
                .build_runtime(&key, binding, database_authority, mode, access)
                .await;
            let outcome = match (outcome, expected_opened_file_identity) {
                (Ok((published, locator, authority)), Some(expected)) => {
                    let opened = published.opened_file_identity();
                    let current = authority
                        .as_ref()
                        .and_then(|_| crate::db::sqlite_generation_identity(locator.path()).ok());
                    let current_mismatch = authority.is_some() && current != Some(expected);
                    match opened {
                        Ok(opened) if opened != expected || current_mismatch => {
                            let close = tokio::task::spawn_blocking(move || {
                                close_unpublished_runtime(published)
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| result);
                            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                operation: "publish identity-bound registered runtime open",
                                message: format!(
                                    "opened identity {opened} and current identity {current:?} do not both match expected identity {expected}; unpublished close={close:?}"
                                ),
                            })
                        }
                        Ok(_) => Ok((published, locator, authority)),
                        Err(message) => {
                            let close = tokio::task::spawn_blocking(move || {
                                close_unpublished_runtime(published)
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| result);
                            Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                operation: "capture identity-bound opened SQLite file identity",
                                message: format!("{message}; unpublished close={close:?}"),
                            })
                        }
                    }
                }
                (outcome, _) => outcome,
            };
            guard.complete(outcome);
        });
        StoreRuntimeOpenBegin::Started(join)
    }

    pub async fn open(&self, request: StoreRuntimeOpenRequest) -> StoreRuntimeOpenResult {
        loop {
            if let Some(path) = request
                .database_authority
                .as_ref()
                .map(crate::db::DatabaseAuthority::canonical_database_path)
            {
                while let Some(mut released) = self.destructive_wait(path) {
                    while !*released.borrow_and_update() {
                        if released.changed().await.is_err() {
                            return StoreRuntimeOpenResult::Failed(
                                StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                    operation: "wait for destructive store maintenance",
                                    message: "destructive reservation closed without release"
                                        .to_owned(),
                                },
                            );
                        }
                    }
                }
            }
            match self.begin_or_join_open(&request) {
                StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::DestructiveMaintenanceInProgress { .. },
                ) => {}
                begin => return begin.wait().await,
            }
        }
    }

    fn fail_reserved_open(
        &self,
        key: &StoreRuntimeKey,
        attempt: u64,
        updates: &watch::Sender<OpenState>,
        failure: StoreRuntimeRegistryFailure,
    ) {
        let mut state = self.lock_state();
        let still_opening = matches!(
            state.entries.get(key),
            Some(RegistryEntry::Opening(opening)) if opening.attempt == attempt
        );
        if still_opening {
            state.entries.remove(key);
            updates.send_replace(OpenState::Failed(failure));
        }
    }

    fn build_runtime<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        binding: StoreRuntimeBindingV1,
        database_authority: Option<crate::db::DatabaseAuthority>,
        mode: StoreRuntimeOpenMode,
        access: StoreRuntimeAccessMode,
    ) -> StoreRuntimeRegistryFuture<
        'a,
        Result<BuiltShardRuntimePublication, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            let resolved = self
                .inner
                .resolver
                .resolve(key, mode, database_authority.as_ref())
                .await?;
            if !resolved.matches(key) {
                return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                    key: Box::new(key.clone()),
                    locator: Box::new(resolved.verified().clone()),
                });
            }
            if let Some(authority) = database_authority.as_ref() {
                authority
                    .require_active_write_scope("publish registered SQLite runtime")
                    .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                        operation: "publish registered SQLite runtime",
                        message: error.to_string(),
                    })?;
                if authority.canonical_database_path() != resolved.path() {
                    return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                        operation: "publish registered SQLite runtime",
                        message: format!(
                            "resolved locator {} does not match originating database authority {}",
                            resolved.path().display(),
                            authority.canonical_database_path().display()
                        ),
                    });
                }
            }
            let locator = RuntimeLocatorRecord::new(key.clone(), resolved);
            let published = self
                .inner
                .publisher
                .publish(ShardRuntimeBuildRequest::new(
                    binding.clone(),
                    locator.clone(),
                    mode,
                    access,
                    database_authority.clone(),
                ))
                .await?;
            if published.logical().binding() != &binding {
                return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                    expected: Box::new(binding),
                    actual: Box::new(published.logical().binding().clone()),
                });
            }
            Ok((published, locator, database_authority))
        })
    }
}

fn open_access_compatible(request: &StoreRuntimeOpenRequest, opening: &OpeningRuntime) -> bool {
    if request.expected_opened_file_identity != opening.expected_opened_file_identity {
        return false;
    }
    match request.access {
        StoreRuntimeAccessMode::ReadOnly => true,
        StoreRuntimeAccessMode::ReadWrite => {
            opening.access == StoreRuntimeAccessMode::ReadWrite
                && matching_database_authority(
                    request.database_authority.as_ref(),
                    opening.database_authority.as_ref(),
                )
        }
    }
}

fn close_unpublished_runtime(published: PublishedShardRuntime) -> Result<(), String> {
    let (runtime, attachment) = published.into_parts();
    let draining = runtime
        .transition(RuntimeMaintenanceStateV1::Draining)
        .map_err(|error| error.to_string());
    let closed = attachment.drain().and_then(|_| attachment.close_and_join());
    let outcome = draining.and(closed);
    let target = if outcome.is_ok() {
        RuntimeMaintenanceStateV1::Closed
    } else {
        RuntimeMaintenanceStateV1::Faulted
    };
    let lifecycle = runtime
        .transition(target)
        .map_err(|error| error.to_string());
    outcome.and(lifecycle)
}

fn retained_database_key(state: &RegistryState, path: &std::path::Path) -> Option<StoreRuntimeKey> {
    state.entries.iter().find_map(|(key, entry)| {
        let candidate = match entry {
            RegistryEntry::Opening(opening)
                if opening.access == StoreRuntimeAccessMode::ReadWrite =>
            {
                opening
                    .database_authority
                    .as_ref()
                    .map(crate::db::DatabaseAuthority::canonical_database_path)
            }
            RegistryEntry::Ready(ready) if ready.handle.writer_present() => {
                Some(ready.handle.canonical_path())
            }
            RegistryEntry::Evicting(evicting) if evicting.handle.writer_present() => {
                Some(evicting.handle.canonical_path())
            }
            RegistryEntry::Opening(_) | RegistryEntry::Ready(_) | RegistryEntry::Evicting(_) => {
                None
            }
        };
        candidate
            .is_some_and(|candidate| candidate == path)
            .then(|| key.clone())
    })
}

struct OpenAttemptGuard {
    registry: StoreRuntimeRegistry,
    key: StoreRuntimeKey,
    attempt: u64,
    updates: watch::Sender<OpenState>,
    armed: bool,
}

impl OpenAttemptGuard {
    fn new(
        registry: StoreRuntimeRegistry,
        key: StoreRuntimeKey,
        attempt: u64,
        updates: watch::Sender<OpenState>,
    ) -> Self {
        Self {
            registry,
            key,
            attempt,
            updates,
            armed: true,
        }
    }

    fn complete(
        mut self,
        outcome: Result<
            (
                PublishedShardRuntime,
                RuntimeLocatorRecord,
                Option<crate::db::DatabaseAuthority>,
            ),
            StoreRuntimeRegistryFailure,
        >,
    ) {
        let mut state = self.registry.lock_state();
        let still_opening = matches!(
            state.entries.get(&self.key),
            Some(RegistryEntry::Opening(opening)) if opening.attempt == self.attempt
        );
        if !still_opening {
            self.armed = false;
            return;
        }

        match outcome {
            Ok((published, locator, database_authority)) => {
                let opened_file_identity = match published.opened_file_identity() {
                    Ok(identity) => identity,
                    Err(message) => {
                        self.fail(
                            &mut state,
                            StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                                operation: "capture opened SQLite file identity",
                                message,
                            },
                        );
                        self.armed = false;
                        return;
                    }
                };
                match allocate_publication(&mut state, published.logical().binding().clone()) {
                    Ok(publication) => {
                        let (runtime, attachment) = published.into_parts();
                        let handle = StoreRuntimeHandle {
                            inner: Arc::new(StoreRuntimeHandleInner {
                                publication,
                                runtime,
                                attachment,
                                locator,
                                opened_file_identity,
                                database_authority,
                            }),
                        };
                        if self.key.is_profile() {
                            state
                                .profile_authorities
                                .insert(self.key.shard_id.clone(), handle.binding().clone());
                        }
                        state.entries.insert(
                            self.key.clone(),
                            RegistryEntry::Ready(ReadyRuntime {
                                handle: handle.clone(),
                            }),
                        );
                        self.updates.send_replace(OpenState::Published(handle));
                    }
                    Err(failure) => self.fail(&mut state, failure),
                }
            }
            Err(failure) => self.fail(&mut state, failure),
        }
        self.armed = false;
    }

    fn fail(&self, state: &mut RegistryState, failure: StoreRuntimeRegistryFailure) {
        state.entries.remove(&self.key);
        self.updates.send_replace(OpenState::Failed(failure));
    }
}

fn matching_database_authority(
    requested: Option<&crate::db::DatabaseAuthority>,
    retained: Option<&crate::db::DatabaseAuthority>,
) -> bool {
    match (requested, retained) {
        (Some(requested), Some(retained)) => {
            requested
                .require_active_write_scope("join registered runtime open")
                .is_ok()
                && requested.token() == retained.token()
                && requested.canonical_database_path() == retained.canonical_database_path()
        }
        #[cfg(test)]
        (None, None) => true,
        _ => false,
    }
}

impl Drop for OpenAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.registry.lock_state();
        let still_opening = matches!(
            state.entries.get(&self.key),
            Some(RegistryEntry::Opening(opening)) if opening.attempt == self.attempt
        );
        if still_opening {
            self.fail(
                &mut state,
                StoreRuntimeRegistryFailure::OpenTaskAbandoned {
                    key: Box::new(self.key.clone()),
                },
            );
        }
    }
}

pub(super) fn retain_authority_epoch_floor(floor: StoreAuthorityEpochV1) {
    PROCESS_AUTHORITY_EPOCH.fetch_max(floor.get(), Ordering::AcqRel);
}

pub(super) fn allocate_authority_epoch()
-> Result<StoreAuthorityEpochV1, StoreRuntimeRegistryFailure> {
    let previous = PROCESS_AUTHORITY_EPOCH
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .map_err(|_| StoreRuntimeRegistryFailure::AuthorityEpochExhausted)?;
    StoreAuthorityEpochV1::new(previous + 1)
        .map_err(|_| StoreRuntimeRegistryFailure::AuthorityEpochExhausted)
}

fn allocate_publication(
    state: &mut RegistryState,
    binding: StoreRuntimeBindingV1,
) -> Result<StoreRuntimeRegistryPublicationV1, StoreRuntimeRegistryFailure> {
    let sequence = allocate_counter(&mut state.next_publication)
        .ok_or(StoreRuntimeRegistryFailure::PublicationIdExhausted)?;
    let publication_id = RuntimePublicationIdV1::new(format!("runtime-publication-{sequence}"))
        .map_err(|_| StoreRuntimeRegistryFailure::PublicationIdExhausted)?;
    Ok(StoreRuntimeRegistryPublicationV1 {
        publication_id,
        binding,
        published_at: utc_now(),
    })
}

fn allocate_counter(counter: &mut u64) -> Option<u64> {
    *counter = counter.checked_add(1)?;
    Some(*counter)
}
