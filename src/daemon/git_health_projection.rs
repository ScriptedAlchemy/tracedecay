//! Daemon lifecycle owner for background Git health projection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionReadPortV1,
    GitHealthProjectionUnavailableReasonV1, ResolvedScope,
};
use tracedecay_graph_db::GraphDb;

use crate::application::context::CancellationToken;
use crate::graph::git::{GitHealthProjectionError, GitHealthProjectionStoreV1};

const DEFAULT_COMMIT_BATCH_LIMIT: usize = 64;
const REFRESH_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(super) struct GitHealthProjectionRegistryV1 {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    state: Mutex<RegistryState>,
    mount_gate: tokio::sync::Mutex<()>,
    next_owner_id: AtomicU64,
    max_owners: usize,
}

#[derive(Default)]
struct RegistryState {
    owners: HashMap<String, Arc<GitHealthProjectionOwnerV1>>,
    databases: HashMap<PathBuf, GraphDb>,
}

struct GitHealthProjectionOwnerV1 {
    owner_id: u64,
    leases: AtomicUsize,
    repository_root: PathBuf,
    store_path: PathBuf,
    scope: ResolvedScope,
    store: Arc<GitHealthProjectionStoreV1>,
    availability: RwLock<GitHealthProjectionAvailabilityV1>,
    wake: tokio::sync::Notify,
    cancellation: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    commit_batch_limit: usize,
}

struct MountedGitHealthProjectionPortV1 {
    registry: Arc<RegistryInner>,
    key: String,
    owner_id: u64,
}

impl Drop for MountedGitHealthProjectionPortV1 {
    fn drop(&mut self) {
        let owner = self
            .registry
            .state
            .lock()
            .ok()
            .and_then(|state| state.owners.get(&self.key).cloned())
            .filter(|owner| owner.owner_id == self.owner_id);
        if owner.is_some_and(|owner| owner.leases.fetch_sub(1, Ordering::AcqRel) == 1) {
            self.registry.retire_if_match(&self.key, self.owner_id);
        }
    }
}

impl GitHealthProjectionRegistryV1 {
    pub(super) fn new(max_owners: usize) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState::default()),
                mount_gate: tokio::sync::Mutex::new(()),
                next_owner_id: AtomicU64::new(1),
                max_owners: max_owners.max(1),
            }),
        }
    }

    pub(super) async fn mount(
        &self,
        repository_root: &Path,
        store_path: PathBuf,
        scope: ResolvedScope,
    ) -> Result<Arc<dyn GitHealthProjectionReadPortV1>, GitHealthProjectionError> {
        let _mount = self.inner.mount_gate.lock().await;
        let key = scope.worktree_id.as_str().to_owned();
        let retired = {
            let mut state = self.inner.state.lock().map_err(|_| {
                GitHealthProjectionError::Graph(
                    "Git health owner registry lock is poisoned".to_owned(),
                )
            })?;
            if let Some(owner) = state.owners.get(&key) {
                if owner.repository_root == repository_root
                    && owner.store_path == store_path
                    && owner.scope == scope
                {
                    owner.leases.fetch_add(1, Ordering::Relaxed);
                    owner.wake.notify_one();
                    return Ok(Arc::new(MountedGitHealthProjectionPortV1 {
                        registry: Arc::clone(&self.inner),
                        key,
                        owner_id: owner.owner_id,
                    }));
                }
            }
            state.owners.remove(&key)
        };
        if let Some(owner) = retired {
            owner.retire().await;
        }

        let database = {
            let state = self.inner.state.lock().map_err(|_| {
                GitHealthProjectionError::Graph(
                    "Git health database registry lock is poisoned".to_owned(),
                )
            })?;
            if state.owners.len() >= self.inner.max_owners {
                return Err(GitHealthProjectionError::Graph(
                    "Git health owner capacity is exhausted".to_owned(),
                ));
            }
            state.databases.get(&store_path).cloned()
        };
        let database = match database {
            Some(database) => database,
            None => {
                let path = store_path.clone();
                let cancellation = CancellationToken::new();
                let opened = tokio::task::spawn_blocking(move || {
                    GitHealthProjectionStoreV1::open(&path, &cancellation)
                })
                .await
                .map_err(|error| {
                    GitHealthProjectionError::Graph(format!(
                        "Git health project graph open task failed: {error}"
                    ))
                })??;
                let database = opened.database();
                let mut state = self.inner.state.lock().map_err(|_| {
                    GitHealthProjectionError::Graph(
                        "Git health database registry lock is poisoned".to_owned(),
                    )
                })?;
                state
                    .databases
                    .entry(store_path.clone())
                    .or_insert_with(|| database.clone())
                    .clone()
            }
        };
        let owner_id = self.inner.next_owner_id.fetch_add(1, Ordering::Relaxed);
        let owner = Arc::new(GitHealthProjectionOwnerV1 {
            owner_id,
            leases: AtomicUsize::new(1),
            repository_root: repository_root.to_path_buf(),
            store_path,
            scope,
            store: Arc::new(GitHealthProjectionStoreV1::from_database(database)),
            availability: RwLock::new(GitHealthProjectionAvailabilityV1::Warming { target: None }),
            wake: tokio::sync::Notify::new(),
            cancellation: CancellationToken::new(),
            task: Mutex::new(None),
            commit_batch_limit: DEFAULT_COMMIT_BATCH_LIMIT,
        });
        {
            let mut state = self.inner.state.lock().map_err(|_| {
                GitHealthProjectionError::Graph(
                    "Git health owner registry lock is poisoned".to_owned(),
                )
            })?;
            state.owners.insert(key.clone(), Arc::clone(&owner));
        }
        owner.start();
        Ok(Arc::new(MountedGitHealthProjectionPortV1 {
            registry: Arc::clone(&self.inner),
            key,
            owner_id,
        }))
    }

    pub(super) async fn shutdown(&self) {
        let (owners, databases) = match self.inner.state.lock() {
            Ok(mut state) => (
                state.owners.drain().map(|(_, owner)| owner).collect(),
                state
                    .databases
                    .drain()
                    .map(|(_, database)| database)
                    .collect(),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };
        for owner in owners {
            owner.retire().await;
        }
        for database in databases {
            let _ = tokio::task::spawn_blocking(move || database.close()).await;
        }
    }

    #[cfg(test)]
    fn owner_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .map_or(0, |state| state.owners.len())
    }
}

impl RegistryInner {
    fn retire_if_match(self: &Arc<Self>, key: &str, owner_id: u64) {
        let (owner, database) = match self.state.lock() {
            Ok(mut state) => {
                let matches = state
                    .owners
                    .get(key)
                    .is_some_and(|owner| owner.owner_id == owner_id);
                if !matches {
                    return;
                }
                let owner = state.owners.remove(key);
                let database = owner.as_ref().and_then(|owner| {
                    (!state
                        .owners
                        .values()
                        .any(|candidate| candidate.store_path == owner.store_path))
                    .then(|| state.databases.remove(&owner.store_path))
                    .flatten()
                });
                (owner, database)
            }
            Err(_) => return,
        };
        let Some(owner) = owner else {
            return;
        };
        owner.cancellation.cancel();
        owner.wake.notify_waiters();
        tokio::spawn(async move {
            owner.join().await;
            if let Some(database) = database {
                let _ = tokio::task::spawn_blocking(move || database.close()).await;
            }
        });
    }
}

impl GitHealthProjectionReadPortV1 for MountedGitHealthProjectionPortV1 {
    fn read_projection(&self, scope: &ResolvedScope) -> GitHealthProjectionAvailabilityV1 {
        let owner = self
            .registry
            .state
            .lock()
            .ok()
            .and_then(|state| state.owners.get(&self.key).cloned())
            .filter(|owner| owner.owner_id == self.owner_id);
        let Some(owner) = owner else {
            return GitHealthProjectionAvailabilityV1::Unavailable {
                reason: GitHealthProjectionUnavailableReasonV1::NotMounted,
            };
        };
        if owner.scope != *scope {
            return GitHealthProjectionAvailabilityV1::Unavailable {
                reason: GitHealthProjectionUnavailableReasonV1::ScopeDrift,
            };
        }
        owner.wake.notify_one();
        owner.read_validated()
    }
}

impl GitHealthProjectionOwnerV1 {
    fn start(self: &Arc<Self>) {
        let owner = Arc::clone(self);
        let task = tokio::spawn(async move {
            owner.run().await;
        });
        match self.task.lock() {
            Ok(mut slot) => *slot = Some(task),
            Err(_) => {
                self.cancellation.cancel();
                task.abort();
            }
        }
    }

    async fn retire(&self) {
        self.cancellation.cancel();
        self.wake.notify_waiters();
        self.join().await;
    }

    async fn join(&self) {
        let task = self.task.lock().ok().and_then(|mut task| task.take());
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    async fn run(self: Arc<Self>) {
        self.publish(self.store.read(&self.scope));
        loop {
            if self.cancellation.is_cancelled() {
                return;
            }
            let store = Arc::clone(&self.store);
            let repository_root = self.repository_root.clone();
            let scope = self.scope.clone();
            let cancellation = self.cancellation.clone();
            let batch_limit = self.commit_batch_limit;
            let outcome = tokio::task::spawn_blocking(move || {
                let now = now_epoch_secs().map_err(GitHealthProjectionError::Git)?;
                store.advance(&repository_root, &scope, now, batch_limit, &cancellation)
            })
            .await;
            if self.cancellation.is_cancelled() {
                return;
            }
            match outcome {
                Ok(Ok(progress)) => {
                    if progress.complete {
                        self.publish(self.store.read(&self.scope));
                        tokio::select! {
                            () = self.cancellation.cancelled() => return,
                            () = self.wake.notified() => {}
                            () = tokio::time::sleep(REFRESH_POLL_INTERVAL) => {}
                        }
                    } else {
                        self.publish_refreshing(progress.target);
                        tokio::task::yield_now().await;
                    }
                }
                Ok(Err(GitHealthProjectionError::Cancelled)) => return,
                Ok(Err(error)) => {
                    self.publish_failure(&error);
                    tokio::select! {
                        () = self.cancellation.cancelled() => return,
                        () = self.wake.notified() => {}
                        () = tokio::time::sleep(REFRESH_POLL_INTERVAL) => {}
                    }
                }
                Err(_) => {
                    self.publish_unavailable(
                        GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable,
                    );
                    return;
                }
            }
        }
    }

    fn read_validated(&self) -> GitHealthProjectionAvailabilityV1 {
        let cached = self.read_cached();
        let target = match now_epoch_secs() {
            Ok(now) => {
                GitHealthProjectionStoreV1::capture_source(&self.repository_root, &self.scope, now)
            }
            Err(error) => Err(GitHealthProjectionError::Git(error)),
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                let reason = error.unavailable_reason();
                return match cached {
                    GitHealthProjectionAvailabilityV1::Ready { snapshot }
                    | GitHealthProjectionAvailabilityV1::Refreshing { snapshot, .. }
                    | GitHealthProjectionAvailabilityV1::Stale { snapshot, .. } => {
                        GitHealthProjectionAvailabilityV1::Stale { snapshot, reason }
                    }
                    GitHealthProjectionAvailabilityV1::Warming { .. }
                    | GitHealthProjectionAvailabilityV1::Unavailable { .. } => {
                        GitHealthProjectionAvailabilityV1::Unavailable { reason }
                    }
                };
            }
        };
        match cached {
            GitHealthProjectionAvailabilityV1::Ready { snapshot } if snapshot.source == target => {
                GitHealthProjectionAvailabilityV1::Ready { snapshot }
            }
            GitHealthProjectionAvailabilityV1::Ready { snapshot }
            | GitHealthProjectionAvailabilityV1::Refreshing { snapshot, .. } => {
                GitHealthProjectionAvailabilityV1::Refreshing { snapshot, target }
            }
            GitHealthProjectionAvailabilityV1::Stale { snapshot, reason } => {
                GitHealthProjectionAvailabilityV1::Stale { snapshot, reason }
            }
            GitHealthProjectionAvailabilityV1::Warming { .. } => {
                GitHealthProjectionAvailabilityV1::Warming {
                    target: Some(target),
                }
            }
            GitHealthProjectionAvailabilityV1::Unavailable { reason } => {
                GitHealthProjectionAvailabilityV1::Unavailable { reason }
            }
        }
    }

    fn read_cached(&self) -> GitHealthProjectionAvailabilityV1 {
        self.availability.read().map_or(
            GitHealthProjectionAvailabilityV1::Unavailable {
                reason: GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable,
            },
            |availability| availability.clone(),
        )
    }

    fn publish(&self, availability: GitHealthProjectionAvailabilityV1) {
        if let Ok(mut current) = self.availability.write() {
            *current = availability;
        }
    }

    fn publish_refreshing(&self, target: tracedecay_application::GitHealthProjectionSourceV1) {
        let availability = match self.read_cached() {
            GitHealthProjectionAvailabilityV1::Ready { snapshot }
            | GitHealthProjectionAvailabilityV1::Refreshing { snapshot, .. }
            | GitHealthProjectionAvailabilityV1::Stale { snapshot, .. } => {
                GitHealthProjectionAvailabilityV1::Refreshing { snapshot, target }
            }
            GitHealthProjectionAvailabilityV1::Warming { .. }
            | GitHealthProjectionAvailabilityV1::Unavailable { .. } => {
                GitHealthProjectionAvailabilityV1::Warming {
                    target: Some(target),
                }
            }
        };
        self.publish(availability);
    }

    fn publish_failure(&self, error: &GitHealthProjectionError) {
        let reason = error.unavailable_reason();
        let availability = match self.read_cached() {
            GitHealthProjectionAvailabilityV1::Ready { snapshot }
            | GitHealthProjectionAvailabilityV1::Refreshing { snapshot, .. }
            | GitHealthProjectionAvailabilityV1::Stale { snapshot, .. } => {
                GitHealthProjectionAvailabilityV1::Stale { snapshot, reason }
            }
            GitHealthProjectionAvailabilityV1::Warming { .. }
            | GitHealthProjectionAvailabilityV1::Unavailable { .. } => {
                GitHealthProjectionAvailabilityV1::Unavailable { reason }
            }
        };
        self.publish(availability);
    }

    fn publish_unavailable(&self, reason: GitHealthProjectionUnavailableReasonV1) {
        self.publish(GitHealthProjectionAvailabilityV1::Unavailable { reason });
    }
}

fn now_epoch_secs() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))
        .and_then(|duration| {
            i64::try_from(duration.as_secs())
                .map_err(|_| "system clock exceeds supported Git health range".to_owned())
        })
}

#[cfg(test)]
#[path = "git_health_projection_tests.rs"]
mod tests;
