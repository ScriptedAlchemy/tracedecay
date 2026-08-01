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
mod bootstrap_route;
use bootstrap_route::{
    apply_daemon_initialize_route, attach_initialize_route_metadata, cached_project_node_count,
    daemon_bootstrap_response,
};
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
mod invocation_dispatch;
#[cfg(any(not(unix), test))]
use invocation_dispatch::execute_portable_daemon_invocation;
#[cfg(unix)]
use invocation_dispatch::{execute_daemon_invocation, write_tool_list_changed_notification};
use invocation_dispatch::{
    git_service_for_project_path, invalid_multi_root_invocation_response,
    resolve_multi_root_projects,
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
mod lsp_sessions;
use lsp_sessions::{
    admitted_lsp_root_for_project_path, admitted_lsp_workspace_for_request,
    cleanup_connection_lsp_sessions, invocation_lsp_session_transition,
    update_connection_lsp_sessions,
};
mod maintenance;
mod maintenance_tasks;
pub use maintenance_tasks::mark_process_long_lived_for_session_maintenance;
use maintenance_tasks::spawn_semantic_artifact_gc_maintenance;
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
mod project_open_handshake;
#[cfg(test)]
use project_open_handshake::{is_missing_index_error, open_project_for_handshake};
use project_open_handshake::{
    open_project_for_handshake_with_health_mode, project_open_error_response,
    write_project_open_error,
};
mod project_open_orchestration;
mod project_routing;
mod project_server_lifecycle;
#[cfg(not(unix))]
use project_open_orchestration::shutdown_portable_project_open_tasks;
use project_open_orchestration::{
    ensure_registered_project_route, spawn_lifecycle_automation_scheduler_activation,
    start_lifecycle_project_open,
};
#[cfg(any(not(unix), test))]
use project_open_orchestration::{
    portable_cached_project_open_failure, portable_cached_project_server,
    portable_project_server_for_request, schedule_portable_project_server_warmup,
};
// The portable reconciler only exists off-unix (or under test transports), so
// its import carries the same gate as its definition.
#[cfg(any(not(unix), test, feature = "test-transport"))]
use project_routing::portable_database_owner_reconciler;
use project_routing::{
    CatalogRefreshClientKey, bind_authenticated_profile_identity, maintenance_transition_gate,
    project_open_cancellation_checkpoint, project_open_cancellation_error, project_open_gate,
    project_open_task_capacity_error, project_open_tasks, project_route_for_handshake,
    project_server_capacity_error, project_warming_error,
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
mod wire_io;
pub(crate) mod work_runtime;
pub(crate) mod workflow_runtime;
use wire_io::{
    read_line_handling_wire_oversized, write_daemon_invocation_response, write_json_rpc_response,
};

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
#[cfg(unix)]
use bootstrap::set_owner_only_permissions;
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
    start_service, stop_service, uninstall_service, verify_installed_service_quiesced_under_lease,
    wait_for_installed_service_state, with_exclusive_maintenance_window,
    with_quiesced_installed_service,
};

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod http_application_tests;
