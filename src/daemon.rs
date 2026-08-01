use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rmcp::ServiceExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};
use tokio_stream::StreamExt;
use tracedecay_lsp::{AdmittedRoot, AuthorizedLspWorkspace, LspSessionRegistry};
use tracedecay_query::code_search;

use crate::application::context::CancellationToken;
use crate::application_surface::ApplicationSurfaceOperation;
use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::ReplayTransport;
use crate::mcp::server::{
    McpMethod, RmcpConnectionAdapter, RmcpInitializeResponseDecorator, SERVER_INSTRUCTIONS,
    classify_mcp_method, initialize_result,
};
use crate::mcp::tools::{
    ToolRegistryMode, default_catalog_discovery_authority, explore_call_budget,
    get_catalog_filtered_tool_definitions_with_budget,
    get_catalog_filtered_tool_definitions_with_warming_budget, project_catalog_discovery_scope,
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
/// Backoff for a persisted-row authority defect, which only an operator can
/// clear. Reopening re-runs the exhaustive authority audit over every
/// `observations` row and fails on the same row every time, so the debounce
/// cadence above would saturate a core for as long as the daemon runs.
const PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF: Duration = Duration::from_mins(5);
const PROJECT_OPEN_FAILURE_RETRY_HINT: &str =
    "project route open is backed off after an invariant rejection";

mod authority;
mod bootstrap;
mod branch_add;
mod branch_admin;
mod broker_stream_transport;
use broker_stream_transport::BrokerStreamTransport;
mod callable_code_authorization;
mod code_index_executor;
use code_index_executor::{
    code_index_scope_unavailable, code_index_search_display_binding, code_index_search_executor,
    code_index_search_hydration_budget, mcp_search_request_termination,
};
pub(crate) mod code_index_scheduler;
mod connection_serving;
pub(crate) mod context_scout_lifecycle;
#[cfg(unix)]
use connection_serving::serve_authenticated_socket_client_with_class;
#[cfg(any(not(unix), test))]
use connection_serving::serve_windows_broker_client_with_class_and_invocation;
#[cfg(test)]
use connection_serving::{
    await_project_owner_or_disconnect, serve_routed_rmcp_connection, serve_windows_broker_client,
    serve_windows_broker_client_with_class,
};
#[cfg(all(unix, test))]
use connection_serving::{serve_authenticated_socket_client, serve_socket_client};
mod core_admission;
mod core_client;
mod core_doctor;
mod core_handshake;
mod core_hooks;
mod core_lifecycle;
mod core_logging;
mod core_proxy;
mod database_owner_registry;
use database_owner_registry::{DatabaseOwnerRegistry, settle_deferred_post_open_health};
pub(crate) mod doctor_kernel;
pub(crate) mod hook_v2_replay;
pub(crate) mod project_open_owners;
pub(crate) mod query_authority_provider;
mod semantic_evaluation;
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
mod github_credential_lifecycle;
mod graph_resolution;
use graph_resolution::retained_project_graph_resolver;
mod http_application;
mod invocation_executor;
use invocation_executor::{
    FederatedSurfaceRequestV1, InProcessDaemonInvocationExecutor, PrecomputedMultiRootQueryPort,
    denied_root_generation, explicit_git_state, extract_work_application_payload,
    frozen_root_generation, invocation_is_git_operation, multi_root_family_allows,
    unavailable_root_generation,
};
mod invocation_state;
use invocation_state::DaemonInvocationState;
mod lsp_gateway;
mod maintenance;
#[path = "daemon/git_watch/store_maintenance.rs"]
mod store_maintenance;
pub(crate) use lsp_gateway::{
    BrokerDiagnosticSnapshotAuthority, DaemonLspSessionFactory, DaemonSemanticProviderAdapter,
    LspDiagnosticDocumentPort, LspSemanticRequestAuthority,
};
#[cfg(unix)]
mod memory_repair_scheduler;
#[cfg(unix)]
pub mod pr_autotrack;
mod production_harness;
#[cfg(any(test, feature = "test-transport"))]
pub use production_harness::ProductionProjectCompositionHarnessV1;
#[cfg(all(unix, feature = "test-transport"))]
pub use production_harness::capture_exact_git_snapshot_for_test;
mod profile_host_admission_replay;
mod projectless;
use projectless::{
    projectless_tool_call, projectless_tools_call_response, projectless_user_session_request,
    serve_projectless_client,
};
pub(crate) mod profile_identity;
mod project_composition;
#[cfg(test)]
use project_composition::daemon_transcript_source_home;
use project_composition::{ProductionProjectCompositionRuntime, production_project_server};
mod project_open_admission;
use project_open_admission::{
    MaintenanceRekeyOutcome, MaintenanceTransitionGate, MaintenanceTransitionGates,
    MaintenanceTransitionKey, ProjectOpenFailure, ProjectOpenGate, ProjectOpenGates,
    ProjectOpenTaskClaim, ProjectOpenTaskState, ProjectOpenTasks, ProjectRouteKey,
    ProjectServerKey, ProjectServerPublication, ProjectServerRequirement, StoreOwnerKey,
    project_open_retry_backoff, project_server_requirement,
};
mod project_routing;
use project_routing::{
    CatalogRefreshClientKey, bind_authenticated_profile_identity, maintenance_transition_gate,
    portable_database_owner_reconciler, project_open_cancellation_checkpoint,
    project_open_cancellation_error, project_open_gate, project_open_task_capacity_error,
    project_open_tasks, project_route_for_handshake, project_server_capacity_error,
    project_warming_error,
};
mod query_mcp_admission;
#[cfg(unix)]
mod scheduler;
mod service;
pub(crate) mod session_temporal_refresh_scheduler;
pub(crate) mod store_runtime;
pub(crate) mod work_runtime;
pub(crate) mod workflow_runtime;

/// Enables background maintenance only for long-lived daemon/MCP processes.
///
/// Session-store mounts retain the registered database authority for the
/// lifetime of each maintenance task. One-shot commands never enable it.
pub fn mark_process_long_lived_for_session_maintenance() {
    store_runtime::session_registry::mark_process_long_lived_for_session_maintenance();
}

const SEMANTIC_ARTIFACT_GC_PERIOD: Duration = Duration::from_hours(24);

struct SemanticArtifactGcMaintenanceTask(JoinHandle<()>);

impl Drop for SemanticArtifactGcMaintenanceTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn spawn_semantic_artifact_gc_maintenance() -> SemanticArtifactGcMaintenanceTask {
    SemanticArtifactGcMaintenanceTask(tokio::spawn(async {
        let mut interval = tokio::time::interval(SEMANTIC_ARTIFACT_GC_PERIOD);
        loop {
            interval.tick().await;
            let Some(owner) = crate::semantic_code::SemanticModelLifecycleOwnerV1::mounted_shared()
            else {
                continue;
            };
            let now_unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if owner.run_daemon_artifact_gc(now_unix).is_err() {
                log_daemon_event(
                    "semantic_artifact_gc",
                    &[("outcome", "retry_next_interval".to_owned())],
                );
            }
        }
    }))
}

pub(crate) mod transport;
#[cfg(test)]
pub(crate) use crate::daemon_contract::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonInvocationProblem,
};
/// The wire contract now lives in `crate::daemon_contract`, outside this module
/// tree. Daemon-internal call sites keep naming it through `crate::daemon::` so
/// the move stayed mechanical; new callers should depend on the contract module
/// directly rather than widening this re-export.
pub(crate) use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationRequest, DaemonInvocationResponse,
    parse_daemon_invocation_request,
};
#[cfg(all(unix, test))]
use bootstrap::drain_client_tasks;
pub use bootstrap::run_foreground;
pub(crate) use service::invocation::{
    BoundedPr13HookOrchestratorV1, DaemonAdvisoryRuntimeRegistrar,
    DaemonAdvisoryRuntimeRegistrationError, DaemonConfigurationRuntimeRegistrar,
    DaemonContextScoutRuntimeRegistrar, DaemonContextScoutRuntimeRegistrationError,
    DaemonFeedbackRuntimeRegistrar, DaemonFeedbackRuntimeRegistrationError,
    DaemonInvocationService, DaemonLspOwnerRegistrar, DaemonPrimitiveRuntimeRegistrar,
    DaemonPrimitiveRuntimeRegistrationError, DaemonSemanticRuntimeRegistrar,
    DaemonSemanticRuntimeRegistrationError, DaemonWorkRuntimeRegistrar,
    Pr13HookOrchestrationAdmissionV1, Pr13HookOrchestrationRequestV1,
    Pr13HookOrchestrationTriggerV1, admit_registered_pr13_hook_orchestration,
    daemon_operation_event_authority,
};
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

#[derive(Clone, Default)]
struct DaemonEngine {
    lifecycle: DaemonLifecycle,
    /// Closed post-handshake operations backed by daemon-owned session actors.
    /// Git and feedback remain unavailable until their authoritative request
    /// owners register daemon-minted handles; no client-side fallback exists.
    invocation: DaemonInvocationState,
    /// Project-scoped canonical application routers served by the daemon's
    /// standalone authenticated loopback HTTP listener.
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
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
    /// Platform-neutral retention owner. The Unix watcher may wake this task,
    /// but never owns its cadence or lifecycle.
    maintenance_coordinator: maintenance::MaintenanceCoordinator,
    /// PR reconciliation task, retained so shutdown never leaves it writing.
    pr_autotrack_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

/// Retain one daemon-owned Git index transaction service for the project store
/// and reconcile any durable records before mutation owners become available.
/// Read-only core tools and edit previews do not depend on this service. The
/// service owns the store actor; constructing a second service for the same
/// database is rejected by the registry.
async fn ensure_git_index_transactions_for_mutation_owners(
    store_administration: &StoreAdministration,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
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
    let Some(repository_root) = crate::worktree::git_worktree_root(project_root) else {
        // Non-Git projects remain valid TraceDecay projects. They advertise no
        // Git mutation authority and must not fail project-open admission.
        return Ok(());
    };
    let observed_at = tracedecay_domain::UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
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

fn ensure_context_scout_owner_before_advertising(
    project: &crate::tracedecay::TraceDecay,
) -> Result<()> {
    if project.store_layout().identity.project_id.is_none() {
        return Ok(());
    }
    let owner = project
        .context_scout_owner()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project Context Scout owner did not start".to_owned(),
        })?;
    if matches!(
        owner.startup_outcome(),
        crate::agents::context_scout_v2::ContextScoutDurableStartupOutcomeV1::Unavailable
    ) {
        return Err(TraceDecayError::Config {
            message: "project Context Scout durable owner is unavailable".to_owned(),
        });
    }
    Ok(())
}

fn build_http_application_router(project_id: &str, project_path: &Path) -> Result<axum::Router> {
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("daemon HTTP project identity is invalid: {error}"),
        }
    })?;
    let handshake =
        DaemonHandshake::for_current_client(Some(project_path.to_path_buf()), None, false, false)?;
    let client = crate::daemon_client::DaemonInvocationClient::for_current(handshake)?;
    crate::application_surface::http_application_router(
        client,
        daemon_operation_event_authority(),
        project_id.clone(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not mount daemon HTTP application routes: {error}"),
    })
}

fn install_http_application_cold_resolver(
    registry: &http_application::DaemonHttpApplicationRegistry,
    store_administration: StoreAdministration,
) -> Result<()> {
    registry.install_resolver(move |project_id| {
        let store_administration = store_administration.clone();
        async move {
            let database = store_administration.registered_profile_database().await?;
            let Some(context) = database
                .project_registry_context_by_id(project_id.as_str())
                .await?
            else {
                return Ok(None);
            };
            if context.project.project_id != project_id.as_str() {
                return Err(TraceDecayError::Config {
                    message: "daemon HTTP project registry identity changed".to_owned(),
                });
            }
            let registered_root = PathBuf::from(&context.project.canonical_root);
            if !registered_root.is_absolute() {
                return Err(TraceDecayError::Config {
                    message: "daemon HTTP registered project root is not absolute".to_owned(),
                });
            }
            let canonical_root =
                registered_root
                    .canonicalize()
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "daemon HTTP registered project root is unavailable: {error}"
                        ),
                    })?;
            if canonical_root != registered_root {
                return Err(TraceDecayError::Config {
                    message: "daemon HTTP registered project root is not canonical".to_owned(),
                });
            }
            build_http_application_router(project_id.as_str(), &canonical_root).map(Some)
        }
    })
}

async fn mount_http_application_router(
    registry: &http_application::DaemonHttpApplicationRegistry,
    project_id: &str,
    project_path: &Path,
) -> Result<()> {
    if !registry.is_active() {
        return Ok(());
    }
    let router = build_http_application_router(project_id, project_path)?;
    registry.mount(project_id, router).await
}

#[cfg(unix)]
impl DaemonEngine {
    fn with_profile_identity(
        mut self,
        profile_identity: profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        self.invocation
            .configure_github_read_only_credentials(&profile_identity);
        self.store_administration = self
            .store_administration
            .with_profile_identity(profile_identity);
        self
    }

    fn with_http_application_registry(
        mut self,
        registry: http_application::DaemonHttpApplicationRegistry,
    ) -> Self {
        self.http_application_registry = registry;
        self
    }

    /// Installs the config-driven git-metadata watcher on this engine. Called
    /// once by `run_foreground_unix` before the accept loop.
    fn with_git_watcher(mut self, watcher: git_watch::GitWatcher) -> Self {
        self.git_watcher = watcher;
        self
    }

    fn with_maintenance_coordinator(
        mut self,
        coordinator: maintenance::MaintenanceCoordinator,
    ) -> Self {
        self.maintenance_coordinator = coordinator;
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

    #[cfg(test)]
    async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let cancellation = CancellationToken::new();
        self.project_server_until_cancelled(handshake, &cancellation)
            .await
    }

    async fn project_server_until_cancelled(
        &self,
        handshake: &DaemonHandshake,
        cancellation: &CancellationToken,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self.cached_project_server(handshake).await? {
            return Ok(server);
        }

        let cached = self
            .store_administration
            .with_writer_until_cancelled(cancellation, || {
                self.open_project_server_until_cancelled(handshake, cancellation)
            })
            .await
            .ok_or_else(project_open_cancellation_error)??;
        let (_key, project_path, server, _inserted) = cached;
        project_open_cancellation_checkpoint(cancellation)?;
        Ok(self.activate_project_server(project_path, server).await)
    }

    async fn cached_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<Arc<crate::mcp::McpServer>>> {
        self.cached_project_server_for_requirement(handshake, ProjectServerRequirement::Core)
            .await
    }

    async fn cached_project_server_for_requirement(
        &self,
        handshake: &DaemonHandshake,
        requirement: ProjectServerRequirement,
    ) -> Result<Option<Arc<crate::mcp::McpServer>>> {
        let (project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route_and_touch_for(&route, requirement)
                .map(|(_, server)| Arc::clone(server))
        };
        let Some(server) = cached else {
            return Ok(None);
        };
        self.ensure_registered_project_route(&project_path, handshake.allow_init)
            .await?;
        Ok(Some(
            self.activate_project_server(project_path, server).await,
        ))
    }

    async fn begin_project_open(
        &self,
        handshake: DaemonHandshake,
        initialize_request: Option<JsonRpcRequest>,
    ) -> Result<ProjectOpenTaskClaim> {
        let (project_path, route) = Self::project_route(&handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        let engine = self.clone();
        let open_handshake = handshake.clone();
        Ok(Box::pin(start_lifecycle_project_open(
            &tasks,
            self.lifecycle.clone(),
            route,
            project_path,
            initialize_request,
            move |cancellation| async move {
                engine
                    .project_server_until_cancelled(&open_handshake, &cancellation)
                    .await
            },
        ))
        .await)
    }

    /// Rejects ambient working directories before scheduling project warm-up.
    ///
    /// Host MCP clients may start from `$HOME` and include that directory in
    /// their handshake. Opening it as a project would perform graph and index
    /// work before session-store resolution eventually notices the missing
    /// enrollment. Registry alias and repository-identity lookups preserve
    /// linked-worktree routing without manufacturing path-derived authority.
    async fn ensure_registered_project_route(
        &self,
        project_path: &Path,
        allow_init: bool,
    ) -> Result<()> {
        ensure_registered_project_route(&self.store_administration, project_path, allow_init).await
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
        requirement: ProjectServerRequirement,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self
            .cached_project_server_for_requirement(handshake, requirement)
            .await?
        {
            return Ok(server);
        }
        let (project_path, _) = Self::project_route(handshake)?;
        // Bound only the wait behind an unrelated writer. An uncontended open
        // is this request's own work and must run to completion.
        let contended = self.store_administration.writer_is_busy();
        let claim = Box::pin(self.begin_project_open(handshake.clone(), None)).await?;
        match claim {
            ProjectOpenTaskClaim::InFlight(mut state) => {
                let publication = async {
                    loop {
                        if let Some(server) = self
                            .cached_project_server_for_requirement(handshake, requirement)
                            .await?
                        {
                            return Ok(server);
                        }
                        let current = state.borrow().clone();
                        match current {
                            ProjectOpenTaskState::Opening => {
                                tokio::select! {
                                    changed = state.changed() => {
                                        changed.map_err(|_| TraceDecayError::Config {
                                            message: "project open task ended before reporting an outcome"
                                                .to_string(),
                                        })?;
                                    }
                                    () = tokio::time::sleep(Duration::from_millis(25)) => {}
                                }
                            }
                            ProjectOpenTaskState::Ready => {
                                return Err(TraceDecayError::Config {
                                    message: "project open completed without publishing a server"
                                        .to_string(),
                                });
                            }
                            ProjectOpenTaskState::Failed(failure) => {
                                return Err(failure.to_error());
                            }
                        }
                    }
                };
                if contended {
                    timeout(PROJECT_OPEN_REQUEST_DEADLINE, publication)
                        .await
                        .map_err(|_| project_warming_error(&project_path))?
                } else {
                    publication.await
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
    #[cfg(test)]
    async fn open_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let cancellation = CancellationToken::new();
        self.open_project_server_until_cancelled(handshake, &cancellation)
            .await
    }

    async fn open_project_server_until_cancelled(
        &self,
        handshake: &DaemonHandshake,
        cancellation: &CancellationToken,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        self.ensure_registered_project_route(&canonical_project_path, handshake.allow_init)
            .await?;
        let composition = production_project_server(
            &self.store_administration,
            self.project_open_gates.as_ref(),
            &self.invocation,
            &self.http_application_registry,
            &canonical_project_path,
            handshake,
            ProductionProjectCompositionRuntime::Unix(Box::new(self.clone())),
            cancellation,
            #[cfg(test)]
            Some(&self.project_open_attempts),
        )
        .await?;
        if composition.inserted {
            self.spawn_project_maintenance_activation(
                composition.key.clone(),
                composition.canonical_project_path.clone(),
                handshake.clone(),
                Arc::clone(&composition.server),
            );
        }
        Ok((
            composition.key,
            composition.canonical_project_path,
            composition.server,
            composition.inserted,
        ))
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
                        let new_session_db = match new_key.owner.project_id.as_deref() {
                            Some(_) => engine
                                .store_administration
                                .registered_project_session_database(
                                    fresh.project_root(),
                                    fresh.store_layout(),
                                )
                                .await
                                .ok(),
                            None => None,
                        };
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
        self.invocation.shutdown().await;
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

        self.maintenance_coordinator.shutdown().await;
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

async fn cancel_project_server_startup_ingests(store_administration: &StoreAdministration) {
    let servers = {
        let registry = store_administration.project_servers().lock().await;
        let mut seen = HashSet::new();
        registry
            .values()
            .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
            .cloned()
            .collect::<Vec<_>>()
    };
    for server in servers {
        server.cancel_startup_transcript_ingest();
    }
}

async fn shutdown_project_servers(store_administration: &StoreAdministration) {
    store_administration.join_project_server_retirements().await;
    let servers = detach_project_servers(store_administration).await;
    shutdown_detached_project_servers(servers).await;
}

async fn detach_project_servers(
    store_administration: &StoreAdministration,
) -> Vec<Arc<crate::mcp::McpServer>> {
    let servers: Vec<Arc<crate::mcp::McpServer>> = store_administration
        .with_writer(|| async {
            let mut registry = store_administration.project_servers().lock().await;
            let mut seen = HashSet::new();
            let servers = registry
                .values()
                .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
                .cloned()
                .collect();
            // Servers retain daemon callbacks that clone StoreAdministration.
            // Remove the registry's side of that cycle before awaiting server
            // shutdown so every physical store runtime can be dropped.
            registry.servers.clear();
            registry.aliases.clear();
            servers
        })
        .await;
    servers
}

async fn shutdown_detached_project_servers(servers: Vec<Arc<crate::mcp::McpServer>>) {
    for server in servers {
        let graph = server.cg().await;
        hook_v2_replay::shutdown_hook_v2_replay_consumer(&graph.hook_store_layout().data_root)
            .await;
        drop(graph);
        server.shutdown().await;
    }
}

const PROJECT_SERVER_REQUEST_DRAIN_DEADLINE: Duration = Duration::from_secs(35);
const PROJECT_SERVER_ABORT_DRAIN_DEADLINE: Duration = Duration::from_secs(2);

async fn wait_for_project_server_request_drains(servers: &[Arc<crate::mcp::McpServer>]) {
    for server in servers {
        server.wait_for_project_server_request_drain().await;
    }
}

async fn retire_project_servers(
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    if tokio::time::timeout(
        PROJECT_SERVER_REQUEST_DRAIN_DEADLINE,
        wait_for_project_server_request_drains(&servers),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            deadline_secs = PROJECT_SERVER_REQUEST_DRAIN_DEADLINE.as_secs(),
            server_count = servers.len(),
            "retired project requests exceeded their drain deadline; cancelling them"
        );
        for server in &servers {
            server.abort_project_server_requests();
        }
        if tokio::time::timeout(
            PROJECT_SERVER_ABORT_DRAIN_DEADLINE,
            wait_for_project_server_request_drains(&servers),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                deadline_secs = PROJECT_SERVER_ABORT_DRAIN_DEADLINE.as_secs(),
                server_count = servers.len(),
                "cancelled project requests have not yielded; retaining safe shutdown ownership"
            );
            wait_for_project_server_request_drains(&servers).await;
        }
    }
    if let Some(route_registered) = route_registered {
        route_registered.store(false, Ordering::Release);
    }
    for server in servers {
        server.shutdown().await;
    }
}

async fn schedule_project_server_retirement(
    store_administration: &StoreAdministration,
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    let retirement = tokio::spawn(retire_project_servers(servers, route_registered));
    store_administration
        .track_project_server_retirement(retirement)
        .await;
}

/// Kick coalesced per-profile replay without awaiting a pass (handshake-safe).
async fn ensure_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    _client_identity: &DaemonClientIdentity,
) -> Result<()> {
    let user_session_db = match store_administration
        .registered_profile_session_database()
        .await
    {
        Ok(database) => database,
        Err(error) => {
            eprintln!(
                "[tracedecay] user-profile host admission disposition: authority_unavailable: {error}"
            );
            return Ok(());
        }
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
    let registry = store_administration.registered_profile_database().await?;
    let Some(route) =
        resolve_daemon_initialize_route(request.params.as_ref(), Some(&registry)).await?
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
            let budget = explore_call_budget(project_node_count.unwrap_or(0));
            let profile_id = tracedecay_tool_catalog::ProfileId::new(
                tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
            );
            let authority = default_catalog_discovery_authority();
            match (profile_id, authority) {
                (Ok(profile_id), Ok(authority)) => {
                    let definitions = match project_node_count {
                        Some(node_count) => get_catalog_filtered_tool_definitions_with_budget(
                            node_count,
                            budget,
                            &profile_id,
                            &authority,
                            &project_catalog_discovery_scope(),
                            ToolRegistryMode::HostAvailable,
                        ),
                        None => get_catalog_filtered_tool_definitions_with_warming_budget(
                            budget,
                            &profile_id,
                            &authority,
                            &project_catalog_discovery_scope(),
                            ToolRegistryMode::HostAvailable,
                        ),
                    };
                    match definitions {
                        Ok(tools) => JsonRpcResponse::success(id, json!({ "tools": tools })),
                        Err(_) => JsonRpcResponse::error(
                            id,
                            ErrorCode::InternalError,
                            "MCP catalog discovery unavailable".to_owned(),
                        ),
                    }
                }
                _ => JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    "MCP catalog discovery unavailable".to_owned(),
                ),
            }
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
    ensure_registered_project_route(
        store_administration,
        &canonical_project_path,
        handshake.allow_init,
    )
    .await
    .ok()?;
    server
        .cg()
        .await
        .get_stats()
        .await
        .ok()
        .map(|stats| stats.node_count)
}

async fn start_lifecycle_project_open<OpenOperation, OpenFuture>(
    tasks: &ProjectOpenTasks,
    lifecycle: DaemonLifecycle,
    route: ProjectRouteKey,
    project_path: PathBuf,
    initialize_request: Option<JsonRpcRequest>,
    open_project_server: OpenOperation,
) -> ProjectOpenTaskClaim
where
    OpenOperation: FnOnce(CancellationToken) -> OpenFuture + Send + 'static,
    OpenFuture: std::future::Future<Output = Result<Arc<crate::mcp::McpServer>>> + Send + 'static,
{
    if !lifecycle.accepting() {
        return ProjectOpenTaskClaim::Failed(ProjectOpenFailure {
            message: "daemon is draining before project warm-up".to_string(),
            retry_at: None,
        });
    }
    tasks
        .start_cancellable(route, move |cancellation| async move {
            let Some(activity) = lifecycle.try_enter() else {
                return Err(TraceDecayError::Config {
                    message: "daemon is draining before project warm-up".to_string(),
                });
            };
            let _activity = activity;
            // Once admitted, warm-up may be inside a schema migration. The
            // cancellation token is observed only at explicit boundaries around
            // those transactionally safe units; dropping this future on drain
            // would untrack the database owner and can interrupt SQLite
            // mid-statement. The lifecycle activity remains held until the task
            // reports its terminal outcome and shutdown explicitly joins it.
            let result = Box::pin(open_project_server(cancellation.clone())).await;
            match result {
                Ok(server) => {
                    project_open_cancellation_checkpoint(&cancellation)?;
                    if let Some(initialize_request) = initialize_request {
                        // Preserve the regular initialize side effect that records
                        // the negotiated MCP client name on the real server.
                        let initialize: std::pin::Pin<
                            Box<
                                dyn std::future::Future<Output = Option<JsonRpcResponse>>
                                    + Send
                                    + '_,
                            >,
                        > = Box::pin(server.handle_request(&initialize_request));
                        let _ = initialize.await;
                    }
                    Ok(())
                }
                Err(error) => {
                    if cancellation.is_cancelled() {
                        return Err(error);
                    }
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

#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
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

async fn ensure_registered_project_route(
    store_administration: &StoreAdministration,
    project_path: &Path,
    allow_init: bool,
) -> Result<()> {
    let registry = store_administration.registered_profile_database().await?;
    let context = match registry
        .project_registry_context_by_alias(project_path)
        .await?
    {
        Some(context) => Some(context),
        None => {
            let git_root = crate::worktree::git_worktree_root(project_path)
                .unwrap_or_else(|| project_path.to_path_buf());
            let git_common_dir = crate::worktree::git_common_dir(&git_root);
            registry
                .project_registry_context_by_identity(&git_root, git_common_dir.as_deref())
                .await?
        }
    };
    if context.is_none() {
        let project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());
        let is_project_root = crate::worktree::git_worktree_root(&project_path)
            .is_none_or(|git_root| git_root == project_path);
        let owns_repository_identity =
            crate::worktree::repository_identity_root(&project_path).is_none();
        if allow_init && is_project_root && owns_repository_identity {
            return Ok(());
        }
        return Err(unenrolled_project_route_error(&project_path));
    }
    Ok(())
}

fn unenrolled_project_route_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}': project is not enrolled in the authenticated \
             profile; run 'tracedecay init' first",
            project_path.display()
        ),
    }
}

#[cfg(any(not(unix), test))]
async fn portable_cached_project_server(
    store_administration: &StoreAdministration,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
    requirement: ProjectServerRequirement,
) -> Result<Option<Arc<crate::mcp::McpServer>>> {
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    let server = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch_for(&route, requirement)
            .map(|(_, server)| Arc::clone(server))
    };
    let Some(server) = server else {
        return Ok(None);
    };
    ensure_registered_project_route(
        store_administration,
        canonical_project_path,
        handshake.allow_init,
    )
    .await?;
    Ok(Some(server))
}

#[cfg(any(not(unix), test))]
// Cohesive route-open context; a params struct would only move the same ownership bundle.
#[allow(clippy::too_many_arguments)]
async fn begin_portable_project_open(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
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
        move |cancellation| async move {
            store_administration
                .with_writer_until_cancelled(&cancellation, || async {
                    production_project_server(
                        &store_administration,
                        open_gates.as_ref(),
                        &invocation,
                        &http_application_registry,
                        &open_project_path,
                        &handshake,
                        ProductionProjectCompositionRuntime::Portable {
                            semantic_auto_download: true,
                            startup_catch_up: true,
                        },
                        &cancellation,
                        #[cfg(test)]
                        project_open_attempts.as_ref(),
                    )
                    .await
                    .map(|composition| composition.server)
                })
                .await
                .ok_or_else(project_open_cancellation_error)?
        },
    ))
    .await
}

#[cfg(any(not(unix), test))]
async fn schedule_portable_project_server_warmup(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: DaemonHandshake,
    initialize_request: JsonRpcRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let (canonical_project_path, route) = project_route_for_handshake(&handshake)?;
    if portable_cached_project_server(
        &store_administration,
        &canonical_project_path,
        &handshake,
        ProjectServerRequirement::Core,
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    match Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration,
        project_open_gates,
        invocation,
        http_application_registry,
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
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: &DaemonHandshake,
    requirement: ProjectServerRequirement,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<Arc<crate::mcp::McpServer>> {
    let (canonical_project_path, route) = project_route_for_handshake(handshake)?;
    if let Some(server) = portable_cached_project_server(
        &store_administration,
        &canonical_project_path,
        handshake,
        requirement,
    )
    .await?
    {
        return Ok(server);
    }
    // Match the Unix path: only a request queued behind an unrelated writer
    // gets the retry deadline.
    let contended = store_administration.writer_is_busy();
    let claim = Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration.clone(),
        project_open_gates,
        invocation,
        http_application_registry,
        handshake.clone(),
        canonical_project_path.clone(),
        route,
        None,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await;
    match claim {
        ProjectOpenTaskClaim::InFlight(mut state) => {
            let publication = async {
                loop {
                    if let Some(server) = portable_cached_project_server(
                        &store_administration,
                        &canonical_project_path,
                        handshake,
                        requirement,
                    )
                    .await?
                    {
                        return Ok(server);
                    }
                    let current = state.borrow().clone();
                    match current {
                        ProjectOpenTaskState::Opening => {
                            tokio::select! {
                                changed = state.changed() => {
                                    changed.map_err(|_| TraceDecayError::Config {
                                        message: "project open task ended before reporting an outcome"
                                            .to_string(),
                                    })?;
                                }
                                () = tokio::time::sleep(Duration::from_millis(25)) => {}
                            }
                        }
                        ProjectOpenTaskState::Ready => {
                            return Err(TraceDecayError::Config {
                                message: "project open completed without publishing a server"
                                    .to_string(),
                            });
                        }
                        ProjectOpenTaskState::Failed(failure) => {
                            return Err(failure.to_error());
                        }
                    }
                }
            };
            if contended {
                timeout(PROJECT_OPEN_REQUEST_DEADLINE, publication)
                    .await
                    .map_err(|_| project_warming_error(&canonical_project_path))?
            } else {
                publication.await
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

/// Multi-root payloads are routed by `invoke_for_project`, which reaches the
/// executor without passing through `DaemonInvocationService::invoke`'s own
/// `validate` gate. Validating them here keeps a malformed multi-root request
/// from costing a project admission before it is rejected; authorization stays
/// with the `AuthorizedScopeSet` compare-and-swap on the executor side.
fn invalid_multi_root_invocation_response(
    request: &DaemonInvocationRequest,
) -> Option<DaemonInvocationResponse> {
    let multi_root_payload = matches!(
        &request.payload,
        service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead { .. }
            | service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. }
            | service::invocation::DaemonInvocationPayload::MultiRootExecute { .. }
    );
    if !multi_root_payload {
        return None;
    }
    request
        .validate()
        .err()
        .map(|problem| DaemonInvocationResponse::problem(request.request_id.clone(), problem))
}

#[cfg(any(not(unix), test))]
async fn execute_portable_daemon_invocation(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: &DaemonHandshake,
    invocation: &DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    request: DaemonInvocationRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> DaemonInvocationResponse {
    if let Some(response) = invalid_multi_root_invocation_response(&request) {
        return response;
    }
    let request_id = request.request_id.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let mut project_path = None;
    if request.requires_project() {
        if Box::pin(portable_project_server_for_request(
            lifecycle,
            store_administration.clone(),
            project_open_gates,
            invocation.clone(),
            http_application_registry,
            handshake,
            ProjectServerRequirement::Core,
            #[cfg(test)]
            project_open_attempts,
        ))
        .await
        .is_err()
        {
            return DaemonInvocationResponse::problem(
                request_id,
                if git_operation {
                    DaemonInvocationProblem::NotFoundOrNotAuthorized
                } else {
                    DaemonInvocationProblem::Unavailable
                },
            );
        }
        let Ok((resolved_project_path, _)) = project_route_for_handshake(handshake) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if admitted_lsp_root_for_project_path(&resolved_project_path).is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    invocation
        .invoke_for_project(&store_administration, project_path.as_deref(), request)
        .await
}

async fn git_service_for_project_path(
    store_administration: &StoreAdministration,
    project_path: Option<&Path>,
) -> Option<git_transactions::DaemonGitInvocationOwner> {
    let project_path = project_path?;
    let repository_root = crate::worktree::git_worktree_root(project_path)
        .unwrap_or_else(|| project_path.to_path_buf());
    store_administration
        .git_index_transaction_services()
        .for_repository_root(&repository_root)
        .await
        .ok()
        .flatten()
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

#[cfg(test)]
async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<crate::tracedecay::TraceDecay> {
    let (cg, _) = open_project_for_handshake_with_health_mode(
        project_path,
        handshake,
        store_administration,
        false,
    )
    .await?;
    Ok(cg)
}

async fn open_project_for_handshake_with_health_mode(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
    defer_post_open_health: bool,
) -> Result<(crate::tracedecay::TraceDecay, Option<crate::db::Database>)> {
    let open_options = handshake.open_options();
    let registry_database = store_administration.registered_profile_database().await?;
    let (store_layout, first_touch) =
        match crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_path,
            &open_options,
            registry_database.as_ref(),
            true,
        )
        .await
        {
            Ok(layout) => (layout, false),
            // A brand-new project has no enrollment marker, registry match, or
            // legacy shard, so identity resolution fails closed. When the client
            // explicitly asked to initialize (first-touch `tracedecay init`),
            // mint a fresh path-derived identity and let the missing-index
            // fallback below bootstrap it. Existing-but-unresolvable stores
            // raise their own identity-cutover errors instead of this one and
            // still fail closed.
            Err(err) if handshake.allow_init && is_unregistered_identity_error(&err) => (
                crate::tracedecay::TraceDecay::resolve_first_touch_configuration_layout(
                    project_path,
                    &open_options,
                    registry_database.as_ref(),
                    true,
                )
                .await?,
                true,
            ),
            Err(err) if is_unregistered_identity_error(&err) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "no TraceDecay index found at '{}'; run 'tracedecay init' first",
                        project_path.display()
                    ),
                });
            }
            Err(err) => return Err(err),
        };
    let project_id =
        store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "registered project open requires an authoritative project identity"
                    .to_owned(),
            })?;
    // First-touch enrollment: the daemon's registered session runtime resolves
    // a project's store through its on-disk enrollment marker, which a
    // never-seen project does not yet have. Persist it now — under the same
    // minted identity the layout carries — so the session store can mount
    // before init bootstraps the graph. This is the honest first enrollment
    // step, not a bypass: it only runs on the explicit allow_init first-touch
    // path, and a subsequent open resolves this same marker deterministically.
    if first_touch {
        let enrollment_root = crate::worktree::repository_identity_root(project_path)
            .unwrap_or_else(|| project_path.to_path_buf());
        crate::storage::write_enrollment_marker(
            &enrollment_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?;
    }
    let configuration_database = store_administration
        .registered_project_session_database(project_path, &store_layout)
        .await?;
    let runtime_registry = store_administration.registered_runtime_registry().await?;
    let open_result = if defer_post_open_health {
        crate::tracedecay::TraceDecay::open_with_registered_configuration_deferred_post_open_health(
            project_path,
            open_options.clone(),
            store_layout.clone(),
            Arc::clone(&configuration_database),
            Arc::clone(&registry_database),
            Arc::clone(&runtime_registry),
        )
        .await
    } else {
        crate::tracedecay::TraceDecay::open_with_registered_configuration(
            project_path,
            open_options.clone(),
            store_layout.clone(),
            Arc::clone(&configuration_database),
            Arc::clone(&registry_database),
            Arc::clone(&runtime_registry),
        )
        .await
    };
    match open_result {
        Ok(cg) => {
            let deferred_post_open_health = defer_post_open_health.then(|| cg.db().clone());
            Ok((cg, deferred_post_open_health))
        }
        Err(open_err) if defer_post_open_health && is_readonly_database_error(&open_err) => {
            match crate::tracedecay::TraceDecay::open_read_only_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    Ok((cg, None))
                }
                Err(_) => Err(open_err),
            }
        }
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            // First-touch (or not-yet-indexed) bootstrap: create and index the
            // store under the daemon's authority. Surface the bootstrap error
            // itself on failure — the original "no index found" open error is a
            // misleading symptom that hides the real reason init could not
            // complete.
            crate::tracedecay::TraceDecay::init_and_index_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            .map(|cg| (cg, None))
        }
        Err(open_err) => Err(open_err),
    }
}

/// Whether `err` is the specific fail-closed error raised when identity
/// resolution finds no enrollment marker, registry match, or legacy shard for a
/// project — i.e. a genuinely never-enrolled project. Conflicting or ambiguous
/// *existing* stores raise distinct identity-cutover errors and are excluded, so
/// first-touch bootstrap never masks a real conflict.
fn is_unregistered_identity_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains(
                "registered configuration layout requires an enrolled or registry-resolved project identity"
            )
    )
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

async fn write_daemon_invocation_response(
    transport: &mut impl McpTransport,
    response: &DaemonInvocationResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

fn invocation_lsp_session_transition(
    request: &DaemonInvocationRequest,
) -> Option<service::invocation::DaemonLspSessionAccess> {
    match &request.payload {
        service::invocation::DaemonInvocationPayload::LspReconnect { session }
        | service::invocation::DaemonInvocationPayload::LspDetach { session } => {
            Some(session.clone())
        }
        _ => None,
    }
}

fn update_connection_lsp_sessions(
    sessions: &mut HashMap<String, service::invocation::DaemonLspSessionAccess>,
    transitioned: Option<&service::invocation::DaemonLspSessionAccess>,
    response: &DaemonInvocationResponse,
) {
    match &response.outcome {
        service::invocation::DaemonInvocationOutcome::LspOpened { session, .. } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        service::invocation::DaemonInvocationOutcome::LspReconnected { session } => {
            sessions.insert(session.session_id.clone(), session.clone());
        }
        service::invocation::DaemonInvocationOutcome::LspDetached => {
            if let Some(detached) = transitioned {
                sessions.remove(&detached.session_id);
            }
        }
        _ => {}
    }
}

async fn cleanup_connection_lsp_sessions(
    invocation: &DaemonInvocationState,
    sessions: HashMap<String, service::invocation::DaemonLspSessionAccess>,
) {
    for session in sessions.into_values() {
        invocation
            .service
            .disconnect_lsp_session(&invocation.lsp_session_registry, session)
            .await;
    }
}

fn admitted_lsp_root_for_project_path(project_path: &Path) -> Option<AdmittedRoot> {
    url::Url::from_file_path(project_path)
        .ok()
        .map(|uri| AdmittedRoot::new(uri.to_string()))
}

async fn admitted_lsp_workspace_for_request(
    store_administration: &StoreAdministration,
    service: &service::invocation::DaemonInvocationService,
    project_path: &Path,
    request: &DaemonInvocationRequest,
) -> Option<AuthorizedLspWorkspace> {
    let requested_uris = match request.lsp_workspace_folders()? {
        [] => vec![url::Url::from_file_path(project_path).ok()?.to_string()],
        folders => folders.to_vec(),
    };
    if requested_uris.len() > tracedecay_lsp::MAX_LSP_WORKSPACE_ROOTS {
        return None;
    }
    // A single folder is only ever the active project: a lone sibling hint
    // must not silently reroute the session. A multi-folder workspace may span
    // registered roots, but the active project must be one of them so the
    // session stays anchored to the admitted route.
    let single_root = requested_uris.len() == 1;
    let active_project_path = project_path.canonicalize().ok()?;
    let graphs = store_administration.mounted_project_graphs().await;
    let mut resolved_roots = Vec::with_capacity(requested_uris.len());
    let mut admits_active_project = false;
    for requested_uri in requested_uris {
        let uri = url::Url::parse(&requested_uri).ok()?;
        if uri.scheme() != "file" || uri.query().is_some() || uri.fragment().is_some() {
            return None;
        }
        let requested_path = uri.to_file_path().ok()?.canonicalize().ok()?;
        if single_root && requested_path != active_project_path {
            return None;
        }
        if requested_path == active_project_path {
            admits_active_project = true;
        }
        let canonical_uri = url::Url::from_file_path(&requested_path).ok()?.to_string();
        let mut candidates = Vec::new();
        for graph in &graphs {
            let Some(raw_project_id) = graph.store_layout().identity.project_id.as_deref() else {
                continue;
            };
            let Ok(project_id) = tracedecay_domain::ProjectId::new(raw_project_id.to_owned())
            else {
                continue;
            };
            #[allow(deprecated)]
            let Ok(scope) = crate::application::context::resolve_registered_root_scope(
                graph.project_root(),
                &requested_path,
                &project_id,
            ) else {
                continue;
            };
            if !service
                .lsp_owner_matches_scope(graph.project_root(), &scope)
                .await
            {
                continue;
            }
            candidates.push((graph.project_root().to_path_buf(), scope));
        }
        candidates.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
        candidates.dedup_by(|left, right| left.1.scope_digest == right.1.scope_digest);
        let [(registered_root, scope)] = candidates.as_slice() else {
            return None;
        };
        resolved_roots.push((registered_root.clone(), canonical_uri, scope.clone()));
    }
    if !admits_active_project {
        return None;
    }
    service
        .authorize_lsp_workspace(resolved_roots, tracedecay_application::clock::now_micros())
        .await
}

async fn resolve_multi_root_projects(
    store_administration: &StoreAdministration,
    service: &service::invocation::DaemonInvocationService,
    project_ids: &[tracedecay_domain::ProjectId],
) -> std::result::Result<
    Vec<(PathBuf, tracedecay_application::ResolvedScope)>,
    service::invocation::DaemonInvocationProblem,
> {
    let database = store_administration
        .registered_profile_database()
        .await
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
    let mut roots = Vec::with_capacity(project_ids.len());
    for project_id in project_ids {
        let context = database
            .project_registry_context_by_id(project_id.as_str())
            .await
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
            .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        if context.project.project_id != project_id.as_str() {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let root = PathBuf::from(context.project.canonical_root);
        if !root.is_absolute() || root.canonicalize().ok().as_ref() != Some(&root) {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        }
        let scope = project_open_owners::resolved_scope_for_project(&root, project_id)
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        if !service.lsp_owner_matches_scope(&root, &scope).await {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        roots.push((root, scope));
    }
    roots.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
    if roots
        .windows(2)
        .any(|pair| pair[0].1.scope_digest == pair[1].1.scope_digest)
    {
        return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
    }
    Ok(roots)
}

#[cfg(unix)]
async fn execute_daemon_invocation(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    request: DaemonInvocationRequest,
) -> DaemonInvocationResponse {
    if let Some(response) = invalid_multi_root_invocation_response(&request) {
        return response;
    }
    let request_id = request.request_id.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let mut project_path = None;
    if request.requires_project() {
        if engine
            .project_server_for_request(handshake, ProjectServerRequirement::Core)
            .await
            .is_err()
        {
            return DaemonInvocationResponse::problem(
                request_id,
                if git_operation {
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized
                } else {
                    service::invocation::DaemonInvocationProblem::Unavailable
                },
            );
        }
        let Ok((resolved_project_path, _)) = DaemonEngine::project_route(handshake) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if admitted_lsp_root_for_project_path(&resolved_project_path).is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    engine
        .invocation
        .invoke_for_project(
            &engine.store_administration,
            project_path.as_deref(),
            request,
        )
        .await
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod http_application_tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod wire_bound_tests {
    use std::sync::Arc;

    use super::{
        BrokerStreamTransport, DaemonLifecycle, read_line_handling_wire_oversized,
        serve_routed_rmcp_connection,
    };
    use crate::application::host_admission::{WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error};
    use crate::mcp::McpTransport;
    use rmcp::transport::Transport;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
    async fn rmcp_broker_transport_keeps_the_tracedecay_frame_limit() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");
        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            client
                .write_all(&vec![
                    b'x';
                    crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
                        + 1
                ])
                .await
                .expect("write oversized frame");
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
        });

        assert!(
            Transport::<rmcp::RoleServer>::receive(&mut transport)
                .await
                .is_none(),
            "rmcp must receive the same bounded rejection as the daemon transport"
        );
        writer.await.expect("oversized frame writer");
    }

    #[tokio::test]
    async fn rmcp_broker_transport_recovers_after_malformed_json() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");
        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut transport = BrokerStreamTransport::new(server);

        client.write_all(b"{not-json}\n").await.expect("malformed");
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .expect("valid frame");
        client.flush().await.expect("flush");

        let recovered = Transport::<rmcp::RoleServer>::receive(&mut transport)
            .await
            .expect("valid frame after parse error");
        let recovered = serde_json::to_value(recovered).expect("serialize received message");
        assert_eq!(recovered["method"], serde_json::json!("ping"));

        let mut client = tokio::io::BufReader::new(client);
        let mut line = String::new();
        client.read_line(&mut line).await.expect("parse response");
        let response: serde_json::Value =
            serde_json::from_str(&line).expect("parse error JSON response");
        assert_eq!(response["error"]["code"], serde_json::json!(-32700));
    }

    #[tokio::test]
    async fn daemon_routed_rmcp_serves_initialize_tools_unknown_and_cancel() {
        let (cg, _dir, _pin) = crate::mcp::server::writer_test_support::init_indexed_repo().await;
        let mcp = crate::mcp::McpServer::new(cg, None).await;
        let lifecycle = DaemonLifecycle::default();
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");
        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "rmcp-production-route-test", "version": "1"}
            }
        })
        .to_string();
        let pending = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tracedecay/unknown"
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 999, "reason": "test cancellation"}
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/list"
            })
            .to_string(),
        ];
        let task = tokio::spawn({
            let mcp = Arc::clone(&mcp);
            let lifecycle = lifecycle.clone();
            async move {
                serve_routed_rmcp_connection(
                    mcp,
                    BrokerStreamTransport::new(server),
                    initialize,
                    pending,
                    None,
                    false,
                    &lifecycle,
                )
                .await
            }
        });
        let mut client = tokio::io::BufReader::new(client);
        let mut line = String::new();

        client
            .read_line(&mut line)
            .await
            .expect("initialize response");
        let initialized: serde_json::Value =
            serde_json::from_str(&line).expect("initialize JSON response");
        assert_eq!(initialized["id"], serde_json::json!(1));
        assert_eq!(
            initialized["result"]["serverInfo"]["name"],
            serde_json::json!("tracedecay")
        );

        line.clear();
        client.read_line(&mut line).await.expect("unknown response");
        let unknown: serde_json::Value =
            serde_json::from_str(&line).expect("unknown method JSON response");
        assert_eq!(unknown["id"], serde_json::json!(2));
        assert_eq!(unknown["error"]["code"], serde_json::json!(-32601));

        line.clear();
        client.read_line(&mut line).await.expect("tools response");
        let tools: serde_json::Value = serde_json::from_str(&line).expect("tools JSON response");
        assert_eq!(tools["id"], serde_json::json!(3));
        assert!(
            tools["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "the production rmcp route must advertise the mounted tool surface"
        );

        lifecycle.begin_draining();
        task.await
            .expect("rmcp route task")
            .expect("rmcp route completion");
        mcp.shutdown_background_tasks().await;
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
