use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;
use tracedecay_store::{
    RuntimePublicationIdV1, StoreAuthorityEpochV1, StoreRuntimeBindingV1,
    StoreRuntimeRegistryPublicationV1,
};

use super::capacity::CapacityReservation;
use super::leases::validate_profile_authority;
use super::{
    PublishedShardRuntime, ReadyRuntime, RegistryEntry, RegistryState, RuntimeLocatorRecord,
    ShardRuntimeBuildRequest, StoreRuntimeHandle, StoreRuntimeHandleInner, StoreRuntimeKey,
    StoreRuntimeOpenRequest, StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
    StoreRuntimeRegistryFuture, utc_now,
};

static PROCESS_AUTHORITY_EPOCH: AtomicU64 = AtomicU64::new(0);

pub(crate) enum StoreRuntimeOpenBegin {
    Ready(StoreRuntimeHandle),
    Started(StoreRuntimeOpenJoin),
    Joined(StoreRuntimeOpenJoin),
    Rejected(StoreRuntimeRegistryFailure),
}

impl StoreRuntimeOpenBegin {
    pub(crate) async fn wait(self) -> StoreRuntimeOpenResult {
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
pub(crate) enum StoreRuntimeOpenResult {
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
    pub(super) attempt: u64,
    pub(super) updates: watch::Sender<OpenState>,
}

impl StoreRuntimeRegistry {
    pub(crate) fn begin_or_join_open(
        &self,
        request: &StoreRuntimeOpenRequest,
    ) -> StoreRuntimeOpenBegin {
        let key = request.key.clone();
        let (binding, attempt, updates, join, eviction) = {
            let mut state = self.lock_state();
            if let Err(failure) = validate_profile_authority(&state, request) {
                return StoreRuntimeOpenBegin::Rejected(failure);
            }
            if let Some(entry) = state.entries.get(&key) {
                return match entry {
                    RegistryEntry::Ready(ready) => {
                        StoreRuntimeOpenBegin::Ready(ready.handle.clone())
                    }
                    RegistryEntry::Opening(opening) => {
                        StoreRuntimeOpenBegin::Joined(StoreRuntimeOpenJoin {
                            key: Box::new(key),
                            updates: opening.updates.subscribe(),
                        })
                    }
                    RegistryEntry::Evicting(_) => StoreRuntimeOpenBegin::Rejected(
                        StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                            key: Box::new(key),
                        },
                    ),
                };
            }

            let authority_epoch = match allocate_authority_epoch() {
                Ok(epoch) => epoch,
                Err(failure) => return StoreRuntimeOpenBegin::Rejected(failure),
            };
            let Some(attempt) = allocate_counter(&mut state.next_open_attempt) else {
                return StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::OpenAttemptExhausted,
                );
            };
            let binding =
                StoreRuntimeBindingV1::new(key.shard_id.clone(), key.incarnation, authority_epoch);
            let (updates, receiver) = watch::channel(OpenState::Opening);
            let eviction = if key.is_profile() {
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
                    attempt,
                    updates: updates.clone(),
                }),
            );
            let join = StoreRuntimeOpenJoin {
                key: Box::new(key.clone()),
                updates: receiver,
            };
            (binding, attempt, updates, join, eviction)
        };

        if let Some(eviction) = eviction {
            if let Err(failure) = self.complete_project_code_eviction(eviction) {
                self.fail_reserved_open(&key, attempt, &updates, failure.clone());
                return StoreRuntimeOpenBegin::Rejected(failure);
            }
        }

        let registry = self.clone();
        tokio::spawn(async move {
            let guard = OpenAttemptGuard::new(registry.clone(), key.clone(), attempt, updates);
            let outcome = registry.build_runtime(&key, binding).await;
            guard.complete(outcome);
        });
        StoreRuntimeOpenBegin::Started(join)
    }

    pub(crate) async fn open(&self, request: StoreRuntimeOpenRequest) -> StoreRuntimeOpenResult {
        self.begin_or_join_open(&request).wait().await
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
    ) -> StoreRuntimeRegistryFuture<
        'a,
        Result<(PublishedShardRuntime, RuntimeLocatorRecord), StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            let resolved = self.inner.resolver.resolve(key).await?;
            if !resolved.matches(key) {
                return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                    key: Box::new(key.clone()),
                    locator: Box::new(resolved.verified().clone()),
                });
            }
            let locator = RuntimeLocatorRecord::new(key.clone(), resolved);
            let published = self
                .inner
                .publisher
                .publish(ShardRuntimeBuildRequest::new(
                    binding.clone(),
                    locator.clone(),
                ))
                .await?;
            if published.logical().binding() != &binding {
                return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                    expected: Box::new(binding),
                    actual: Box::new(published.logical().binding().clone()),
                });
            }
            Ok((published, locator))
        })
    }
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
        outcome: Result<(PublishedShardRuntime, RuntimeLocatorRecord), StoreRuntimeRegistryFailure>,
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
            Ok((published, locator)) => {
                match allocate_publication(&mut state, published.logical().binding().clone()) {
                    Ok(publication) => {
                        let (runtime, attachment) = published.into_parts();
                        let handle = StoreRuntimeHandle {
                            inner: Arc::new(StoreRuntimeHandleInner {
                                publication,
                                runtime,
                                attachment,
                                locator,
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

fn allocate_authority_epoch() -> Result<StoreAuthorityEpochV1, StoreRuntimeRegistryFailure> {
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
