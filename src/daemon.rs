use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::ReplayTransport;
use crate::mcp::server::{McpMethod, SERVER_INSTRUCTIONS, classify_mcp_method, initialize_result};
use crate::mcp::tools::{
    explore_call_budget, get_tool_definitions_with_budget, get_tool_definitions_with_warming_budget,
};
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};
use branch_add::{branch_add_response, coordinated_hook_branch_writer, parse_branch_add_request};
use branch_admin::{StoreAdministration, parse_branch_admin_request, write_branch_admin_response};
#[cfg(all(unix, test))]
use memory_repair_scheduler::{
    MemoryRepairPassDecision, MemoryRepairSchedulerHandle, MemoryRepairTickOutcome,
    legacy_memory_cutover_should_retry, memory_repair_tick_outcome,
    run_memory_repair_scheduler_tick,
};
#[cfg(all(unix, test))]
use scheduler::{
    AutomationSchedulerHandle, automation_scheduler_configured,
    automation_scheduler_tick_secs_for_project, automation_staged_log_fields,
    daemon_scheduler_record_log_line, run_automation_scheduler_tick, scheduler_task_log_fields,
    user_config_for_client,
};
use transport::{BrokerListener, BrokerStream, DaemonAuthPreface, DaemonEndpoint};

pub const SERVICE_NAME: &str = "tracedecay.service";
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";
pub(crate) const PROJECT_WARMING_RETRY_HINT: &str =
    "is warming in the background; retry the same tool shortly";
#[cfg(unix)]
const TOOL_LIST_CHANGED_METHOD: &str = "notifications/tools/list_changed";
#[cfg(unix)]
const MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION: usize = 1_024;
const MAX_CACHED_PROJECT_SERVERS: usize = 8;
const MAX_TRACKED_PROJECT_OPEN_TASKS: usize = MAX_CACHED_PROJECT_SERVERS;
const PROJECT_OPEN_REQUEST_DEADLINE: Duration = Duration::from_millis(500);
const PROJECT_OPEN_FAILURE_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const PROJECT_OPEN_FAILURE_RETRY_HINT: &str =
    "project route open is backed off after an invariant rejection";

mod authority;
mod branch_add;
mod branch_admin;
mod core_admission;
mod core_client;
mod core_doctor;
mod core_handshake;
mod core_hooks;
mod core_lifecycle;
mod core_logging;
mod core_proxy;
pub(crate) use core_admission::*;
pub use core_client::*;
pub(crate) use core_doctor::*;
pub use core_handshake::*;
pub use core_hooks::*;
pub(crate) use core_lifecycle::*;
pub use core_logging::*;
pub use core_proxy::*;
mod git_transactions;
#[cfg(unix)]
mod git_watch;
#[cfg(unix)]
mod memory_repair_scheduler;
#[cfg(unix)]
pub mod pr_autotrack;
mod profile_host_admission_replay;
#[cfg(unix)]
mod scheduler;
mod service;
pub(crate) mod session_temporal_refresh_scheduler;
pub(crate) mod transport;
pub use service::{
    DaemonServiceSpec, DaemonServiceState, QuiescedDaemonLifecycle, daemon_reachable,
    default_socket_path, enforce_forward_only_service_recovery, install_service,
    installed_service_socket_path, quiesce_installed_service_before_lease,
    refresh_installed_service, refresh_installed_service_under_lease,
    refresh_installed_service_under_lease_with_state, refresh_service,
    restore_installed_service_after_update, service_spec, service_status, socket_path_or_default,
    uninstall_service, verify_installed_service_quiesced_under_lease,
    wait_for_installed_service_state, with_exclusive_maintenance_window,
    with_quiesced_installed_service,
};

#[cfg(unix)]
pub async fn run_foreground(socket_path: PathBuf) -> Result<()> {
    run_foreground_unix(socket_path).await
}

#[cfg(not(unix))]
pub async fn run_foreground(_socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let requested = transport::default_loopback_endpoint();
    let _lifecycle_lease = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &requested, binary_version())?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let (listener, endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&endpoint)?;
    log_daemon_event("daemon_listening", &[("endpoint", endpoint.to_string())]);

    let lifecycle = DaemonLifecycle::default();
    let store_administration = StoreAdministration::default();
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let per_client_admission = DaemonPerClientAdmission::default();
    let mut clients: JoinSet<Result<()>> = JoinSet::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(Err(error)) = completed {
                    log_daemon_event("daemon_client", &[("outcome", error.to_string())]);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
        };
        let permit = match admission.try_admit() {
            DaemonClientAdmissionOutcome::Admitted(permit) => permit,
            DaemonClientAdmissionOutcome::Saturated(response) => {
                reject_saturated_daemon_client(stream, response).await;
                continue;
            }
        };
        let admission_class = permit.class();
        let auth_token = authority.auth_token().to_string();
        let client_lifecycle = lifecycle.clone();
        let store_administration = store_administration.clone();
        let project_open_gates = Arc::clone(&project_open_gates);
        let per_client_admission = per_client_admission.clone();
        clients.spawn(async move {
            let _permit = permit;
            Box::pin(serve_windows_broker_client_with_class(
                stream,
                &auth_token,
                &client_lifecycle,
                store_administration,
                project_open_gates,
                per_client_admission,
                admission_class,
                #[cfg(test)]
                None,
            ))
            .await
        });
    }
    lifecycle.begin_draining();
    shutdown_portable_project_open_tasks(project_open_gates.as_ref()).await;
    let in_flight_drained = timeout(DAEMON_CLIENT_DRAIN_DEADLINE, lifecycle.wait_for_idle())
        .await
        .is_ok();
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    store_administration.shutdown_host_admission_replay().await;
    if !in_flight_drained {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "client_drain_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_CLIENT_DRAIN_DEADLINE.as_secs().to_string(),
                ),
                (
                    "checkpoint",
                    "skipped_active_clients_were_aborted".to_string(),
                ),
            ],
        );
        return endpoint_cleanup;
    }
    shutdown_project_servers(&store_administration).await;
    endpoint_cleanup
}

#[cfg(unix)]
async fn run_foreground_unix(socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let endpoint = transport::DaemonEndpoint::Unix(socket_path);
    let _lifecycle = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &endpoint, binary_version())?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path.clone(),
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    if let Some(parent) = socket_path.parent() {
        let parent_existed = parent.exists();
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to create socket directory '{}': {e}",
                parent.display()
            ),
        })?;
        if !parent_existed {
            set_owner_only_permissions(parent, 0o700)?;
        }
    }
    prepare_socket_path(&authority).await?;

    let (listener, bound_endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&bound_endpoint)?;
    set_owner_only_permissions(&socket_path, 0o600)?;
    log_daemon_event(
        "daemon_listening",
        &[("endpoint", bound_endpoint.to_string())],
    );
    let engine = DaemonEngine::default();
    // Install the git-metadata watcher (design D3/D5). The daemon has no single
    // project root, so it uses the default `[sync]` config plus env overrides.
    // When `auto_watch` is off the watcher is inert. The watcher shares the
    // engine's administration coordinator before it can spawn any writer.
    let git_watcher = git_watch::GitWatcher::new_with_administration(
        crate::config::SyncConfig::default().with_env_overrides(),
        engine.store_administration.clone(),
        profile_root.clone(),
    );
    git_watcher.spawn(crate::global_db::global_db_path()).await;
    // PR-branch auto-tracking runs independently of the metadata watcher: it is
    // gated per-project on `sync.auto_track_pr_branches` (default off), so this
    // loop is inert unless a project opts in.
    let pr_autotrack_task = pr_autotrack::spawn_with_administration(
        crate::global_db::global_db_path(),
        engine.store_administration.clone(),
    );
    let engine = engine
        .with_git_watcher(git_watcher)
        .with_pr_autotrack_task(pr_autotrack_task)
        .await;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let mut client_tasks: JoinSet<Result<()>> = JoinSet::new();

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = client_tasks.join_next(), if !client_tasks.is_empty() => {
                if let Some(completed) = completed {
                    log_client_task_result(completed);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        };
        let permit = match admission.try_admit() {
            DaemonClientAdmissionOutcome::Admitted(permit) => permit,
            DaemonClientAdmissionOutcome::Saturated(response) => {
                reject_saturated_daemon_client(stream, response).await;
                continue;
            }
        };
        let admission_class = permit.class();
        let engine = engine.clone();
        let auth_token = authority.auth_token().to_string();
        client_tasks.spawn(async move {
            let _permit = permit;
            Box::pin(serve_authenticated_socket_client_with_class(
                stream,
                engine,
                auth_token,
                admission_class,
            ))
            .await
        });
    }
    engine.lifecycle.begin_draining();
    engine.shutdown_project_open_tasks().await;
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    // Keep auxiliary process creation blocked until every scheduler and client
    // task is drained or abandoned. A killed app-server call may retry before
    // unwinding, so a shorter guard leaves a shutdown-time respawn race.
    let _codex_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
    // Stop automation before announcing shutdown or waiting for clients.
    // Scheduler tasks may be inside a synchronous auxiliary-agent call, so
    // shutdown also terminates their tracked process trees before joining.
    engine.shutdown_automation_schedulers().await;
    engine.shutdown_memory_repair_schedulers().await;
    log_daemon_event(
        "daemon_shutdown",
        &[("socket", socket_path.display().to_string())],
    );
    let in_flight_drained = timeout(
        DAEMON_CLIENT_DRAIN_DEADLINE,
        engine.lifecycle.wait_for_idle(),
    )
    .await
    .is_ok();
    // Once admitted requests are finished (or their bound elapsed), every
    // remaining client task is an idle socket reader or already-cancelled
    // request wrapper. Abort those immediately instead of making shutdown wait
    // for clients to close persistent connections themselves.
    client_tasks.abort_all();
    let clients_drained = drain_client_tasks(&mut client_tasks, DAEMON_TASK_ABORT_DEADLINE).await;
    // Client setup and in-flight requests may create schedulers or project
    // servers. Sweep owned background tasks only after all client work drains.
    engine.shutdown_background_tasks().await;
    if !in_flight_drained || !clients_drained {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "client_drain_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_CLIENT_DRAIN_DEADLINE.as_secs().to_string(),
                ),
                (
                    "checkpoint",
                    "skipped_active_clients_were_aborted".to_string(),
                ),
            ],
        );
        return endpoint_cleanup;
    }
    // Graceful shutdown persists tokens-saved counters and checkpoints WALs
    // for every live project server sequentially; with many servers or large
    // WALs that can exceed systemd's stop timeout, which then sends `SIGKILL`
    // to the daemon. On timeout the shutdown future is dropped and we proceed
    // to exit: the remaining persistence is best-effort and the database WAL
    // keeps state crash-safe.
    let completed = timeout(DAEMON_SHUTDOWN_DEADLINE, engine.shutdown_servers())
        .await
        .is_ok();
    if !completed {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_SHUTDOWN_DEADLINE.as_secs().to_string(),
                ),
            ],
        );
    }
    endpoint_cleanup
}

#[cfg(unix)]
fn log_client_task_result(completed: std::result::Result<Result<()>, tokio::task::JoinError>) {
    let error = match completed {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error.to_string(),
        Err(error) if error.is_cancelled() => return,
        Err(error) => error.to_string(),
    };
    log_daemon_event(
        "daemon_client",
        &[("outcome", "error".to_string()), ("error", error)],
    );
}

#[cfg(unix)]
async fn drain_client_tasks(clients: &mut JoinSet<Result<()>>, deadline: Duration) -> bool {
    let drained = timeout(deadline, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await
    .is_ok();
    if drained {
        return true;
    }

    clients.abort_all();
    let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await;
    false
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path, mode: u32) -> Result<()> {
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to restrict permissions on '{}': {e}",
            path.display()
        ),
    })
}

#[cfg(unix)]
async fn prepare_socket_path(authority: &authority::DaemonAuthority) -> Result<()> {
    authority.ensure_current()?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path,
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    match UnixStream::connect(socket_path).await {
        Ok(_) => Err(TraceDecayError::Config {
            message: format!(
                "daemon socket '{}' is already in use",
                socket_path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => std::fs::remove_file(socket_path).map_err(|remove_err| TraceDecayError::Config {
            message: format!(
                "failed to remove stale daemon socket '{}': {remove_err}",
                socket_path.display()
            ),
        }),
    }
}

#[cfg(unix)]
#[derive(Clone, Default)]
struct DaemonEngine {
    lifecycle: DaemonLifecycle,
    /// Lightweight per-proxy leases keep one reconnecting client from
    /// consuming every bulk slot while preserving reserved control capacity.
    per_client_admission: DaemonPerClientAdmission,
    /// One coordinator owns the project-server registry, scheduler registry,
    /// and the writer gate that orders all mutations of either identity map.
    store_administration: StoreAdministration,
    /// Per-canonical-route gates plus a bounded, route-local warm-up task
    /// registry. Weak gates disappear after the last waiter; deterministic
    /// route failures remain only for their short retry backoff.
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    /// Per-logical-owner transition guards. Task-map locks are released before
    /// stale owners are awaited; this guard alone spans retirement so a
    /// concurrent activation or rekey cannot publish a replacement early.
    maintenance_transition_gates: Arc<tokio::sync::Mutex<MaintenanceTransitionGates>>,
    #[cfg(test)]
    project_open_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    memory_repair_start_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    automation_config_probe_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    automation_configured_override: Arc<AtomicBool>,
    #[cfg(test)]
    automation_scheduler_exit_barrier:
        Arc<tokio::sync::Mutex<Option<Arc<scheduler::AutomationSchedulerExitBarrier>>>>,
    #[cfg(test)]
    automation_scheduler_state_changed: Arc<tokio::sync::Notify>,
    /// Client versions whose skew was already logged. Proxy clients reconnect
    /// per request, so without this the mismatch would flood the daemon log.
    logged_client_version_skews: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Client processes already told to refresh their tool catalog during
    /// this daemon generation. The set is process-local by design: a daemon
    /// restart creates a new generation and permits one fresh notification.
    catalog_refresh_notified_clients: Arc<tokio::sync::Mutex<HashSet<CatalogRefreshClientKey>>>,
    /// Prevents capacity exhaustion from flooding the daemon log.
    catalog_refresh_saturation_logged: Arc<AtomicBool>,
    /// Git-metadata watcher (design D3/D5). Default-constructed inert; the real
    /// config-driven watcher is installed by `run_foreground_unix` via
    /// [`DaemonEngine::with_git_watcher`] before the accept loop starts.
    git_watcher: git_watch::GitWatcher,
    /// PR reconciliation task, retained so shutdown never leaves it writing.
    pr_autotrack_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

/// Retain one daemon-owned Git index transaction service for the project store
/// and reconcile any durable records before the project server can advertise
/// tools. The service owns the store actor; constructing a second service for
/// the same database is rejected by the registry.
async fn ensure_git_index_transactions_before_advertising(
    store_administration: &StoreAdministration,
    session_db: Arc<crate::global_db::GlobalDb>,
    project_root: &Path,
    project_id: Option<&str>,
) -> Result<()> {
    let Some(project_id) = project_id else {
        // Linked/anonymous project opens without a durable project id cannot
        // own index-mutation authority; skip rather than invent an identity.
        return Ok(());
    };
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("git index transaction project identity is invalid: {error}"),
        }
    })?;
    let repository_root = crate::worktree::git_worktree_root(project_root)
        .unwrap_or_else(|| project_root.to_path_buf());
    let observed_at = tracedecay_domain::UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_micros())
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX),
    );
    store_administration
        .git_index_transaction_services()
        .ensure(session_db, repository_root, project_id, observed_at)
        .await
        .map(|_| ())
        .map_err(|error| TraceDecayError::Config {
            message: format!("git index transaction startup did not complete: {error}"),
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectServerKey {
    owner: StoreOwnerKey,
    scope_prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct StoreOwnerKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    project_id: Option<String>,
    store_root: PathBuf,
    graph_db_path: PathBuf,
}

/// A client route known before any project database is opened. This is the
/// cache/singleflight key; [`ProjectServerKey`] remains the post-open physical
/// owner key so linked aliases and branch DBs still converge correctly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectRouteKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    project_path: PathBuf,
    scope_prefix: Option<String>,
}

type ProjectOpenGate = tokio::sync::Mutex<()>;
#[derive(Default)]
struct ProjectOpenGates {
    gates: HashMap<ProjectRouteKey, std::sync::Weak<ProjectOpenGate>>,
    tasks: ProjectOpenTasks,
}
type MaintenanceTransitionGate = tokio::sync::Mutex<()>;
type MaintenanceTransitionGates =
    HashMap<MaintenanceTransitionKey, std::sync::Weak<MaintenanceTransitionGate>>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MaintenanceTransitionKey {
    profile_root: PathBuf,
    project_id: Option<String>,
    scope_prefix: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MaintenanceRekeyOutcome {
    Completed,
    Retiring,
}

/// Route-local project-open work. A route owns at most one task, and
/// deterministic configuration failures retain a short backoff record so a
/// reconnecting MCP host cannot repeatedly reopen the same rejected store.
#[derive(Clone, Default)]
struct ProjectOpenTasks {
    registry: Arc<tokio::sync::Mutex<ProjectOpenTaskRegistry>>,
}

#[derive(Default)]
struct ProjectOpenTaskRegistry {
    routes: HashMap<ProjectRouteKey, ProjectOpenTaskEntry>,
}

struct ProjectOpenTaskEntry {
    state: tokio::sync::watch::Receiver<ProjectOpenTaskState>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
enum ProjectOpenTaskState {
    Opening,
    Ready,
    Failed(ProjectOpenFailure),
}

#[derive(Clone)]
struct ProjectOpenFailure {
    message: String,
    retry_at: Option<Instant>,
}

enum ProjectOpenTaskClaim {
    InFlight(tokio::sync::watch::Receiver<ProjectOpenTaskState>),
    Failed(ProjectOpenFailure),
    Saturated,
}

fn is_invariant_rejected_project_route(error: &TraceDecayError) -> bool {
    match error {
        TraceDecayError::Config { message } => {
            message.contains("identity cutover conflict")
                || message.contains("ambiguous legacy profile stores")
                || message.contains("enrollment marker did not resolve a profile store")
        }
        TraceDecayError::Database { message, operation } => {
            operation == "ensure global database authority invariants"
                && message.contains("session temporal receipts or cursor keys are mutable")
        }
        _ => false,
    }
}

impl ProjectOpenFailure {
    fn from_error(error: &TraceDecayError) -> Self {
        // Operator-repairable authority rejections decline implicit repair.
        // Reopening before maintenance changes that state is not useful and
        // only multiplies daemon warm-up tasks.
        let retry_at = is_invariant_rejected_project_route(error)
            .then(|| Instant::now() + PROJECT_OPEN_FAILURE_RETRY_BACKOFF);
        Self {
            message: error.to_string(),
            retry_at,
        }
    }

    fn is_backed_off(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|retry_at| retry_at > now)
    }

    fn to_error(&self) -> TraceDecayError {
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
    }
}

impl ProjectOpenTasks {
    async fn start<OpenFuture>(
        &self,
        route: ProjectRouteKey,
        open: OpenFuture,
    ) -> ProjectOpenTaskClaim
    where
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
        if registry.routes.len() >= MAX_TRACKED_PROJECT_OPEN_TASKS {
            return ProjectOpenTaskClaim::Saturated;
        }

        let (updates, state) = tokio::sync::watch::channel(ProjectOpenTaskState::Opening);
        let task = tokio::spawn(async move {
            let state = match open.await {
                Ok(()) => ProjectOpenTaskState::Ready,
                Err(error) => ProjectOpenTaskState::Failed(ProjectOpenFailure::from_error(&error)),
            };
            updates.send_replace(state);
        });
        registry.routes.insert(
            route,
            ProjectOpenTaskEntry {
                state: state.clone(),
                task,
            },
        );
        ProjectOpenTaskClaim::InFlight(state)
    }

    async fn wait_for_completion(
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

    async fn cached_failure(&self, route: &ProjectRouteKey) -> Option<ProjectOpenFailure> {
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

    async fn shutdown(&self) {
        let entries = {
            let mut registry = self.registry.lock().await;
            std::mem::take(&mut registry.routes)
        };
        for entry in entries.values() {
            entry.task.abort();
        }
        let drained = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            for entry in entries.into_values() {
                let _ = entry.task.await;
            }
        })
        .await
        .is_ok();
        if !drained {
            log_daemon_event(
                "project_server_warmup",
                &[("outcome", "shutdown_abort_timeout".to_string())],
            );
        }
    }

    #[cfg(test)]
    async fn tracked_task_count(&self) -> usize {
        let mut registry = self.registry.lock().await;
        registry.prune(Instant::now());
        registry
            .routes
            .values()
            .filter(|entry| !entry.task.is_finished())
            .count()
    }

    #[cfg(test)]
    async fn tracked_route_count(&self) -> usize {
        let mut registry = self.registry.lock().await;
        registry.prune(Instant::now());
        registry.routes.len()
    }
}

/// Scope-specific MCP servers routed through one canonical physical DB owner.
/// `Database` performs the actual same-process handle sharing; this registry
/// keeps daemon cache aliases and branch-drift rekeys consistent with it.
struct DatabaseOwnerEntry<Server> {
    server: Server,
    last_used: Instant,
}

struct DatabaseOwnerRegistry<Server = Arc<crate::mcp::McpServer>> {
    servers: HashMap<ProjectServerKey, DatabaseOwnerEntry<Server>>,
    aliases: HashMap<ProjectRouteKey, ProjectServerKey>,
}

impl<Server> Default for DatabaseOwnerRegistry<Server> {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
            aliases: HashMap::new(),
        }
    }
}

impl<Server> DatabaseOwnerRegistry<Server> {
    fn get(&self, key: &ProjectServerKey) -> Option<&Server> {
        self.servers.get(key).map(|entry| &entry.server)
    }

    fn insert(&mut self, key: ProjectServerKey, server: Server) {
        self.insert_at(key, server, Instant::now());
    }

    fn insert_at(&mut self, key: ProjectServerKey, server: Server, last_used: Instant) {
        self.servers
            .insert(key, DatabaseOwnerEntry { server, last_used });
    }

    fn get_route(&self, route: &ProjectRouteKey) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?;
        let (key, entry) = self.servers.get_key_value(key)?;
        Some((key, &entry.server))
    }

    fn get_route_and_touch(
        &mut self,
        route: &ProjectRouteKey,
    ) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?.clone();
        let entry = self.servers.get_mut(&key)?;
        entry.last_used = Instant::now();
        Some((self.aliases.get(route)?, &entry.server))
    }

    fn bind_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey) {
        debug_assert!(self.servers.contains_key(&key));
        if let Some(entry) = self.servers.get_mut(&key) {
            entry.last_used = Instant::now();
        }
        self.aliases.insert(route, key);
    }

    fn insert_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey, server: Server) {
        self.insert(key.clone(), server);
        self.bind_route(route, key);
    }

    fn bind_or_insert_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
    ) -> (Server, bool)
    where
        Server: Clone,
    {
        if let Some(existing) = self.get(&key).cloned() {
            self.bind_route(route, key);
            return (existing, false);
        }
        self.insert_route(route, key, candidate.clone());
        (candidate, true)
    }

    fn bind_or_insert_route_bounded<F>(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
        capacity: usize,
        mut is_leased: F,
    ) -> Option<(Server, bool)>
    where
        Server: Clone,
        F: FnMut(&Server) -> bool,
    {
        if let Some(existing) = self.get(&key).cloned() {
            self.bind_route(route, key);
            return Some((existing, false));
        }
        while self.servers.len() >= capacity {
            let evict = self
                .servers
                .iter()
                .filter(|(_, entry)| !is_leased(&entry.server))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())?;
            self.servers.remove(&evict);
            self.aliases.retain(|_, key| key != &evict);
        }
        self.insert_route(route, key, candidate.clone());
        Some((candidate, true))
    }

    fn rekey(&mut self, old: &ProjectServerKey, new: &ProjectServerKey) -> bool {
        if old == new {
            return true;
        }
        let Some(server) = self.servers.remove(old) else {
            return false;
        };
        if self.servers.contains_key(new) {
            self.aliases.retain(|_, key| key != old);
            return false;
        }
        self.servers.insert(new.clone(), server);
        for key in self.aliases.values_mut() {
            if key == old {
                *key = new.clone();
            }
        }
        true
    }

    fn values(&self) -> impl Iterator<Item = &Server> {
        self.servers.values().map(|entry| &entry.server)
    }
}

fn project_server_capacity_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "daemon project server capacity reached (capacity={MAX_CACHED_PROJECT_SERVERS}); retry after active clients finish"
        ),
    }
}

fn project_open_task_capacity_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "daemon project open task capacity reached (capacity={MAX_TRACKED_PROJECT_OPEN_TASKS}); retry shortly"
        ),
    }
}

fn project_warming_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay project '{}' {PROJECT_WARMING_RETRY_HINT}",
            project_path.display(),
        ),
    }
}

impl StoreOwnerKey {
    fn from_paths(
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
    fn from_handshake(project_path: &Path, handshake: &DaemonHandshake) -> Result<Self> {
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

fn project_route_for_handshake(handshake: &DaemonHandshake) -> Result<(PathBuf, ProjectRouteKey)> {
    let Some(project_path) = handshake.project_path.as_ref() else {
        return Err(TraceDecayError::Config {
            message: "project server requested without project_path".to_string(),
        });
    };
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.clone());
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake)?;
    Ok((canonical_project_path, route))
}

async fn project_open_gate(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
    route: &ProjectRouteKey,
) -> Arc<ProjectOpenGate> {
    let mut gates = gates.lock().await;
    if let Some(gate) = gates.gates.get(route).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(ProjectOpenGate::new(()));
    gates.gates.insert(route.clone(), Arc::downgrade(&gate));
    gate
}

async fn project_open_tasks(gates: &tokio::sync::Mutex<ProjectOpenGates>) -> ProjectOpenTasks {
    gates.lock().await.tasks.clone()
}

async fn maintenance_transition_gate(
    gates: &tokio::sync::Mutex<MaintenanceTransitionGates>,
    key: &ProjectServerKey,
) -> Arc<MaintenanceTransitionGate> {
    let transition_key = MaintenanceTransitionKey {
        profile_root: key.owner.profile_root.clone(),
        project_id: key.owner.project_id.clone(),
        scope_prefix: key.scope_prefix.clone(),
    };
    let mut gates = gates.lock().await;
    if let Some(gate) = gates
        .get(&transition_key)
        .and_then(std::sync::Weak::upgrade)
    {
        return gate;
    }
    let gate = Arc::new(MaintenanceTransitionGate::new(()));
    gates.insert(transition_key, Arc::downgrade(&gate));
    gate
}

#[cfg(any(not(unix), test))]
fn portable_database_owner_reconciler(
    store_administration: StoreAdministration,
    current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
    route_registered: Arc<AtomicBool>,
    handshake: DaemonHandshake,
) -> crate::mcp::DatabaseOwnerReconciler {
    Arc::new(move |fresh| {
        let store_administration = store_administration.clone();
        let current_key = Arc::clone(&current_key);
        let route_registered = Arc::clone(&route_registered);
        let handshake = handshake.clone();
        Box::pin(async move {
            let transition = store_administration
                .with_writer(|| async {
                    if !route_registered.load(Ordering::Acquire) {
                        return None;
                    }
                    let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake) {
                        Ok(key) => key,
                        Err(error) => {
                            eprintln!(
                                "[tracedecay] failed to rekey daemon database owner: {error}"
                            );
                            return None;
                        }
                    };
                    let mut current = current_key.lock().await;
                    if *current == new_key {
                        return None;
                    }
                    let old_key = current.clone();
                    let rekeyed = store_administration
                        .project_servers()
                        .lock()
                        .await
                        .rekey(&old_key, &new_key);
                    if !rekeyed {
                        route_registered.store(false, Ordering::Release);
                    }
                    *current = new_key.clone();
                    Some((old_key.owner, new_key.owner, rekeyed))
                })
                .await;
            let Some((old_owner, new_owner, rekeyed)) = transition else {
                return;
            };
            if rekeyed
                && let Ok(database) = store_administration
                    .global_database(&fresh.store_layout().sessions_db_path)
                    .await
            {
                store_administration
                    .session_temporal_refresh_schedulers()
                    .rekey_project(&old_owner, new_owner, database)
                    .await;
            } else {
                store_administration
                    .session_temporal_refresh_schedulers()
                    .retire_project(&old_owner)
                    .await;
            }
        })
    })
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CatalogRefreshClientKey {
    client_identity: DaemonClientIdentity,
    client_instance_id: String,
}

#[cfg(unix)]
impl CatalogRefreshClientKey {
    fn from_handshake(handshake: &DaemonHandshake) -> Self {
        Self {
            client_identity: handshake.client_identity.clone(),
            client_instance_id: handshake.client_instance_id.clone(),
        }
    }
}

impl ProjectServerKey {
    fn from_open_project(
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
            scope_prefix: handshake.scope_prefix.clone(),
        })
    }
}

#[cfg(unix)]
impl DaemonEngine {
    /// Installs the config-driven git-metadata watcher on this engine. Called
    /// once by `run_foreground_unix` before the accept loop.
    fn with_git_watcher(mut self, watcher: git_watch::GitWatcher) -> Self {
        self.git_watcher = watcher;
        self
    }

    async fn with_pr_autotrack_task(self, task: JoinHandle<()>) -> Self {
        *self.pr_autotrack_task.lock().await = Some(task);
        self
    }

    async fn maintenance_transition_gate(
        &self,
        key: &ProjectServerKey,
    ) -> Arc<MaintenanceTransitionGate> {
        maintenance_transition_gate(&self.maintenance_transition_gates, key).await
    }

    /// Runs destructive branch administration before any project server is
    /// opened for the request, under the daemon-wide store administration gate.
    async fn execute_branch_admin(
        &self,
        handshake: &DaemonHandshake,
        action: crate::branch::BranchAdminAction,
    ) -> Result<crate::branch::BranchAdminReport> {
        self.store_administration
            .execute_branch_admin_for_handshake(handshake, action)
            .await
    }

    /// Returns the client version to log for this handshake, once per distinct
    /// skewed version; repeat connections from the same client return `None`.
    async fn client_version_skew_to_log(&self, handshake: &DaemonHandshake) -> Option<String> {
        let skew = client_version_skew(&handshake.client_version, binary_version())?;
        let mut logged = self.logged_client_version_skews.lock().await;
        logged.insert(skew.clone()).then_some(skew)
    }

    /// Logs a `daemon_version_skew` event when this handshake's client runs a
    /// different binary version, deduped per distinct client version.
    async fn log_client_version_skew(&self, handshake: &DaemonHandshake) {
        let Some(client_version) = self.client_version_skew_to_log(handshake).await else {
            return;
        };
        let hint = version_skew_action(binary_version(), &client_version).to_string();
        log_daemon_event(
            "daemon_version_skew",
            &[
                ("daemon_version", binary_version().to_string()),
                ("client_version", client_version),
                ("hint", hint),
            ],
        );
    }

    /// Claims the one catalog-refresh notification for this client in the
    /// current daemon generation. Only proxies that already advertised the
    /// capability are eligible. `initialize` and `tools/list` mark the client
    /// current without emitting because those requests already fetch the new
    /// generation's catalog.
    async fn claim_catalog_refresh(
        &self,
        handshake: &DaemonHandshake,
        request_line: &str,
    ) -> Option<CatalogRefreshClientKey> {
        if !valid_client_instance_id(&handshake.client_instance_id) {
            return None;
        }
        let request = serde_json::from_str::<JsonRpcRequest>(request_line).ok()?;
        if request.method == HOOK_EVENT_METHOD {
            return None;
        }
        let catalog_is_current = matches!(request.method.as_str(), "initialize" | "tools/list");
        if !catalog_is_current
            && (!handshake.tool_list_changed_capable || handshake.catalog_version.is_empty())
        {
            return None;
        }
        let key = CatalogRefreshClientKey::from_handshake(handshake);
        let mut notified_clients = self.catalog_refresh_notified_clients.lock().await;
        if notified_clients.contains(&key) {
            return None;
        }
        if notified_clients.len() >= MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION {
            drop(notified_clients);
            if !self
                .catalog_refresh_saturation_logged
                .swap(true, Ordering::Relaxed)
            {
                log_daemon_event(
                    "catalog_refresh",
                    &[
                        ("outcome", "skipped".to_string()),
                        ("reason", "client_capacity_reached".to_string()),
                        (
                            "capacity",
                            MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION.to_string(),
                        ),
                    ],
                );
            }
            return None;
        }
        notified_clients.insert(key.clone());
        drop(notified_clients);
        if catalog_is_current {
            return None;
        }
        Some(key)
    }

    async fn release_catalog_refresh(&self, key: CatalogRefreshClientKey) {
        self.catalog_refresh_notified_clients
            .lock()
            .await
            .remove(&key);
    }

    async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self.cached_project_server(handshake).await? {
            return Ok(server);
        }

        let cached = {
            self.store_administration
                .with_writer(|| self.open_project_server(handshake))
                .await?
        };
        let (_key, project_path, server, _inserted) = cached;
        Ok(self.activate_project_server(project_path, server).await)
    }

    async fn cached_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<Arc<crate::mcp::McpServer>>> {
        let (project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route_and_touch(&route)
                .map(|(_, server)| Arc::clone(server))
        };
        Ok(match cached {
            Some(server) => Some(self.activate_project_server(project_path, server).await),
            None => None,
        })
    }

    async fn begin_project_open(
        &self,
        handshake: DaemonHandshake,
        initialize_request: Option<JsonRpcRequest>,
    ) -> Result<ProjectOpenTaskClaim> {
        let (project_path, route) = Self::project_route(&handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        let engine = self.clone();
        Ok(Box::pin(start_lifecycle_project_open(
            &tasks,
            self.lifecycle.clone(),
            route,
            project_path,
            initialize_request,
            async move { engine.project_server(&handshake).await },
        ))
        .await)
    }

    async fn schedule_project_server_warmup(
        &self,
        handshake: DaemonHandshake,
        initialize_request: JsonRpcRequest,
    ) -> Result<()> {
        if self.cached_project_server(&handshake).await?.is_some() {
            return Ok(());
        }
        match Box::pin(self.begin_project_open(handshake, Some(initialize_request))).await? {
            ProjectOpenTaskClaim::InFlight(_) => Ok(()),
            ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
            ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
        }
    }

    async fn project_server_for_request(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self.cached_project_server(handshake).await? {
            return Ok(server);
        }
        let (project_path, _) = Self::project_route(handshake)?;
        let claim = Box::pin(self.begin_project_open(handshake.clone(), None)).await?;
        match claim {
            ProjectOpenTaskClaim::InFlight(state) => {
                match timeout(
                    PROJECT_OPEN_REQUEST_DEADLINE,
                    ProjectOpenTasks::wait_for_completion(state),
                )
                .await
                {
                    Ok(Ok(())) => self.cached_project_server(handshake).await?.ok_or_else(|| {
                        TraceDecayError::Config {
                            message: "project open completed without publishing a server"
                                .to_string(),
                        }
                    }),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(project_warming_error(&project_path)),
                }
            }
            ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
            ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
        }
    }

    async fn cached_project_open_failure(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<ProjectOpenFailure>> {
        let (_, route) = Self::project_route(handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        Ok(tasks.cached_failure(&route).await)
    }

    async fn shutdown_project_open_tasks(&self) {
        project_open_tasks(&self.project_open_gates)
            .await
            .shutdown()
            .await;
    }

    /// Opens or resolves a project server while writer administration is held.
    /// Watcher and scheduler activation happen only after this returns so those
    /// components can acquire the same coordinator without recursive locking.
    async fn open_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake)?;
        let cached = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route_and_touch(&route)
                .map(|(key, server)| (key.clone(), Arc::clone(server)))
        };
        if let Some((key, server)) = cached {
            return Ok((key, canonical_project_path, server, false));
        }

        let gate = project_open_gate(&self.project_open_gates, &route).await;
        let _singleflight = gate.lock().await;
        let cached = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route_and_touch(&route)
                .map(|(key, server)| (key.clone(), Arc::clone(server)))
        };
        if let Some((key, server)) = cached {
            return Ok((key, canonical_project_path, server, false));
        }

        #[cfg(test)]
        self.project_open_attempts.fetch_add(1, Ordering::Relaxed);
        let cg = Box::pin(open_project_for_handshake(
            &canonical_project_path,
            handshake,
        ))
        .await?;
        cg.register_project_store_in_global_registry().await;
        let key = ProjectServerKey::from_open_project(&cg, handshake)?;

        let existing = {
            let mut servers = self.store_administration.project_servers().lock().await;
            let server = servers.get(&key).cloned();
            if server.is_some() {
                servers.bind_route(route.clone(), key.clone());
            }
            server
        };
        if let Some(server) = existing {
            return Ok((key, canonical_project_path, server, false));
        }

        let registry_db = self
            .store_administration
            .global_database(&handshake.client_identity.global_db_path)
            .await?;
        let session_db = self
            .store_administration
            .global_database(&cg.store_layout().sessions_db_path)
            .await?;
        ensure_git_index_transactions_before_advertising(
            &self.store_administration,
            Arc::clone(&session_db),
            cg.project_root(),
            key.owner.project_id.as_deref(),
        )
        .await?;
        let host_admission_broker = self
            .store_administration
            .host_admission_broker(&session_db)
            .await?
            .broker()
            .cloned();
        let user_session_db = self
            .store_administration
            .user_session_database(&handshake.client_identity.global_db_path)
            .await?;
        let project_session_refresh_wake = self
            .store_administration
            .session_temporal_refresh_schedulers()
            .ensure_project(key.owner.clone(), Arc::clone(&session_db))
            .await;
        let user_session_refresh_wake = self
            .store_administration
            .session_temporal_refresh_schedulers()
            .ensure_profile(
                user_session_db.db_path().to_path_buf(),
                Arc::clone(&user_session_db),
            )
            .await;
        let accounting_db =
            crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registry_db));
        let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
        let current_project_path =
            Arc::new(tokio::sync::Mutex::new(canonical_project_path.clone()));
        let route_registered = Arc::new(AtomicBool::new(true));
        let reconciler = self.automation_scheduler_reconciler(
            Arc::clone(&current_key),
            Arc::clone(&current_project_path),
            handshake.clone(),
        );
        let database_owner_reconciler = self.database_owner_reconciler(
            current_key,
            current_project_path,
            Arc::clone(&route_registered),
            handshake.clone(),
        );
        let context = crate::mcp::server::McpServerConstructionContext::daemon_owned(
            cg,
            handshake.scope_prefix.clone(),
            crate::mcp::server::McpServerDaemonAuthority {
                profile_root: handshake.client_identity.profile_root.clone(),
                databases: crate::mcp::server::McpServerDaemonDatabases {
                    accounting: accounting_db,
                    registry: registry_db,
                    project_sessions: session_db,
                    user_sessions: user_session_db,
                },
                host_admission_broker,
                project_session_refresh_wake,
                user_session_refresh_wake,
                database_owner_reconciler,
                writers: crate::mcp::server::McpServerWriters::daemon_owned(
                    coordinated_dashboard_automation_writer(self.store_administration.clone()),
                    coordinated_hook_branch_writer(self.store_administration.clone()),
                    coordinated_background_refresh_writer(self.store_administration.clone()),
                ),
            },
        )
        .with_automation_scheduler_reconciler(reconciler);
        let candidate = crate::mcp::McpServer::new_with_context(context).await;
        let resolved = self
            .store_administration
            .project_servers()
            .lock()
            .await
            .bind_or_insert_route_bounded(
                route,
                key.clone(),
                candidate,
                MAX_CACHED_PROJECT_SERVERS,
                |server| Arc::strong_count(server) > 1,
            );
        let Some((server, inserted)) = resolved else {
            route_registered.store(false, Ordering::Release);
            return Err(project_server_capacity_error());
        };
        if !inserted {
            route_registered.store(false, Ordering::Release);
        } else {
            self.spawn_project_maintenance_activation(
                key.clone(),
                canonical_project_path.clone(),
                handshake.clone(),
                Arc::clone(&server),
            );
        }
        Ok((key, canonical_project_path, server, inserted))
    }

    fn project_route(handshake: &DaemonHandshake) -> Result<(PathBuf, ProjectRouteKey)> {
        project_route_for_handshake(handshake)
    }

    async fn activate_project_server(
        &self,
        project_path: PathBuf,
        server: Arc<crate::mcp::McpServer>,
    ) -> Arc<crate::mcp::McpServer> {
        // A freshly-handshaken project should be watched even on a cache hit
        // (the watcher may have started after this server was cached).
        self.git_watcher.ensure_watching(&project_path).await;
        server
    }

    fn spawn_project_maintenance_activation(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        server: Arc<crate::mcp::McpServer>,
    ) {
        let repair_key = key.clone();
        let repair_project_path = project_path.clone();
        let repair_handshake = handshake.clone();
        let engine = self.clone();
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            engine
                .activate_project_maintenance(repair_key, repair_project_path, repair_handshake)
                .await;
        });
        let engine = self.clone();
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            let cg = server.cg().await;
            engine
                .activate_automation_scheduler_for_open_project(key, project_path, handshake, cg)
                .await;
        });
    }

    async fn activate_project_maintenance(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        self.store_administration
            .with_writer(|| async move {
                if self
                    .store_administration
                    .project_servers()
                    .lock()
                    .await
                    .get(&key)
                    .is_none()
                {
                    return;
                }
                self.start_memory_repair_scheduler(
                    key.clone(),
                    project_path.clone(),
                    handshake.clone(),
                )
                .await;
            })
            .await;
    }

    async fn rekey_project_maintenance(
        &self,
        old_key: &ProjectServerKey,
        new_key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        acquire_new: bool,
    ) -> MaintenanceRekeyOutcome {
        let transition = self.maintenance_transition_gate(old_key).await;
        let _transition = transition.lock().await;
        let repair_retirement = self.retire_memory_repair_scheduler_locked(old_key).await;
        let automation_retirement = self.retire_automation_scheduler_locked(old_key).await;
        let retired = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            if let Some(retirement) = repair_retirement {
                retirement.wait().await;
            }
            if let Some(retirement) = automation_retirement {
                retirement.wait().await;
            }
        })
        .await
        .is_ok();
        if !retired {
            log_daemon_event(
                "maintenance_rekey",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "retirement_timeout".to_string()),
                ],
            );
            return MaintenanceRekeyOutcome::Retiring;
        }
        if !acquire_new || !self.lifecycle.accepting() {
            return MaintenanceRekeyOutcome::Completed;
        }
        let repair_outcome = self
            .reconcile_memory_repair_scheduler_locked(
                new_key.clone(),
                project_path.clone(),
                handshake.clone(),
            )
            .await;
        let automation_outcome = self
            .reconcile_automation_scheduler_locked(new_key, project_path, handshake)
            .await;
        if matches!(
            repair_outcome,
            memory_repair_scheduler::MemoryRepairSchedulerReconcileOutcome::Retiring
        ) || matches!(
            automation_outcome,
            crate::dashboard::AutomationSchedulerReconcileOutcome::Retiring
        ) {
            MaintenanceRekeyOutcome::Retiring
        } else {
            MaintenanceRekeyOutcome::Completed
        }
    }

    fn database_owner_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        let engine = self.clone();
        Arc::new(move |fresh| {
            let engine = engine.clone();
            let current_key = Arc::clone(&current_key);
            let current_project_path = Arc::clone(&current_project_path);
            let route_registered = Arc::clone(&route_registered);
            let handshake = handshake.clone();
            Box::pin(async move {
                let transition = engine
                    .store_administration
                    .with_writer(|| async {
                        if !route_registered.load(Ordering::Acquire) {
                            return None;
                        }
                        let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake)
                        {
                            Ok(key) => key,
                            Err(error) => {
                                eprintln!(
                                    "[tracedecay] failed to rekey daemon database owner: {error}"
                                );
                                return None;
                            }
                        };
                        let mut current = current_key.lock().await;
                        if *current == new_key {
                            return None;
                        }
                        let old_key = current.clone();
                        let rekeyed = engine
                            .store_administration
                            .project_servers()
                            .lock()
                            .await
                            .rekey(&old_key, &new_key);
                        if !rekeyed {
                            route_registered.store(false, Ordering::Release);
                        }
                        let project_path = fresh.project_root().to_path_buf();
                        let new_session_db = engine
                            .store_administration
                            .global_database(&fresh.store_layout().sessions_db_path)
                            .await
                            .ok();
                        *current_project_path.lock().await = project_path;
                        *current = new_key.clone();
                        Some((
                            old_key,
                            new_key,
                            new_session_db,
                            fresh.project_root().to_path_buf(),
                            rekeyed,
                        ))
                    })
                    .await;
                if let Some((old_key, new_key, new_session_db, project_path, acquire_new)) =
                    transition
                {
                    let old_owner = old_key.owner.clone();
                    let new_owner = new_key.owner.clone();
                    let outcome = engine
                        .rekey_project_maintenance(
                            &old_key,
                            new_key,
                            project_path,
                            handshake,
                            acquire_new,
                        )
                        .await;
                    if outcome == MaintenanceRekeyOutcome::Completed {
                        if acquire_new
                            && engine.lifecycle.accepting()
                            && let Some(new_session_db) = new_session_db
                        {
                            engine
                                .store_administration
                                .session_temporal_refresh_schedulers()
                                .rekey_project(&old_owner, new_owner, new_session_db)
                                .await;
                        } else {
                            engine
                                .store_administration
                                .session_temporal_refresh_schedulers()
                                .retire_project(&old_owner)
                                .await;
                        }
                    }
                }
            })
        })
    }

    async fn shutdown_background_tasks(&self) {
        self.shutdown_project_open_tasks().await;
        self.store_administration
            .session_temporal_refresh_schedulers()
            .shutdown()
            .await;
        self.shutdown_automation_schedulers().await;
        self.shutdown_memory_repair_schedulers().await;
        self.store_administration
            .shutdown_retirement_reapers()
            .await;
        self.store_administration
            .shutdown_host_admission_replay()
            .await;

        self.git_watcher.shutdown().await;
        if let Some(handle) = self.pr_autotrack_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn shutdown_servers(&self) {
        shutdown_project_servers(&self.store_administration).await;
    }

    #[cfg(test)]
    async fn shutdown_all(&self) {
        self.lifecycle.begin_draining();
        self.shutdown_background_tasks().await;
        self.shutdown_servers().await;
    }
}

async fn shutdown_project_servers(store_administration: &StoreAdministration) {
    let servers: Vec<Arc<crate::mcp::McpServer>> = store_administration
        .with_writer(|| async {
            let servers = store_administration.project_servers().lock().await;
            let mut seen = HashSet::new();
            servers
                .values()
                .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
                .cloned()
                .collect()
        })
        .await;
    for server in servers {
        server.shutdown().await;
    }
}

/// Kick coalesced per-profile replay without awaiting a pass (handshake-safe).
async fn ensure_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    let Ok(user_session_db) = store_administration
        .user_session_database(&client_identity.global_db_path)
        .await
    else {
        eprintln!("[tracedecay] user-profile host admission disposition: authority_unavailable");
        return Ok(());
    };
    let Ok(state) = store_administration
        .host_admission_broker(&user_session_db)
        .await
    else {
        eprintln!("[tracedecay] user-profile host admission disposition: authority_unavailable");
        return Ok(());
    };
    if let Some(outcome) = state.unavailable_outcome() {
        eprintln!(
            "[tracedecay] user-profile host admission disposition: {}",
            outcome.reason_code.unwrap_or("spool_unavailable")
        );
    }
    // host_admission_broker already kicks the coalesced worker for user-sessions DBs.
    Ok(())
}

#[cfg(test)]
async fn replay_user_profile_host_admission_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    ensure_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
        .await?;
    let Ok(broker_path) = authority::canonical_identity_path(
        &crate::sessions::user_sessions_db_path(&client_identity.profile_root),
    ) else {
        return Ok(());
    };
    let _ = store_administration
        .wait_user_profile_host_admission_replay_idle(&broker_path, Duration::from_secs(5))
        .await;
    Ok(())
}

#[cfg(all(unix, test))]
async fn serve_socket_client(stream: tokio::net::UnixStream, engine: DaemonEngine) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        BrokerStream::Unix(stream),
        engine,
        None,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
async fn serve_authenticated_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
) -> Result<()> {
    Box::pin(serve_authenticated_socket_client_with_class(
        stream,
        engine,
        auth_token,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
async fn serve_authenticated_socket_client_with_class(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        stream,
        engine,
        Some(auth_token),
        admission_class,
    ))
    .await
}

async fn apply_daemon_initialize_route(
    handshake: &mut DaemonHandshake,
    first_request_line: &str,
    store_administration: &StoreAdministration,
) -> Result<Option<InitializeRouteMetadata>> {
    if !handshake.allow_initialize_root_routing {
        return Ok(None);
    }
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) else {
        return Ok(None);
    };
    if request.method != "initialize" {
        return Ok(None);
    }
    let registry = store_administration
        .global_database(&handshake.client_identity.global_db_path)
        .await?;
    let Some(route) =
        resolve_daemon_initialize_route(request.params.as_ref(), Some(&registry)).await
    else {
        return Ok(None);
    };
    if handshake.project_path.as_deref() != Some(route.project_path.as_path()) {
        handshake.scope_prefix = None;
    }
    handshake.project_path = Some(route.project_path.clone());
    handshake.allow_init = route.allow_init;
    Ok(Some(route))
}

fn attach_initialize_route_metadata(
    response: &mut JsonRpcResponse,
    route: &InitializeRouteMetadata,
) {
    let Some(result) = response.result.as_mut() else {
        return;
    };
    result["_meta"]["tracedecayInitializeRoute"] = json!(route);
}

/// Returns `None` for project-dependent requests, `Some(None)` for handled
/// notifications, and `Some(Some(response))` for static MCP bootstrap calls.
fn daemon_bootstrap_response(
    request: &JsonRpcRequest,
    route: Option<&InitializeRouteMetadata>,
    project_node_count: Option<u64>,
) -> Option<Option<JsonRpcResponse>> {
    match classify_mcp_method(&request.method) {
        McpMethod::Initialize => Some(request.id.clone().map(|id| {
            let mut response = JsonRpcResponse::success(id, initialize_result(SERVER_INSTRUCTIONS));
            if let Some(route) = route {
                attach_initialize_route_metadata(&mut response, route);
            }
            response
        })),
        McpMethod::InitializedAck => Some(None),
        McpMethod::ToolsList => Some(request.id.clone().map(|id| {
            let tools = project_node_count.map_or_else(
                || get_tool_definitions_with_warming_budget(10),
                |node_count| {
                    let budget = explore_call_budget(node_count);
                    get_tool_definitions_with_budget(node_count, budget)
                },
            );
            JsonRpcResponse::success(id, json!({ "tools": tools }))
        })),
        _ => None,
    }
}

async fn cached_project_node_count(
    store_administration: &StoreAdministration,
    handshake: &DaemonHandshake,
) -> Option<u64> {
    let project_path = handshake.project_path.as_ref()?;
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.clone());
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake).ok()?;
    let server = {
        let servers = store_administration.project_servers().lock().await;
        servers
            .get_route(&route)
            .map(|(_, server)| Arc::clone(server))
    }?;
    server
        .cg()
        .await
        .get_stats()
        .await
        .ok()
        .map(|stats| stats.node_count)
}

async fn start_lifecycle_project_open<OpenFuture>(
    tasks: &ProjectOpenTasks,
    lifecycle: DaemonLifecycle,
    route: ProjectRouteKey,
    project_path: PathBuf,
    initialize_request: Option<JsonRpcRequest>,
    open_project_server: OpenFuture,
) -> ProjectOpenTaskClaim
where
    OpenFuture: std::future::Future<Output = Result<Arc<crate::mcp::McpServer>>> + Send + 'static,
{
    if !lifecycle.accepting() {
        return ProjectOpenTaskClaim::Failed(ProjectOpenFailure {
            message: "daemon is draining before project warm-up".to_string(),
            retry_at: None,
        });
    }
    tasks
        .start(route, async move {
            let Some(activity) = lifecycle.try_enter() else {
                return Err(TraceDecayError::Config {
                    message: "daemon is draining before project warm-up".to_string(),
                });
            };
            let _activity = activity;
            let result = tokio::select! {
                biased;
                () = lifecycle.wait_for_draining() => Err(TraceDecayError::Config {
                    message: "daemon began draining during project warm-up".to_string(),
                }),
                result = Box::pin(open_project_server) => result,
            };
            match result {
                Ok(server) => {
                    if let Some(initialize_request) = initialize_request {
                        // Preserve the regular initialize side effect that records
                        // the negotiated MCP client name on the real server.
                        let _ = server.handle_request(&initialize_request).await;
                    }
                    Ok(())
                }
                Err(error) => {
                    log_daemon_event(
                        "project_server_warmup",
                        &[
                            ("outcome", "error".to_string()),
                            ("project", project_path.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    Err(error)
                }
            }
        })
        .await
}

fn spawn_lifecycle_automation_scheduler_activation<ActivationFuture>(
    lifecycle: DaemonLifecycle,
    activation: ActivationFuture,
) where
    ActivationFuture: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(activity) = lifecycle.try_enter() else {
        return;
    };
    tokio::spawn(async move {
        let _activity = activity;
        tokio::select! {
            biased;
            () = lifecycle.wait_for_draining() => {}
            () = activation => {}
        }
    });
}

#[cfg(any(not(unix), test))]
async fn portable_cached_project_server(
    store_administration: &StoreAdministration,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<Option<Arc<crate::mcp::McpServer>>> {
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    let server = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(_, server)| Arc::clone(server))
    };
    Ok(server)
}

#[cfg(any(not(unix), test))]
// Cohesive route-open context; a params struct would only move the same ownership bundle.
#[allow(clippy::too_many_arguments)]
async fn begin_portable_project_open(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: DaemonHandshake,
    canonical_project_path: PathBuf,
    route: ProjectRouteKey,
    initialize_request: Option<JsonRpcRequest>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> ProjectOpenTaskClaim {
    let tasks = project_open_tasks(project_open_gates.as_ref()).await;
    let open_project_path = canonical_project_path.clone();
    let open_gates = Arc::clone(&project_open_gates);
    Box::pin(start_lifecycle_project_open(
        &tasks,
        lifecycle,
        route,
        canonical_project_path,
        initialize_request,
        async move {
            store_administration
                .with_writer(|| {
                    portable_project_server(
                        &store_administration,
                        open_gates.as_ref(),
                        &open_project_path,
                        &handshake,
                        #[cfg(test)]
                        project_open_attempts.as_ref(),
                    )
                })
                .await
        },
    ))
    .await
}

#[cfg(any(not(unix), test))]
async fn schedule_portable_project_server_warmup(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: DaemonHandshake,
    initialize_request: JsonRpcRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let (canonical_project_path, route) = project_route_for_handshake(&handshake)?;
    if portable_cached_project_server(&store_administration, &canonical_project_path, &handshake)
        .await?
        .is_some()
    {
        return Ok(());
    }
    match Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration,
        project_open_gates,
        handshake,
        canonical_project_path,
        route,
        Some(initialize_request),
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
    {
        ProjectOpenTaskClaim::InFlight(_) => Ok(()),
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}

#[cfg(any(not(unix), test))]
async fn portable_project_server_for_request(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: &DaemonHandshake,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<Arc<crate::mcp::McpServer>> {
    let (canonical_project_path, route) = project_route_for_handshake(handshake)?;
    if let Some(server) =
        portable_cached_project_server(&store_administration, &canonical_project_path, handshake)
            .await?
    {
        return Ok(server);
    }
    let claim = Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration.clone(),
        project_open_gates,
        handshake.clone(),
        canonical_project_path.clone(),
        route,
        None,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await;
    match claim {
        ProjectOpenTaskClaim::InFlight(state) => {
            match timeout(
                PROJECT_OPEN_REQUEST_DEADLINE,
                ProjectOpenTasks::wait_for_completion(state),
            )
            .await
            {
                Ok(Ok(())) => portable_cached_project_server(
                    &store_administration,
                    &canonical_project_path,
                    handshake,
                )
                .await?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project open completed without publishing a server".to_string(),
                }),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(project_warming_error(&canonical_project_path)),
            }
        }
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}

#[cfg(any(not(unix), test))]
async fn portable_cached_project_open_failure(
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    handshake: &DaemonHandshake,
) -> Result<Option<ProjectOpenFailure>> {
    let (_, route) = project_route_for_handshake(handshake)?;
    let tasks = project_open_tasks(project_open_gates).await;
    Ok(tasks.cached_failure(&route).await)
}

#[cfg(not(unix))]
async fn shutdown_portable_project_open_tasks(
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
) {
    project_open_tasks(project_open_gates)
        .await
        .shutdown()
        .await;
}

#[cfg(any(not(unix), test))]
async fn portable_project_server(
    store_administration: &StoreAdministration,
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
    #[cfg(test)] project_open_attempts: Option<&Arc<AtomicUsize>>,
) -> Result<Arc<crate::mcp::McpServer>> {
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    if let Some(server) = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(_, server)| Arc::clone(server))
    } {
        return Ok(server);
    }

    let gate = project_open_gate(project_open_gates, &route).await;
    let _singleflight = gate.lock().await;
    if let Some(server) = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(_, server)| Arc::clone(server))
    } {
        return Ok(server);
    }

    #[cfg(test)]
    if let Some(attempts) = project_open_attempts {
        attempts.fetch_add(1, Ordering::Relaxed);
    }
    let cg = Box::pin(open_project_for_handshake(
        canonical_project_path,
        handshake,
    ))
    .await?;
    cg.register_project_store_in_global_registry().await;
    let key = ProjectServerKey::from_open_project(&cg, handshake)?;
    let existing = {
        let mut servers = store_administration.project_servers().lock().await;
        let existing = servers.get(&key).cloned();
        if existing.is_some() {
            servers.bind_route(route.clone(), key.clone());
        }
        existing
    };
    if let Some(existing) = existing {
        return Ok(existing);
    }

    let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
    let route_registered = Arc::new(AtomicBool::new(true));
    let database_owner_reconciler = portable_database_owner_reconciler(
        store_administration.clone(),
        current_key,
        Arc::clone(&route_registered),
        handshake.clone(),
    );
    let registry_db = store_administration
        .global_database(&handshake.client_identity.global_db_path)
        .await?;
    let session_db = store_administration
        .global_database(&cg.store_layout().sessions_db_path)
        .await?;
    ensure_git_index_transactions_before_advertising(
        &store_administration,
        Arc::clone(&session_db),
        cg.project_root(),
        key.owner.project_id.as_deref(),
    )
    .await?;
    let host_admission_broker = store_administration
        .host_admission_broker(&session_db)
        .await?
        .broker()
        .cloned();
    let user_session_db = store_administration
        .user_session_database(&handshake.client_identity.global_db_path)
        .await?;
    let project_session_refresh_wake = store_administration
        .session_temporal_refresh_schedulers()
        .ensure_project(key.owner.clone(), Arc::clone(&session_db))
        .await;
    let user_session_refresh_wake = store_administration
        .session_temporal_refresh_schedulers()
        .ensure_profile(
            user_session_db.db_path().to_path_buf(),
            Arc::clone(&user_session_db),
        )
        .await;
    let accounting_db =
        crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registry_db));
    let context = crate::mcp::server::McpServerConstructionContext::daemon_owned(
        cg,
        handshake.scope_prefix.clone(),
        crate::mcp::server::McpServerDaemonAuthority {
            profile_root: handshake.client_identity.profile_root.clone(),
            databases: crate::mcp::server::McpServerDaemonDatabases {
                accounting: accounting_db,
                registry: registry_db,
                project_sessions: session_db,
                user_sessions: user_session_db,
            },
            host_admission_broker,
            project_session_refresh_wake,
            user_session_refresh_wake,
            database_owner_reconciler,
            writers: crate::mcp::server::McpServerWriters::daemon_owned(
                coordinated_dashboard_automation_writer(store_administration.clone()),
                coordinated_hook_branch_writer(store_administration.clone()),
                coordinated_background_refresh_writer(store_administration.clone()),
            ),
        },
    );
    let candidate = crate::mcp::McpServer::new_with_context(context).await;
    let resolution = store_administration
        .project_servers()
        .lock()
        .await
        .bind_or_insert_route_bounded(
            route,
            key,
            candidate,
            MAX_CACHED_PROJECT_SERVERS,
            |server| Arc::strong_count(server) > 1,
        );
    let Some((resolved, inserted)) = resolution else {
        route_registered.store(false, Ordering::Release);
        return Err(project_server_capacity_error());
    };
    if !inserted {
        route_registered.store(false, Ordering::Release);
    }
    Ok(resolved)
}

async fn write_routed_initialize_response(
    server: &crate::mcp::McpServer,
    transport: &mut impl McpTransport,
    first_request_line: &str,
    route: Option<&InitializeRouteMetadata>,
) -> Result<bool> {
    let Some(route) = route else {
        return Ok(false);
    };
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) else {
        return Ok(false);
    };
    if request.method != "initialize" {
        return Ok(false);
    }
    let Some(mut response) = server.handle_request(&request).await else {
        return Ok(false);
    };
    attach_initialize_route_metadata(&mut response, route);
    write_json_rpc_response(transport, &response).await?;
    Ok(true)
}

#[cfg(unix)]
async fn serve_broker_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    if let Some(expected_token) = auth_token.as_deref() {
        let preface_line = tokio::select! {
            result = read_line_handling_wire_oversized(&mut transport) => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(()),
        };
        let Some(preface_line) = preface_line else {
            return Ok(());
        };
        let preface =
            DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            })?;
        if !preface.authenticate(expected_token) {
            return Err(TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            });
        }
    }
    let line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(line) = line else {
        return Ok(());
    };
    let Some(setup_activity) = engine.lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&line)?;
    let first_request_line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(first_request_line) = first_request_line else {
        return Ok(());
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match engine
            .per_client_admission
            .try_admit_request(&handshake, &first_request_line)
        {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(request) = doctor_runtime_request(&first_request_line) {
        drop(setup_activity);
        write_doctor_runtime_response(&mut transport, &handshake, request).await?;
        return Ok(());
    }
    engine.log_client_version_skew(&handshake).await;
    ensure_user_profile_host_admission_replay_for_identity(
        &engine.store_administration,
        &handshake.client_identity,
    )
    .await?;
    // Resolve initialize roots only after authentication and inside daemon
    // authority. The proxy process never opens the registry database.
    let initialize_route = apply_daemon_initialize_route(
        &mut handshake,
        &first_request_line,
        &engine.store_administration,
    )
    .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => engine.execute_branch_admin(&handshake, action).await,
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response =
            branch_add_response(&engine.store_administration, &handshake, &request).await;
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&engine.store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if let Some(mut response) =
            daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match engine.cached_project_open_failure(&handshake).await {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(
                            engine
                                .schedule_project_server_warmup(handshake.clone(), request.clone()),
                        )
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            // Keep catalog-refresh bookkeeping consistent with the regular MCP
            // server path: initialize and tools/list mark this catalog current.
            if let Some(key) = engine
                .claim_catalog_refresh(&handshake, &first_request_line)
                .await
                && let Err(error) = write_tool_list_changed_notification(&mut transport).await
            {
                engine.release_catalog_refresh(key).await;
                return Err(error);
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let server = if handshake.project_path.is_some() {
        match Box::pin(engine.project_server_for_request(&handshake)).await {
            Ok(server) => Some(server),
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    drop(setup_activity);
    if !engine.lifecycle.accepting() {
        return Ok(());
    }

    // The stdio proxy creates one daemon connection per request. The request
    // was peeked above so initialize-root routing happens before project open.
    if let Some(key) = engine
        .claim_catalog_refresh(&handshake, &first_request_line)
        .await
        && let Err(error) = write_tool_list_changed_notification(&mut transport).await
    {
        engine.release_catalog_refresh(key).await;
        return Err(error);
    }
    let initialize_handled = match server.as_deref() {
        Some(server) => {
            write_routed_initialize_response(
                server,
                &mut transport,
                &first_request_line,
                initialize_route.as_ref(),
            )
            .await?
        }
        None => false,
    };
    let mut transport = ReplayTransport::new(transport);
    if !initialize_handled {
        transport.push_replay(first_request_line)?;
    }

    if let Some(server) = server {
        Box::pin(server.run_daemon_connection_with_timings(
            &mut transport,
            handshake.timings,
            &engine.lifecycle,
        ))
        .await?;
    } else {
        serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            &engine.lifecycle,
            &engine.store_administration,
        )
        .await?;
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
async fn serve_windows_broker_client(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonPerClientAdmission::default(),
        DaemonClientAdmissionClass::General,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(any(not(unix), test))]
// Cohesive per-connection serving context; bundling into a params struct would churn every caller.
#[allow(clippy::too_many_arguments)]
async fn serve_windows_broker_client_with_class(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    let Some(preface_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let preface =
        DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        })?;
    if !preface.authenticate(auth_token) {
        return Err(TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        });
    }
    let Some(handshake_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let Some(setup_activity) = lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&handshake_line)?;
    let Some(first_request_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match per_client_admission.try_admit_request(&handshake, &first_request_line) {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(request) = doctor_runtime_request(&first_request_line) {
        drop(setup_activity);
        write_doctor_runtime_response(&mut transport, &handshake, request).await?;
        return Ok(());
    }
    ensure_user_profile_host_admission_replay_for_identity(
        &store_administration,
        &handshake.client_identity,
    )
    .await?;
    let initialize_route =
        apply_daemon_initialize_route(&mut handshake, &first_request_line, &store_administration)
            .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => {
                store_administration
                    .execute_branch_admin_for_handshake(&handshake, action)
                    .await
            }
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = branch_add_response(&store_administration, &handshake, &request).await;
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if let Some(mut response) =
            daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match portable_cached_project_open_failure(project_open_gates.as_ref(), &handshake)
                    .await
                {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(schedule_portable_project_server_warmup(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            handshake.clone(),
                            request.clone(),
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        ))
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    if handshake.project_path.is_some() {
        let server = match Box::pin(portable_project_server_for_request(
            lifecycle.clone(),
            store_administration.clone(),
            Arc::clone(&project_open_gates),
            &handshake,
            #[cfg(test)]
            project_open_attempts.clone(),
        ))
        .await
        {
            Ok(server) => server,
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        };
        drop(setup_activity);
        let initialize_handled = write_routed_initialize_response(
            &server,
            &mut transport,
            &first_request_line,
            initialize_route.as_ref(),
        )
        .await?;
        let mut transport = ReplayTransport::new(transport);
        if !initialize_handled {
            transport.push_replay(first_request_line)?;
        }
        Box::pin(server.run_daemon_connection_with_timings(
            &mut transport,
            handshake.timings,
            lifecycle,
        ))
        .await?;
    } else {
        drop(setup_activity);
        let mut transport = ReplayTransport::new(transport);
        transport.push_replay(first_request_line)?;
        Box::pin(serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            lifecycle,
            &store_administration,
        ))
        .await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn write_tool_list_changed_notification(transport: &mut impl McpTransport) -> Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": TOOL_LIST_CHANGED_METHOD,
    });
    transport
        .write_line(&format!("{}\n", serde_json::to_string(&notification)?))
        .await?;
    transport.flush().await?;
    Ok(())
}

async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<crate::tracedecay::TraceDecay> {
    let open_options = handshake.open_options();
    match Box::pin(open_existing_project_with_options(
        project_path,
        open_options.clone(),
    ))
    .await
    {
        Ok(cg) => Ok(cg),
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            match crate::tracedecay::TraceDecay::init_and_index_with_options(
                project_path,
                open_options,
            )
            .await
            {
                Ok(cg) => Ok(cg),
                Err(_) => Err(open_err),
            }
        }
        Err(open_err) => Err(open_err),
    }
}

fn is_missing_index_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains("no TraceDecay index found")
                || message.contains("no TraceDecay database found")
                || message.contains("parent DB not found")
                || (message.contains("parent branch '") && message.contains("' has no DB"))
    )
}

fn is_readonly_database_error(err: &TraceDecayError) -> bool {
    if !err.is_database_error() {
        return false;
    }
    match err {
        TraceDecayError::Database { message, .. } => {
            message.to_ascii_lowercase().contains("readonly database")
        }
        #[allow(deprecated)]
        TraceDecayError::DatabaseOperation { source, .. } => source
            .to_string()
            .to_ascii_lowercase()
            .contains("readonly database"),
        _ => false,
    }
}

fn missing_index_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}' — run 'tracedecay init' first",
            project_path.display()
        ),
    }
}

async fn open_existing_project_with_options(
    project_path: &Path,
    open_options: crate::tracedecay::TraceDecayOpenOptions,
) -> Result<crate::tracedecay::TraceDecay> {
    match crate::tracedecay::TraceDecay::open_with_options(project_path, open_options.clone()).await
    {
        Ok(cg) => Ok(cg),
        Err(open_err) if is_readonly_database_error(&open_err) => {
            match crate::tracedecay::TraceDecay::open_read_only_with_options(
                project_path,
                open_options,
            )
            .await
            {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    Ok(cg)
                }
                Err(_) => Err(open_err),
            }
        }
        Err(error) if is_missing_index_error(&error) => Err(missing_index_error(project_path)),
        Err(error) => Err(error),
    }
}

async fn write_project_open_error(
    transport: &mut impl McpTransport,
    request_line: &str,
    error: &TraceDecayError,
) -> Result<()> {
    let id = serde_json::from_str::<JsonRpcRequest>(request_line)
        .ok()
        .and_then(|request| request.id)
        .unwrap_or(serde_json::Value::Null);
    let response = project_open_error_response(id, error);
    write_json_rpc_response(transport, &response).await
}

fn project_open_error_response(id: serde_json::Value, error: &TraceDecayError) -> JsonRpcResponse {
    match error {
        TraceDecayError::Config { message }
            if message.contains(PROJECT_OPEN_FAILURE_RETRY_HINT) =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_route_open_backoff",
                    "retryable": true,
                    "retry_after_ms": PROJECT_OPEN_FAILURE_RETRY_BACKOFF.as_millis() as u64,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project open task capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_open_task_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_TRACKED_PROJECT_OPEN_TASKS,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project server capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_server_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_CACHED_PROJECT_SERVERS,
                })),
            )
        }
        _ => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}

async fn write_json_rpc_response(
    transport: &mut impl McpTransport,
    response: &crate::mcp::JsonRpcResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

/// Read one newline-delimited frame. Oversized input gets a typed non-durable
/// rejection and returns `Ok(None)` without retaining payload bytes.
async fn read_line_handling_wire_oversized(
    transport: &mut impl McpTransport,
) -> Result<Option<String>> {
    match transport.read_line().await {
        Ok(line) => Ok(line),
        Err(error) if crate::application::host_admission::is_wire_oversized_io_error(&error) => {
            let _ = crate::mcp::transport::write_wire_oversized_rejection(transport, &error).await;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn serve_projectless_client(
    transport: &mut impl McpTransport,
    client_identity: &DaemonClientIdentity,
    lifecycle: &DaemonLifecycle,
    store_administration: &StoreAdministration,
) -> Result<()> {
    loop {
        let line = tokio::select! {
            result = read_line_handling_wire_oversized(transport) => result?,
            () = lifecycle.wait_for_draining() => break,
        };
        let Some(line) = line else {
            break;
        };
        let Some(_activity) = lifecycle.try_enter() else {
            break;
        };
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => {
                projectless_response(&request, client_identity, store_administration).await
            }
            Err(e) => Some(JsonRpcResponse::error(
                json!(null),
                ErrorCode::ParseError,
                format!("Parse error: {e}"),
            )),
        };
        if let Some(response) = response {
            write_json_rpc_response(transport, &response).await?;
        }
        if !lifecycle.accepting() {
            break;
        }
    }
    Ok(())
}

async fn projectless_response(
    request: &crate::mcp::JsonRpcRequest,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> Option<crate::mcp::JsonRpcResponse> {
    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {
                        "listChanged": true
                    }
                },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "tools/call" => Some(
            projectless_tools_call_response(
                id,
                request.params.as_ref(),
                client_identity,
                store_administration,
            )
            .await,
        ),
        "ping" | "logging/setLevel" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    client_identity: &DaemonClientIdentity,
    store_administration: &StoreAdministration,
) -> crate::mcp::JsonRpcResponse {
    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };
    if tool_name == "tracedecay_admin_project" {
        #[derive(serde::Deserialize)]
        #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
        enum ProjectlessAdminProjectAction {
            AutomationReconcile {
                scope: crate::dashboard::AutomationReconcileScope,
            },
        }

        let request = match serde_json::from_value::<ProjectlessAdminProjectAction>(arguments) {
            Ok(request) => request,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InvalidParams,
                    format!("invalid projectless tracedecay_admin_project arguments: {error}"),
                );
            }
        };
        let ProjectlessAdminProjectAction::AutomationReconcile { scope } = request;
        if scope != crate::dashboard::AutomationReconcileScope::Profile {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "project-scoped automation reconciliation requires a project path".to_string(),
            );
        }
        let outcomes = match store_administration
            .reconcile_cached_automation_for_profile(&client_identity.profile_root)
            .await
        {
            Ok(outcomes) => outcomes,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let report = crate::dashboard::ProfileAutomationReconcileReport {
            scope,
            cached_owners: outcomes.len(),
            outcomes,
            uncached_projects:
                crate::dashboard::UncachedProjectReconcileOutcome::DeferredUntilProjectStartup,
        };
        return JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string())
                }]
            }),
        );
    }
    if tool_name == "tracedecay_hook_runtime" {
        let global_db = match store_administration
            .global_database(&client_identity.global_db_path)
            .await
        {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let user_session_db = match store_administration
            .user_session_database(&client_identity.global_db_path)
            .await
        {
            Ok(user_session_db) => user_session_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_state = match store_administration
            .host_admission_broker(&user_session_db)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        let host_admission_broker = match &host_admission_state {
            branch_admin::HostAdmissionBrokerState::Available(broker) => Ok(broker),
            branch_admin::HostAdmissionBrokerState::Unavailable(outcome) => Err(*outcome),
        };
        let refresh_wake = store_administration
            .session_temporal_refresh_schedulers()
            .ensure_profile(
                user_session_db.db_path().to_path_buf(),
                Arc::clone(&user_session_db),
            )
            .await;
        return match crate::mcp::tools::handle_projectless_hook_runtime(
            arguments,
            &client_identity.profile_root,
            global_db.as_ref(),
            crate::mcp::tools::SessionAuthorities::new(None, Some(&user_session_db)),
            host_admission_broker,
        )
        .await
        {
            Ok(result) => {
                refresh_wake.wake();
                JsonRpcResponse::success(id, result.value)
            }
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    if tool_name == "tracedecay_admin_cli" {
        let global_db = match store_administration
            .global_database(&client_identity.global_db_path)
            .await
        {
            Ok(global_db) => global_db,
            Err(error) => {
                return JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
            }
        };
        return match crate::mcp::tools::handle_projectless_admin_cli(
            arguments,
            &global_db,
            &client_identity.profile_root,
        )
        .await
        {
            Ok(result) => JsonRpcResponse::success(id, result.value),
            Err(error) => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
        };
    }
    JsonRpcResponse::error(
        id,
        ErrorCode::InternalError,
        format!("{tool_name} requires an initialized code project"),
    )
}

fn projectless_tool_call(
    params: Option<&serde_json::Value>,
) -> std::result::Result<(&str, serde_json::Value), &'static str> {
    let Some(params) = params else {
        return Err("missing params for tools/call");
    };
    let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
        return Err("missing 'name' in tools/call params");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Ok((tool_name, arguments))
}

struct BrokerStreamTransport {
    reader: tokio::io::BufReader<tokio::io::ReadHalf<BrokerStream>>,
    writer: tokio::io::WriteHalf<BrokerStream>,
}

impl BrokerStreamTransport {
    fn new(stream: BrokerStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: tokio::io::BufReader::new(reader),
            writer,
        }
    }
}

impl crate::mcp::McpTransport for BrokerStreamTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        crate::application::host_admission::read_bounded_mcp_line(&mut self.reader).await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.writer.write_all(line.as_bytes()).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush().await
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod wire_bound_tests {
    use super::{BrokerStreamTransport, read_line_handling_wire_oversized};
    use crate::application::host_admission::{WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error};
    use crate::mcp::McpTransport;
    use tokio::io::AsyncWriteExt;

    use super::transport::{BrokerListener, BrokerStream, default_loopback_endpoint};

    #[tokio::test]
    async fn broker_transport_streams_hostile_frame_and_typed_rejection_has_no_payload() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let mut client = client;
            // Stream hostile bytes without pre-building a MAX+1 String in the
            // product reader path; allocate only a small chunk buffer here.
            let chunk = vec![b'w'; 8192];
            let mut remaining =
                crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 64 * 1024;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
        });

        let err = server_transport.read_line().await.expect_err("oversized");
        assert!(is_wire_oversized_io_error(&err));
        assert_eq!(err.to_string(), WIRE_RECORD_TOO_LARGE);
        // Reason code is `wire_record_too_large` (contains 'w'); assert the
        // hostile fill pattern itself is not echoed.
        assert!(!err.to_string().contains("wwww"));
        writer.await.expect("writer");
    }

    #[tokio::test]
    async fn broker_transport_accepts_exact_cap_and_recovers_next_frame_after_oversize() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let mut client = client;
            let chunk = vec![b'a'; 8192];
            let mut remaining = crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write exact");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("exact newline");

            let chunk = vec![b'z'; 8192];
            let mut remaining = crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 1;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client
                    .write_all(&chunk[..n])
                    .await
                    .expect("write oversized");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("oversized newline");
            client
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
                .await
                .expect("next frame");
            client.flush().await.expect("flush");
        });

        assert_eq!(
            server_transport
                .read_line()
                .await
                .expect("exact accepted")
                .expect("exact line")
                .len(),
            crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
        );
        let error = server_transport
            .read_line()
            .await
            .expect_err("one over rejected");
        assert!(is_wire_oversized_io_error(&error));
        assert_eq!(
            server_transport
                .read_line()
                .await
                .expect("next read")
                .as_deref(),
            Some(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        );
        writer.await.expect("writer");
    }

    #[tokio::test]
    async fn read_line_handling_writes_typed_rejection_without_payload_bytes() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let prefix =
                br#"{"jsonrpc":"2.0","id":"daemon-7","method":"tools/call","params":{"payload":""#;
            client.write_all(prefix).await.expect("prefix");
            let chunk = vec![b'q'; 4096];
            let mut remaining = crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
                + 32 * 1024
                - prefix.len();
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
            client
        });

        let outcome = read_line_handling_wire_oversized(&mut server_transport)
            .await
            .expect("typed handling");
        assert!(outcome.is_none());

        let mut client = writer.await.expect("writer");
        let mut response = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut client, &mut buf)
                .await
                .expect("read rejection");
            if n == 0 {
                break;
            }
            response.extend_from_slice(&buf[..n]);
            if response.contains(&b'\n') {
                break;
            }
        }
        let response: serde_json::Value =
            serde_json::from_slice(&response).expect("JSON-RPC rejection");
        assert_eq!(response["id"], serde_json::json!("daemon-7"));
        assert_eq!(response["error"]["code"], serde_json::json!(-32600));
        assert_eq!(
            response["error"]["message"],
            serde_json::json!(WIRE_RECORD_TOO_LARGE)
        );
        assert!(!response.to_string().contains('q'));
    }
}
