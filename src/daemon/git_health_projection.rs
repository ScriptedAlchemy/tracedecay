//! Daemon lifecycle owner for background Git health projection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    GitHealthProjectionAvailabilityV1, GitHealthProjectionReadPortV1,
    GitHealthProjectionUnavailableReasonV1, ResolvedScope,
};

use crate::application::context::CancellationToken;
use crate::graph::git::{GitHealthProjectionError, GitHealthProjectionStoreV1};

const DEFAULT_COMMIT_BATCH_LIMIT: usize = 64;
const REFRESH_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(super) struct GitHealthProjectionRegistryV1 {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    owners: Mutex<HashMap<String, Arc<GitHealthProjectionOwnerV1>>>,
    max_owners: usize,
}

struct GitHealthProjectionOwnerV1 {
    repository_root: PathBuf,
    store_path: PathBuf,
    scope: ResolvedScope,
    availability: RwLock<GitHealthProjectionAvailabilityV1>,
    wake: tokio::sync::Notify,
    cancellation: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    commit_batch_limit: usize,
}

impl GitHealthProjectionRegistryV1 {
    pub(super) fn new(max_owners: usize) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                owners: Mutex::new(HashMap::new()),
                max_owners: max_owners.max(1),
            }),
        }
    }

    pub(super) fn mount(
        &self,
        repository_root: &Path,
        store_path: PathBuf,
        scope: ResolvedScope,
    ) -> bool {
        let key = scope.worktree_id.as_str().to_owned();
        let mut owners = match self.inner.owners.lock() {
            Ok(owners) => owners,
            Err(_) => return false,
        };
        if let Some(owner) = owners.get(&key) {
            owner.wake.notify_one();
            return owner.repository_root == repository_root
                && owner.store_path == store_path
                && owner.scope == scope;
        }
        if owners.len() >= self.inner.max_owners {
            return false;
        }
        let owner = Arc::new(GitHealthProjectionOwnerV1 {
            repository_root: repository_root.to_path_buf(),
            store_path,
            scope,
            availability: RwLock::new(GitHealthProjectionAvailabilityV1::Warming { target: None }),
            wake: tokio::sync::Notify::new(),
            cancellation: CancellationToken::new(),
            task: Mutex::new(None),
            commit_batch_limit: DEFAULT_COMMIT_BATCH_LIMIT,
        });
        owner.start();
        owners.insert(key, owner);
        true
    }

    pub(super) async fn shutdown(&self) {
        let owners: Vec<_> = match self.inner.owners.lock() {
            Ok(mut owners) => owners.drain().map(|(_, owner)| owner).collect(),
            Err(_) => Vec::new(),
        };
        for owner in &owners {
            owner.cancellation.cancel();
            owner.wake.notify_waiters();
        }
        for owner in owners {
            let task = owner.task.lock().ok().and_then(|mut task| task.take());
            if let Some(task) = task {
                let _ = task.await;
            }
        }
    }
}

impl GitHealthProjectionReadPortV1 for GitHealthProjectionRegistryV1 {
    fn read_projection(&self, scope: &ResolvedScope) -> GitHealthProjectionAvailabilityV1 {
        let owner = self
            .inner
            .owners
            .lock()
            .ok()
            .and_then(|owners| owners.get(scope.worktree_id.as_str()).cloned());
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
        owner.read_cached()
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

    async fn run(self: Arc<Self>) {
        let store_path = self.store_path.clone();
        let cancellation = self.cancellation.clone();
        let store = match tokio::task::spawn_blocking(move || {
            GitHealthProjectionStoreV1::open(&store_path, &cancellation)
        })
        .await
        {
            Ok(Ok(store)) => Arc::new(store),
            Ok(Err(GitHealthProjectionError::Cancelled)) => return,
            Ok(Err(error)) => {
                self.publish_failure(&error);
                return;
            }
            Err(_) => {
                self.publish_unavailable(
                    GitHealthProjectionUnavailableReasonV1::ProjectionStoreUnavailable,
                );
                return;
            }
        };
        self.publish(store.read(&self.scope));

        loop {
            if self.cancellation.is_cancelled() {
                return;
            }
            let store_for_batch = Arc::clone(&store);
            let repository_root = self.repository_root.clone();
            let scope = self.scope.clone();
            let cancellation = self.cancellation.clone();
            let batch_limit = self.commit_batch_limit;
            let outcome = tokio::task::spawn_blocking(move || {
                let now = now_epoch_secs().map_err(GitHealthProjectionError::Git)?;
                store_for_batch.advance(&repository_root, &scope, now, batch_limit, &cancellation)
            })
            .await;
            if self.cancellation.is_cancelled() {
                return;
            }
            match outcome {
                Ok(Ok(progress)) => {
                    self.publish(store.read(&self.scope));
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
