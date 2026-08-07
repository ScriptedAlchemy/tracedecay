use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_runtime_core::git_discovery::GitRepositoryIdentity;

use super::ownership::{join_retired_repository_state, retire_missing_repository_owners};
use super::state::{WatchState, WorktreeRegistration};
use super::{
    GitWatcher, GitWatcherAdmission, MAX_WORKTREES_PER_REPOSITORY, WatchIdentityResolution,
    log_daemon_event, resolve_watch_identity, supervise_repository,
};
use crate::config::SyncConfig;

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

    pub(in crate::daemon) async fn ensure_watching_with_config(
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
        let identity = match resolve_watch_identity(
            project_root.to_path_buf(),
            self.inner.cancellation.clone(),
        )
        .await
        {
            WatchIdentityResolution::Ready(identity) => identity,
            WatchIdentityResolution::Cancelled => return GitWatcherAdmission::ShuttingDown,
            WatchIdentityResolution::NotRepository => {
                return GitWatcherAdmission::NotRepository;
            }
            WatchIdentityResolution::Unknown => {
                return GitWatcherAdmission::IdentityUnavailable;
            }
        };
        let GitRepositoryIdentity {
            worktree_root: canonical_root,
            common_dir,
            git_dir,
        } = identity;

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
            #[cfg(test)]
            self.inner.lifecycle_receipts.record_repository();
            log_daemon_event(
                "git_watch_started",
                &[("git_common_dir", common_dir.display().to_string())],
            );
            return GitWatcherAdmission::Ready;
        }
    }
}
