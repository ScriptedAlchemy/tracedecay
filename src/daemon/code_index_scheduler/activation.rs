//! Demand-driven activation for one exact daemon project route.
//!
//! Project open installs this lightweight route-local owner, but does not mount
//! a code-index scheduler. The first code-index demand starts one background
//! mount. Hook hints received while that mount is in flight are coalesced into a
//! bounded queue and delivered after the exact worktree is mounted.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_application::ResolvedScope;

use crate::application::context::CancellationToken;

use super::identity::IndexingIdentityV1;

const ACTIVATION_IDLE: u8 = 0;
const ACTIVATION_MOUNTING: u8 = 1;
const ACTIVATION_MOUNTED: u8 = 2;
const MAX_PENDING_HOOK_PATHS: usize = 512;

pub(in crate::daemon) type CodeIndexActivationMountFutureV1 =
    Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;
pub(in crate::daemon) type CodeIndexActivationMountV1 =
    Arc<dyn Fn() -> CodeIndexActivationMountFutureV1 + Send + Sync + 'static>;
pub(in crate::daemon) type CodeIndexActivationHintFutureV1 =
    Pin<Box<dyn Future<Output = bool> + Send + 'static>>;
pub(in crate::daemon) type CodeIndexActivationHintSinkV1 =
    Arc<dyn Fn(CodeIndexActivationHookBatchV1) -> CodeIndexActivationHintFutureV1 + Send + Sync>;

#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::daemon) struct CodeIndexActivationHookBatchV1 {
    pub paths: Vec<String>,
    pub overflow: bool,
}

#[derive(Default)]
struct PendingHookPathsV1 {
    paths: BTreeSet<String>,
    overflow: bool,
}

impl PendingHookPathsV1 {
    fn extend(&mut self, paths: impl IntoIterator<Item = String>) {
        for path in paths {
            if path.is_empty() || self.paths.contains(&path) {
                continue;
            }
            if self.paths.len() >= MAX_PENDING_HOOK_PATHS {
                self.overflow = true;
                continue;
            }
            self.paths.insert(path);
        }
    }

    fn take(&mut self) -> CodeIndexActivationHookBatchV1 {
        CodeIndexActivationHookBatchV1 {
            paths: std::mem::take(&mut self.paths).into_iter().collect(),
            overflow: std::mem::take(&mut self.overflow),
        }
    }
}

struct CodeIndexActivationRetirementV1 {
    callbacks: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl CodeIndexActivationRetirementV1 {
    fn new() -> Self {
        Self {
            callbacks: Mutex::new(Vec::new()),
        }
    }

    fn install(&self, callback: Box<dyn FnOnce() + Send + 'static>) {
        self.callbacks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(callback);
    }
}

impl Drop for CodeIndexActivationRetirementV1 {
    fn drop(&mut self) {
        let callbacks = std::mem::take(
            self.callbacks
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for callback in callbacks {
            callback();
        }
    }
}

/// One activation owner per exact canonical route/worktree.
///
/// Clones share state. The captured structural identity permits HEAD movement
/// inside this worktree, but rejects a different linked worktree even when both
/// checkouts contain identical bytes.
#[derive(Clone)]
pub(in crate::daemon) struct CodeIndexActivationV1 {
    project_root: PathBuf,
    identity: Option<IndexingIdentityV1>,
    route_registered: Arc<AtomicBool>,
    cancellation: CancellationToken,
    state: Arc<AtomicU8>,
    pending_hooks: Arc<Mutex<PendingHookPathsV1>>,
    mount: CodeIndexActivationMountV1,
    hint_sink: CodeIndexActivationHintSinkV1,
    retirement: Arc<CodeIndexActivationRetirementV1>,
    #[cfg(test)]
    activation_attempts: Arc<std::sync::atomic::AtomicUsize>,
}

impl CodeIndexActivationV1 {
    pub(in crate::daemon) fn new(
        project_root: &Path,
        route_registered: Arc<AtomicBool>,
        cancellation: CancellationToken,
        mount: CodeIndexActivationMountV1,
        hint_sink: CodeIndexActivationHintSinkV1,
    ) -> Self {
        let project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let identity = IndexingIdentityV1::resolve(&project_root).ok();
        Self {
            project_root,
            identity,
            route_registered,
            cancellation,
            state: Arc::new(AtomicU8::new(ACTIVATION_IDLE)),
            pending_hooks: Arc::new(Mutex::new(PendingHookPathsV1::default())),
            mount,
            hint_sink,
            retirement: Arc::new(CodeIndexActivationRetirementV1::new()),
            #[cfg(test)]
            activation_attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn route_is_live(&self) -> bool {
        self.route_registered.load(Ordering::Acquire) && !self.cancellation.is_cancelled()
    }

    fn accepts_root(&self, project_root: &Path) -> bool {
        self.identity.is_some()
            && project_root
                .canonicalize()
                .is_ok_and(|root| root == self.project_root)
    }

    pub(super) fn identity(&self) -> Option<&IndexingIdentityV1> {
        self.identity.as_ref()
    }

    pub(super) fn install_retirement(&self, callback: Box<dyn FnOnce() + Send + 'static>) {
        self.retirement.install(callback);
    }

    pub(super) fn authorizes_scope(&self, scope: &ResolvedScope) -> bool {
        self.route_is_live()
            && scope.validate().is_ok()
            && self.identity.as_ref().is_some_and(|identity| {
                identity.repository_id() == &scope.repository_id
                    && identity.worktree_id() == &scope.worktree_id
            })
    }

    fn identity_is_current(project_root: &Path, expected_identity: &IndexingIdentityV1) -> bool {
        IndexingIdentityV1::resolve(project_root)
            .is_ok_and(|current| current.authorizes_reuse_of(expected_identity))
    }

    /// Request activation for the route's exact worktree without waiting for
    /// mount, generation publication, or query-authority installation.
    pub(in crate::daemon) fn activate_for_root(&self, project_root: &Path) -> bool {
        if !self.accepts_root(project_root) {
            return false;
        }
        self.activate()
    }

    pub(in crate::daemon) fn activate(&self) -> bool {
        if !self.route_is_live() {
            return false;
        }
        let Some(expected_identity) = self.identity.clone() else {
            return false;
        };
        match self.state.compare_exchange(
            ACTIVATION_IDLE,
            ACTIVATION_MOUNTING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(ACTIVATION_MOUNTING | ACTIVATION_MOUNTED) => return true,
            Err(_) => return false,
        }
        #[cfg(test)]
        self.activation_attempts.fetch_add(1, Ordering::SeqCst);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.state.store(ACTIVATION_IDLE, Ordering::Release);
            return false;
        };
        let project_root = self.project_root.clone();
        let route_registered = Arc::clone(&self.route_registered);
        let cancellation = self.cancellation.clone();
        let state = Arc::clone(&self.state);
        let pending_hooks = Arc::clone(&self.pending_hooks);
        let mount = Arc::clone(&self.mount);
        let hint_sink = Arc::clone(&self.hint_sink);
        runtime.spawn(async move {
            let route_is_live =
                || route_registered.load(Ordering::Acquire) && !cancellation.is_cancelled();
            if !route_is_live() || !Self::identity_is_current(&project_root, &expected_identity) {
                state.store(ACTIVATION_IDLE, Ordering::Release);
                return;
            }
            if let Err(error) = mount().await {
                state.store(ACTIVATION_IDLE, Ordering::Release);
                tracing::warn!(
                    event = "code_index_activation",
                    project = %project_root.display(),
                    outcome = "degraded",
                    error = %error,
                    "demand-driven code-index activation failed"
                );
                return;
            }
            if !route_is_live() || !Self::identity_is_current(&project_root, &expected_identity) {
                state.store(ACTIVATION_IDLE, Ordering::Release);
                return;
            }
            let batch = {
                let mut pending = pending_hooks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.store(ACTIVATION_MOUNTED, Ordering::Release);
                pending.take()
            };
            if route_is_live() && (!batch.paths.is_empty() || batch.overflow) {
                let _ = hint_sink(batch).await;
            }
            tracing::info!(
                event = "code_index_activation",
                project = %project_root.display(),
                outcome = "mounted",
                "demand-driven code-index activation mounted"
            );
        });
        true
    }

    /// Accept exact after-edit hints immediately, even while the background
    /// mount is still opening. Returns `true` only when this exact live route
    /// accepted the paths for queued or direct delivery.
    pub(in crate::daemon) async fn notify_hook_paths(
        &self,
        project_root: &Path,
        rel_paths: Vec<String>,
    ) -> bool {
        if rel_paths.is_empty() || !self.route_is_live() || !self.accepts_root(project_root) {
            return false;
        }
        let direct = {
            let mut pending = self
                .pending_hooks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.state.load(Ordering::Acquire) == ACTIVATION_MOUNTED {
                Some(CodeIndexActivationHookBatchV1 {
                    paths: rel_paths,
                    overflow: false,
                })
            } else {
                pending.extend(rel_paths);
                None
            }
        };
        match direct {
            Some(batch) if self.route_is_live() => (self.hint_sink)(batch).await,
            Some(_) => false,
            None => self.activate(),
        }
    }

    #[cfg(test)]
    pub(super) fn activation_attempts(&self) -> usize {
        self.activation_attempts.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn is_mounted(&self) -> bool {
        self.state.load(Ordering::Acquire) == ACTIVATION_MOUNTED
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        self.state.load(Ordering::Acquire) == ACTIVATION_IDLE
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;
    use tracedecay_domain::ProjectId;

    use super::*;

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new(crate::git::git_program())
            .current_dir(root)
            .args(arguments)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?}");
    }

    fn repository() -> TempDir {
        let root = TempDir::new().expect("repository root");
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(root.path().join("lib.rs"), "pub fn seed() {}\n").expect("seed source");
        git(root.path(), &["add", "."]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "seed",
            ],
        );
        root
    }

    fn activation(
        root: &Path,
        mount_attempts: Arc<AtomicUsize>,
        mount_gate: Option<Arc<tokio::sync::Notify>>,
        batches: Arc<Mutex<Vec<CodeIndexActivationHookBatchV1>>>,
    ) -> CodeIndexActivationV1 {
        let mount: CodeIndexActivationMountV1 = Arc::new(move || {
            let attempts = Arc::clone(&mount_attempts);
            let gate = mount_gate.clone();
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                Ok(())
            })
        });
        let hint_sink: CodeIndexActivationHintSinkV1 = Arc::new(move |batch| {
            let batches = Arc::clone(&batches);
            Box::pin(async move {
                batches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(batch);
                true
            })
        });
        CodeIndexActivationV1::new(
            root,
            Arc::new(AtomicBool::new(true)),
            CancellationToken::new(),
            mount,
            hint_sink,
        )
    }

    async fn wait_until(predicate: impl Fn() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("activation condition");
    }

    #[tokio::test]
    async fn first_demand_mounts_once_and_concurrent_demand_singleflights() {
        let repository = repository();
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let activation = activation(
            repository.path(),
            Arc::clone(&mount_attempts),
            Some(Arc::clone(&gate)),
            Arc::new(Mutex::new(Vec::new())),
        );

        let mut demands = Vec::new();
        for _ in 0..32 {
            let activation = activation.clone();
            let root = repository.path().to_path_buf();
            demands.push(tokio::spawn(
                async move { activation.activate_for_root(&root) },
            ));
        }
        for demand in demands {
            assert!(demand.await.expect("demand task"));
        }
        wait_until(|| mount_attempts.load(Ordering::SeqCst) == 1).await;
        assert_eq!(activation.activation_attempts(), 1);
        gate.notify_waiters();
        wait_until(|| activation.is_mounted()).await;
        assert_eq!(mount_attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn queued_hook_hints_are_bounded_coalesced_and_flushed_after_mount() {
        let repository = repository();
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let batches = Arc::new(Mutex::new(Vec::new()));
        let activation = activation(
            repository.path(),
            mount_attempts,
            Some(Arc::clone(&gate)),
            Arc::clone(&batches),
        );
        let mut paths = vec!["src/lib.rs".to_owned(), "src/lib.rs".to_owned()];
        paths.extend((0..=MAX_PENDING_HOOK_PATHS).map(|index| format!("src/{index}.rs")));

        assert!(activation.notify_hook_paths(repository.path(), paths).await);
        gate.notify_waiters();
        wait_until(|| !batches.lock().expect("batches").is_empty()).await;
        let batch = batches.lock().expect("batches").remove(0);
        assert!(batch.overflow);
        assert_eq!(batch.paths.len(), MAX_PENDING_HOOK_PATHS);
        assert_eq!(
            batch
                .paths
                .iter()
                .filter(|path| path.as_str() == "src/lib.rs")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn linked_worktree_activation_remains_route_local() {
        let primary = repository();
        let linked_parent = TempDir::new().expect("linked parent");
        let linked = linked_parent.path().join("linked");
        git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                linked.to_str().expect("linked path"),
                "main",
            ],
        );
        let primary_mounts = Arc::new(AtomicUsize::new(0));
        let linked_mounts = Arc::new(AtomicUsize::new(0));
        let primary_activation = activation(
            primary.path(),
            Arc::clone(&primary_mounts),
            None,
            Arc::new(Mutex::new(Vec::new())),
        );
        let linked_activation = activation(
            &linked,
            Arc::clone(&linked_mounts),
            None,
            Arc::new(Mutex::new(Vec::new())),
        );

        assert!(!primary_activation.activate_for_root(&linked));
        assert!(primary_activation.activate_for_root(primary.path()));
        wait_until(|| primary_activation.is_mounted()).await;
        assert_eq!(linked_mounts.load(Ordering::SeqCst), 0);

        assert!(linked_activation.activate_for_root(&linked));
        wait_until(|| linked_activation.is_mounted()).await;
        assert_eq!(primary_mounts.load(Ordering::SeqCst), 1);
        assert_eq!(linked_mounts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn search_and_callable_lookups_activate_registered_route_once() {
        let repository = repository();
        let identity = IndexingIdentityV1::resolve(repository.path()).expect("indexing identity");
        let scope = ResolvedScope::new(
            ProjectId::new("project.lazy-code-index").expect("project id"),
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            identity.head_ref().cloned(),
        )
        .expect("resolved scope");
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let activation = Arc::new(activation(
            repository.path(),
            Arc::clone(&mount_attempts),
            Some(Arc::clone(&gate)),
            Arc::new(Mutex::new(Vec::new())),
        ));
        let registry = super::super::CodeIndexSchedulerRegistryV1::new(1);
        assert!(registry.register_activation(&scope, &activation));

        assert!(
            registry
                .latest_complete_ready_for_scope(&scope)
                .await
                .is_none()
        );
        for _ in 0..16 {
            assert!(registry.query_authority_for_scope(&scope).await.is_none());
        }
        wait_until(|| mount_attempts.load(Ordering::SeqCst) == 1).await;
        assert_eq!(activation.activation_attempts(), 1);

        gate.notify_waiters();
        wait_until(|| activation.is_mounted()).await;
        assert_eq!(registry.activation_count(), 1);
        drop(activation);
        assert_eq!(registry.activation_count(), 0);
    }

    #[tokio::test]
    async fn revoked_route_cannot_publish_a_completed_mount() {
        let repository = repository();
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let route_registered = Arc::new(AtomicBool::new(true));
        let mount: CodeIndexActivationMountV1 = {
            let mount_attempts = Arc::clone(&mount_attempts);
            let gate = Arc::clone(&gate);
            Arc::new(move || {
                let mount_attempts = Arc::clone(&mount_attempts);
                let gate = Arc::clone(&gate);
                Box::pin(async move {
                    mount_attempts.fetch_add(1, Ordering::SeqCst);
                    gate.notified().await;
                    Ok(())
                })
            })
        };
        let activation = CodeIndexActivationV1::new(
            repository.path(),
            Arc::clone(&route_registered),
            CancellationToken::new(),
            mount,
            Arc::new(|_| Box::pin(async { true })),
        );

        assert!(activation.activate());
        wait_until(|| mount_attempts.load(Ordering::SeqCst) == 1).await;
        route_registered.store(false, Ordering::Release);
        gate.notify_waiters();
        wait_until(|| activation.is_idle()).await;

        assert!(!activation.is_mounted());
        assert!(!activation.activate());
    }
}
