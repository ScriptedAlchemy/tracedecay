//! Daemon lifecycle owner for bounded Git health projections.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionBindingV1,
    GitHealthProjectionChurnPageV1, GitHealthProjectionReadPortV1,
    GitHealthProjectionReadServiceV1, GitHealthProjectionUnavailableReasonV1,
};
use tracedecay_graph_db::GraphDb;

use crate::application::context::CancellationToken;
use crate::graph::git::{GitHealthProjectionError, GitHealthProjectionStoreV1};

const DEFAULT_COMMIT_BATCH_LIMIT: usize = 64;
const REFRESH_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub(super) enum GitHealthProjectionMountErrorV1 {
    #[error("Git health projection mount was cancelled")]
    Cancelled,
    #[error("Git health projection owner capacity is exhausted")]
    Capacity,
    #[error("Git health projection store could not open: {0}")]
    Store(String),
    #[error("Git health projection task could not start: {0}")]
    Task(String),
}

#[derive(Clone)]
pub(super) struct GitHealthProjectionRegistryV1 {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    state: Mutex<RegistryState>,
    mount_gate: tokio::sync::Mutex<()>,
    max_owners: usize,
}

#[derive(Default)]
struct RegistryState {
    owners: HashMap<String, Arc<GitHealthProjectionOwnerV1>>,
    databases: HashMap<PathBuf, SharedDatabase>,
    retirements: Vec<tokio::task::JoinHandle<()>>,
}

struct SharedDatabase {
    database: GraphDb,
    owners: usize,
}

struct GitHealthProjectionOwnerV1 {
    repository_root: PathBuf,
    store_path: PathBuf,
    binding: GitHealthProjectionBindingV1,
    store: Arc<GitHealthProjectionStoreV1>,
    availability: RwLock<GitHealthProjectionAvailabilityV1>,
    wake: tokio::sync::Notify,
    cancellation: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    leases: AtomicUsize,
    database_registered: std::sync::atomic::AtomicBool,
}

pub(super) struct GitHealthProjectionLeaseV1 {
    registry: GitHealthProjectionRegistryV1,
    owner: Arc<GitHealthProjectionOwnerV1>,
}

impl GitHealthProjectionRegistryV1 {
    pub(super) fn new(max_owners: usize) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState::default()),
                mount_gate: tokio::sync::Mutex::new(()),
                max_owners: max_owners.max(1),
            }),
        }
    }

    pub(super) async fn mount(
        &self,
        repository_root: &Path,
        store_path: PathBuf,
        binding: GitHealthProjectionBindingV1,
        project_open_cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionLeaseV1, GitHealthProjectionMountErrorV1> {
        self.mount_with_limit(
            repository_root,
            store_path,
            binding,
            self.inner.max_owners,
            project_open_cancellation,
        )
        .await
    }

    /// Reserves a bounded candidate owner before the project-server registry
    /// has had the opportunity to evict an idle server and release its lease.
    pub(super) async fn mount_candidate(
        &self,
        repository_root: &Path,
        store_path: PathBuf,
        binding: GitHealthProjectionBindingV1,
        project_open_cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionLeaseV1, GitHealthProjectionMountErrorV1> {
        self.mount_with_limit(
            repository_root,
            store_path,
            binding,
            self.inner.max_owners.saturating_mul(2),
            project_open_cancellation,
        )
        .await
    }

    async fn mount_with_limit(
        &self,
        repository_root: &Path,
        store_path: PathBuf,
        binding: GitHealthProjectionBindingV1,
        owner_limit: usize,
        project_open_cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionLeaseV1, GitHealthProjectionMountErrorV1> {
        let _gate = tokio::select! {
            gate = self.inner.mount_gate.lock() => gate,
            () = project_open_cancellation.cancelled() => {
                return Err(GitHealthProjectionMountErrorV1::Cancelled);
            }
        };
        self.await_retirements().await;
        cancellation_checkpoint(project_open_cancellation)?;

        let key = binding.scope.worktree_id.as_str().to_owned();
        let prior = {
            let mut state = self.lock_state()?;
            if let Some(owner) = state.owners.get(&key) {
                if owner.repository_root == repository_root
                    && owner.store_path == store_path
                    && owner.binding == binding
                {
                    owner.leases.fetch_add(1, Ordering::AcqRel);
                    owner.wake.notify_one();
                    return Ok(GitHealthProjectionLeaseV1 {
                        registry: self.clone(),
                        owner: Arc::clone(owner),
                    });
                }
            }
            state.owners.remove(&key)
        };
        let retired_binding = prior
            .as_ref()
            .filter(|owner| owner.store_path == store_path)
            .map(|owner| owner.binding.clone());
        if let Some(prior) = prior {
            self.retire(prior);
            self.await_retirements().await;
        }
        cancellation_checkpoint(project_open_cancellation)?;
        if self.lock_state()?.owners.len() >= owner_limit {
            return Err(GitHealthProjectionMountErrorV1::Capacity);
        }

        let cached_database = self
            .lock_state()?
            .databases
            .get(&store_path)
            .map(|entry| entry.database.clone());
        let (store, opened_new) = if let Some(database) = cached_database {
            (GitHealthProjectionStoreV1::from_database(database), false)
        } else {
            let open_path = store_path.clone();
            let open_cancellation = project_open_cancellation.clone();
            let store = tokio::task::spawn_blocking(move || {
                GitHealthProjectionStoreV1::open(&open_path, &open_cancellation)
            })
            .await
            .map_err(|error| GitHealthProjectionMountErrorV1::Task(error.to_string()))?
            .map_err(|error| match error {
                GitHealthProjectionError::Cancelled => GitHealthProjectionMountErrorV1::Cancelled,
                other => GitHealthProjectionMountErrorV1::Store(other.to_string()),
            })?;
            (store, true)
        };
        if project_open_cancellation.is_cancelled() {
            if opened_new {
                let database = store.database();
                let _ = tokio::task::spawn_blocking(move || database.close()).await;
            }
            return Err(GitHealthProjectionMountErrorV1::Cancelled);
        }
        if let Some(retired_binding) = retired_binding {
            let reset_store = store.clone();
            let reset_cancellation = project_open_cancellation.clone();
            let reset = tokio::task::spawn_blocking(move || {
                reset_store.reset_binding(&retired_binding, &reset_cancellation)
            })
            .await
            .map_err(|error| GitHealthProjectionMountErrorV1::Task(error.to_string()))
            .and_then(|result| {
                result.map_err(|error| match error {
                    GitHealthProjectionError::Cancelled => {
                        GitHealthProjectionMountErrorV1::Cancelled
                    }
                    other => GitHealthProjectionMountErrorV1::Store(other.to_string()),
                })
            });
            if let Err(error) = reset {
                if opened_new {
                    let database = store.database();
                    let _ = tokio::task::spawn_blocking(move || database.close()).await;
                }
                return Err(error);
            }
        }

        let owner = Arc::new(GitHealthProjectionOwnerV1 {
            repository_root: repository_root.to_path_buf(),
            store_path,
            binding,
            store: Arc::new(store),
            availability: RwLock::new(GitHealthProjectionAvailabilityV1::Warming { target: None }),
            wake: tokio::sync::Notify::new(),
            cancellation: CancellationToken::new(),
            task: Mutex::new(None),
            leases: AtomicUsize::new(1),
            database_registered: std::sync::atomic::AtomicBool::new(true),
        });
        {
            let mut state = self.lock_state()?;
            let shared = state
                .databases
                .entry(owner.store_path.clone())
                .or_insert_with(|| SharedDatabase {
                    database: owner.store.database(),
                    owners: 0,
                });
            shared.owners = shared.owners.checked_add(1).ok_or_else(|| {
                GitHealthProjectionMountErrorV1::Store(
                    "shared graph database owner count overflowed".to_owned(),
                )
            })?;
            state.owners.insert(key.clone(), Arc::clone(&owner));
        }
        if let Err(error) = owner.start() {
            self.remove_if_mounted(&key, &owner);
            self.retire(Arc::clone(&owner));
            self.await_retirements().await;
            return Err(error);
        }
        if project_open_cancellation.is_cancelled() {
            self.remove_if_mounted(&key, &owner);
            self.retire(Arc::clone(&owner));
            self.await_retirements().await;
            return Err(GitHealthProjectionMountErrorV1::Cancelled);
        }
        Ok(GitHealthProjectionLeaseV1 {
            registry: self.clone(),
            owner,
        })
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, RegistryState>, GitHealthProjectionMountErrorV1> {
        self.inner.state.lock().map_err(|_| {
            GitHealthProjectionMountErrorV1::Store("owner lock is poisoned".to_owned())
        })
    }

    fn retire(&self, owner: Arc<GitHealthProjectionOwnerV1>) {
        owner.cancellation.cancel();
        owner.wake.notify_waiters();
        let task = owner.task.lock().ok().and_then(|mut task| task.take());
        let database = if owner.database_registered.swap(false, Ordering::AcqRel) {
            self.inner.state.lock().ok().and_then(|mut state| {
                let shared = state.databases.get_mut(&owner.store_path)?;
                if shared.owners <= 1 {
                    if shared.owners == 0 {
                        tracing::error!(
                            event = "git_health_projection_retire",
                            store = %owner.store_path.display(),
                            reason = "shared graph database owner count underflow"
                        );
                    }
                    state
                        .databases
                        .remove(&owner.store_path)
                        .map(|shared| shared.database)
                } else {
                    shared.owners -= 1;
                    None
                }
            })
        } else {
            None
        };
        if let Ok(mut state) = self.inner.state.lock() {
            state.retirements.push(tokio::spawn(async move {
                if let Some(task) = task {
                    let _ = task.await;
                }
                if let Some(database) = database {
                    let _ = tokio::task::spawn_blocking(move || database.close()).await;
                }
                drop(owner);
            }));
        }
    }

    fn remove_if_mounted(&self, key: &str, owner: &Arc<GitHealthProjectionOwnerV1>) {
        if let Ok(mut state) = self.inner.state.lock()
            && state
                .owners
                .get(key)
                .is_some_and(|mounted| Arc::ptr_eq(mounted, owner))
        {
            state.owners.remove(key);
        }
    }

    async fn await_retirements(&self) {
        let tasks = self
            .inner
            .state
            .lock()
            .map(|mut state| std::mem::take(&mut state.retirements))
            .unwrap_or_default();
        for task in tasks {
            let _ = task.await;
        }
    }

    fn release(&self, owner: &Arc<GitHealthProjectionOwnerV1>) {
        if owner.leases.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let removed = self.inner.state.lock().ok().and_then(|mut state| {
            let key = owner.binding.scope.worktree_id.as_str();
            state
                .owners
                .get(key)
                .filter(|mounted| Arc::ptr_eq(mounted, owner))?;
            state.owners.remove(key)
        });
        if let Some(removed) = removed {
            self.retire(removed);
        }
    }

    pub(super) async fn shutdown(&self) {
        let owners = self
            .inner
            .state
            .lock()
            .map(|mut state| {
                state
                    .owners
                    .drain()
                    .map(|(_, owner)| owner)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for owner in owners {
            self.retire(owner);
        }
        self.await_retirements().await;
    }

    #[cfg(test)]
    fn owner_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.owners.len())
            .unwrap_or_default()
    }
}

impl Drop for GitHealthProjectionLeaseV1 {
    fn drop(&mut self) {
        self.registry.release(&self.owner);
    }
}

impl GitHealthProjectionReadPortV1 for GitHealthProjectionLeaseV1 {
    fn read_projection(
        &self,
        binding: &GitHealthProjectionBindingV1,
    ) -> GitHealthProjectionAvailabilityV1 {
        if binding != &self.owner.binding {
            return GitHealthProjectionAvailabilityV1::Unavailable {
                reason: GitHealthProjectionUnavailableReasonV1::ScopeDrift,
            };
        }
        let mounted = self
            .registry
            .inner
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .owners
                    .get(binding.scope.worktree_id.as_str())
                    .map(|owner| Arc::ptr_eq(owner, &self.owner))
            })
            .unwrap_or(false);
        if !mounted {
            return GitHealthProjectionAvailabilityV1::Unavailable {
                reason: GitHealthProjectionUnavailableReasonV1::NotMounted,
            };
        }
        self.owner.wake.notify_one();
        self.owner.read_cached()
    }

    fn read_churn_page(
        &self,
        binding: &GitHealthProjectionBindingV1,
        after_cursor: Option<&str>,
        limit: usize,
    ) -> Result<GitHealthProjectionChurnPageV1, GitHealthProjectionUnavailableReasonV1> {
        match self.read_projection(binding) {
            GitHealthProjectionAvailabilityV1::Ready { snapshot }
            | GitHealthProjectionAvailabilityV1::Refreshing { snapshot, .. } => self
                .owner
                .store
                .read_churn_page(binding, &snapshot, after_cursor, limit)
                .map_err(|error| error.unavailable_reason()),
            GitHealthProjectionAvailabilityV1::Stale { reason, .. }
            | GitHealthProjectionAvailabilityV1::Unavailable { reason } => Err(reason),
            GitHealthProjectionAvailabilityV1::Warming { .. } => {
                Err(GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable)
            }
        }
    }
}

impl GitHealthProjectionOwnerV1 {
    fn start(self: &Arc<Self>) -> Result<(), GitHealthProjectionMountErrorV1> {
        let owner = Arc::clone(self);
        let task = tokio::spawn(async move {
            owner.run().await;
        });
        let mut slot = self.task.lock().map_err(|_| {
            GitHealthProjectionMountErrorV1::Task("task lock is poisoned".to_owned())
        })?;
        *slot = Some(task);
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        self.publish(self.store.read(&self.binding));
        loop {
            if self.cancellation.is_cancelled() {
                return;
            }
            let store = Arc::clone(&self.store);
            let root = self.repository_root.clone();
            let binding = self.binding.clone();
            let cancellation = self.cancellation.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let now = now_epoch_secs().map_err(GitHealthProjectionError::Git)?;
                store.advance(
                    &root,
                    &binding,
                    now,
                    DEFAULT_COMMIT_BATCH_LIMIT,
                    &cancellation,
                )
            })
            .await;
            if self.cancellation.is_cancelled() {
                return;
            }
            match outcome {
                Ok(Ok(progress)) => {
                    self.publish_progress(&progress);
                    if progress.complete {
                        tokio::select! {
                            () = self.cancellation.cancelled() => return,
                            () = self.wake.notified() => {}
                            () = tokio::time::sleep(REFRESH_POLL_INTERVAL) => {}
                        }
                    } else {
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

    fn publish_progress(&self, progress: &crate::graph::git::GitHealthProjectionProgressV1) {
        if progress.complete {
            self.publish(self.store.read(&self.binding));
            return;
        }
        let availability = match self.read_cached() {
            GitHealthProjectionAvailabilityV1::Ready { snapshot }
            | GitHealthProjectionAvailabilityV1::Refreshing { snapshot, .. }
            | GitHealthProjectionAvailabilityV1::Stale { snapshot, .. } => {
                GitHealthProjectionAvailabilityV1::Refreshing {
                    snapshot,
                    target: progress.target.clone(),
                }
            }
            GitHealthProjectionAvailabilityV1::Warming { .. }
            | GitHealthProjectionAvailabilityV1::Unavailable { .. } => {
                self.store.read(&self.binding)
            }
        };
        self.publish(availability);
    }

    fn publish_unavailable(&self, reason: GitHealthProjectionUnavailableReasonV1) {
        self.publish(GitHealthProjectionAvailabilityV1::Unavailable { reason });
    }
}

fn cancellation_checkpoint(
    cancellation: &CancellationToken,
) -> Result<(), GitHealthProjectionMountErrorV1> {
    if cancellation.is_cancelled() {
        Err(GitHealthProjectionMountErrorV1::Cancelled)
    } else {
        Ok(())
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

pub(super) fn reconciling_database_owner(
    base: crate::mcp::DatabaseOwnerReconciler,
    projections: GitHealthProjectionRegistryV1,
    reader: GitHealthProjectionReadServiceV1,
    prototype: GitHealthProjectionBindingV1,
    route_registered: Arc<AtomicBool>,
) -> crate::mcp::DatabaseOwnerReconciler {
    Arc::new(move |fresh| {
        let base = Arc::clone(&base);
        let projections = projections.clone();
        let reader = reader.clone();
        let prototype = prototype.clone();
        let route_registered = Arc::clone(&route_registered);
        Box::pin(async move {
            base(Arc::clone(&fresh)).await;
            if !route_registered.load(Ordering::Acquire) {
                return;
            }
            let scope = match crate::daemon::project_open_owners::resolved_scope_for_project(
                fresh.project_root(),
                &prototype.scope.project_id,
            ) {
                Ok(scope) => scope,
                Err(error) => {
                    tracing::error!(
                        error = ?error,
                        "Git health projection could not resolve its branch-reopen scope"
                    );
                    return;
                }
            };
            let binding = match GitHealthProjectionBindingV1::new(
                scope,
                prototype.profile_id,
                prototype.store_id,
            ) {
                Ok(binding) => binding,
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "Git health projection could not bind its branch-reopen authority"
                    );
                    return;
                }
            };
            if reader.binding().is_ok_and(|current| current == binding) {
                return;
            }
            let cancellation = CancellationToken::new();
            let lease = match projections
                .mount(
                    fresh.project_root(),
                    fresh.store_layout().data_root.join("project-graph.grafeo"),
                    binding.clone(),
                    &cancellation,
                )
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "Git health projection could not remount after branch reopen"
                    );
                    return;
                }
            };
            if !route_registered.load(Ordering::Acquire) {
                return;
            }
            let port: Arc<dyn GitHealthProjectionReadPortV1> = Arc::new(lease);
            if let Err(error) = reader.rebind(binding, port) {
                tracing::error!(
                    error = %error,
                    "Git health projection could not publish its branch-reopen binding"
                );
            }
        })
    })
}

#[cfg(test)]
#[path = "git_health_projection_tests.rs"]
mod tests;
