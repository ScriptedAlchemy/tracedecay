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
