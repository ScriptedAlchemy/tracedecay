use std::collections::{BTreeMap, HashMap, VecDeque};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};
use tokio_stream::StreamExt;
use tracedecay_lsp::{AdmittedRoot, AuthorizedLspWorkspace};

use tracedecay_mcp::server::RmcpConnectionAdapter;

use crate::mcp::server::{
    McpMethod, ProductionMcpConnectionContext, RmcpInitializeResponseDecorator,
    SERVER_INSTRUCTIONS, classify_mcp_method, initialize_result,
};
use branch_add::{branch_add_response, parse_branch_add_request};
use branch_admin::{StoreAdministration, parse_branch_admin_request, write_branch_admin_response};
#[cfg(all(unix, test))]
use scheduler::{
    AutomationSchedulerHandle, automation_scheduler_configured,
    automation_scheduler_tick_secs_for_project, run_automation_scheduler_tick,
};
#[allow(unused_imports)]
pub(crate) use tracedecay_daemon_protocol::{
    BrokerListener, BrokerStream, DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION,
    DaemonAuthPreface, DaemonEndpoint, DaemonInvocationOutcome, DaemonInvocationRequest,
    DaemonInvocationResponse, default_loopback_endpoint, parse_daemon_invocation_request,
};
pub(crate) use tracedecay_daemon_protocol::{DaemonClientIdentity, DaemonHandshake};
#[cfg(unix)]
#[allow(unused_imports)]
pub(crate) use tracedecay_daemon_protocol::{
    ensure_private_socket_parent, unix_socket_path_within_limit,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::tools::catalog_discovery::{
    catalog_discovery_tools_list_payload, default_catalog_discovery_authority,
};
use tracedecay_mcp::transport::ReplayTransport;
use tracedecay_mcp::{
    BrokerStreamTransport, ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport,
};
use tracedecay_mcp::{ToolRegistryMode, explore_call_budget, project_catalog_discovery_scope};
use tracedecay_runtime_core::cancellation::CancellationToken;

pub(crate) const PROJECT_WARMING_RETRY_HINT: &str =
    "is warming in the background; retry the same tool shortly";
#[cfg(unix)]
const TOOL_LIST_CHANGED_METHOD: &str = "notifications/tools/list_changed";
#[cfg(unix)]
const MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION: usize = 1_024;
const MAX_CACHED_PROJECT_SERVERS: usize = 8;
const MAX_TRACKED_PROJECT_OPEN_TASKS: usize = MAX_CACHED_PROJECT_SERVERS;
const MAX_CACHED_PROJECT_OPEN_FAILURES: usize = 64;
const PROJECT_OPEN_REQUEST_DEADLINE: Duration = Duration::from_millis(500);
const PROJECT_OPEN_FAILURE_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const PROJECT_OPEN_RESOURCE_RETRY_BACKOFF: Duration = Duration::from_secs(1);
/// Backoff for a persisted-row authority defect, which only an operator can
/// clear. Reopening re-runs the exhaustive authority audit over every
/// `observations` row and fails on the same row every time, so the debounce
/// cadence above would saturate a core for as long as the daemon runs.
const PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF: Duration = Duration::from_mins(5);
const PROJECT_OPEN_FAILURE_RETRY_HINT: &str =
    "project route open is backed off after an invariant rejection";

/// One authenticated connection's bounded first request.
///
/// Routing shares the parsed JSON-RPC view, while the selected transport
/// consumes the byte-exact raw line and performs its own authoritative decode.
pub(super) struct AuthenticatedFirstRequest {
    raw: String,
    parsed: Option<JsonRpcRequest>,
}

impl AuthenticatedFirstRequest {
    pub(super) fn new(raw: String) -> Self {
        hotpath::gauge!("daemon.engine.first_request.decode").inc(1_u64);
        let parsed = JsonRpcRequest::decode(raw.trim()).ok();
        Self { raw, parsed }
    }

    pub(super) fn raw(&self) -> &str {
        &self.raw
    }

    pub(super) fn parsed(&self) -> Option<&JsonRpcRequest> {
        self.parsed.as_ref()
    }

    pub(super) fn into_raw(self) -> String {
        self.raw
    }
}

/// How long a client rides out a project open that has not finished yet.
///
/// A cold project open (create/migrate DBs, config runtime, first index) takes
/// ~2.5s release / ~3.3s debug even for a tiny repo, so a 2s grace abandoned
/// legitimate opens just before they completed. Retry loops still exit
/// immediately on a real failure.
pub(crate) const PROJECT_OPEN_RETRY_GRACE: Duration = Duration::from_secs(15);
pub(crate) const PROJECT_OPEN_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Daemon error messages for a saturated project-open queue. Both clear on
/// their own as in-flight opens finish, so they are retryable for the same
/// reason [`PROJECT_WARMING_RETRY_HINT`] is.
const PROJECT_OPEN_CAPACITY_MESSAGES: [&str; 2] = [
    "daemon project open task capacity reached",
    "daemon project server capacity reached",
];
/// Typed `error.data.kind` values for the same two capacity states.
const PROJECT_OPEN_CAPACITY_ERROR_KINDS: [&str; 2] = [
    "project_open_task_capacity_reached",
    "project_server_capacity_reached",
];
/// Message fragments emitted when a daemon request misses its read deadline.
const DAEMON_READ_DEADLINE_MESSAGES: [&str; 3] = [
    "before deadline",
    "deadline already elapsed",
    "did not answer after",
];

/// True when a daemon error message carries the project warming hint.
pub(crate) fn error_message_is_project_warming(message: &str) -> bool {
    message.contains(PROJECT_WARMING_RETRY_HINT)
}

/// True when a daemon error message describes a project open that has not
/// finished yet: either the route's warming hint or a saturated open queue.
pub(crate) fn error_message_is_project_open_retryable(message: &str) -> bool {
    error_message_is_project_warming(message)
        || PROJECT_OPEN_CAPACITY_MESSAGES
            .iter()
            .any(|capacity| message.contains(capacity))
}

/// Response-side form of [`error_message_is_project_open_retryable`] for
/// clients that still hold the JSON-RPC `error` member, where the capacity
/// states also carry a typed `data.kind`.
pub(crate) fn json_rpc_error_is_project_open_retryable(error: &serde_json::Value) -> bool {
    error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .is_some_and(error_message_is_project_open_retryable)
        || error
            .pointer("/data/kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| PROJECT_OPEN_CAPACITY_ERROR_KINDS.contains(&kind))
}

/// True when a daemon error message reports a missed read deadline.
pub fn error_message_is_read_deadline(message: &str) -> bool {
    DAEMON_READ_DEADLINE_MESSAGES
        .iter()
        .any(|deadline| message.contains(deadline))
}

/// True when a daemon client missed its read deadline, including the typed
/// `daemon_response_stalled` reason code.
pub fn error_is_read_deadline(error: &TraceDecayError) -> bool {
    matches!(
        error.project_route_context(),
        Some((tracedecay_daemon_protocol::DAEMON_RESPONSE_STALLED, _, _))
    ) || error_message_is_read_deadline(&error.to_string())
}

mod bootstrap;
mod bootstrap_route;
use bootstrap_route::{
    apply_daemon_initialize_route, attach_initialize_route_metadata, cached_project_node_count,
    daemon_bootstrap_response, prewarm_daemon_bootstrap_catalog,
};
mod branch_add;
mod branch_admin;
use tracedecay_code_index_runtime::code_index_branch_diff::code_index_branch_diff_executor;
use tracedecay_code_index_runtime::code_index_executor::code_index_search_executor;
#[cfg(test)]
use tracedecay_code_index_runtime::code_index_executor::{
    code_index_search_display_binding, mcp_search_request_termination,
};
#[cfg(test)]
use tracedecay_code_index_runtime::code_index_task_support::{
    code_index_scope_unavailable, code_index_search_hydration_budget,
};
mod connection_serving;
#[cfg(feature = "rmcp-benchmark")]
#[doc(hidden)]
pub use connection_serving::rmcp_benchmark;
#[cfg(unix)]
use connection_serving::serve_authenticated_socket_client_with_class;
#[cfg(all(unix, test))]
use connection_serving::serve_socket_client;
#[cfg(not(unix))]
use connection_serving::serve_windows_broker_client_with_class_and_invocation;
#[cfg(test)]
use connection_serving::{
    await_project_owner_or_disconnect, serve_routed_rmcp_connection, serve_windows_broker_client,
    serve_windows_broker_client_with_class,
};
mod core_admission;
mod engine;
#[cfg(unix)]
use engine::DaemonEngine;
use engine::{
    ensure_context_scout_owner_before_advertising,
    ensure_git_index_transactions_for_mutation_owners,
};
pub(crate) use tracedecay_daemon_service::automation_observation::{
    project_run_observation_producer as project_automation_observation_producer,
    record_project_run as record_project_automation_run,
};
mod core_client;
mod core_doctor;
mod core_handshake;
mod core_hooks;
mod core_proxy;
mod database_owner_registry;
use database_owner_registry::DatabaseOwnerRegistry;
pub(crate) mod dashboard_automation;
#[cfg(feature = "test-transport")]
#[path = "../tests/common/dashboard_configuration_test_runtime.rs"]
mod dashboard_configuration_test_runtime;
pub(crate) mod hook_v2_replay_consumer;
pub(crate) mod project_open_owners;
#[cfg(feature = "test-transport")]
pub(crate) use dashboard_configuration_test_runtime::{
    dashboard_configuration_authorities_for_test, register_dashboard_test_retained_runtime,
};
#[cfg(any(test, feature = "test-transport"))]
pub(crate) mod retained_test_support;
pub(crate) use core_admission::*;
pub use core_client::*;
pub(crate) use core_doctor::*;
pub use core_handshake::*;
pub use core_hooks::*;
pub use core_proxy::*;
// Daemon process lifecycle and logging live in `tracedecay-daemon-service`;
// the root's engine, bootstrap, and connection serving still read them by
// these names until they move.
pub(crate) use tracedecay_daemon_service::logging::{recent_watcher_events, unavailable_error};
#[cfg(feature = "hotpath")]
pub use tracedecay_daemon_service::shutdown::install_hotpath_shutdown_finalizer;
pub(crate) use tracedecay_daemon_service::shutdown::{
    DAEMON_CLIENT_DRAIN_DEADLINE, DAEMON_TASK_ABORT_DEADLINE, DaemonLifecycle, ShutdownStatus,
};
mod github_credential_lifecycle;
mod graph_resolution;
use graph_resolution::retained_project_server_resolver;
mod http_application;
pub use http_application::live_remote_operational_status;
mod http_application_router;
use http_application_router::{
    install_http_application_cold_resolver, install_remote_http_application_router,
    mount_http_application_router,
};
mod invocation_dispatch;
#[cfg(any(not(unix), test))]
use invocation_dispatch::execute_portable_daemon_invocation;
#[cfg(unix)]
use invocation_dispatch::{execute_daemon_invocation, write_tool_list_changed_notification};
use invocation_dispatch::{
    git_service_for_project_path, invalid_multi_root_invocation_response,
    native_integration_service_for_project_path, resolve_multi_root_projects,
};
mod invocation_executor;
use invocation_executor::{
    FederatedSurfaceRequestV1, InProcessDaemonInvocationExecutor, PrecomputedMultiRootQueryPort,
    denied_root_generation, explicit_git_state, extract_work_application_payload,
    frozen_root_generation, invocation_is_git_operation,
    invocation_is_native_integration_operation, multi_root_family_allows,
    unavailable_root_generation,
};
mod invocation_state;
pub(crate) use invocation_state::DaemonInvocationState;
mod lsp_sessions;
use lsp_sessions::{
    admitted_lsp_root_for_project_path, admitted_lsp_workspace_for_request,
    cleanup_connection_lsp_sessions, invocation_lsp_session_transition,
    settle_pending_lsp_workspace_mutation, update_connection_lsp_sessions,
};
mod maintenance;
pub mod pr_autotrack;
#[cfg(any(test, feature = "test-transport"))]
#[allow(clippy::too_many_lines)]
mod production_harness;
mod store_maintenance;
#[cfg(any(test, feature = "test-transport"))]
pub use production_harness::ProductionProjectCompositionHarnessV1;
#[cfg(all(unix, feature = "test-transport"))]
pub use production_harness::capture_exact_git_snapshot_for_test;
mod projectless;
mod remote_deletion;
#[cfg(test)]
use projectless::projectless_tools_call_response;
use projectless::{
    projectless_tool_call, projectless_user_session_request, serve_projectless_client,
};
mod project_composition;
mod project_delivery_mount;
use project_composition::{ProductionProjectCompositionRuntime, production_project_server};
mod project_open_admission;
#[cfg(test)]
use project_open_admission::project_open_retry_backoff;
#[cfg(unix)]
use project_open_admission::{
    MaintenanceRekeyOutcome, MaintenanceTransitionGate, MaintenanceTransitionGates,
    MaintenanceTransitionKey,
};
use project_open_admission::{
    ProjectOpenFailure, ProjectOpenGate, ProjectOpenGates, ProjectOpenTaskClaim,
    ProjectOpenTaskState, ProjectOpenTasks, ProjectRouteKey, ProjectServerKey,
    ProjectServerPublication, ProjectServerRequirement, project_server_requirement,
    store_owner_key_from_paths,
};
pub(crate) use tracedecay_session_runtime::StoreOwnerKey;
mod project_open_handshake;
#[cfg(test)]
use project_open_handshake::is_missing_index_error;
use project_open_handshake::{
    open_project_for_handshake, project_open_error_response, write_project_open_error,
};
mod project_open_orchestration;
mod project_routing;
mod project_server_lifecycle;
use project_open_orchestration::{
    durable_enrollment_resolves_existing_store, ensure_registered_project_route,
};
#[cfg(any(not(unix), test))]
use project_open_orchestration::{
    portable_cached_project_open_failure, portable_cached_project_server,
    portable_project_server_for_request, schedule_portable_project_server_warmup,
};
#[cfg(unix)]
use project_open_orchestration::{
    spawn_lifecycle_automation_scheduler_activation, start_lifecycle_project_open,
    wait_for_project_open_publication,
};
// The portable reconciler only exists off-unix (or under test transports), so
// its import carries the same gate as its definition.
#[cfg(any(not(unix), test, feature = "test-transport"))]
use project_routing::portable_database_owner_reconciler;
#[cfg(unix)]
use project_routing::{CatalogRefreshClientKey, maintenance_transition_gate};
use project_routing::{
    bind_authenticated_profile_identity, cached_or_bind_ready_project_server,
    prefer_recorded_open_failure, project_open_cancellation_checkpoint,
    project_open_cancellation_error, project_open_capacity_gate, project_open_gate,
    project_open_task_capacity_error, project_open_tasks, project_route_for_handshake,
    project_server_capacity_error, project_warming_error, resolved_project_server_key,
};
#[cfg(test)]
use project_server_lifecycle::replay_user_profile_host_admission_for_identity;
use project_server_lifecycle::{
    await_user_profile_host_admission_replay_for_identity, cancel_retained_session_history,
    schedule_project_server_retirement, schedule_user_profile_host_admission_replay_for_identity,
    shutdown_project_servers,
};
#[cfg(unix)]
mod scheduler;
#[cfg(test)]
pub(crate) mod session_runtime_tests;

#[cfg(test)]
pub(crate) mod store_runtime_tests;

mod wire_io;
#[cfg(test)]
mod work_evidence_retrieval_tests;
use wire_io::{
    read_line_handling_wire_oversized, write_daemon_invocation_response, write_json_rpc_response,
};

pub use bootstrap::run_foreground;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod invocation_tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod code_index_runtime_generation_census_tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod code_index_runtime_graph_activation_tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod http_application_tests;

#[cfg(all(test, unix))]
#[allow(clippy::expect_used)]
mod broker_stream_transport_tests;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod remote_protocol_tests;
