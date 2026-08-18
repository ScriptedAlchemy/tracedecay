//! Project-open admission bookkeeping: route/owner keys, open gates, and the
//! in-flight open-task registry.
//!
//! Tracks which project opens are running, which failed, and how long a failed
//! open stays backed off, so a repeated request neither stampedes nor retries a
//! known-unrepairable store.
//!
//! `use super::*` re-exposes the daemon authorities used by these admission
//! and ownership records.

use super::*;
use std::collections::{BTreeMap, BTreeSet, HashSet};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ProjectServerKey {
    pub(super) owner: StoreOwnerKey,
    pub(super) project_root: PathBuf,
    pub(super) scope_prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct StoreOwnerKey {
    pub(super) profile_root: PathBuf,
    pub(super) global_db_path: PathBuf,
    pub(super) project_id: Option<String>,
    pub(super) store_root: PathBuf,
    pub(super) graph_db_path: PathBuf,
}

/// A client route known before any project database is opened. This is the
/// cache/singleflight key; [`ProjectServerKey`] remains the post-open server
/// key so filesystem aliases converge while distinct linked worktrees retain
/// exact root-bound servers over one shared [`StoreOwnerKey`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ProjectRouteKey {
    pub(super) profile_root: PathBuf,
    pub(super) global_db_path: PathBuf,
    pub(super) project_path: PathBuf,
    pub(super) scope_prefix: Option<String>,
}

pub(super) type ProjectOpenGate = tokio::sync::Mutex<()>;
#[derive(Default)]
pub(super) struct ProjectOpenGates {
    pub(super) gates: HashMap<ProjectRouteKey, std::sync::Weak<ProjectOpenGate>>,
    pub(super) tasks: ProjectOpenTasks,
}
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
pub(super) type MaintenanceTransitionGate = tokio::sync::Mutex<()>;
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
pub(super) type MaintenanceTransitionGates =
    HashMap<MaintenanceTransitionKey, std::sync::Weak<MaintenanceTransitionGate>>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
pub(super) struct MaintenanceTransitionKey {
    pub(super) profile_root: PathBuf,
    pub(super) project_id: Option<String>,
    pub(super) scope_prefix: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
pub(super) enum MaintenanceRekeyOutcome {
    Completed,
    Retiring,
}

/// Route-local project-open work. A route owns at most one task, and
/// deterministic configuration failures retain a short backoff record so a
/// reconnecting MCP host cannot repeatedly reopen the same rejected store.
#[derive(Clone, Default)]
pub(super) struct ProjectOpenTasks {
    registry: Arc<StdMutex<ProjectOpenTaskRegistry>>,
}

#[derive(Default)]
struct ProjectOpenTaskRegistry {
    routes: HashMap<ProjectRouteKey, ProjectOpenTaskEntry>,
    retiring: HashMap<ProjectRouteKey, ProjectOpenTaskEntry>,
    closed_profiles: BTreeSet<PathBuf>,
    quiesced_projects: BTreeSet<ProjectOpenIdentityV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectOpenIdentityV1 {
    profile_root: PathBuf,
    project_id: String,
    project_roots: BTreeSet<PathBuf>,
}

pub(super) struct ProjectOpenIdentityQuiescenceV1 {
    tasks: ProjectOpenTasks,
    identity: ProjectOpenIdentityV1,
}

impl Drop for ProjectOpenIdentityQuiescenceV1 {
    fn drop(&mut self) {
        self.tasks
            .lock_registry()
            .quiesced_projects
            .remove(&self.identity);
    }
}

struct ProjectOpenTaskEntry {
    state: tokio::sync::watch::Receiver<ProjectOpenTaskState>,
    cancellation: CancellationToken,
    completion: tokio::sync::watch::Receiver<bool>,
    task: JoinHandle<()>,
}

struct ProjectOpenTaskCompletionFinalizer(tokio::sync::watch::Sender<bool>);

impl Drop for ProjectOpenTaskCompletionFinalizer {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

#[derive(Clone)]
pub(super) enum ProjectOpenTaskState {
    Opening,
    Ready,
    Failed(ProjectOpenFailure),
}

#[derive(Clone)]
pub(super) struct ProjectOpenFailure {
    pub(super) message: String,
    pub(super) retry_at: Option<Instant>,
    pub(super) typed: Option<ProjectOpenTypedFailure>,
    /// On-disk identity of the refused store's graph databases at the moment
    /// a `ResetRequired` refusal was recorded. A cached refusal is a property
    /// of exactly those files; when they change (an operator reset deleted or
    /// replaced them) the refusal no longer describes the store and must not
    /// be served from this cache.
    refused_store: Option<RefusedStoreFingerprintV1>,
}

/// Identity of one refused store file when its refusal was recorded. Length,
/// modification time, and (on unix) device/inode together distinguish "the
/// same refused file" from "deleted, replaced, or rewritten since".
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefusedStoreFileIdentityV1 {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// Every graph database the refused project store carried when the refusal
/// was recorded: the root graph DB plus the per-branch graph DBs under
/// `branches/`. Comparing the whole map catches deletions, replacements, and
/// newly recreated databases alike.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefusedStoreFingerprintV1 {
    graph_dbs: BTreeMap<PathBuf, RefusedStoreFileIdentityV1>,
}

fn refused_store_file_identity(path: &Path) -> Option<RefusedStoreFileIdentityV1> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(RefusedStoreFileIdentityV1 {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

/// Fingerprints the route's project store graph databases from the same
/// persisted layout authority the open itself resolves. `None` when the route
/// resolves no persisted store, in which case a recorded refusal keeps its
/// plain time-based backoff.
fn refused_store_fingerprint(route: &ProjectRouteKey) -> Option<RefusedStoreFingerprintV1> {
    let layout = crate::storage::resolve_persisted_layout(&route.project_path, &route.profile_root)
        .ok()
        .flatten()?;
    let mut graph_dbs = BTreeMap::new();
    if let Some(identity) = refused_store_file_identity(&layout.graph_db_path) {
        graph_dbs.insert(layout.graph_db_path.clone(), identity);
    }
    if let Ok(entries) = std::fs::read_dir(layout.data_root.join("branches")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("db")
                && let Some(identity) = refused_store_file_identity(&path)
            {
                graph_dbs.insert(path, identity);
            }
        }
    }
    Some(RefusedStoreFingerprintV1 { graph_dbs })
}

#[derive(Clone)]
pub(super) enum ProjectOpenTypedFailure {
    ProfileResetRequired {
        component: &'static str,
        found_version: Option<i64>,
        required_version: i64,
    },
    ResetRequired {
        authority: String,
        reason: String,
    },
}

pub(super) enum ProjectOpenTaskClaim {
    InFlight(tokio::sync::watch::Receiver<ProjectOpenTaskState>),
    Failed(ProjectOpenFailure),
    Saturated,
}

/// Result of waiting for the route's tracked full-capability project open.
///
/// Core publication is intentionally independent from this wait: ordinary
/// project requests may use the core server while dependent owners finish,
/// whereas LSP admission needs the exact route's owner set to be complete.
#[derive(Debug)]
pub(super) enum ProjectOpenWaitOutcome {
    Completed,
    NotTracked,
    Failed(TraceDecayError),
    Cancelled,
    TimedOut,
}

fn project_route_matches_identity(
    route: &ProjectRouteKey,
    profile_root: &Path,
    project_id: &str,
    project_roots: &BTreeSet<PathBuf>,
) -> bool {
    route.profile_root == profile_root
        && (project_roots.contains(&route.project_path)
            || crate::storage::resolve_persisted_layout(&route.project_path, profile_root)
                .ok()
                .flatten()
                .and_then(|layout| layout.identity.project_id)
                .as_deref()
                == Some(project_id))
}

fn project_routes_for_retirement(
    registry: &mut ProjectOpenTaskRegistry,
    identity: &ProjectOpenIdentityV1,
) -> Vec<ProjectRouteKey> {
    let mut routes = registry
        .routes
        .keys()
        .filter(|route| {
            project_route_matches_identity(
                route,
                &identity.profile_root,
                &identity.project_id,
                &identity.project_roots,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    for route in &routes {
        if let Some(entry) = registry.routes.remove(route) {
            entry.cancellation.cancel();
            registry.retiring.insert(route.clone(), entry);
        }
    }
    let already_selected = routes.iter().cloned().collect::<HashSet<_>>();
    routes.extend(
        registry
            .retiring
            .keys()
            .filter(|route| {
                !already_selected.contains(*route)
                    && project_route_matches_identity(
                        route,
                        &identity.profile_root,
                        &identity.project_id,
                        &identity.project_roots,
                    )
            })
            .cloned(),
    );
    routes
}

async fn wait_for_project_open_task(mut completion: tokio::sync::watch::Receiver<bool>) {
    while !*completion.borrow() {
        if completion.changed().await.is_err() {
            return;
        }
    }
}

/// Whether the authority audit failed because it could not read the database,
/// rather than because it judged what it read.
///
/// These are the only failures under that audit whose answer can differ on the
/// next open without anything being repaired.
fn is_database_read_failure(message: &str) -> bool {
    const DRIVER_FAILURES: [&str; 5] = [
        "database is locked",
        "database is busy",
        "disk I/O error",
        "unable to open database file",
        "interrupted",
    ];
    DRIVER_FAILURES
        .iter()
        .any(|failure| message.contains(failure))
}

/// How long a failed project-open route declines reopening, or `None` when the
/// failure may clear on its own.
pub(super) fn project_open_retry_backoff(error: &TraceDecayError) -> Option<Duration> {
    match error {
        TraceDecayError::Config { message } => (message.contains("identity cutover conflict")
            || message.contains("ambiguous legacy profile stores")
            || message.contains("enrollment marker did not resolve a profile store"))
        .then_some(PROJECT_OPEN_FAILURE_RETRY_BACKOFF),
        // This audit's whole job is to read persisted rows and judge them, so
        // its verdict is a property of the stored data: a row rejected now is
        // rejected identically 250ms from now. Back off for the whole family
        // and name the exceptions, rather than listing the failures that
        // deserve a backoff — that ordering meant every newly surfaced
        // invariant message spun warm-up at the debounce cadence until someone
        // noticed the CPU. Decode failures and column-versus-JSON
        // disagreements both land here without being enumerated.
        TraceDecayError::Database { message, operation } => {
            // A failed code-shard open may already have published its typed
            // resolver authority. Retrying cannot repair a conflicting binding
            // and previously repeated the whole warm-up on every hook request.
            if operation == "register code-shard authority"
                && message.starts_with("DuplicateCodeAuthority {")
            {
                return Some(PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF);
            }
            // Code-runtime capacity may clear after another project retires,
            // but rebuilding this route for every concurrent request only
            // prolongs the resource pressure that rejected it.
            if operation == "open registered session runtime"
                && message.starts_with("ProjectCodeBudgetExhausted {")
            {
                return Some(PROJECT_OPEN_RESOURCE_RETRY_BACKOFF);
            }
            if operation != "ensure global database authority invariants" {
                return None;
            }
            if is_database_read_failure(message) {
                return None;
            }
            // A migration still in flight can be what leaves these mutable.
            if message.contains("session temporal receipts or cursor keys are mutable") {
                return Some(PROJECT_OPEN_FAILURE_RETRY_BACKOFF);
            }
            Some(PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF)
        }
        TraceDecayError::ProfileResetRequired { .. } | TraceDecayError::ResetRequired { .. } => {
            Some(PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF)
        }
        _ => None,
    }
}

impl ProjectOpenFailure {
    /// An admission-side denial with no typed classification, no backoff, and
    /// no refused-store fingerprint.
    pub(super) fn untyped(message: String) -> Self {
        Self {
            message,
            retry_at: None,
            typed: None,
            refused_store: None,
        }
    }

    fn from_error(error: &TraceDecayError) -> Self {
        // Operator-repairable authority rejections decline implicit repair.
        // Reopening before maintenance changes that state is not useful and
        // only multiplies daemon warm-up tasks.
        let retry_at = project_open_retry_backoff(error).map(|backoff| Instant::now() + backoff);
        Self {
            message: error.to_string(),
            retry_at,
            typed: match error {
                TraceDecayError::ProfileResetRequired {
                    component,
                    found_version,
                    required_version,
                } => Some(ProjectOpenTypedFailure::ProfileResetRequired {
                    component,
                    found_version: *found_version,
                    required_version: *required_version,
                }),
                TraceDecayError::ResetRequired { authority, reason } => {
                    Some(ProjectOpenTypedFailure::ResetRequired {
                        authority: authority.clone(),
                        reason: reason.clone(),
                    })
                }
                _ => None,
            },
            refused_store: None,
        }
    }

    /// Records a failed open for its route. A `ResetRequired` refusal
    /// additionally captures the refused store's on-disk fingerprint so a
    /// later retry can distinguish "still the refused files" from "the
    /// operator reset the store on disk".
    fn recorded_for_route(error: &TraceDecayError, route: &ProjectRouteKey) -> Self {
        let mut failure = Self::from_error(error);
        if matches!(
            failure.typed,
            Some(ProjectOpenTypedFailure::ResetRequired { .. })
        ) {
            failure.refused_store = refused_store_fingerprint(route);
        }
        failure
    }

    /// Whether this cached refusal no longer describes the store on disk.
    /// Serving a refusal recorded against files that have since been deleted
    /// or replaced forced a daemon restart after `storage
    /// reset-project-store`; a stale entry is dropped so the next open
    /// re-derives the typed truth from disk.
    fn is_stale_for(&self, route: &ProjectRouteKey) -> bool {
        let Some(recorded) = &self.refused_store else {
            return false;
        };
        refused_store_fingerprint(route).as_ref() != Some(recorded)
    }

    fn is_backed_off(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|retry_at| retry_at > now)
    }

    pub(super) fn to_error(&self) -> TraceDecayError {
        match &self.typed {
            Some(ProjectOpenTypedFailure::ProfileResetRequired {
                component,
                found_version,
                required_version,
            }) => {
                return TraceDecayError::ProfileResetRequired {
                    component,
                    found_version: *found_version,
                    required_version: *required_version,
                };
            }
            Some(ProjectOpenTypedFailure::ResetRequired { authority, reason }) => {
                return TraceDecayError::ResetRequired {
                    authority: authority.clone(),
                    reason: reason.clone(),
                };
            }
            None => {}
        }
        let message = match self.retry_at {
            Some(retry_at) => format!(
                "{PROJECT_OPEN_FAILURE_RETRY_HINT}; retry after {} ms: {}",
                retry_at
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
                self.message
            ),
            None => self.message.clone(),
        };
        TraceDecayError::Config { message }
    }
}

impl ProjectOpenTaskRegistry {
    fn prune(&mut self, now: Instant) {
        self.routes.retain(|_, entry| {
            let state = entry.state.borrow().clone();
            match state {
                ProjectOpenTaskState::Opening | ProjectOpenTaskState::Ready => {
                    !entry.task.is_finished()
                }
                ProjectOpenTaskState::Failed(failure) => {
                    !entry.task.is_finished() || failure.is_backed_off(now)
                }
            }
        });
        while self.cached_failure_count() > MAX_CACHED_PROJECT_OPEN_FAILURES {
            let Some(route) = self
                .routes
                .iter()
                .filter_map(|(route, entry)| {
                    let ProjectOpenTaskState::Failed(failure) = entry.state.borrow().clone() else {
                        return None;
                    };
                    entry
                        .task
                        .is_finished()
                        .then_some((route.clone(), failure.retry_at))
                })
                .min_by_key(|(_, retry_at)| *retry_at)
                .map(|(route, _)| route)
            else {
                break;
            };
            self.routes.remove(&route);
        }
    }

    fn active_task_count(&self) -> usize {
        self.routes
            .values()
            .filter(|entry| !entry.task.is_finished())
            .count()
    }

    fn cached_failure_count(&self) -> usize {
        self.routes
            .values()
            .filter(|entry| {
                entry.task.is_finished()
                    && matches!(
                        entry.state.borrow().clone(),
                        ProjectOpenTaskState::Failed(_)
                    )
            })
            .count()
    }
}

impl ProjectOpenTasks {
    fn lock_registry(&self) -> StdMutexGuard<'_, ProjectOpenTaskRegistry> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) async fn start<OpenFuture>(
        &self,
        route: ProjectRouteKey,
        open: OpenFuture,
    ) -> ProjectOpenTaskClaim
    where
        OpenFuture: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.start_cancellable(route, |_| open).await
    }

    pub(super) async fn start_cancellable<OpenOperation, OpenFuture>(
        &self,
        route: ProjectRouteKey,
        open: OpenOperation,
    ) -> ProjectOpenTaskClaim
    where
        OpenOperation: FnOnce(CancellationToken) -> OpenFuture + Send + 'static,
        OpenFuture: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let now = Instant::now();
        let mut registry = self.lock_registry();
        registry.prune(now);
        if registry.closed_profiles.contains(&route.profile_root) {
            return ProjectOpenTaskClaim::Failed(ProjectOpenFailure::untyped(
                "project open denied: authenticated profile was remotely deleted".to_owned(),
            ));
        }
        if registry.quiesced_projects.iter().any(|identity| {
            project_route_matches_identity(
                &route,
                &identity.profile_root,
                &identity.project_id,
                &identity.project_roots,
            )
        }) {
            return ProjectOpenTaskClaim::Failed(ProjectOpenFailure::untyped(
                "project open temporarily unavailable during remote recovery".to_owned(),
            ));
        }
        if let Some(entry) = registry.retiring.get(&route) {
            return ProjectOpenTaskClaim::InFlight(entry.state.clone());
        }
        if let Some(entry) = registry.routes.get(&route) {
            let state = entry.state.borrow().clone();
            let receiver = entry.state.clone();
            let finished = entry.task.is_finished();
            match state {
                // The refusal's on-disk basis changed (an operator reset the
                // store): drop the cached route and fall through to a fresh
                // open that re-derives the typed state from disk.
                ProjectOpenTaskState::Failed(failure)
                    if finished && failure.is_stale_for(&route) =>
                {
                    registry.routes.remove(&route);
                }
                ProjectOpenTaskState::Failed(failure) => {
                    return ProjectOpenTaskClaim::Failed(failure);
                }
                ProjectOpenTaskState::Opening | ProjectOpenTaskState::Ready => {
                    return ProjectOpenTaskClaim::InFlight(receiver);
                }
            }
        }
        if registry.active_task_count() >= MAX_TRACKED_PROJECT_OPEN_TASKS {
            return ProjectOpenTaskClaim::Saturated;
        }

        let (updates, state) = tokio::sync::watch::channel(ProjectOpenTaskState::Opening);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (task_completion, completion) = tokio::sync::watch::channel(false);
        let failure_route = route.clone();
        let task = tokio::spawn(async move {
            let _completion = ProjectOpenTaskCompletionFinalizer(task_completion);
            let state = match open(task_cancellation).await {
                Ok(()) => ProjectOpenTaskState::Ready,
                Err(error) => ProjectOpenTaskState::Failed(ProjectOpenFailure::recorded_for_route(
                    &error,
                    &failure_route,
                )),
            };
            updates.send_replace(state);
        });
        registry.routes.insert(
            route,
            ProjectOpenTaskEntry {
                state: state.clone(),
                cancellation,
                completion,
                task,
            },
        );
        ProjectOpenTaskClaim::InFlight(state)
    }

    pub(super) async fn cached_failure(
        &self,
        route: &ProjectRouteKey,
    ) -> Option<ProjectOpenFailure> {
        let now = Instant::now();
        let mut registry = self.lock_registry();
        registry.prune(now);
        let (state, finished) = {
            let entry = registry.routes.get(route)?;
            (entry.state.borrow().clone(), entry.task.is_finished())
        };
        match state {
            ProjectOpenTaskState::Failed(failure) if failure.is_backed_off(now) => {
                if finished && failure.is_stale_for(route) {
                    // The store changed on disk since the refusal was
                    // recorded; the next open must re-derive it.
                    registry.routes.remove(route);
                    return None;
                }
                Some(failure)
            }
            ProjectOpenTaskState::Opening
            | ProjectOpenTaskState::Ready
            | ProjectOpenTaskState::Failed(_) => None,
        }
    }

    /// Waits for the exact route's tracked project-open task to publish its
    /// full owner set. This is deliberately a route-local operation: callers
    /// must re-read the canonical route after it returns rather than carrying
    /// a core publication's stale project identity into LSP admission.
    pub(super) async fn wait_for_lsp_upgrade(
        &self,
        route: &ProjectRouteKey,
        deadline: &tracedecay_application::Deadline,
        request_cancellation: &CancellationToken,
    ) -> ProjectOpenWaitOutcome {
        let mut state = {
            let registry = self.lock_registry();
            let Some(entry) = registry.routes.get(route) else {
                return ProjectOpenWaitOutcome::NotTracked;
            };
            entry.state.clone()
        };

        loop {
            if request_cancellation.is_cancelled() {
                return ProjectOpenWaitOutcome::Cancelled;
            }
            let now = tracedecay_application::clock::now_micros();
            if deadline.is_elapsed_at(now) {
                return ProjectOpenWaitOutcome::TimedOut;
            }
            match state.borrow().clone() {
                ProjectOpenTaskState::Ready => return ProjectOpenWaitOutcome::Completed,
                ProjectOpenTaskState::Failed(failure) => {
                    return ProjectOpenWaitOutcome::Failed(failure.to_error());
                }
                ProjectOpenTaskState::Opening => {}
            }

            let remaining_micros = deadline.expires_at.0.saturating_sub(now.0);
            let Ok(remaining_micros) = u64::try_from(remaining_micros) else {
                return ProjectOpenWaitOutcome::TimedOut;
            };
            let sleep = tokio::time::sleep(Duration::from_micros(remaining_micros));
            tokio::pin!(sleep);
            tokio::select! {
                biased;
                () = request_cancellation.cancelled() => {
                    return ProjectOpenWaitOutcome::Cancelled;
                }
                () = &mut sleep => {
                    return ProjectOpenWaitOutcome::TimedOut;
                }
                changed = state.changed() => {
                    if changed.is_err() {
                        return ProjectOpenWaitOutcome::Failed(TraceDecayError::Config {
                            message: "project open task ended before reporting an outcome".to_owned(),
                        });
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_for_completion(
        mut state: tokio::sync::watch::Receiver<ProjectOpenTaskState>,
    ) -> Result<()> {
        loop {
            let current = state.borrow().clone();
            match current {
                ProjectOpenTaskState::Opening => {
                    state.changed().await.map_err(|_| TraceDecayError::Config {
                        message: "project open task ended before reporting an outcome".to_string(),
                    })?;
                }
                ProjectOpenTaskState::Ready => return Ok(()),
                ProjectOpenTaskState::Failed(failure) => return Err(failure.to_error()),
            }
        }
    }

    pub(super) async fn shutdown(&self) -> bool {
        self.shutdown_with_deadline(DAEMON_TASK_ABORT_DEADLINE, DAEMON_TASK_ABORT_DEADLINE)
            .await
    }

    pub(super) async fn shutdown_project_identity(
        &self,
        profile_root: &Path,
        project_id: &str,
        project_roots: &std::collections::BTreeSet<PathBuf>,
    ) -> bool {
        self.shutdown_project_identity_with_deadline(
            profile_root,
            project_id,
            project_roots,
            DAEMON_TASK_ABORT_DEADLINE,
        )
        .await
    }

    pub(super) async fn quiesce_project_identity(
        &self,
        profile_root: &Path,
        project_id: &str,
        project_roots: &BTreeSet<PathBuf>,
    ) -> Option<ProjectOpenIdentityQuiescenceV1> {
        let identity = ProjectOpenIdentityV1 {
            profile_root: profile_root.to_path_buf(),
            project_id: project_id.to_owned(),
            project_roots: project_roots.clone(),
        };
        let routes = {
            let mut registry = self.lock_registry();
            if !registry.quiesced_projects.insert(identity.clone()) {
                return None;
            }
            project_routes_for_retirement(&mut registry, &identity)
        };
        if !self
            .drain_retiring_routes(routes, DAEMON_TASK_ABORT_DEADLINE)
            .await
        {
            self.lock_registry().quiesced_projects.remove(&identity);
            return None;
        }
        Some(ProjectOpenIdentityQuiescenceV1 {
            tasks: self.clone(),
            identity,
        })
    }

    pub(super) async fn shutdown_project_identity_with_deadline(
        &self,
        profile_root: &Path,
        project_id: &str,
        project_roots: &std::collections::BTreeSet<PathBuf>,
        timeout: Duration,
    ) -> bool {
        let routes = {
            let mut registry = self.lock_registry();
            project_routes_for_retirement(
                &mut registry,
                &ProjectOpenIdentityV1 {
                    profile_root: profile_root.to_path_buf(),
                    project_id: project_id.to_owned(),
                    project_roots: project_roots.clone(),
                },
            )
        };
        self.drain_retiring_routes(routes, timeout).await
    }

    pub(super) async fn shutdown_profile_with_deadline(
        &self,
        profile_root: &Path,
        timeout: Duration,
    ) -> bool {
        let routes = {
            let mut registry = self.lock_registry();
            registry.closed_profiles.insert(profile_root.to_path_buf());
            let active = registry
                .routes
                .keys()
                .filter(|route| route.profile_root == profile_root)
                .cloned()
                .collect::<Vec<_>>();
            for route in active {
                if let Some(entry) = registry.routes.remove(&route) {
                    entry.cancellation.cancel();
                    registry.retiring.insert(route, entry);
                }
            }
            registry
                .retiring
                .keys()
                .filter(|route| route.profile_root == profile_root)
                .cloned()
                .collect::<Vec<_>>()
        };
        self.drain_retiring_routes(routes, timeout).await
    }

    pub(super) async fn shutdown_with_deadline(
        &self,
        cooperative_deadline: Duration,
        post_abort_deadline: Duration,
    ) -> bool {
        {
            let mut registry = self.lock_registry();
            for (route, entry) in std::mem::take(&mut registry.routes) {
                entry.cancellation.cancel();
                registry.retiring.insert(route, entry);
            }
        }
        let routes = self
            .lock_registry()
            .retiring
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if self
            .drain_retiring_routes(routes.clone(), cooperative_deadline)
            .await
        {
            return true;
        }
        // The cooperative window expired. An open that ignores its
        // cancellation token must not leak a tracked task past daemon
        // shutdown, so abort what is still running and give the aborts their
        // own bounded window; the task's completion finalizer fires on abort.
        {
            let registry = self.lock_registry();
            for route in &routes {
                if let Some(entry) = registry.retiring.get(route) {
                    entry.task.abort();
                }
            }
        }
        self.drain_retiring_routes(routes, post_abort_deadline)
            .await
    }

    async fn drain_retiring_routes(&self, routes: Vec<ProjectRouteKey>, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        let completions = {
            let mut registry = self.lock_registry();
            routes
                .into_iter()
                .filter_map(|route| {
                    let entry = registry.retiring.get_mut(&route)?;
                    entry.cancellation.cancel();
                    Some((route, entry.completion.clone()))
                })
                .collect::<Vec<_>>()
        };
        let mut joined = Vec::new();
        let mut drained = true;
        for (route, completion) in completions {
            match tokio::time::timeout_at(deadline, wait_for_project_open_task(completion)).await {
                Ok(()) => joined.push(route),
                Err(_) => drained = false,
            }
        }
        let mut registry = self.lock_registry();
        for route in joined {
            registry.retiring.remove(&route);
        }
        drained
    }

    #[cfg(test)]
    pub(super) async fn tracked_task_count(&self) -> usize {
        let mut registry = self.lock_registry();
        registry.prune(Instant::now());
        let active = registry
            .routes
            .values()
            .filter(|entry| !entry.task.is_finished())
            .count();
        active
            + registry
                .retiring
                .values()
                .filter(|entry| !entry.task.is_finished())
                .count()
    }

    #[cfg(test)]
    pub(super) async fn tracked_route_count(&self) -> usize {
        let mut registry = self.lock_registry();
        registry.prune(Instant::now());
        registry.routes.len() + registry.retiring.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectServerRequirement {
    Core,
    RegisteredHostIngest,
}

pub(super) fn project_server_requirement(request_line: &str) -> ProjectServerRequirement {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(request_line.trim()) else {
        return ProjectServerRequirement::Core;
    };
    match classify_mcp_method(&request.method) {
        McpMethod::HookEvent => ProjectServerRequirement::RegisteredHostIngest,
        McpMethod::ToolsCall => match projectless_tool_call(request.params.as_ref()) {
            Ok(("tracedecay_hook_runtime", arguments))
                if arguments.get("action").and_then(serde_json::Value::as_str)
                    == Some("reset_counter") =>
            {
                ProjectServerRequirement::Core
            }
            Ok(("tracedecay_hook_runtime", _)) => ProjectServerRequirement::RegisteredHostIngest,
            _ => ProjectServerRequirement::Core,
        },
        _ => ProjectServerRequirement::Core,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectServerPublication {
    Pending,
    Core,
    RegisteredHostIngest,
}

impl ProjectServerPublication {
    pub(super) fn satisfies(self, requirement: ProjectServerRequirement) -> bool {
        match requirement {
            ProjectServerRequirement::Core => self != Self::Pending,
            ProjectServerRequirement::RegisteredHostIngest => self == Self::RegisteredHostIngest,
        }
    }
}

impl StoreOwnerKey {
    pub(super) fn from_paths(
        profile_root: &Path,
        global_db_path: &Path,
        project_id: Option<String>,
        store_root: &Path,
        graph_db_path: &Path,
    ) -> Result<Self> {
        Ok(Self {
            profile_root: authority::canonical_identity_path(profile_root)?,
            global_db_path: authority::canonical_identity_path(global_db_path)?,
            project_id,
            store_root: authority::canonical_identity_path(store_root)?,
            graph_db_path: authority::canonical_identity_path(graph_db_path)?,
        })
    }
}

impl ProjectRouteKey {
    pub(super) fn from_handshake(project_path: &Path, handshake: &DaemonHandshake) -> Result<Self> {
        Ok(Self {
            profile_root: authority::canonical_identity_path(
                &handshake.client_identity.profile_root,
            )?,
            global_db_path: authority::canonical_identity_path(
                &handshake.client_identity.global_db_path,
            )?,
            project_path: authority::canonical_identity_path(project_path)?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}

impl ProjectServerKey {
    pub(super) fn from_open_project(
        cg: &crate::tracedecay::TraceDecay,
        handshake: &DaemonHandshake,
    ) -> Result<Self> {
        let layout = cg.store_layout();
        Ok(Self {
            owner: StoreOwnerKey::from_paths(
                &handshake.client_identity.profile_root,
                &handshake.client_identity.global_db_path,
                layout.identity.project_id.clone(),
                &layout.data_root,
                &layout.graph_db_path,
            )?,
            project_root: authority::canonical_identity_path(cg.project_root())?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}

#[cfg(test)]
mod typed_failure_tests {
    use super::*;

    #[test]
    fn cached_project_open_failure_preserves_workflow_reset_authority() {
        let error = TraceDecayError::reset_required("workflow", "partial workflow schema");
        let failure = ProjectOpenFailure::from_error(&error);

        assert!(matches!(
            failure.to_error(),
            TraceDecayError::ResetRequired {
                ref authority,
                ref reason,
            } if authority == "workflow" && reason == "partial workflow schema"
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod refused_store_invalidation_tests {
    use super::*;

    fn route_for(profile_root: &Path, project_root: &Path) -> ProjectRouteKey {
        ProjectRouteKey {
            profile_root: profile_root.to_path_buf(),
            global_db_path: profile_root.join("global.db"),
            project_path: project_root.to_path_buf(),
            scope_prefix: None,
        }
    }

    fn store_data_root(profile_root: &Path, project_root: &Path) -> PathBuf {
        let project_id = crate::storage::default_profile_project_id(project_root);
        crate::storage::profile_sharded_data_root(profile_root, &project_id)
    }

    fn seed_refused_store(profile_root: &Path, project_root: &Path) -> PathBuf {
        let data_root = store_data_root(profile_root, project_root);
        std::fs::create_dir_all(&data_root).unwrap();
        let db_path = data_root.join(crate::config::db_filename(&data_root));
        std::fs::write(&db_path, b"refused-store-stand-in").unwrap();
        db_path
    }

    async fn record_reset_required_failure(tasks: &ProjectOpenTasks, route: ProjectRouteKey) {
        let claim = tasks
            .start_cancellable(route, |_| async {
                Err(TraceDecayError::reset_required(
                    "graph",
                    "unsupported schema version 18",
                ))
            })
            .await;
        let ProjectOpenTaskClaim::InFlight(state) = claim else {
            panic!("the first open must start a tracked task");
        };
        assert!(
            ProjectOpenTasks::wait_for_completion(state).await.is_err(),
            "the scripted open must record its refusal"
        );
        // The watch publishes `Failed` just before the task itself finishes,
        // and staleness eviction only acts on finished tasks.
        while tasks.tracked_task_count().await > 0 {
            tokio::task::yield_now().await;
        }
    }

    /// Live recovery bug: after `storage reset-project-store` deleted the
    /// refused graph DB on disk, the daemon kept serving the recorded
    /// `ResetRequired` from this cache until it was restarted. An unchanged
    /// store keeps its backed-off refusal, but a refusal whose on-disk basis
    /// changed must be dropped so the next open re-derives the state.
    #[tokio::test]
    async fn on_disk_reset_invalidates_the_cached_refusal_without_restart() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let db_path = seed_refused_store(&profile_root, &project_root);
        let route = route_for(&profile_root, &project_root);
        let tasks = ProjectOpenTasks::default();
        record_reset_required_failure(&tasks, route.clone()).await;

        // Unchanged store: the refusal stays cached and backed off.
        assert!(tasks.cached_failure(&route).await.is_some());
        let claim = tasks
            .start_cancellable(route.clone(), |_| async { Ok(()) })
            .await;
        assert!(
            matches!(claim, ProjectOpenTaskClaim::Failed(_)),
            "an unchanged refused store must keep declining reopen"
        );

        // The operator reset deletes the refused graph DB on disk.
        std::fs::remove_file(&db_path).unwrap();

        assert!(
            tasks.cached_failure(&route).await.is_none(),
            "a refusal recorded against deleted files must not be served"
        );
        let claim = tasks
            .start_cancellable(route.clone(), |_| async { Ok(()) })
            .await;
        let ProjectOpenTaskClaim::InFlight(state) = claim else {
            panic!("the reset store must admit a fresh open without a daemon restart");
        };
        ProjectOpenTasks::wait_for_completion(state).await.unwrap();
    }

    /// Per-branch graph DBs are part of the refused store's fingerprint, so
    /// a reset that removes only `branches/*.db` also clears the refusal.
    #[tokio::test]
    async fn branch_graph_db_reset_invalidates_the_cached_refusal() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        seed_refused_store(&profile_root, &project_root);
        let branches_dir = store_data_root(&profile_root, &project_root).join("branches");
        std::fs::create_dir_all(&branches_dir).unwrap();
        let branch_db = branches_dir.join("develop.db");
        std::fs::write(&branch_db, b"refused-branch-stand-in").unwrap();
        let route = route_for(&profile_root, &project_root);
        let tasks = ProjectOpenTasks::default();
        record_reset_required_failure(&tasks, route.clone()).await;
        assert!(tasks.cached_failure(&route).await.is_some());

        std::fs::remove_file(&branch_db).unwrap();

        assert!(
            tasks.cached_failure(&route).await.is_none(),
            "a branch graph DB reset must invalidate the cached refusal"
        );
    }

    /// The invalidation is scoped to typed `ResetRequired` refusals: other
    /// backed-off failures carry no store fingerprint and keep their plain
    /// time-based backoff even when store files change.
    #[tokio::test]
    async fn non_reset_failures_keep_their_backoff_when_files_change() {
        let temp = tempfile::TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let db_path = seed_refused_store(&profile_root, &project_root);
        let route = route_for(&profile_root, &project_root);
        let tasks = ProjectOpenTasks::default();
        let claim = tasks
            .start_cancellable(route.clone(), |_| async {
                Err(TraceDecayError::Database {
                    operation: "ensure global database authority invariants".to_string(),
                    message: "persisted row violates an invariant".to_string(),
                })
            })
            .await;
        let ProjectOpenTaskClaim::InFlight(state) = claim else {
            panic!("the first open must start a tracked task");
        };
        assert!(ProjectOpenTasks::wait_for_completion(state).await.is_err());

        std::fs::remove_file(&db_path).unwrap();

        assert!(
            tasks.cached_failure(&route).await.is_some(),
            "non-ResetRequired backoffs are time-based and must survive file churn"
        );
    }
}

#[cfg(test)]
mod lsp_upgrade_tests;
#[cfg(test)]
mod quiescence_tests;
