use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tracedecay_runtime_core::git_discovery::GitRepositoryIdentity;

use super::identity::{IdentityDiscoveryDisposition, identity_discovery_disposition};
use super::ownership::{join_retired_repository_state, retire_missing_repository_owners};
use super::state::{WatchState, WorktreeRegistration};
use super::{
    GitWatcher, GitWatcherAdmission, MAX_WORKTREES_PER_REPOSITORY, RESTART_BACKOFF_MAX,
    log_daemon_event, resolve_watch_identity, supervise_repository,
};
use crate::ports::GitWatchSyncConfigV1 as SyncConfig;

impl GitWatcher {
    /// Lazily starts watching `project_root` if not already watched and under
    /// the repository cap. Linked worktrees register distinct scheduler roots
    /// on one common-directory watcher.
    #[cfg(test)]
    pub async fn ensure_watching(&self, project_root: &Path) -> GitWatcherAdmission {
        let config = self.inner.config.clone();
        self.ensure_watching_with_config(project_root, &config)
            .await
    }

    #[hotpath::measure(label = "daemon.git.watch.ensure", future = true)]
    pub async fn ensure_watching_with_config(
        &self,
        project_root: &Path,
        config: &SyncConfig,
    ) -> GitWatcherAdmission {
        let admission = self.ensure_watching_admission(project_root, config).await;
        record_admission_outcome(admission);
        admission
    }

    /// Admission body behind [`Self::ensure_watching_with_config`], separated
    /// so every typed outcome — refusals included — crosses one counter choke
    /// point instead of instrumenting each early return.
    async fn ensure_watching_admission(
        &self,
        project_root: &Path,
        config: &SyncConfig,
    ) -> GitWatcherAdmission {
        if !self.inner.enabled || !config.auto_watch {
            return GitWatcherAdmission::Disabled;
        }
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return GitWatcherAdmission::ShuttingDown;
        }
        if crate::config::is_ambient_project_root(project_root) {
            log_daemon_event(
                "git_watch_skipped",
                &[
                    ("project", project_root.display().to_string()),
                    ("reason", "ambient_root".to_string()),
                ],
            );
            return GitWatcherAdmission::NotRepository;
        }
        let resolution =
            resolve_watch_identity(project_root.to_path_buf(), self.inner.cancellation.clone())
                .await;
        let identity = match identity_discovery_disposition(resolution) {
            IdentityDiscoveryDisposition::Watch(identity) => identity,
            IdentityDiscoveryDisposition::ShutDown => return GitWatcherAdmission::ShuttingDown,
            IdentityDiscoveryDisposition::NotRepository => {
                return GitWatcherAdmission::NotRepository;
            }
            IdentityDiscoveryDisposition::Retry => {
                // A bounded git timeout is uncertainty, not absence: arm the
                // daemon-owned backoff retry instead of leaving the repository
                // unwatched until the next handshake.
                self.arm_identity_discovery_retry(project_root.to_path_buf(), config.clone());
                return GitWatcherAdmission::IdentityUnavailable;
            }
        };
        let admission = self.admit_resolved(identity.clone(), config).await;
        match admission {
            GitWatcherAdmission::Capacity => {
                // A capacity refusal must not strand the repository without a
                // freshness floor: the bounded overflow roster keeps it covered
                // through the scheduler ingress until a slot frees.
                self.cover_capacity_overflow(identity, config.clone());
            }
            GitWatcherAdmission::Ready => {
                self.inner
                    .overflow
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&identity.worktree_root);
            }
            _ => {}
        }
        admission
    }

    /// Admission for a repository whose identity is already resolved: pure
    /// in-memory registration against the live capacity caps, no git IO.
    #[hotpath::measure(label = "daemon.git.watch.admit", future = true)]
    pub async fn admit_resolved(
        &self,
        identity: GitRepositoryIdentity,
        config: &SyncConfig,
    ) -> GitWatcherAdmission {
        let GitRepositoryIdentity {
            worktree_root: canonical_root,
            common_dir,
            git_dir,
        } = identity;
        if git_dir != common_dir && !config.watch_linked_worktrees {
            log_daemon_event(
                "git_watch_skipped",
                &[
                    ("project", canonical_root.display().to_string()),
                    ("reason", "linked_worktree_disabled".to_string()),
                ],
            );
            return GitWatcherAdmission::LinkedWorktreeDisabled;
        }

        loop {
            retire_missing_repository_owners(&self.inner).await;
            let mut projects = self.inner.projects.lock().await;
            let admission = self
                .inner
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.inner.shutting_down.load(Ordering::Acquire) {
                return GitWatcherAdmission::ShuttingDown;
            }
            #[cfg(test)]
            self.inner.repository_publication_probe.block_if_armed();
            if let Some(state) = projects.get(&common_dir).cloned() {
                match state.register_worktree_with_config(
                    canonical_root.clone(),
                    git_dir.clone(),
                    config.clone(),
                    MAX_WORKTREES_PER_REPOSITORY,
                ) {
                    WorktreeRegistration::Ready => {
                        #[cfg(test)]
                        self.inner.lifecycle_receipts.record_registration();
                        return GitWatcherAdmission::Ready;
                    }
                    WorktreeRegistration::Capacity => return GitWatcherAdmission::Capacity,
                    WorktreeRegistration::Retired => {
                        projects.remove(&common_dir);
                        hotpath::gauge!("daemon.git.watch.repositories.watched")
                            .set(projects.len());
                        drop(admission);
                        join_retired_repository_state(&state).await;
                        drop(projects);
                        continue;
                    }
                }
            }
            if projects.len() >= config.watch_max_projects {
                // Capacity is repository-scoped so linked worktrees never consume
                // additional OS-watcher slots.
                return GitWatcherAdmission::Capacity;
            }

            let state = Arc::new(WatchState::new_with_config(
                common_dir.clone(),
                canonical_root.clone(),
                git_dir.clone(),
                self.inner.maintenance.clone(),
                config.clone(),
            ));
            let inner = Arc::clone(&self.inner);
            let handle = tokio::spawn(supervise_repository(inner, Arc::clone(&state)));
            state.retain_task(handle);
            projects.insert(common_dir.clone(), Arc::clone(&state));
            hotpath::gauge!("daemon.git.watch.repositories.watched").set(projects.len());
            #[cfg(test)]
            self.inner.lifecycle_receipts.record_repository();
            log_daemon_event(
                "git_watch_started",
                &[("git_common_dir", common_dir.display().to_string())],
            );
            return GitWatcherAdmission::Ready;
        }
    }

    /// Arms a single-flight bounded retry owner for a root whose repository
    /// identity discovery timed out. The owner re-attempts admission with
    /// capped exponential backoff until discovery is definitive (watched or
    /// not a repository) or the watcher shuts down.
    fn arm_identity_discovery_retry(&self, project_root: PathBuf, config: SyncConfig) {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let mut retries = self
            .inner
            .identity_retries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        retries.retain(|_, owner| !owner.is_finished());
        if retries.contains_key(&project_root) {
            return;
        }
        let watcher = self.clone();
        let root = project_root.clone();
        let owner = tokio::spawn(hotpath::future!(
            async move {
                watcher.retry_identity_discovery(root, config).await;
            },
            label = "daemon.git.watch.identity_retry"
        ));
        retries.insert(project_root, owner);
    }

    async fn retry_identity_discovery(&self, project_root: PathBuf, config: SyncConfig) {
        let _retry_owner = IdentityRetryGaugeGuard::enter();
        let mut backoff = Duration::from_millis(500);
        loop {
            log_daemon_event(
                "git_watch_discovery_retry",
                &[
                    ("project", project_root.display().to_string()),
                    ("backoff_ms", backoff.as_millis().to_string()),
                ],
            );
            tokio::select! {
                biased;
                () = self.inner.cancellation.cancelled() => break,
                () = tokio::time::sleep(backoff) => {}
            }
            hotpath::gauge!("daemon.git.watch.identity_retry.attempts_total").inc(1_u64);
            match self
                .ensure_watching_with_config(&project_root, &config)
                .await
            {
                GitWatcherAdmission::IdentityUnavailable => {
                    backoff = (backoff * 2).min(RESTART_BACKOFF_MAX);
                }
                // Every other admission outcome is definitive for this owner:
                // watched, rejected, at capacity, disabled, or shutting down.
                _ => break,
            }
        }
        let mut retries = self
            .inner
            .identity_retries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        retries.remove(&project_root);
    }
}

/// Bounded typed admission counters. Refusals are first-class evidence: a
/// profile must separate capacity pressure from identity-discovery churn and
/// shutdown races without recording repository paths.
fn record_admission_outcome(admission: GitWatcherAdmission) {
    match admission {
        GitWatcherAdmission::Ready => {
            hotpath::gauge!("daemon.git.watch.admission.ready_total").inc(1_u64);
        }
        GitWatcherAdmission::Disabled => {
            hotpath::gauge!("daemon.git.watch.admission.disabled_total").inc(1_u64);
        }
        GitWatcherAdmission::LinkedWorktreeDisabled => {
            hotpath::gauge!("daemon.git.watch.admission.linked_worktree_disabled_total").inc(1_u64);
        }
        GitWatcherAdmission::ShuttingDown => {
            hotpath::gauge!("daemon.git.watch.admission.shutting_down_total").inc(1_u64);
        }
        GitWatcherAdmission::Capacity => {
            hotpath::gauge!("daemon.git.watch.admission.capacity_total").inc(1_u64);
        }
        GitWatcherAdmission::NotRepository => {
            hotpath::gauge!("daemon.git.watch.admission.not_repository_total").inc(1_u64);
        }
        GitWatcherAdmission::IdentityUnavailable => {
            hotpath::gauge!("daemon.git.watch.admission.identity_unavailable_total").inc(1_u64);
        }
    }
}

/// RAII gauge for live single-flight identity-retry owners so cancellation,
/// panic, or definitive admission can never leak the count.
struct IdentityRetryGaugeGuard;

impl IdentityRetryGaugeGuard {
    fn enter() -> Self {
        hotpath::gauge!("daemon.git.watch.identity_retry.active").inc(1_u64);
        Self
    }
}

impl Drop for IdentityRetryGaugeGuard {
    fn drop(&mut self) {
        hotpath::gauge!("daemon.git.watch.identity_retry.active").dec(1_u64);
    }
}
