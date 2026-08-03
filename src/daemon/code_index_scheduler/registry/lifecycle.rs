use std::path::Path;
use std::sync::atomic::Ordering;

use super::CodeIndexSchedulerRegistryV1;

impl CodeIndexSchedulerRegistryV1 {
    pub async fn shutdown(&self) {
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for worktree in mounted.values() {
            worktree.shutting_down.store(true, Ordering::Release);
            if let Some(latest) = worktree
                .serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                latest.warm_control.cancel();
            }
            *worktree
                .serving_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            worktree.wake.notify_one();
        }
        for (_, worktree) in mounted {
            let _ = worktree.task.await;
        }
    }

    pub(in crate::daemon) async fn unmount_worktree(&self, project_root: &Path) -> bool {
        let project_root = match project_root.canonicalize() {
            Ok(root) => root,
            Err(_) => project_root.to_path_buf(),
        };
        let Some(worktree) = self.mounted.lock().await.remove(&project_root) else {
            return false;
        };
        worktree.shutting_down.store(true, Ordering::Release);
        if let Some(latest) = worktree
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            latest.warm_control.cancel();
        }
        *worktree
            .serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        worktree.wake.notify_one();
        let _ = worktree.task.await;
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&project_root);
        true
    }
}
