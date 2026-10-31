use std::time::Instant;

#[cfg(unix)]
use tokio::time::Duration;

use tracedecay_daemon_protocol::{
    PROJECT_SERVER_CAPACITY_REASON_CODE, PROJECT_SERVER_RESPONSE_REVOKED_REASON_CODE,
    PROJECT_WARMING_REASON_CODE,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_mcp::JsonRpcRequest;
use tracedecay_runtime_core::git_discovery::REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE;

/// Human-facing detail fragment for a still-warming project/profile owner.
///
/// Protocol control flow must key on [`PROJECT_WARMING_REASON_CODE`] (or the
/// sibling deferred, capacity, and revoked codes below), never on this English
/// prose.
pub(crate) const PROJECT_WARMING_RETRY_HINT: &str =
    "is warming in the background; retry the same tool shortly";
/// Typed reason the in-flight project-open task table is full.
pub const PROJECT_OPEN_TASK_CAPACITY_REASON_CODE: &str = "project_open_task_capacity_reached";

/// Typed reason a project-scoped request arrived on a handshake that names no
/// project at all (no `--project`, and no enrolled project above the cwd).
pub const PROJECT_REQUIRED_REASON_CODE: &str = "project_required";
#[cfg(unix)]
const TOOL_LIST_CHANGED_METHOD: &str = "notifications/tools/list_changed";
#[cfg(unix)]
const MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION: usize = 1_024;
const MAX_CACHED_PROJECT_SERVERS: usize = 8;
const MAX_TRACKED_PROJECT_OPEN_TASKS: usize = MAX_CACHED_PROJECT_SERVERS;
const MAX_CACHED_PROJECT_OPEN_FAILURES: usize = 64;
const PROJECT_OPEN_REQUEST_DEADLINE: Duration = Duration::from_millis(500);

/// Instant a foreground request stops waiting for an open it has already claimed.
///
/// Measured from the claim, not from connection arrival. Route enrollment and
/// git discovery have their own bounds. Charging them to this deadline made a
/// restart's first request answer warming for a store whose open had already
/// recorded `reset_required`: the connection spent the budget before it joined
/// the watch, so the publication wait returned immediately and never read the
/// refusal.
fn project_open_publication_deadline(claimed_at: Instant) -> Instant {
    claimed_at + PROJECT_OPEN_REQUEST_DEADLINE
}
/// One budget for every blocking repository probe a route resolution runs.
///
/// Route resolution reads the repository's topology, enrollment marker, and
/// HEAD. Those are filesystem operations whose cost belongs to the volume the
/// checkout lives on, not to this daemon, so they run off the async workers
/// and a probe that outlives this budget becomes the retryable deferred
/// discovery refusal instead of holding the caller.
const REPOSITORY_DISCOVERY_DEADLINE: Duration = Duration::from_secs(2);
const PROJECT_OPEN_FAILURE_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const PROJECT_OPEN_RESOURCE_RETRY_BACKOFF: Duration = Duration::from_secs(1);
/// Backoff for a persisted-row authority defect, which only an operator can
/// clear. Reopening re-runs the exhaustive authority audit over every
/// `observations` row and fails on the same row every time, so the debounce
/// cadence above would saturate a core for as long as the daemon runs.
const PROJECT_OPEN_UNREPAIRABLE_RETRY_BACKOFF: Duration = Duration::from_mins(5);

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

/// True when repository discovery deferred or blocked this route.
pub(crate) fn error_is_repository_discovery_deferred(error: &TraceDecayError) -> bool {
    matches!(
        error.project_route_context(),
        Some((REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE, _, _))
    )
}

fn project_open_retryable_reason(reason_code: &str) -> bool {
    matches!(
        reason_code,
        PROJECT_WARMING_REASON_CODE
            | REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE
            | PROJECT_OPEN_TASK_CAPACITY_REASON_CODE
            | PROJECT_SERVER_CAPACITY_REASON_CODE
    )
}

fn project_open_capacity_limit(reason_code: &str) -> Option<usize> {
    match reason_code {
        PROJECT_OPEN_TASK_CAPACITY_REASON_CODE => Some(MAX_TRACKED_PROJECT_OPEN_TASKS),
        PROJECT_SERVER_CAPACITY_REASON_CODE => Some(MAX_CACHED_PROJECT_SERVERS),
        _ => None,
    }
}

/// True when a daemon error describes a project open that has not finished
/// yet: typed warming, deferred repository discovery, or a saturated open
/// queue. Capacity clears as in-flight opens finish, so it is retryable for
/// the same reason [`PROJECT_WARMING_REASON_CODE`] is. Classification keys on
/// the reason code, never on English detail.
pub(crate) fn error_is_project_open_retryable(error: &TraceDecayError) -> bool {
    error
        .project_route_context()
        .is_some_and(|(reason, retryable, _)| retryable && project_open_retryable_reason(reason))
}

/// Response-side form of [`error_is_project_open_retryable`] for clients that
/// still hold the JSON-RPC `error` member. Every retryable open refusal,
/// including capacity, carries `data.reason_code`.
pub(crate) fn json_rpc_error_is_project_open_retryable(error: &serde_json::Value) -> bool {
    error
        .pointer("/data/reason_code")
        .and_then(serde_json::Value::as_str)
        .is_some_and(project_open_retryable_reason)
}

/// Identifies a transient tool-call transport refusal by typed reason code.
/// A revoked response may follow a committed effect, so automatic replay also
/// requires the tool's canonical read-only contract.
pub fn tool_call_transport_error_is_retryable(error: &TraceDecayError) -> bool {
    matches!(
        error.project_route_context(),
        Some((reason, true, _))
            if reason == PROJECT_WARMING_REASON_CODE
                || reason == REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE
                || reason == PROJECT_SERVER_RESPONSE_REVOKED_REASON_CODE
    )
}

/// Aborts its tasks when dropped. A shutdown join holds one so that the
/// coordinator dropping the join at its phase deadline aborts the tasks the
/// join was waiting on. Aborting a task that already finished does nothing.
pub(crate) struct AbortTasksOnDrop(pub(crate) Vec<tokio::task::AbortHandle>);

impl Drop for AbortTasksOnDrop {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

mod bootstrap;
mod bootstrap_route;
mod branch_add;
mod branch_admin;
mod connection_serving;
#[cfg(feature = "rmcp-benchmark")]
#[doc(hidden)]
pub use connection_serving::rmcp_benchmark;
#[cfg(unix)]
#[cfg(not(unix))]
use connection_serving::serve_windows_broker_client_with_class_and_invocation;
#[cfg(any(test, feature = "test-transport"))]
pub(crate) use connection_serving::{RoutedRmcpReplay, serve_routed_rmcp_connection};
mod core_admission;
mod core_client;
mod core_doctor;
mod core_handshake;
mod core_hooks;
mod core_proxy;
pub(crate) mod dashboard_automation;
#[cfg(feature = "test-transport")]
#[path = "../tests/common/dashboard_configuration_test_runtime.rs"]
mod dashboard_configuration_test_runtime;
mod database_owner_registry;
mod engine;
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
pub use core_handshake::*;
pub use core_hooks::*;
pub use core_proxy::*;
mod github_secret_reader;
mod graph_resolution;
mod http_application;
pub use http_application::live_remote_operational_status;
mod http_application_router;
mod invocation_dispatch;
#[cfg(unix)]
mod invocation_executor;
pub(crate) mod invocation_state;
mod lsp_sessions;
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
mod profile_configuration;
mod profile_owner;
mod profile_retained;
#[cfg(unix)]
mod profile_watch;
mod projectless;
mod remote_deletion;
mod remote_drain;
mod remote_join;
mod remote_revocation;
pub use remote_drain::{RemoteNodeDrainReceiptV1, live_remote_drain};
pub use remote_join::{RemoteNodeJoinRequestV1, live_remote_join};
pub use remote_revocation::{RemoteNodeRevocationV1, live_remote_node_revocation};
mod project_composition;
mod project_delivery_mount;
mod project_open_admission;
mod project_open_handshake;
mod project_open_orchestration;
mod project_routing;
mod project_server_lifecycle;
// The portable reconciler only exists off-unix (or under test transports), so
// its import carries the same gate as its definition.
#[cfg(unix)]
mod scheduler;
#[cfg(test)]
pub(crate) mod session_runtime_tests;

#[cfg(test)]
pub(crate) mod store_runtime_tests;

mod repeat_reads;
mod repeated_queries;
mod tool_surface;
mod wire_io;
#[cfg(test)]
mod work_evidence_retrieval_tests;

pub use bootstrap::{DaemonRunEnd, PROFILE_LOST_EXIT_CODE, run_foreground};

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
mod http_application_tests;

#[cfg(all(test, unix))]
#[allow(clippy::expect_used)]
mod broker_stream_transport_tests;
