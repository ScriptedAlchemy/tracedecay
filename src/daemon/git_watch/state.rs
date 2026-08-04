use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::AtomicU64;

use tokio::sync::{Mutex, Notify};

use super::{DirtySet, ProjectHealth};
use crate::daemon::maintenance::MaintenanceCoordinator;

pub(super) enum WorktreeRegistration {
    Ready,
    Capacity,
}

/// Repository-scoped watcher state.
///
/// Git metadata belongs to the repository common directory, while HEAD,
/// operation markers, and scheduler ownership remain per worktree. Keeping
/// those two identities together prevents linked worktrees from multiplying
/// OS watchers without collapsing their freshness requests.
pub(super) struct WatchState {
    pub(super) common_dir: PathBuf,
    worktrees: RwLock<BTreeMap<PathBuf, PathBuf>>,
    pub(super) dirty: Mutex<DirtySet>,
    pub(super) reconciliation_pending: AtomicBool,
    pub(super) wake: Notify,
    pub(super) reconfigure: Notify,
    pub(super) maintenance: MaintenanceCoordinator,
    pub(super) health: ProjectHealth,
    pub(super) task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg(test)]
    pub(super) entered_debounce: Notify,
    #[cfg(test)]
    pub(super) drained_plans: AtomicU64,
    #[cfg(test)]
    pub(super) plan_drained: Notify,
}

impl WatchState {
    pub(super) fn new(
        common_dir: PathBuf,
        project_root: PathBuf,
        git_dir: PathBuf,
        maintenance: MaintenanceCoordinator,
    ) -> Self {
        Self {
            common_dir,
            worktrees: RwLock::new(BTreeMap::from([(project_root, git_dir)])),
            dirty: Mutex::new(DirtySet::default()),
            reconciliation_pending: AtomicBool::new(false),
            wake: Notify::new(),
            reconfigure: Notify::new(),
            maintenance,
            health: ProjectHealth::default(),
            task: Mutex::new(None),
            #[cfg(test)]
            entered_debounce: Notify::new(),
            #[cfg(test)]
            drained_plans: AtomicU64::new(0),
            #[cfg(test)]
            plan_drained: Notify::new(),
        }
    }

    /// Adds one scheduler-owned worktree to this repository watcher.
    ///
    /// A new git directory changes the exact set of marker paths watched by
    /// the repository task, so the task is told to rebuild its small metadata
    /// watch set. Re-registering an existing root is a no-op.
    pub(super) fn register_worktree(
        &self,
        project_root: PathBuf,
        git_dir: PathBuf,
        max_worktrees: usize,
    ) -> WorktreeRegistration {
        let mut worktrees = self
            .worktrees
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktrees.get(&project_root) == Some(&git_dir) {
            return WorktreeRegistration::Ready;
        }
        if !worktrees.contains_key(&project_root) && worktrees.len() >= max_worktrees {
            return WorktreeRegistration::Capacity;
        }
        worktrees.insert(project_root, git_dir);
        drop(worktrees);
        self.reconfigure.notify_one();
        WorktreeRegistration::Ready
    }

    pub(super) fn worktree_roots(&self) -> Vec<PathBuf> {
        self.worktrees
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }

    pub(super) fn git_dirs(&self) -> Vec<PathBuf> {
        self.worktrees
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    /// Git directories whose operation markers can transiently move shared
    /// repository refs. This includes linked worktrees not yet mounted by the
    /// daemon: their operation still affects every registered root.
    pub(super) fn operation_git_dirs(&self, max_worktrees: usize) -> Option<Vec<PathBuf>> {
        let mut git_dirs = self.git_dirs().into_iter().collect::<BTreeSet<_>>();
        if git_dirs.len() > max_worktrees {
            return None;
        }
        match std::fs::read_dir(self.common_dir.join("worktrees")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.ok()?;
                    if entry.file_type().ok()?.is_dir() {
                        git_dirs.insert(entry.path());
                        if git_dirs.len() > max_worktrees {
                            return None;
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        Some(git_dirs.into_iter().collect())
    }

    pub(super) fn prune_missing_worktrees(&self) {
        self.worktrees
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|root, git_dir| root.is_dir() && git_dir.is_dir());
    }

    #[cfg(test)]
    pub(super) fn contains_worktree(&self, project_root: &Path) -> bool {
        self.worktrees
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(project_root)
    }
}
