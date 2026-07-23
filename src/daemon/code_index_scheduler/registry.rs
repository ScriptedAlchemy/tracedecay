//! Daemon-owned registry of mounted per-worktree code-index schedulers.
//!
//! Owns the map of live worktree schedulers, their reconciliation worker tasks,
//! and the shared content-addressed byte pool. The registry is the async-facing
//! surface: hook-hint delivery, query-admission freshness, and lifecycle
//! (mount/shutdown). The synchronous per-worktree indexing logic lives on
//! [`CodeIndexWorktreeSchedulerV1`]; this module never runs it while holding the
//! registry map lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tracedecay_domain::CodeGenerationId;

use super::{
    CodeIndexBytePoolStatsV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    LatestCompleteCodeIndexV1, SharedCodeIndexBytePoolV1, sha256_hex,
};

pub(super) struct MountedCodeIndexWorktreeV1 {
    pub(super) scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    pub(super) task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub(in crate::daemon) struct CodeIndexSchedulerRegistryV1 {
    pub(super) max_worktrees: usize,
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    pub(super) mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
}

impl CodeIndexSchedulerRegistryV1 {
    pub fn new(max_worktrees: usize) -> Self {
        Self {
            max_worktrees,
            byte_pool: Arc::new(SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    pub fn open_worktree(
        &self,
        project_root: &Path,
        store_root: PathBuf,
    ) -> Result<CodeIndexWorktreeSchedulerV1, CodeIndexSchedulerErrorV1> {
        if self.max_worktrees == 0 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is zero".to_owned(),
            ));
        }
        CodeIndexWorktreeSchedulerV1::open(project_root, store_root, Arc::clone(&self.byte_pool))
    }

    pub fn byte_pool_stats(&self) -> CodeIndexBytePoolStatsV1 {
        self.byte_pool.stats()
    }

    pub async fn mount_worktree(
        &self,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        if mounted.contains_key(&project_root) {
            return Ok(false);
        }
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        let mut opened = self.open_worktree(
            &project_root,
            store_root.join(sha256_hex(project_root.to_string_lossy().as_bytes())),
        )?;
        if let Some(hook) = semantic_schedule {
            if let Some(latest) = opened.latest_complete() {
                let _ = hook(&latest.generation);
            }
            opened.set_semantic_schedule_hook(hook);
        }
        let scheduler = Arc::new(Mutex::new(opened));
        let (wake, shutting_down) = {
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(|_| panic!("code-index scheduler lock"));
            (
                Arc::clone(&scheduler.wake),
                Arc::clone(&scheduler.shutting_down),
            )
        };
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_wake = Arc::clone(&wake);
        let task = tokio::spawn(async move {
            loop {
                worker_wake.notified().await;
                if shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let scheduler = Arc::clone(&worker_scheduler);
                let result = tokio::task::spawn_blocking(move || {
                    scheduler
                        .lock()
                        .unwrap_or_else(|_| panic!("code-index scheduler lock"))
                        .reconcile_now()
                })
                .await;
                if result.is_err() || shutting_down.load(Ordering::Acquire) {
                    return;
                }
            }
        });
        mounted.insert(project_root, MountedCodeIndexWorktreeV1 { scheduler, task });
        wake.notify_one();
        Ok(true)
    }

    pub async fn notify_path(&self, project_root: &Path, path: PathBuf) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get(&project_root) else {
            return false;
        };
        worktree
            .scheduler
            .lock()
            .unwrap_or_else(|_| panic!("code-index scheduler lock"))
            .notify_path(path);
        true
    }

    /// Primary hint path: deliver the exact touched paths carried by a host
    /// after-file-edit hook into the mounted worktree's incremental queue.
    /// `rel_paths` are repository-relative; they are resolved against the
    /// project root. Returns `true` when a worktree was mounted to receive them.
    pub async fn notify_hook_paths(&self, project_root: &Path, rel_paths: &[String]) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get(&project_root) else {
            return false;
        };
        let absolute = rel_paths
            .iter()
            .map(|rel| project_root.join(rel))
            .collect::<Vec<_>>();
        worktree
            .scheduler
            .lock()
            .unwrap_or_else(|_| panic!("code-index scheduler lock"))
            .notify_hook_paths(absolute);
        true
    }

    pub async fn latest_generation_id(&self, project_root: &Path) -> Option<CodeGenerationId> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root)?;
        worktree
            .scheduler
            .lock()
            .ok()?
            .latest_complete()
            .map(|latest| latest.generation.manifest().generation_id.clone())
    }

    /// Query-admission entry point: run the freshness ladder (tier-1 git
    /// metadata, tier-2 bounded staleness, tier-3 identity re-resolution) before
    /// returning the latest complete generation, so external out-of-band changes
    /// are reconciled without any standing filesystem watcher.
    pub async fn latest_complete_fresh(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Clone the per-worktree handle under a short map lock, then drop the
        // registry guard *before* running freshness. The synchronous freshness
        // ladder (gix status + hashing + build_and_publish) must never run while
        // the registry map is locked, or one worktree's reconcile would
        // serialize every other worktree's queries and stall the executor.
        let scheduler = {
            let mounted = self.mounted.lock().await;
            Arc::clone(&mounted.get(&project_root)?.scheduler)
        };
        // Run the synchronous reconcile off the async executor. A freshness
        // reconcile failure is non-fatal for serving: fall back to the last
        // complete generation rather than denying the query.
        tokio::task::spawn_blocking(move || {
            let mut scheduler = scheduler
                .lock()
                .unwrap_or_else(|_| panic!("code-index scheduler lock"));
            let _ = scheduler.ensure_fresh_for_query();
            scheduler.latest_complete()
        })
        .await
        .ok()
        .flatten()
    }

    /// Unpinned serving resolution: run the freshness ladder over each mounted
    /// worktree and return the first latest complete generation. The daemon
    /// mounts one worktree per query context, so this resolves that worktree's
    /// freshest complete generation. This is the freshness-gated entry ordinary
    /// (unpinned) search uses when the caller pins no explicit generation, so
    /// out-of-band changes are reconciled at query admission with no watcher.
    ///
    /// Keys are cloned under a short map lock and each per-worktree freshness
    /// reconcile then runs through [`Self::latest_complete_fresh`], which drops
    /// the registry guard before its blocking work — one worktree's reconcile
    /// never serializes another worktree's query on the registry map.
    pub async fn latest_complete_fresh_any(&self) -> Option<LatestCompleteCodeIndexV1> {
        let roots = {
            let mounted = self.mounted.lock().await;
            mounted.keys().cloned().collect::<Vec<PathBuf>>()
        };
        for root in roots {
            if let Some(latest) = self.latest_complete_fresh(&root).await {
                return Some(latest);
            }
        }
        None
    }

    /// The per-worktree scheduler handle, cloned out of the registry map. Test
    /// support for proving that holding one worktree's scheduler lock does not
    /// block another worktree's freshness query on the registry map.
    #[cfg(test)]
    pub(super) async fn scheduler_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<Mutex<CodeIndexWorktreeSchedulerV1>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.scheduler))
    }

    pub async fn shutdown(&self) {
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
        for worktree in mounted.values() {
            let scheduler = worktree
                .scheduler
                .lock()
                .unwrap_or_else(|_| panic!("code-index scheduler lock"));
            scheduler.shutting_down.store(true, Ordering::Release);
            scheduler.wake.notify_one();
        }
        for (_, worktree) in mounted {
            let _ = worktree.task.await;
        }
    }
}
