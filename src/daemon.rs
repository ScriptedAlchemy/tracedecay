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
mod engine;
use engine::{
    DaemonEngine, ensure_context_scout_owner_before_advertising,
    ensure_git_index_transactions_for_mutation_owners,
};
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
mod http_application_router;
use http_application_router::{
    install_http_application_cold_resolver, mount_http_application_router,
};
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
mod project_server_lifecycle;
use project_routing::{
    CatalogRefreshClientKey, bind_authenticated_profile_identity, maintenance_transition_gate,
    portable_database_owner_reconciler, project_open_cancellation_checkpoint,
    project_open_cancellation_error, project_open_gate, project_open_task_capacity_error,
    project_open_tasks, project_route_for_handshake, project_server_capacity_error,
    project_warming_error,
};
#[cfg(test)]
use project_server_lifecycle::replay_user_profile_host_admission_for_identity;
use project_server_lifecycle::{
    cancel_project_server_startup_ingests, detach_project_servers,
    ensure_user_profile_host_admission_replay_for_identity, schedule_project_server_retirement,
    shutdown_detached_project_servers, shutdown_project_servers,
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
