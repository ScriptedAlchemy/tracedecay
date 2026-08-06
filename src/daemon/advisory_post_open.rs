//! Retained daemon-generation owner for optional advisory provider mount.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracedecay_domain::{CommitId, ManifestDigest, RepositoryId, WorktreeId};
use tracedecay_runtime_core::cancellation::CancellationToken;

use crate::application::advisory::GitHubReviewProviderIdentityV1;
use crate::application::advisory::github_runtime::GitHubExactCommitPullRequestV1;
use crate::errors::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectOpenAdvisoryWorkKeyV1 {
    pub(super) repository_id: RepositoryId,
    pub(super) worktree_id: WorktreeId,
    pub(super) head_commit_id: CommitId,
    pub(super) configuration_digest: ManifestDigest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectOpenAdvisoryPostOpenStatusV1 {
    Warming,
    Ready,
    Unavailable,
}

struct ProjectOpenAdvisoryPostOpenEntryV1 {
    key: ProjectOpenAdvisoryWorkKeyV1,
    status: ProjectOpenAdvisoryPostOpenStatusV1,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHubProviderDiscoveryCacheKeyV1 {
    pub(super) repository_id: RepositoryId,
    pub(super) canonical_common_dir: PathBuf,
    pub(super) head_commit_id: CommitId,
    pub(super) remote_identity: String,
}

#[derive(Clone)]
pub(super) struct CachedGitHubProviderResolutionV1 {
    pub(super) pull: GitHubExactCommitPullRequestV1,
    pub(super) identity: GitHubReviewProviderIdentityV1,
}

#[derive(Default)]
struct ProjectOpenAdvisoryPostOpenStateV1 {
    closed: bool,
    entries: BTreeMap<PathBuf, ProjectOpenAdvisoryPostOpenEntryV1>,
    provider_cache: VecDeque<(
        GitHubProviderDiscoveryCacheKeyV1,
        CachedGitHubProviderResolutionV1,
    )>,
}

#[derive(Clone, Default)]
pub(super) struct ProjectOpenAdvisoryPostOpenRegistryV1 {
    state: Arc<Mutex<ProjectOpenAdvisoryPostOpenStateV1>>,
}

impl ProjectOpenAdvisoryPostOpenRegistryV1 {
    const PROVIDER_CACHE_CAPACITY: usize = 32;

    pub(super) fn enqueue<F, Fut>(
        &self,
        project_root: PathBuf,
        key: ProjectOpenAdvisoryWorkKeyV1,
        work: F,
    ) -> ProjectOpenAdvisoryPostOpenStatusV1
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return ProjectOpenAdvisoryPostOpenStatusV1::Unavailable;
        }
        if let Some(existing) = state.entries.get(&project_root)
            && existing.key == key
            && existing.status != ProjectOpenAdvisoryPostOpenStatusV1::Unavailable
        {
            return existing.status;
        }
        let predecessor = state.entries.remove(&project_root).map(|existing| {
            existing.cancellation.cancel();
            existing.task
        });

        let cancellation = CancellationToken::new();
        let work_cancellation = cancellation.clone();
        let registry = self.clone();
        let status_root = project_root.clone();
        let status_key = key.clone();
        let (start, admitted) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            if admitted.await.is_err() {
                return;
            }
            if let Some(predecessor) = predecessor
                && let Err(error) = predecessor.await
                && !error.is_cancelled()
            {
                tracing::warn!(
                    event = "project_open_advisory_predecessor_join_failed",
                    project = %status_root.display(),
                    error = %error,
                );
            }
            let outcome = work(work_cancellation.clone()).await;
            let status = if outcome.is_ok() && !work_cancellation.is_cancelled() {
                ProjectOpenAdvisoryPostOpenStatusV1::Ready
            } else {
                ProjectOpenAdvisoryPostOpenStatusV1::Unavailable
            };
            registry.update_status(&status_root, &status_key, status);
            if let Err(error) = outcome {
                tracing::warn!(
                    event = "project_open_owner_phase",
                    project = %status_root.display(),
                    phase = "advisory_owner_deferred_failed",
                    error = %error,
                );
            }
        });
        state.entries.insert(
            project_root.clone(),
            ProjectOpenAdvisoryPostOpenEntryV1 {
                key: key.clone(),
                status: ProjectOpenAdvisoryPostOpenStatusV1::Warming,
                cancellation,
                task,
            },
        );
        drop(state);
        if start.send(()).is_err() {
            self.update_status(
                &project_root,
                &key,
                ProjectOpenAdvisoryPostOpenStatusV1::Unavailable,
            );
            return ProjectOpenAdvisoryPostOpenStatusV1::Unavailable;
        }
        ProjectOpenAdvisoryPostOpenStatusV1::Warming
    }

    fn update_status(
        &self,
        project_root: &Path,
        key: &ProjectOpenAdvisoryWorkKeyV1,
        status: ProjectOpenAdvisoryPostOpenStatusV1,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = state.entries.get_mut(project_root)
            && entry.key == *key
        {
            entry.status = status;
        }
    }

    #[cfg(test)]
    pub(super) fn status(
        &self,
        project_root: &Path,
    ) -> Option<ProjectOpenAdvisoryPostOpenStatusV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(project_root)
            .map(|entry| entry.status)
    }

    pub(super) fn cached_provider(
        &self,
        key: &GitHubProviderDiscoveryCacheKeyV1,
    ) -> Option<CachedGitHubProviderResolutionV1> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = state
            .provider_cache
            .iter()
            .position(|(cached_key, _)| cached_key == key)?;
        let cached = state.provider_cache.remove(index)?;
        let resolution = cached.1.clone();
        state.provider_cache.push_front(cached);
        Some(resolution)
    }

    pub(super) fn cache_provider(
        &self,
        key: GitHubProviderDiscoveryCacheKeyV1,
        resolution: CachedGitHubProviderResolutionV1,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .provider_cache
            .retain(|(cached_key, _)| cached_key != &key);
        state.provider_cache.push_front((key, resolution));
        state.provider_cache.truncate(Self::PROVIDER_CACHE_CAPACITY);
    }

    pub(super) async fn shutdown(&self) {
        let tasks = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            let entries = std::mem::take(&mut state.entries);
            for entry in entries.values() {
                entry.cancellation.cancel();
            }
            state.provider_cache.clear();
            entries
                .into_values()
                .map(|entry| entry.task)
                .collect::<Vec<_>>()
        };
        for task in tasks {
            if let Err(error) = task.await
                && !error.is_cancelled()
            {
                tracing::warn!(
                    event = "project_open_advisory_owner_join_failed",
                    error = %error,
                );
            }
        }
    }
}
