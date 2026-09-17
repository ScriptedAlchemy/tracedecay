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
use std::time::{Duration, Instant};

use tracedecay_contracts::ResolvedScope;

use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity,
};

use super::demand_admission::{
    CodeIndexDemandAdmissionV1, CodeIndexDemandUnavailableV1, CodeIndexDemandV1,
};
use super::identity::IndexingIdentityV1;

const ACTIVATION_IDLE: u8 = 0;
const ACTIVATION_MOUNTING: u8 = 1;
const ACTIVATION_MOUNTED: u8 = 2;
const MAX_PENDING_HOOK_PATHS: usize = 512;

pub type CodeIndexActivationMountFutureV1 =
    Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;
pub type CodeIndexActivationMountV1 =
    Arc<dyn Fn() -> CodeIndexActivationMountFutureV1 + Send + Sync + 'static>;
pub type CodeIndexActivationHintFutureV1 =
    Pin<Box<dyn Future<Output = CodeIndexDemandAdmissionV1> + Send + 'static>>;
pub type CodeIndexActivationHintSinkV1 =
    Arc<dyn Fn(CodeIndexActivationHookBatchV1) -> CodeIndexActivationHintFutureV1 + Send + Sync>;

/// Policy decision for demand-driven indexing on one exact project route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexAutomaticAdmissionV1 {
    Admitted,
    LinkedWorktreeDisabled,
}

/// Who asked for this activation: the daemon's own watch/hook plumbing, or an
/// operator naming the route. Only the former is subject to
/// [`CodeIndexAutomaticAdmissionV1`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationDemandV1 {
    Automatic,
    Explicit,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CodeIndexActivationHookBatchV1 {
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
pub struct CodeIndexActivationV1 {
    project_root: PathBuf,
    identity: Arc<Mutex<Option<IndexingIdentityV1>>>,
    route_registered: Arc<AtomicBool>,
    cancellation: CancellationToken,
    automatic_admission: CodeIndexAutomaticAdmissionV1,
    state: Arc<AtomicU8>,
    pending_hooks: Arc<Mutex<PendingHookPathsV1>>,
    mount: CodeIndexActivationMountV1,
    hint_sink: CodeIndexActivationHintSinkV1,
    retirement: Arc<CodeIndexActivationRetirementV1>,
    #[cfg(test)]
    activation_attempts: Arc<std::sync::atomic::AtomicUsize>,
}

impl CodeIndexActivationV1 {
    pub fn new(
        project_root: &Path,
        route_registered: Arc<AtomicBool>,
        cancellation: CancellationToken,
        mount: CodeIndexActivationMountV1,
        hint_sink: CodeIndexActivationHintSinkV1,
    ) -> Self {
        Self::new_with_admission(
            project_root,
            route_registered,
            cancellation,
            CodeIndexAutomaticAdmissionV1::Admitted,
            mount,
            hint_sink,
        )
    }

    /// Creates a route activation with an explicit automatic-admission policy.
    pub fn new_with_admission(
        project_root: &Path,
        route_registered: Arc<AtomicBool>,
        cancellation: CancellationToken,
        automatic_admission: CodeIndexAutomaticAdmissionV1,
        mount: CodeIndexActivationMountV1,
        hint_sink: CodeIndexActivationHintSinkV1,
    ) -> Self {
        let project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let identity = Arc::new(Mutex::new(IndexingIdentityV1::resolve(&project_root).ok()));
        Self {
            project_root,
            identity,
            route_registered,
            cancellation,
            automatic_admission,
            state: Arc::new(AtomicU8::new(ACTIVATION_IDLE)),
            pending_hooks: Arc::new(Mutex::new(PendingHookPathsV1::default())),
            mount,
            hint_sink,
            retirement: Arc::new(CodeIndexActivationRetirementV1::new()),
            #[cfg(test)]
            activation_attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Returns the retained automatic-admission decision for this route.
    pub fn automatic_admission(&self) -> CodeIndexAutomaticAdmissionV1 {
        self.automatic_admission
    }

    fn route_is_live(&self) -> bool {
        self.route_registered.load(Ordering::Acquire) && !self.cancellation.is_cancelled()
    }

    fn accepts_root(&self, project_root: &Path) -> bool {
        project_root
            .canonicalize()
            .is_ok_and(|root| root == self.project_root)
    }

    pub fn identity(&self) -> Option<IndexingIdentityV1> {
        self.identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn install_retirement(&self, callback: Box<dyn FnOnce() + Send + 'static>) {
        self.retirement.install(callback);
    }

    pub fn authorizes_scope(&self, scope: &ResolvedScope) -> bool {
        self.route_is_live()
            && scope.validate().is_ok()
            && self.identity().is_some_and(|identity| {
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
    #[cfg(test)]
    pub fn activate_for_root(&self, project_root: &Path) -> bool {
        if !self.accepts_root(project_root) {
            return false;
        }
        self.activate()
    }

    pub fn activate(&self) -> bool {
        self.activate_with_demand(ActivationDemandV1::Automatic)
    }

    /// Start the mount for a demand `admit` already cleared: the watcher policy
    /// question was answered there, and asking it again would refuse an
    /// operator-named reconcile on a linked worktree.
    fn activate_for_demand(&self) -> bool {
        self.activate_with_demand(ActivationDemandV1::Explicit)
    }

    /// Start automatic or explicit activation.
    ///
    /// [`CodeIndexAutomaticAdmissionV1`] answers "may the daemon start indexing
    /// this route on its own?" — a watcher policy, not an authorization
    /// boundary. Explicit demand (an operator-named reconcile: `tracedecay
    /// init`, `tracedecay sync`, `tracedecay_admin_sync`) skips exactly that
    /// question and nothing else: route liveness, the indexing identity check
    /// inside the mount, and the activation state machine all still apply.
    #[hotpath::measure(label = "daemon.code_index.activation.activate")]
    fn activate_with_demand(&self, demand: ActivationDemandV1) -> bool {
        if (demand == ActivationDemandV1::Automatic
            && self.automatic_admission != CodeIndexAutomaticAdmissionV1::Admitted)
            || !self.route_is_live()
        {
            return false;
        }
        let Some(expected_identity) = self.identity() else {
            return false;
        };
        match self.state.compare_exchange(
            ACTIVATION_IDLE,
            ACTIVATION_MOUNTING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                hotpath::gauge!("daemon.code_index.generation_state")
                    .set(f64::from(ACTIVATION_MOUNTING));
            }
            Err(ACTIVATION_MOUNTING | ACTIVATION_MOUNTED) => return true,
            Err(_) => return false,
        }
        #[cfg(test)]
        self.activation_attempts.fetch_add(1, Ordering::SeqCst);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.state.store(ACTIVATION_IDLE, Ordering::Release);
            hotpath::gauge!("daemon.code_index.generation_state").set(f64::from(ACTIVATION_IDLE));
            return false;
        };
        let project_root = self.project_root.clone();
        let route_registered = Arc::clone(&self.route_registered);
        let cancellation = self.cancellation.clone();
        let state = Arc::clone(&self.state);
        let pending_hooks = Arc::clone(&self.pending_hooks);
        let mount = Arc::clone(&self.mount);
        let hint_sink = Arc::clone(&self.hint_sink);
        runtime.spawn(hotpath::future!(
            async move {
                let route_is_live =
                    || route_registered.load(Ordering::Acquire) && !cancellation.is_cancelled();
                if !route_is_live() || !Self::identity_is_current(&project_root, &expected_identity)
                {
                    state.store(ACTIVATION_IDLE, Ordering::Release);
                    hotpath::gauge!("daemon.code_index.generation_state")
                        .set(f64::from(ACTIVATION_IDLE));
                    return;
                }
                if let Err(error) = mount().await {
                    state.store(ACTIVATION_IDLE, Ordering::Release);
                    hotpath::gauge!("daemon.code_index.generation_state")
                        .set(f64::from(ACTIVATION_IDLE));
                    tracing::warn!(
                        event = "code_index_activation",
                        project = %project_root.display(),
                        outcome = "degraded",
                        error = %error,
                        "demand-driven code-index activation failed"
                    );
                    return;
                }
                if !route_is_live() || !Self::identity_is_current(&project_root, &expected_identity)
                {
                    state.store(ACTIVATION_IDLE, Ordering::Release);
                    hotpath::gauge!("daemon.code_index.generation_state")
                        .set(f64::from(ACTIVATION_IDLE));
                    return;
                }
                let batch = {
                    let mut pending = pending_hooks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.store(ACTIVATION_MOUNTED, Ordering::Release);
                    hotpath::gauge!("daemon.code_index.generation_state")
                        .set(f64::from(ACTIVATION_MOUNTED));
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
            },
            label = "daemon.code_index.activation.mount"
        ));
        true
    }

    /// Classify a missing constructor identity. A confirmed non-repository is
    /// terminal. An undecided probe is retryable and is not cached.
    async fn ensure_indexing_identity(&self) -> Result<(), CodeIndexDemandAdmissionV1> {
        if self.identity().is_some() {
            return Ok(());
        }
        let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_secs(1));
        match discover_repository_identity(&self.project_root, deadline, &self.cancellation).await {
            GitRepositoryIdentityOutcome::NotRepository => {
                Err(CodeIndexDemandAdmissionV1::NotApplicable)
            }
            GitRepositoryIdentityOutcome::Resolved(_) => {
                match IndexingIdentityV1::resolve(&self.project_root) {
                    Ok(identity) => {
                        *self
                            .identity
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(identity);
                        Ok(())
                    }
                    Err(_) => Err(CodeIndexDemandAdmissionV1::Unavailable(
                        CodeIndexDemandUnavailableV1::IdentityUnresolved,
                    )),
                }
            }
            GitRepositoryIdentityOutcome::Unknown(_) => {
                Err(CodeIndexDemandAdmissionV1::Unavailable(
                    CodeIndexDemandUnavailableV1::IdentityUnresolved,
                ))
            }
        }
    }

    /// Route, policy, and identity checks shared by demand admission and the
    /// freshness probe. `None` means the scheduler may be asked.
    pub async fn gate_demand(
        &self,
        project_root: &Path,
        demand: &CodeIndexDemandV1,
    ) -> Option<CodeIndexDemandAdmissionV1> {
        if !self.route_is_live() {
            return Some(CodeIndexDemandAdmissionV1::Unavailable(
                CodeIndexDemandUnavailableV1::RouteRetired,
            ));
        }
        if !self.accepts_root(project_root) {
            return Some(CodeIndexDemandAdmissionV1::Unavailable(
                CodeIndexDemandUnavailableV1::ForeignRoot,
            ));
        }
        if demand.is_watcher_policy_governed()
            && self.automatic_admission != CodeIndexAutomaticAdmissionV1::Admitted
        {
            return Some(CodeIndexDemandAdmissionV1::RefusedByPolicy);
        }
        self.ensure_indexing_identity().await.err()
    }

    /// The one front door for code-index demand.
    ///
    /// Every caller above — MCP after-edit hooks, `tracedecay sync`, the
    /// server's startup catch-up, host admission — asks here and reports the
    /// verdict it gets. The watcher policy, route liveness, the exact-root
    /// check, and the choice between the mounted scheduler and the bounded
    /// pre-mount queue all live in this one place, so no layer above can
    /// rebuild a reason the front door did not mint.
    #[hotpath::measure(label = "daemon.code_index.activation.admit", future = true)]
    pub async fn admit(
        &self,
        project_root: &Path,
        demand: CodeIndexDemandV1,
    ) -> CodeIndexDemandAdmissionV1 {
        if let Some(verdict) = self.gate_demand(project_root, &demand).await {
            return verdict;
        }
        let overflow = !matches!(demand, CodeIndexDemandV1::HookPaths(_));
        let rel_paths = match demand {
            CodeIndexDemandV1::HookPaths(rel_paths) => {
                if rel_paths.is_empty() {
                    // Empty path batches are a no-op, not a queue seat.
                    return CodeIndexDemandAdmissionV1::Unavailable(
                        CodeIndexDemandUnavailableV1::NoProvenChange,
                    );
                }
                rel_paths
            }
            CodeIndexDemandV1::Reconcile | CodeIndexDemandV1::OperatorReconcile => Vec::new(),
        };
        let direct = {
            let mut pending = self
                .pending_hooks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.state.load(Ordering::Acquire) == ACTIVATION_MOUNTED {
                Some(CodeIndexActivationHookBatchV1 {
                    paths: rel_paths,
                    overflow,
                })
            } else {
                pending.extend(rel_paths);
                pending.overflow |= overflow;
                None
            }
        };
        match direct {
            // A mounted route forwards to the scheduler, which owns the only
            // verdict this activation cannot know: the terminal park.
            Some(batch) if self.route_is_live() => (self.hint_sink)(batch).await,
            Some(_) => {
                CodeIndexDemandAdmissionV1::Unavailable(CodeIndexDemandUnavailableV1::RouteRetired)
            }
            // The bounded queue holds the demand; the mount it starts delivers
            // it. Nothing terminal is knowable before a scheduler is mounted.
            None if self.activate_for_demand() => CodeIndexDemandAdmissionV1::Queued,
            None => CodeIndexDemandAdmissionV1::Unavailable(
                CodeIndexDemandUnavailableV1::SchedulerUnmounted,
            ),
        }
    }

    #[cfg(test)]
    pub fn activation_attempts(&self) -> usize {
        self.activation_attempts.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub fn is_mounted(&self) -> bool {
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
        let status = Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
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
                CodeIndexDemandAdmissionV1::Queued
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
    async fn empty_hook_paths_are_unavailable_no_proven_change_not_queued() {
        let repository = repository();
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let activation = activation(
            repository.path(),
            Arc::clone(&mount_attempts),
            None,
            Arc::new(Mutex::new(Vec::new())),
        );

        assert_eq!(
            activation
                .admit(repository.path(), CodeIndexDemandV1::HookPaths(Vec::new()))
                .await,
            CodeIndexDemandAdmissionV1::Unavailable(CodeIndexDemandUnavailableV1::NoProvenChange)
        );
        assert_eq!(mount_attempts.load(Ordering::SeqCst), 0);
        assert!(!activation.is_mounted());
    }

    #[tokio::test]
    async fn operator_reconcile_admits_a_non_git_project_root() {
        let project = TempDir::new().expect("project root");
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let activation = activation(
            project.path(),
            Arc::clone(&mount_attempts),
            None,
            Arc::new(Mutex::new(Vec::new())),
        );

        assert_eq!(
            activation
                .admit(project.path(), CodeIndexDemandV1::OperatorReconcile)
                .await,
            CodeIndexDemandAdmissionV1::NotApplicable
        );
        assert_eq!(mount_attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn queued_hook_hints_are_bounded_coalesced_and_flushed_after_mount() {
        let repository = repository();
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let batches = Arc::new(Mutex::new(Vec::new()));
        let activation = activation(
            repository.path(),
            Arc::clone(&mount_attempts),
            Some(Arc::clone(&gate)),
            Arc::clone(&batches),
        );
        let mut paths = vec!["src/lib.rs".to_owned(), "src/lib.rs".to_owned()];
        paths.extend((0..=MAX_PENDING_HOOK_PATHS).map(|index| format!("src/{index}.rs")));

        assert!(matches!(
            activation
                .admit(repository.path(), CodeIndexDemandV1::HookPaths(paths))
                .await,
            CodeIndexDemandAdmissionV1::Queued
        ));
        wait_until(|| mount_attempts.load(Ordering::SeqCst) == 1).await;
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
    async fn linked_worktree_disabled_admission_never_mounts() {
        let repository = repository();
        let foreign = TempDir::new().expect("foreign root");
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let attempts = Arc::clone(&mount_attempts);
        let cancellation = CancellationToken::new();
        let activation = CodeIndexActivationV1::new_with_admission(
            repository.path(),
            Arc::new(AtomicBool::new(true)),
            cancellation.clone(),
            CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled,
            Arc::new(move || {
                let attempts = Arc::clone(&attempts);
                Box::pin(async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
            Arc::new(|_| Box::pin(async { CodeIndexDemandAdmissionV1::Queued })),
        );

        assert_eq!(
            activation.automatic_admission(),
            CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled
        );
        assert_eq!(
            activation
                .admit(foreign.path(), CodeIndexDemandV1::Reconcile)
                .await,
            CodeIndexDemandAdmissionV1::Unavailable(CodeIndexDemandUnavailableV1::ForeignRoot)
        );
        assert_eq!(
            activation
                .admit(repository.path(), CodeIndexDemandV1::Reconcile)
                .await,
            CodeIndexDemandAdmissionV1::RefusedByPolicy
        );
        cancellation.cancel();
        assert_eq!(
            activation
                .admit(repository.path(), CodeIndexDemandV1::Reconcile)
                .await,
            CodeIndexDemandAdmissionV1::Unavailable(CodeIndexDemandUnavailableV1::RouteRetired)
        );
        assert!(!activation.activate());
        tokio::task::yield_now().await;
        assert_eq!(mount_attempts.load(Ordering::SeqCst), 0);
    }

    /// `3b0d7c458` answers both project-open deferred owners with
    /// `automatic_admission_for_scope` at spawn time, so a route the daemon may
    /// never index automatically parks no background task. That admission is
    /// frozen into the activation when the route is composed from
    /// `sync.watch_linked_worktrees`; nothing re-reads the configuration
    /// afterwards. Disabling automatic indexing must therefore not be a
    /// permanent loss of the dependent owners: this pins the requirement the
    /// code actually carries — the enabling transition takes effect on the next
    /// route mount, which replaces the registered activation for the scope.
    #[tokio::test]
    async fn a_disabled_route_regains_its_deferred_owners_only_on_a_remount() {
        let repository = repository();
        let identity = IndexingIdentityV1::resolve(repository.path()).expect("indexing identity");
        let scope = ResolvedScope::new(
            ProjectId::new("project.disabled-route-remount").expect("project id"),
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            identity.head_ref().cloned(),
        )
        .expect("resolved scope");
        let registry = super::super::CodeIndexSchedulerRegistryV1::new(1);

        let disabled = Arc::new(CodeIndexActivationV1::new_with_admission(
            repository.path(),
            Arc::new(AtomicBool::new(true)),
            CancellationToken::new(),
            CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled,
            Arc::new(|| Box::pin(async { Ok(()) })),
            Arc::new(|_| Box::pin(async { CodeIndexDemandAdmissionV1::Queued })),
        ));
        assert!(registry.register_activation(&scope, &disabled));
        assert_eq!(
            registry.automatic_admission_for_scope(&scope),
            Some(CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled),
            "the deferred owners must read the route's disabled admission"
        );

        // The configuration flips to enabled. The retained activation is the
        // only thing the deferred owners consult, and it does not re-read
        // configuration, so on its own the flip changes nothing.
        assert_eq!(
            disabled.automatic_admission(),
            CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled,
            "a live activation must not silently change the admission it was composed with"
        );
        assert_eq!(
            registry.automatic_admission_for_scope(&scope),
            Some(CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled)
        );

        // The route remount composes a new activation from the new
        // configuration and re-registers it for the same scope. From here the
        // deferred owners are admitted again: the disablement was never
        // terminal for the capability, only for that mount.
        let enabled = Arc::new(CodeIndexActivationV1::new_with_admission(
            repository.path(),
            Arc::new(AtomicBool::new(true)),
            CancellationToken::new(),
            CodeIndexAutomaticAdmissionV1::Admitted,
            Arc::new(|| Box::pin(async { Ok(()) })),
            Arc::new(|_| Box::pin(async { CodeIndexDemandAdmissionV1::Queued })),
        ));
        assert!(registry.register_activation(&scope, &enabled));
        assert_eq!(
            registry.automatic_admission_for_scope(&scope),
            Some(CodeIndexAutomaticAdmissionV1::Admitted),
            "a route remount under the enabled configuration must readmit the deferred owners"
        );
        drop(disabled);
        assert_eq!(
            registry.automatic_admission_for_scope(&scope),
            Some(CodeIndexAutomaticAdmissionV1::Admitted),
            "retiring the superseded activation must not revoke the remounted admission"
        );
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
            Arc::new(|_| Box::pin(async { CodeIndexDemandAdmissionV1::Queued })),
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
