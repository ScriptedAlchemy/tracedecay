//! Project-open admission bookkeeping: route/owner keys, open gates, and the
//! in-flight open-task registry.
//!
//! Tracks which project opens are running, which failed, and how long a failed
//! open stays backed off, so a repeated request neither stampedes nor retries a
//! known-unrepairable store.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

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
    registry: Arc<tokio::sync::Mutex<ProjectOpenTaskRegistry>>,
}

#[derive(Default)]
struct ProjectOpenTaskRegistry {
    routes: HashMap<ProjectRouteKey, ProjectOpenTaskEntry>,
}

struct ProjectOpenTaskEntry {
    state: tokio::sync::watch::Receiver<ProjectOpenTaskState>,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
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
}

pub(super) enum ProjectOpenTaskClaim {
    InFlight(tokio::sync::watch::Receiver<ProjectOpenTaskState>),
    Failed(ProjectOpenFailure),
    Saturated,
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
        _ => None,
    }
}

impl ProjectOpenFailure {
    fn from_error(error: &TraceDecayError) -> Self {
        // Operator-repairable authority rejections decline implicit repair.
        // Reopening before maintenance changes that state is not useful and
        // only multiplies daemon warm-up tasks.
        let retry_at = project_open_retry_backoff(error).map(|backoff| Instant::now() + backoff);
        Self {
            message: error.to_string(),
            retry_at,
        }
    }

    fn is_backed_off(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|retry_at| retry_at > now)
    }

    pub(super) fn to_error(&self) -> TraceDecayError {
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
        let mut registry = self.registry.lock().await;
        registry.prune(now);
        if let Some(entry) = registry.routes.get(&route) {
            return match entry.state.borrow().clone() {
                ProjectOpenTaskState::Failed(failure) => ProjectOpenTaskClaim::Failed(failure),
                ProjectOpenTaskState::Opening | ProjectOpenTaskState::Ready => {
                    ProjectOpenTaskClaim::InFlight(entry.state.clone())
                }
            };
        }
        if registry.active_task_count() >= MAX_TRACKED_PROJECT_OPEN_TASKS {
            return ProjectOpenTaskClaim::Saturated;
        }

        let (updates, state) = tokio::sync::watch::channel(ProjectOpenTaskState::Opening);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let state = match open(task_cancellation).await {
                Ok(()) => ProjectOpenTaskState::Ready,
                Err(error) => ProjectOpenTaskState::Failed(ProjectOpenFailure::from_error(&error)),
            };
            updates.send_replace(state);
        });
        registry.routes.insert(
            route,
            ProjectOpenTaskEntry {
                state: state.clone(),
                cancellation,
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
        let mut registry = self.registry.lock().await;
        registry.prune(now);
        let entry = registry.routes.get(route)?;
        match entry.state.borrow().clone() {
            ProjectOpenTaskState::Failed(failure) if failure.is_backed_off(now) => Some(failure),
            ProjectOpenTaskState::Opening
            | ProjectOpenTaskState::Ready
            | ProjectOpenTaskState::Failed(_) => None,
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

    pub(super) async fn shutdown_with_deadline(
        &self,
        cooperative_deadline: Duration,
        post_abort_deadline: Duration,
    ) -> bool {
        let mut entries = {
            let mut registry = self.registry.lock().await;
            std::mem::take(&mut registry.routes)
        }
        .into_values()
        .collect::<Vec<_>>();
        for entry in &entries {
            entry.cancellation.cancel();
        }
        let cooperative_deadline = tokio::time::Instant::now() + cooperative_deadline;
        let mut drained = true;
        for entry in &mut entries {
            if tokio::time::timeout_at(cooperative_deadline, &mut entry.task)
                .await
                .is_err()
            {
                drained = false;
                entry.task.abort();
            }
        }
        if !drained {
            let post_abort_deadline = tokio::time::Instant::now() + post_abort_deadline;
            for entry in &mut entries {
                if entry.task.is_finished() {
                    continue;
                }
                let _ = tokio::time::timeout_at(post_abort_deadline, &mut entry.task).await;
            }
        }
        if !drained {
            log_daemon_event(
                "project_server_warmup",
                &[("outcome", "shutdown_abort_timeout".to_string())],
            );
        }
        drained
    }

    #[cfg(test)]
    pub(super) async fn tracked_task_count(&self) -> usize {
        let mut registry = self.registry.lock().await;
        registry.prune(Instant::now());
        registry
            .routes
            .values()
            .filter(|entry| !entry.task.is_finished())
            .count()
    }

    #[cfg(test)]
    pub(super) async fn tracked_route_count(&self) -> usize {
        let mut registry = self.registry.lock().await;
        registry.prune(Instant::now());
        registry.routes.len()
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
                &cg.db_path(),
            )?,
            project_root: authority::canonical_identity_path(cg.project_root())?,
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}
