// Rust guideline compliant 2025-10-17
//! MCP server that reads JSON-RPC 2.0 messages from stdin and writes
//! responses to stdout.
//!
//! The server exposes code graph tools via the Model Context Protocol,
//! allowing AI assistants to query the code graph interactively.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::project_route::{
    HookProjectRouteCache, SharedHookProjectRouteCache, mcp_analytics_session_id,
};
use crate::mcp::response_handles::{cleanup_expired_response_handles, response_handle_stats_json};
use crate::mcp::tool_analytics::{
    McpToolAnalyticsEvent, hook_route_analytics_event, mcp_tool_analytics_event,
};
use crate::request_identity::McpConnectionIdentityAuthority;
use crate::tracedecay::TraceDecay;
use tracedecay_sessions::runtime::git_correlation::{
    self as git_correlation, DEFAULT_SPAN_MERGE_GAP_SECS, DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS,
    SpanObservation, SpanSource,
};
use tracedecay_usecases::host_admission::{
    HostAdmissionOutcome, HostAdmissionStatus, TerminalReason, is_wire_oversized_io_error,
};

use super::hook_events::{self, HookAgent, HookEventPlan};
use super::tools::{
    ProjectRegistryReadPort, SessionRefreshServicePort, ToolRegistryMode,
    default_catalog_discovery_authority, explore_call_budget, project_catalog_discovery_scope,
};
use super::transport::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

mod connection;
mod construction;
mod dispatch_settlement;
mod hook_dispatch;
mod hook_writes;
mod ledger;
mod lifecycle;
mod live_transcript_refresh;
mod project_open_access;
mod project_registry;
mod protocol;
mod read_coalescing;
mod requests;
mod rmcp;
mod routing;
mod session_refresh;
mod staleness;
mod status_resource;
mod tool_errors;
mod workflow_index;

pub(crate) use project_registry::DaemonProjectRegistryReadService;
pub(crate) use workflow_index::DaemonWorkflowIndexReadService;

pub(crate) use construction::*;
use dispatch_settlement::RetainedDispatchAuthority;
pub(crate) use hook_writes::*;
pub(crate) use ledger::McpToolErrorAnalyticsRequest;
pub(crate) use lifecycle::{
    McpBackgroundTaskOwner, ProjectServerResponseLifecycle, StartupCatchUpMachineV1,
    VersionCheckState,
};
pub(crate) use live_transcript_refresh::{
    LiveTranscriptRefreshJoin, join_required_live_transcript_refresh,
};
pub(crate) use protocol::*;
use read_coalescing::*;
pub(crate) use rmcp::{
    RmcpConnectionAdapter, RmcpInitializeResponseDecorator,
    RmcpSelectedProjectResponseAuthority, RmcpWorkDeliverySettlement,
};
pub(crate) use routing::*;
pub(crate) use session_refresh::*;
pub(crate) use staleness::*;
pub(crate) use tool_errors::*;

/// Runtime statistics for the MCP server.
pub struct ServerStats {
    started_at: Instant,
    total_requests: AtomicU64,
    tool_calls: AtomicU64,
    errors: AtomicU64,
}

impl ServerStats {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            total_requests: AtomicU64::new(0),
            tool_calls: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }
}

use super::transport::write_wire_oversized_rejection;

/// Future returned by a [`CodeIndexHookSink`] invocation. Resolves to `true`
/// when a mounted worktree scheduler accepted the touched paths.
pub(crate) type CodeIndexHookNotifyFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'static>>;

/// Type-erased bridge from the MCP hook boundary to the daemon-owned code-index
/// scheduler registry. The daemon constructs this closing over its cloneable
/// `CodeIndexSchedulerRegistryV1`; direct (non-daemon) servers leave it `None`.
/// Erasing the concrete registry type keeps the daemon-private scheduler type
/// out of `crate::mcp` while still delivering after-edit paths into the
/// incremental indexing queue without any standing filesystem watcher.
pub(crate) type CodeIndexHookSink =
    Arc<dyn Fn(PathBuf, Vec<String>) -> CodeIndexHookNotifyFuture + Send + Sync + 'static>;

/// Non-blocking bridge for hook/admin requests that require one authoritative
/// worktree reconciliation but do not carry exact touched paths. A successful
/// future means the bounded daemon scheduler accepted the overflow signal; it
/// never means indexing has completed.
pub(crate) type CodeIndexReconcileSink =
    Arc<dyn Fn(PathBuf) -> CodeIndexHookNotifyFuture + Send + Sync + 'static>;

/// Type-erased bridge from a tool handler to the daemon-owned code-index
/// generation authority. The daemon constructs this from its cloneable
/// `CodeIndexSchedulerRegistryV1`; direct (non-daemon) servers leave it `None`,
/// and a producer without it publishes nothing rather than minting its own file
/// identity.
pub(crate) type CodeIndexPublicationIdentityResolver =
    Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1 + 'static>;

pub(crate) type CodeGraphProjectionReadPort =
    Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort + 'static>;
pub(crate) type CodeGraphReadAdmissionPort =
    Arc<dyn tracedecay_usecases::graph::CodeGraphReadAdmissionPort + 'static>;
pub(crate) type CodeIndexIgnoredDependencyAdmissionPort =
    Arc<dyn tracedecay_usecases::code_index::CodeIndexIgnoredDependencyAdmissionPortV1 + 'static>;

/// Code-index search boundary contracts, owned by the query kernel.
///
/// The whole `CodeIndexSearch*V1` family is pure request/outcome data with no
/// MCP coupling, so it lives in `tracedecay_query::code_search`. Re-exporting
/// it here keeps the historical `crate::mcp::server::CodeIndexSearch*` paths
/// resolving while the daemon depends on the query kernel instead of on
/// `crate::mcp`.
pub(crate) use tracedecay_query::code_search::*;

/// User-controlled fields admitted at the MCP source-edit boundary.
///
/// The daemon-owned executor closes over project authority and constructs the
/// request context, authority receipt, policy proof, and authorization service.
/// None of those authority-bearing values may be supplied by the transport.
pub(crate) struct SourceEditInvocationV1 {
    pub(crate) edit: tracedecay_application::SourceEditRequest,
    pub(crate) idempotency_key: Option<tracedecay_application::IdempotencyKey>,
    pub(crate) expected_state: Option<tracedecay_domain::ManifestDigest>,
    pub(crate) request_id: tracedecay_application::RequestId,
    pub(crate) deadline: tracedecay_application::Deadline,
    pub(crate) cancellation: tracedecay_application::CancellationSignal,
}

pub(crate) type SourceEditFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = crate::errors::Result<
                    tracedecay_application::source_edit::SourceEditSurfaceResultV1,
                >,
            > + Send
            + 'static,
    >,
>;

pub(crate) type SourceEditExecutor =
    Arc<dyn Fn(SourceEditInvocationV1) -> SourceEditFuture + Send + Sync + 'static>;

/// User-controlled identity and inspection conclusion for one uncertain edit.
///
/// Authority-bearing context and proof fields are deliberately absent: the
/// daemon-owned executor constructs them from the current project admission.
pub(crate) struct SourceEditReconciliationInvocationV1 {
    pub(crate) kind: tracedecay_application::SourceEditKind,
    pub(crate) effect_id: tracedecay_application::EffectId,
    pub(crate) idempotency_key: tracedecay_application::IdempotencyKey,
    pub(crate) attempt_idempotency_key: tracedecay_application::IdempotencyKey,
    pub(crate) input_digest: tracedecay_domain::ManifestDigest,
    pub(crate) disposition: tracedecay_application::SourceEditReconciliationDispositionV1,
    pub(crate) request_id: tracedecay_application::RequestId,
    pub(crate) deadline: tracedecay_application::Deadline,
    pub(crate) cancellation: tracedecay_application::CancellationSignal,
}

pub(crate) type SourceEditReconciliationExecutor =
    Arc<dyn Fn(SourceEditReconciliationInvocationV1) -> SourceEditFuture + Send + Sync + 'static>;

/// User-controlled identity of one completed source edit whose retained
/// preimages the caller asks the daemon to restore.
///
/// Authority-bearing context and proof fields are deliberately absent: the
/// daemon-owned executor constructs them from the current project admission.
/// The preimage bytes never cross this boundary either — they stay in the
/// server-side rollback record and the caller only names public digests.
pub(crate) struct SourceEditRollbackInvocationV1 {
    pub(crate) effect_id: tracedecay_application::EffectId,
    pub(crate) original_idempotency_key: tracedecay_application::IdempotencyKey,
    pub(crate) idempotency_key: tracedecay_application::IdempotencyKey,
    pub(crate) original_input_digest: tracedecay_domain::ManifestDigest,
    pub(crate) expected_state: tracedecay_domain::ManifestDigest,
    pub(crate) request_id: tracedecay_application::RequestId,
    pub(crate) deadline: tracedecay_application::Deadline,
    pub(crate) cancellation: tracedecay_application::CancellationSignal,
}

pub(crate) type SourceEditRollbackExecutor =
    Arc<dyn Fn(SourceEditRollbackInvocationV1) -> SourceEditFuture + Send + Sync + 'static>;

/// The MCP server wrapping a `TraceDecay` instance.
// Lock ordering: file_token_map -> method/resource/tool call counts (never nested)
pub struct McpServer {
    /// The served code graph. Guarded so a mid-session `git checkout` can
    /// hot-swap the instance onto the new branch's DB
    /// ([`Self::reopen_if_branch_drifted`]). Readers clone the `Arc` out and
    /// drop the lock immediately — no read guard is ever held across a
    /// handler await, so a swap never contends with in-flight calls. Calls
    /// already running when a swap lands finish against the old snapshot;
    /// each call is internally consistent.
    /// `Arc` so the retained branch-reopen task can hold a cheap clone and swap
    /// the freshly opened instance in when it lands.
    cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
    /// Single-flights branch reopen work. Held by the retained reopen task, not
    /// by the request that noticed the drift: a reopen is a full DB open plus a
    /// sealed restore, and no caller ever waits on it.
    branch_reopen: Arc<tokio::sync::Mutex<()>>,
    /// Count of settled branch reopens (success, failure, cancellation, or
    /// shutdown-time rejection) without exposing the retained task handle.
    branch_reopen_completions: Arc<AtomicU64>,
    /// Retains non-request maintenance futures so shutdown can fence task
    /// admission, cancel every live future, and join it before stores close.
    background_tasks: McpBackgroundTaskOwner,
    stats: ServerStats,
    method_call_counts: std::sync::Mutex<HashMap<String, u64>>,
    resource_read_counts: std::sync::Mutex<HashMap<String, u64>>,
    tool_call_counts: std::sync::Mutex<HashMap<String, u64>>,
    identical_read_coalescer: IdenticalReadCoalescer,
    diagnostics_cache: crate::diagnostics::DiagnosticsCache,
    diagnostics_lsp: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    /// Approximate token count per indexed file (`file_path` -> tokens).
    /// `Arc` so the retained D4 background-refresh task can hold a cheap
    /// clone and swap in the freshly synced map on completion.
    file_token_map: Arc<std::sync::Mutex<HashMap<String, u64>>>,
    /// Running total of tokens saved by serving from the graph.
    tokens_saved: AtomicU64,
    /// Tokens already flushed to the worldwide counter this session.
    last_flushed_tokens: AtomicU64,
    /// UNIX timestamp of last worldwide flush (0 = never).
    last_flush_at: AtomicI64,
    /// User-level database tracking all projects (best-effort). Wrapped in
    /// `Arc` so spawned savings-recording tasks can hold a cheap clone of
    /// the handle instead of opening a new connection per call.
    global_db: Option<Arc<RegisteredGlobalDb>>,
    profile_root: Option<PathBuf>,
    profile_identity: Option<crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    profile_retained_authority:
        Option<crate::daemon::retained_owner::ProfileRetainedConnectionAuthorityV1>,
    accounting_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
    /// Authoritative project session store retained for startup recovery.
    /// Recovery borrows this handle and never discovers or opens another DB.
    session_db: Option<Arc<RegisteredGlobalDb>>,
    /// Daemon-owned user-scope session store. All project servers borrow this
    /// shared authority instead of reopening `user-sessions.db` per tool call.
    user_session_db: Option<Arc<RegisteredGlobalDb>>,
    registered_session_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
    registered_user_session_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
    /// Daemon-retained admission queue for non-replayable project host events.
    /// Direct servers do not create an independent spool authority.
    host_admission_broker: Option<tracedecay_usecases::host_admission::SharedHostAdmissionBroker>,
    project_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    user_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    project_session_refresh_service: Option<Arc<dyn SessionRefreshServicePort>>,
    /// Exact registered session-store coordinates retained with the project
    /// refresh authority. V2 refresh requests must match these values; caller
    /// selectors never rename the mounted store in receipts or digest inputs.
    project_session_store_id: Option<tracedecay_usecases::context::SessionStoreId>,
    project_session_root_id: Option<tracedecay_usecases::context::SessionRootId>,
    session_sync_service:
        Option<std::sync::Weak<dyn tracedecay_application::session_sync::SessionSyncServicePort>>,
    project_application_retrieval: Option<MountedProjectApplicationRetrievalV1>,
    project_lcm_authority: Option<Arc<dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>>,
    user_lcm_authority: Option<Arc<dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>>,
    /// Owned cancellable project replay worker (daemon-owned servers). Joined on
    /// [`Self::shutdown`] so Unix and Windows drain the same way.
    project_host_admission_replay:
        tokio::sync::Mutex<Option<project_host_admission_replay::ProjectHostAdmissionReplayTask>>,
    /// Registry used for project-selector reads. This remains available even
    /// when global accounting is disabled so daemon clients do not fall back
    /// to the daemon process profile for selector resolution.
    registry_db: Option<Arc<RegisteredGlobalDb>>,
    /// Registry-read service handed to MCP handlers so they read registered
    /// projects through a port instead of holding [`Self::registry_db`].
    project_registry_reads: Option<Arc<dyn ProjectRegistryReadPort>>,
    automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
    database_owner_reconciler: Option<DatabaseOwnerReconciler>,
    dashboard_automation_writer: crate::dashboard::DashboardAutomationWriter,
    dashboard_doctor_report_reader: Option<crate::dashboard::DoctorReportReader>,
    doctor_report_published: AtomicBool,
    dashboard_code_index_freshness_reader:
        Option<crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader>,
    dashboard_feedback_status_reader: Option<crate::dashboard::feedback_api::FeedbackStatusReader>,
    background_refresh_writer: BackgroundRefreshWriter,
    /// Bridge delivering after-edit hook paths into the daemon-owned code-index
    /// scheduler queue. `None` for direct servers with no scheduler registry.
    code_index_hook_sink: Option<CodeIndexHookSink>,
    code_index_reconcile_sink: Option<CodeIndexReconcileSink>,
    /// Daemon-owned bridge to the code-index generation authority, the single
    /// mint for `file.daemon.<digest>` file identity and the generation every
    /// diagnostic producer must publish under. `None` for direct servers.
    code_index_publication_identity: Option<CodeIndexPublicationIdentityResolver>,
    /// Daemon-owned, authority-gated search bridge.
    code_index_search_executor: Option<CodeIndexSearchExecutor>,
    /// Daemon-owned exact sealed-generation branch comparison bridge.
    code_index_branch_diff_executor: Option<CodeIndexBranchDiffExecutor>,
    code_graph_projection_read_port: Option<CodeGraphProjectionReadPort>,
    code_graph_read_admission_port: Option<CodeGraphReadAdmissionPort>,
    code_index_ignored_dependency_admission: Option<CodeIndexIgnoredDependencyAdmissionPort>,
    /// Exact-scope sealed-generation census authority. It is installed only
    /// by daemon project-open after the route identity has resolved.
    generation_census_reader:
        tokio::sync::OnceCell<crate::runtime_telemetry::GenerationCensusReader>,
    /// Installed only after project-open has resolved current source-edit
    /// authority. Direct servers remain fail-closed.
    source_edit_executor: tokio::sync::OnceCell<SourceEditExecutor>,
    source_edit_reconciliation_executor: tokio::sync::OnceCell<SourceEditReconciliationExecutor>,
    source_edit_rollback_executor: tokio::sync::OnceCell<SourceEditRollbackExecutor>,
    /// Admission supplied by an authenticated daemon application route. It is
    /// deliberately absent until such a route/grant is available.
    code_index_search_authority: Option<CodeIndexSearchAuthorityV1>,
    retained_project_server_resolver: Option<RetainedProjectServerResolver>,
    #[cfg(any(test, feature = "test-transport"))]
    _host_admission_test_runtime: Option<Arc<crate::host_admission::HostAdmissionTestRuntimeV1>>,
    initialize_root_routing_enabled: AtomicBool,
    hook_project_routes: SharedHookProjectRouteCache,
    /// Cached latest-version check result.
    version_cache: std::sync::Mutex<VersionCheckState>,
    /// Pending JSON-RPC notifications to send before the next response.
    pending_notifications: std::sync::Mutex<Vec<Value>>,
    /// When the MCP server was started from a subdirectory of the project root,
    /// this holds the relative path prefix (e.g. `"src/mcp"`). Listing tools
    /// use it as the default path filter. `None` when cwd == project root.
    scope_prefix: Option<String>,
    /// Retains the single shutdown coordinator independently of its waiters.
    shutdown: connection::McpShutdownCompletion,
    /// When true, every `tools/call` response gains a `_meta.duration_us`
    /// field measuring the handler's pure execution time. Toggled by
    /// `tracedecay serve --timings`. Off by default to keep responses clean.
    timings_enabled: AtomicBool,
    /// UNIX timestamp (secs) of the most recent staleness check started by
    /// the server. Read-modify-update via `compare_exchange` in
    /// [`maybe_sync_if_stale`](Self::maybe_sync_if_stale) so concurrent
    /// tool calls don't pile on the same walk.
    last_staleness_check_at: AtomicI64,
    /// Cached worktree-vs-index mismatch detection for this session. `None`
    /// when no mismatch exists (the common case) or detection was skipped
    /// (not a git repo / git missing). Computed once at startup so we
    /// spawn at most one pair of `git rev-parse` per session no matter how
    /// many tool calls fire. See [`crate::worktree`] and #312.
    worktree_mismatch: Option<crate::worktree::WorktreeIndexMismatch>,
    /// Startup code-index catch-up lifecycle (D1): dispatch claim, retained
    /// task handle, and readiness state behind one lock. Historical session
    /// convergence is owned by the daemon scheduler, not this server.
    startup_catch_up: Arc<StartupCatchUpMachineV1>,
    /// `true` while a retained sync-on-read refresh (D4) is in flight.
    /// Single-flights the background refresh: `compare_exchange`d to `true`
    /// before spawning and cleared on completion. Also read by the D7
    /// staleness banner so an in-progress refresh emits the informational
    /// "refresh in progress" note instead of the manual-sync warning.
    /// `Arc` so the retained refresh task holds a cheap clone to clear it on
    /// completion.
    background_refresh_running: Arc<AtomicBool>,
    /// UNIX timestamp (secs) of the most recent sync-on-read background
    /// refresh spawn (D4). Gates the read-refresh cooldown independently of
    /// [`last_staleness_check_at`](Self::last_staleness_check_at), which
    /// gates the *blocking* edit-tool path — the two cooldowns must not
    /// share a stamp or one path would starve the other.
    last_background_refresh_at: AtomicI64,
    /// UNIX timestamp (secs) at which the most recent background refresh (D4)
    /// *completed*. `0` = never. Read by the D7 staleness banner so a refresh
    /// that finished within `read_cooldown_secs` suppresses the banner
    /// entirely (the index is as fresh as auto-sync can make it). `Arc` so
    /// the retained refresh task can stamp it on completion.
    last_background_refresh_done_at: Arc<AtomicI64>,
    /// The `[sync]` config resolved once at construction from the project
    /// root (plus `TRACEDECAY_SYNC_*` env overrides). Cached so the read
    /// hot path never re-reads the config file per `tools/call`.
    sync_config: crate::config::SyncConfig,
    /// Savings-ledger recorder tasks spawned so far / finished so far, plus
    /// a notifier pinged on every completion. Production never awaits these
    /// (ledger writes stay fire-and-forget); tests await
    /// [`Self::ledger_writes_settled`] to observe durability
    /// deterministically instead of polling the DB against a deadline.
    ledger_writes_started: Arc<AtomicU64>,
    ledger_writes_finished: Arc<AtomicU64>,
    ledger_write_notify: Arc<tokio::sync::Notify>,
    /// In-process debounce for live hook-route span observations, so a burst
    /// of tool-use events for one session/branch/worktree writes at most once
    /// per [`tracedecay_sessions::runtime::git_correlation::DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS`].
    span_observation_debounce:
        std::sync::Mutex<tracedecay_sessions::runtime::git_correlation::SpanObservationDebounce>,
    /// The negotiated MCP client name from the most recent `initialize`
    /// handshake's `clientInfo.name` (e.g. `"claude-code"`, `"codex"`,
    /// `"cursor"`). `None` until the first `initialize` request lands.
    /// Plumbed into analytics events so per-host tool adoption is visible
    /// (previously every call recorded `provider="mcp"` with no client
    /// identity). Re-set on every `initialize` so a long-lived daemon
    /// connection that gets re-initialized by a different client picks up
    /// the new identity.
    client_name: std::sync::Mutex<Option<String>>,
    /// Entropy-backed identity authority for persisted per-connection request
    /// scopes. If OS entropy was unavailable during server construction,
    /// connection establishment fails instead of reusing a timestamp fallback.
    ///
    /// The MCP transport negotiates only `clientInfo` (host name) at
    /// `initialize` — never a session/conversation id — and no session env var
    /// is passed to the server process, so a call's `session_id` column is
    /// populated only when the client happens to thread `session_id`/`sessionId`
    /// through the tool arguments (rare; historically ~97.6% of events had a
    /// NULL `session_id`). This id is the honest fallback: it cannot recover a
    /// true session identity, but it lets every event from one server lifetime
    /// be grouped. It is deliberately kept out of the `session_id` column so it
    /// never masquerades as a real session.
    connection_identity: McpConnectionIdentityAuthority,
    /// One lazy authenticated application client retained for this server.
    application_surface_client: tokio::sync::OnceCell<crate::daemon_client::DaemonInvocationClient>,
    /// Daemon-local executor installed by production project composition.
    /// External/direct servers fall back to the authenticated socket client.
    application_invocation_executor:
        Option<Arc<dyn crate::daemon_client::DaemonInvocationExecutor>>,
    daemon_invocation_service: Option<crate::daemon::DaemonInvocationService>,
    delivery_settlement_authority:
        Option<Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>>,
    delivery_settlement_recorder:
        Option<Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>>,
    /// Daemon-owned route liveness. A failed post-open health check revokes
    /// every tool on retained transports before cache retirement can await.
    project_server_live: Option<Arc<AtomicBool>>,
    /// The transport-visible response lifecycle for a retained project route.
    project_server_lifecycle: ProjectServerResponseLifecycle,
    dispatch_authority: RetainedDispatchAuthority,
}

#[derive(Clone)]
struct MountedProjectApplicationRetrievalV1 {
    identity: tracedecay_usecases::context::ResolvedSessionIdentity,
    service: Arc<dyn crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1>,
}

impl MountedProjectApplicationRetrievalV1 {
    fn work_evidence_retrieval(
        &self,
        expected_scope: &tracedecay_application::ResolvedScope,
        federated_authority: Arc<
            dyn crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1,
        >,
    ) -> Result<crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1> {
        let mounted_scope =
            self.identity
                .session_request_scope()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("mounted project session identity is invalid: {error}"),
                })?;
        let same_coordinates = mounted_scope.project_id == expected_scope.project_id
            && mounted_scope.repository_id == expected_scope.repository_id
            && mounted_scope.worktree_id == expected_scope.worktree_id;
        let reference_matches = expected_scope.reference.is_none()
            || mounted_scope.reference == expected_scope.reference;
        if !same_coordinates || !reference_matches {
            return Err(TraceDecayError::Config {
                message: "Work evidence retrieval scope does not match the mounted project session authority"
                    .to_owned(),
            });
        }
        Ok(
            crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1::new(Arc::clone(
                &self.service,
            ))
            .with_federated_authority(federated_authority),
        )
    }
}

impl McpServer {
    pub(crate) fn doctor_report_ready(&self) -> bool {
        self.dashboard_doctor_report_reader.is_some()
            && self.doctor_report_published.load(Ordering::Acquire)
    }

    /// Daemon-owned route liveness for a retained project server. `None` when
    /// this server is not a daemon-retained project route.
    pub(crate) fn project_route_live(&self) -> Option<bool> {
        self.project_server_live
            .as_ref()
            .map(|live| live.load(Ordering::Acquire))
    }

    pub(crate) fn publish_doctor_report(&self) {
        debug_assert!(self.dashboard_doctor_report_reader.is_some());
        self.doctor_report_published.store(true, Ordering::Release);
    }

    /// Creates a new MCP server backed by the given code graph.
    ///
    /// Index freshness for source-editing tools is maintained by a lazy
    /// staleness check ([`maybe_sync_if_stale`](Self::maybe_sync_if_stale))
    /// gated by a 30 s cooldown — there is no background watcher task. This
    /// replaces the
    /// `notify-debouncer-full` watcher removed in v6.x (#80), which was
    /// the source of severe CPU and memory pressure on large monorepos
    /// where nested ignored directories (`apps/*/node_modules`,
    /// `packages/*/target`) drove unbounded event traffic and `FileId`
    /// cache growth.
    pub async fn new(cg: TraceDecay, scope_prefix: Option<String>) -> Arc<Self> {
        Self::new_with_context(McpServerConstructionContext::direct(cg, scope_prefix)).await
    }

    #[cfg(test)]
    pub(crate) async fn new_with_dbs(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<RegisteredGlobalDb>>,
        registry_db: Option<Arc<RegisteredGlobalDb>>,
        use_default_profile_root: bool,
    ) -> Arc<Self> {
        let profile_root = use_default_profile_root
            .then(crate::storage::default_profile_root)
            .and_then(std::result::Result::ok);
        let context =
            Self::direct_context_with_dbs(cg, scope_prefix, profile_root, global_db, registry_db)
                .await;
        Self::new_with_context(context).await
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub fn host_admission_test_runtime_for_test(
        &self,
    ) -> Option<&crate::host_admission::HostAdmissionTestRuntimeV1> {
        self._host_admission_test_runtime.as_deref()
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub async fn new_with_host_admission_test_runtime_for_test(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        runtime: crate::host_admission::ProjectScopedTestRuntimeV1,
    ) -> crate::errors::Result<Arc<Self>> {
        Self::new_with_retained_test_servers_for_test(cg, scope_prefix, runtime, Vec::new()).await
    }

    /// As [`Self::new_with_host_admission_test_runtime_for_test`], plus exact
    /// servers for projects other than `cg`'s.
    ///
    /// Tools that name another project reach it only through the retained
    /// resolver. A cross-project fixture therefore supplies the whole target
    /// server, preserving the graph, ports, session stores, and lifecycle as
    /// one authority.
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub async fn new_with_retained_test_servers_for_test(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        runtime: crate::host_admission::ProjectScopedTestRuntimeV1,
        retained_servers: Vec<Arc<McpServer>>,
    ) -> crate::errors::Result<Arc<Self>> {
        let runtime = runtime.into_runtime();
        let mut context = runtime.mcp_server_context_for_test(cg, scope_prefix)?;
        // Hook notifications require a durable admission spool before their
        // plans replay and their post-commit side writes (route analytics,
        // span observations) run. Mount one on the runtime's project
        // sessions database, exactly like the daemon does in production.
        if context.host_admission_broker.is_none()
            && let Some(session_db) = context.session_db.as_ref()
        {
            let database_path = session_db.db_path().to_path_buf();
            let admission_runtime = tokio::task::spawn_blocking(move || {
                tracedecay_usecases::host_admission::HostAdmissionRuntime::open_for_database(
                    &database_path,
                )
            })
            .await
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!("test server host-admission task failed: {error}"),
            })?;
            let (admission_runtime, _) = admission_runtime?;
            context.host_admission_broker = Some(Arc::new(
                tracedecay_usecases::host_admission::HostAdmissionBroker::new(admission_runtime),
            ));
        }
        Self::new_with_registered_test_context(context, retained_servers).await
    }

    /// Mounts the daemon-equivalent retained project-server resolver on an
    /// already-assembled registered test context, then constructs the server.
    ///
    /// Path-selector reads — including the hook workspace route resolved
    /// before durable host admission — need both the registry database the
    /// test runtime supplies and a resolver for the selected server. A context
    /// without the resolver makes every
    /// hook notification fail closed as `project_registry_route_unavailable`
    /// before it ever reaches the spool, which is a fixture gap rather than
    /// the daemon's behaviour.
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub(crate) async fn new_with_registered_test_context(
        mut context: McpServerConstructionContext,
        retained_servers: Vec<Arc<McpServer>>,
    ) -> crate::errors::Result<Arc<Self>> {
        context
            .host_admission_test_runtime
            .as_ref()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "registered test context is missing its host-admission runtime".to_owned(),
            })?;
        let retained_root = context.cg.project_root().to_path_buf();
        // The daemon always has the active project mounted, so its resolver can
        // serve it like any other registered project. Mirror that here through
        // a late-bound weak slot: the server is only available after
        // construction, and retaining it strongly in its own resolver would
        // create a lifecycle cycle. Without this fallback a repo-local fixture makes
        // every path-selector read — including hook route resolution — report
        // the active project as unmounted.
        let active_server_slot: Arc<std::sync::OnceLock<std::sync::Weak<McpServer>>> =
            Arc::new(std::sync::OnceLock::new());
        let resolver_slot = Arc::clone(&active_server_slot);
        let active_root = canonical_or_original(&retained_root);
        let resolver: RetainedProjectServerResolver = Arc::new(move |request| {
            let retained_servers = retained_servers.clone();
            let resolver_slot = Arc::clone(&resolver_slot);
            let active_root = active_root.clone();
            Box::pin(async move {
                let requested = canonical_or_original(&request.requested_worktree_root);
                let registered = canonical_or_original(&request.registered_root);
                let project_id = request
                    .owner
                    .as_ref()
                    .map(|owner| owner.project.project_id.as_str());
                let mut matches = Vec::new();
                for server in &retained_servers {
                    let graph = server.cg_snapshot().await;
                    let root = canonical_or_original(graph.project_root());
                    let identity_matches = project_id.is_none_or(|project_id| {
                        graph.store_layout().identity.project_id.as_deref() == Some(project_id)
                    });
                    if (root == requested || root == registered) && identity_matches {
                        matches.push(Arc::clone(server));
                    }
                }
                if matches.len() == 1 {
                    return Ok(matches.pop());
                }
                if !matches.is_empty() {
                    return Err(crate::errors::TraceDecayError::project_route(
                        "project_route_ambiguous",
                        false,
                        "multiple retained test servers match one registered project route",
                    ));
                }
                let active = resolver_slot.get().and_then(std::sync::Weak::upgrade);
                let Some(active) = active else {
                    return Ok(None);
                };
                let graph = active.cg_snapshot().await;
                let identity_matches = project_id.is_none_or(|project_id| {
                    graph.store_layout().identity.project_id.as_deref() == Some(project_id)
                });
                Ok(
                    ((active_root == requested || active_root == registered) && identity_matches)
                        .then_some(active),
                )
            })
        });
        context = context.with_retained_project_server_resolver(resolver);
        let server = Self::new_with_context(context).await;
        // Registered test servers exercise real completion without consulting
        // the operator's network. A fresh cache entry for this exact build
        // keeps version-notice behavior deterministic while preserving the
        // production completion path.
        {
            let mut version_cache = server
                .version_cache
                .lock()
                .expect("registered test server version cache");
            version_cache.latest = Some(env!("CARGO_PKG_VERSION").to_owned());
            version_cache.checked_at = Some(Instant::now());
        }
        let _ = active_server_slot.set(Arc::downgrade(&server));
        Ok(server)
    }

    #[cfg(test)]
    async fn direct_context_with_dbs(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        profile_root: Option<PathBuf>,
        global_db: Option<Arc<RegisteredGlobalDb>>,
        registry_db: Option<Arc<RegisteredGlobalDb>>,
    ) -> McpServerConstructionContext {
        let user_session_db = None;
        let session_db = None;
        let mut context = McpServerConstructionContext::direct(cg, scope_prefix)
            .with_direct_databases(global_db, registry_db, session_db, user_session_db);
        context.profile_root = profile_root;
        context
    }

    pub(crate) async fn new_with_context(context: McpServerConstructionContext) -> Arc<Self> {
        let McpServerConstructionContext {
            cg,
            scope_prefix,
            profile_root,
            profile_identity,
            global_db,
            accounting_db,
            registry_db,
            session_db,
            user_session_db,
            registered_session_db,
            registered_user_session_db,
            session_sync_service,
            host_admission_broker,
            project_session_refresh_wake,
            user_session_refresh_wake,
            own_project_host_admission_replay,
            startup_catch_up_enabled,
            automation_scheduler_reconciler,
            database_owner_reconciler,
            dashboard_automation_writer,
            dashboard_doctor_report_reader,
            dashboard_code_index_freshness_reader,
            dashboard_feedback_status_reader,
            diagnostics_lsp,
            background_refresh_writer,
            code_index_hook_sink,
            code_index_reconcile_sink,
            code_index_publication_identity,
            code_index_search_executor,
            code_index_branch_diff_executor,
            code_graph_projection_read_port,
            code_graph_read_admission_port,
            code_index_ignored_dependency_admission,
            code_index_search_authority,
            retained_project_server_resolver,
            project_routes,
            application_invocation_executor,
            daemon_invocation_service,
            delivery_settlement_authority,
            delivery_settlement_recorder,
            project_server_live,
            #[cfg(any(test, feature = "test-transport"))]
            host_admission_test_runtime,
        } = context;
        let file_token_map = HashMap::new();
        let persisted = cg.get_tokens_saved().await.unwrap_or(0);
        let response_handle_project_root = cg.project_root().to_path_buf();
        // Register this project in the global DB with its current tokens
        if let Some(ref gdb) = accounting_db {
            gdb.upsert(cg.project_root(), persisted).await;
        } else if global_db.is_none() {
            // Name the gap where it is created. Every later savings and
            // analytics write from this server is a no-op (see
            // `LedgerSink::NotMounted`); without this, a fixture that forgot
            // to mount a database only failed much later, as an absent row.
            tracing::debug!(
                project_root = %cg.project_root().display(),
                "MCP server built with no accounting database; ledger and analytics writes are inert"
            );
        }

        // Detect borrowed-worktree index once at startup so every read
        // tool can cheaply prefix a heads-up. Two git rev-parse spawns
        // worst case (#312). spawn_blocking because the underlying
        // `Command::output()` can sit on slow disks.
        let worktree_mismatch = {
            let project_root = cg.project_root().to_path_buf();
            let scope_prefix = scope_prefix.clone();
            tokio::task::spawn_blocking(move || {
                crate::worktree::detect_scoped_worktree_index_mismatch(
                    &project_root,
                    scope_prefix.as_deref(),
                )
            })
            .await
            .ok()
            .flatten()
        };

        // `TraceDecay` materializes this from one resolved configuration
        // snapshot when it opens. Copy it once so D1/D4/D7 and telemetry
        // never re-read legacy input, a database, or IPC per call.
        let sync_config = cg.get_config().sync.clone();
        let telemetry_config = cg.get_config().telemetry.clone();
        let diagnostics_lsp = match diagnostics_lsp {
            Some(diagnostics_lsp) => diagnostics_lsp,
            None => {
                tracedecay_usecases::dashboard_diagnostics::open_diagnostic_broker(
                    cg.project_root().to_path_buf(),
                    &cg.store_layout().dashboard_root,
                )
                .await
            }
        };
        let active_project_id = cg.store_layout().identity.project_id.clone();
        let project_session_retrieval_root = match registry_db.as_deref() {
            Some(registry) => {
                crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project(&cg, registry)
                    .await
            }
            None => None,
        };
        #[cfg(any(test, feature = "test-transport"))]
        let project_session_retrieval_root = project_session_retrieval_root.or_else(|| {
            session_db.as_ref().map(|_| {
                crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project_for_test(&cg)
            })
        });
        let project_session_retrieval_root =
            project_session_retrieval_root.and_then(|root| match profile_identity.as_ref() {
                Some(identity) => root.with_project_runtime_shard(identity),
                None => Some(root),
            });
        let project_session_store_id = project_session_retrieval_root
            .as_ref()
            .map(|root| root.identity().store_id().clone());
        let project_session_root_id = project_session_retrieval_root
            .as_ref()
            .map(|root| root.identity().root_id().clone());
        let profile_session_retrieval_root =
            crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::profile().and_then(
                |root| match profile_identity.as_ref() {
                    Some(identity) => root.with_profile_runtime_shard(identity),
                    None => Some(root),
                },
            );
        let project_session_refresh_service = session_db
            .as_ref()
            .zip(project_session_refresh_wake.as_ref())
            .zip(active_project_id.clone())
            .map(|((database, wake), project_id)| {
                Arc::new(DaemonSessionRefreshService::new(
                    Arc::clone(database),
                    wake.clone(),
                    Some(project_id),
                )) as Arc<dyn SessionRefreshServicePort>
            });
        let project_registry_reads = registry_db.as_ref().map(|registry| {
            Arc::new(DaemonProjectRegistryReadService::new(Arc::clone(registry)))
                as Arc<dyn ProjectRegistryReadPort>
        });
        let project_application_retrieval = session_db
            .as_ref()
            .zip(project_session_retrieval_root.clone())
            .and_then(|(database, root)| {
                let identity = root.identity().clone();
                let service = match registered_session_db.as_ref() {
                    Some(registered) => {
                    crate::daemon::session_retrieval::DaemonSessionRetrievalService::new_registered(
                        Arc::clone(database),
                        Arc::clone(registered),
                        root,
                        project_session_refresh_wake.clone(),
                    )
                    }
                    None => crate::daemon::session_retrieval::DaemonSessionRetrievalService::new(
                        Arc::clone(database),
                        root,
                        project_session_refresh_wake.clone(),
                    ),
                }?;
                Some(MountedProjectApplicationRetrievalV1 {
                    identity,
                    service: Arc::new(service)
                        as Arc<
                            dyn crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1,
                        >,
                })
            });
        let project_lcm_authority = project_session_retrieval_root
            .as_ref()
            .zip(registered_session_db.as_ref())
            .and_then(|(root, database)| {
                crate::daemon::lcm_authority::mount_registered_lcm_authority(
                    Arc::clone(database),
                    root.identity().clone(),
                    root.expected_runtime_shard()?,
                )
            });
        let user_lcm_authority = profile_session_retrieval_root
            .as_ref()
            .zip(registered_user_session_db.as_ref())
            .and_then(|(root, database)| {
                crate::daemon::lcm_authority::mount_registered_lcm_authority(
                    Arc::clone(database),
                    root.identity().clone(),
                    root.expected_runtime_shard()?,
                )
            });
        let profile_retained_authority = match profile_identity
            .as_ref()
            .zip(profile_session_retrieval_root.as_ref())
        {
            Some((identity, root)) => {
                match crate::daemon::retained_owner::profile_retained_connection_authority(
                    identity,
                    root.identity(),
                ) {
                    Ok(authority) => Some(authority),
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "profile retained connection authority is unavailable"
                        );
                        None
                    }
                }
            }
            None => None,
        };

        let server = Arc::new_cyclic(|dispatch_server| Self {
            cg: Arc::new(tokio::sync::RwLock::new(cg)),
            branch_reopen: Arc::new(tokio::sync::Mutex::new(())),
            branch_reopen_completions: Arc::new(AtomicU64::new(0)),
            background_tasks: McpBackgroundTaskOwner::default(),
            stats: ServerStats::new(),
            method_call_counts: std::sync::Mutex::new(HashMap::new()),
            resource_read_counts: std::sync::Mutex::new(HashMap::new()),
            tool_call_counts: std::sync::Mutex::new(HashMap::new()),
            identical_read_coalescer: IdenticalReadCoalescer::default(),
            diagnostics_cache: crate::diagnostics::DiagnosticsCache::default(),
            diagnostics_lsp,
            file_token_map: Arc::new(std::sync::Mutex::new(file_token_map)),
            tokens_saved: AtomicU64::new(persisted),
            last_flushed_tokens: AtomicU64::new(persisted),
            last_flush_at: AtomicI64::new(0),
            global_db,
            accounting_db,
            profile_root,
            profile_identity,
            profile_retained_authority,
            session_db,
            registry_db,
            project_registry_reads,
            user_session_db,
            registered_session_db,
            registered_user_session_db,
            host_admission_broker,
            project_session_refresh_wake,
            user_session_refresh_wake,
            project_session_refresh_service,
            project_session_store_id,
            project_session_root_id,
            session_sync_service,
            project_application_retrieval,
            project_lcm_authority,
            user_lcm_authority,
            project_host_admission_replay: tokio::sync::Mutex::new(None),
            automation_scheduler_reconciler,
            database_owner_reconciler,
            dashboard_automation_writer,
            dashboard_doctor_report_reader,
            doctor_report_published: AtomicBool::new(false),
            dashboard_code_index_freshness_reader,
            dashboard_feedback_status_reader,
            background_refresh_writer,
            code_index_hook_sink,
            code_index_reconcile_sink,
            code_index_publication_identity,
            code_index_search_executor,
            code_index_branch_diff_executor,
            code_graph_projection_read_port,
            code_graph_read_admission_port,
            code_index_ignored_dependency_admission,
            generation_census_reader: tokio::sync::OnceCell::new(),
            source_edit_executor: tokio::sync::OnceCell::new(),
            source_edit_reconciliation_executor: tokio::sync::OnceCell::new(),
            source_edit_rollback_executor: tokio::sync::OnceCell::new(),
            code_index_search_authority,
            retained_project_server_resolver,
            #[cfg(any(test, feature = "test-transport"))]
            _host_admission_test_runtime: host_admission_test_runtime,
            initialize_root_routing_enabled: AtomicBool::new(true),
            hook_project_routes: project_routes,
            version_cache: std::sync::Mutex::new(VersionCheckState {
                latest: None,
                checked_at: None,
            }),
            pending_notifications: std::sync::Mutex::new(Vec::new()),
            scope_prefix,
            shutdown: connection::McpShutdownCompletion::default(),
            timings_enabled: AtomicBool::new(telemetry_config.timings),
            last_staleness_check_at: AtomicI64::new(0),
            worktree_mismatch,
            startup_catch_up: Arc::new(StartupCatchUpMachineV1::default()),
            background_refresh_running: Arc::new(AtomicBool::new(false)),
            last_background_refresh_at: AtomicI64::new(0),
            last_background_refresh_done_at: Arc::new(AtomicI64::new(0)),
            sync_config,
            ledger_writes_started: Arc::new(AtomicU64::new(0)),
            ledger_writes_finished: Arc::new(AtomicU64::new(0)),
            ledger_write_notify: Arc::new(tokio::sync::Notify::new()),
            span_observation_debounce: std::sync::Mutex::new(
                tracedecay_sessions::runtime::git_correlation::SpanObservationDebounce::new(),
            ),
            client_name: std::sync::Mutex::new(None),
            connection_identity: McpConnectionIdentityAuthority::from_os_entropy(),
            application_surface_client: tokio::sync::OnceCell::new(),
            application_invocation_executor,
            daemon_invocation_service,
            delivery_settlement_authority,
            delivery_settlement_recorder,
            project_server_live,
            project_server_lifecycle: ProjectServerResponseLifecycle::default(),
            dispatch_authority: RetainedDispatchAuthority::new(dispatch_server.clone()),
        });

        tokio::task::spawn_blocking(move || {
            let _ = cleanup_expired_response_handles(
                &response_handle_project_root,
                crate::tracedecay::current_timestamp(),
            );
        });
        if own_project_host_admission_replay
            && let Some(broker) = server.host_admission_broker.clone()
        {
            let server_for_pass = Arc::downgrade(&server);
            let pass = Arc::new(move || {
                let server = server_for_pass.clone();
                Box::pin(async move {
                    let Some(server) = server.upgrade() else {
                        return HostAdmissionOutcome::retained_unavailable("spool_unavailable");
                    };
                    let outcome = Box::pin(server.replay_host_admission(None)).await;
                    Self::report_host_admission_outcome(outcome);
                    outcome
                })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>,
                    >
            });
            let worker =
                project_host_admission_replay::ProjectHostAdmissionReplayTask::start(broker, pass);
            *server.project_host_admission_replay.lock().await = Some(worker);
        }

        // D1: startup catch-up sync. Reconciles changes made while the server
        // was down (terminal `git pull`, IDE edits before launch, another
        // tool's writes) so read-only sessions start fresh instead of serving
        // a stale index forever. `run_startup_catch_up_sync` advances its
        // state on every exit path, so we spawn it and return immediately.
        //
        // Gated on `SyncConfig.session_start_sync` (default true) and single-
        // flighted by the machine's dispatch claim so it runs at most once
        // per server even if two `new_with_dbs` paths overlap.
        //
        // Claiming dispatch is the transition into `Syncing`, so no waiter
        // can observe a claimed startup walk as already settled.
        if startup_catch_up_enabled
            && server.sync_config.session_start_sync
            && server.startup_catch_up.try_claim_dispatch()
        {
            let s = Arc::clone(&server);
            let task = tokio::spawn(async move {
                s.run_startup_catch_up_sync().await;
            });
            server.startup_catch_up.install_sync_task(task);
        }

        server
    }

    pub fn set_initialize_root_routing_enabled(&self, enabled: bool) {
        self.initialize_root_routing_enabled
            .store(enabled, Ordering::Relaxed);
    }

    /// Returns the active scope prefix, if the server was launched from a subdirectory.
    pub fn scope_prefix(&self) -> Option<&str> {
        self.scope_prefix.as_deref()
    }

    pub(crate) async fn reconcile_automation_scheduler(
        &self,
    ) -> crate::dashboard::AutomationSchedulerReconcileOutcome {
        match &self.automation_scheduler_reconciler {
            Some(reconcile) => reconcile().await,
            None => crate::dashboard::AutomationSchedulerReconcileOutcome::OwnerUnavailable,
        }
    }

    pub(crate) fn watcher_sync_config(&self) -> &crate::config::SyncConfig {
        &self.sync_config
    }

    /// Enables or disables per-call timing reporting. When enabled, every
    /// `tools/call` response gains a `_meta.duration_us` field with the
    /// handler's pure execution time in microseconds. Useful for profiling
    /// where time is spent inside the index vs. on the JSON-RPC/stdio
    /// transport. Safe to flip at any time — the next call observes the
    /// new setting.
    pub fn set_timings_enabled(&self, enabled: bool) {
        self.timings_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns whether timing reporting is currently enabled.
    pub fn timings_enabled(&self) -> bool {
        self.timings_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Test-only accessor for the backing `TraceDecay`. Exposed so
    /// integration tests can drive the staleness pipeline directly,
    /// bypassing the 30 s cooldown in
    /// [`maybe_sync_if_stale`](Self::maybe_sync_if_stale).
    #[doc(hidden)]
    pub async fn cg(&self) -> Arc<TraceDecay> {
        self.cg_snapshot().await
    }

    pub(crate) fn profile_identity(
        &self,
    ) -> Option<&crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1> {
        self.profile_identity.as_ref()
    }

    pub fn diagnostics_lsp(
        &self,
    ) -> Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>> {
        Arc::clone(&self.diagnostics_lsp)
    }

    #[cfg(feature = "test-transport")]
    #[doc(hidden)]
    pub async fn install_project_open_source_edit_authority_for_test(
        &self,
    ) -> crate::errors::Result<()> {
        crate::daemon::project_open_owners::install_project_open_source_edit_owners_for_test(self)
            .await
    }

    pub(crate) fn project_session_db(&self) -> Option<Arc<RegisteredGlobalDb>> {
        self.session_db.clone()
    }

    #[cfg(feature = "test-transport")]
    #[doc(hidden)]
    pub fn has_project_application_retrieval_for_test(&self) -> bool {
        self.project_application_retrieval.is_some()
    }

    pub(crate) fn work_evidence_retrieval(
        &self,
        expected_scope: &tracedecay_application::ResolvedScope,
        federated_authority: Arc<
            dyn crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1,
        >,
    ) -> Result<crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1> {
        let Some(mounted) = self.project_application_retrieval.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "Work evidence retrieval requires a mounted project session authority"
                    .to_owned(),
            });
        };
        mounted.work_evidence_retrieval(expected_scope, federated_authority)
    }

    pub(crate) fn project_session_application_retrieval_service(
        &self,
    ) -> Option<Arc<dyn crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1>> {
        self.project_application_retrieval
            .as_ref()
            .map(|mounted| Arc::clone(&mounted.service))
    }

    pub(crate) fn retained_surface_ports(
        &self,
        project_root: &Path,
        project_id: tracedecay_domain::ProjectId,
        configuration_digest: tracedecay_domain::ManifestDigest,
    ) -> Arc<tracedecay_application::retained_surfaces::RetainedSurfacePortsV1<'static>> {
        let project_workflow_index = self.registered_session_db.as_ref().map(|database| {
            Arc::new(DaemonWorkflowIndexReadService::new(Arc::clone(database)))
                as Arc<dyn tracedecay_sessions::WorkflowIndexReadPort>
        });
        crate::daemon::retained_owner::retained_surface_ports(
            crate::daemon::retained_owner::ProductionRetainedAuthoritiesV1 {
                cg: Arc::clone(&self.cg),
                project_root: project_root.to_path_buf(),
                project_id,
                configuration_digest,
                mounted_profile_id: self
                    .profile_identity()
                    .map(|identity| identity.profile_id().clone()),
                mounted_session_store_id: self.project_session_store_id.clone(),
                mounted_session_root_id: self.project_session_root_id.clone(),
                registered_session_db: self.registered_session_db.clone(),
                project_refresh: self.project_session_refresh_service.clone(),
                project_retrieval: self
                    .project_application_retrieval
                    .as_ref()
                    .map(|mounted| Arc::clone(&mounted.service)),
                project_workflow_index,
                project_lcm: self.project_lcm_authority.clone(),
            },
        )
    }

    /// Clones out the currently served `TraceDecay` instance. The lock is
    /// held only for the clone, never across an await on the instance.
    pub(crate) async fn cg_snapshot(&self) -> Arc<TraceDecay> {
        self.cg.read().await.clone()
    }

    /// Returns the current server runtime statistics as a JSON value.
    pub async fn server_stats_json(&self) -> Value {
        let uptime = self.stats.started_at.elapsed();
        let total_requests = self.stats.total_requests.load(Ordering::Relaxed);
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let errors = self.stats.errors.load(Ordering::Relaxed);
        let method_counts: Value = self
            .method_call_counts
            .lock()
            .map(|counts| json!(*counts))
            .unwrap_or(json!({}));
        let resource_counts: Value = self
            .resource_read_counts
            .lock()
            .map(|counts| json!(*counts))
            .unwrap_or(json!({}));
        let tool_counts: Value = self
            .tool_call_counts
            .lock()
            .map(|counts| json!(*counts))
            .unwrap_or(json!({}));
        let read_coalescing = self.identical_read_coalescer.snapshot();
        let file_token_entries = self
            .file_token_map
            .lock()
            .map(|tokens| tokens.len())
            .unwrap_or_default();
        let ratio = |n: u64| {
            if total_requests == 0 {
                0.0
            } else {
                n as f64 / total_requests as f64
            }
        };

        let mut stats = json!({
            "uptime_secs": uptime.as_secs(),
            "total_requests": total_requests,
            "jsonrpc_messages": total_requests,
            "tool_calls": tool_calls,
            "errors": errors,
            "method_call_counts": method_counts,
            "resource_read_counts": resource_counts,
            "tool_call_counts": tool_counts,
            "identical_read_coalescing": {
                "leaders": read_coalescing.leaders,
                "followers": read_coalescing.followers,
                "active_flights": read_coalescing.active_flights,
            },
            "retained_state_proxy": {
                "file_token_entries": file_token_entries,
                "database_authorities": {
                    "accounting": self.ledger_sink_is_mounted(),
                    "registry": self.registry_db.is_some(),
                    "project_sessions": self.session_db.is_some(),
                    "user_sessions": self.user_session_db.is_some(),
                },
            },
            "ratios": {
                "tool_calls_per_jsonrpc_message": ratio(tool_calls),
                "errors_per_jsonrpc_message": ratio(errors),
            },
            "approx_tokens_saved": self.tokens_saved.load(Ordering::Relaxed),
        });

        if let Some(ref gdb) = self.accounting_db
            && let Some(global_total) = gdb.global_tokens_saved().await
        {
            let local = self.tokens_saved.load(Ordering::Relaxed);
            stats["global_tokens_saved"] = json!(global_total.saturating_sub(local));
        } else if let Some(ref gdb) = self.global_db
            && let Some(global_total) = gdb.global_tokens_saved().await
        {
            let local = self.tokens_saved.load(Ordering::Relaxed);
            stats["global_tokens_saved"] = json!(global_total.saturating_sub(local));
        }

        let cg = self.cg_snapshot().await;
        stats["response_handles"] = response_handle_stats_json(Some(cg.project_root()));

        // Surface the verbose worktree-mismatch warning when present, so
        // `tracedecay_status` is the one tool whose output is loud about
        // serving a borrowed index (#312).
        if let Some(ref m) = self.worktree_mismatch {
            stats["worktree_mismatch"] = json!({
                "worktree_root": m.worktree_root.display().to_string(),
                "index_root": m.index_root.display().to_string(),
                "warning": crate::worktree::worktree_mismatch_warning(m),
            });
        }

        stats
    }
}

#[cfg(any(test, feature = "test-transport"))]
fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn json_rpc_request_id_string(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

fn application_surface_request_id(id: &Value, connection_scope: &str) -> Option<String> {
    crate::request_identity::mcp_connection_request_id(id, connection_scope)
        .map(|request_id| request_id.as_str().to_owned())
}

#[cfg(test)]
mod application_surface_request_id_tests {
    use serde_json::json;

    use super::application_surface_request_id;

    #[test]
    fn request_id_hash_preserves_json_rpc_id_type() {
        let numeric = application_surface_request_id(&json!(1), "connection").unwrap();
        let string = application_surface_request_id(&json!("1"), "connection").unwrap();

        assert_ne!(numeric, string);
        assert_eq!(
            numeric,
            application_surface_request_id(&json!(1), "connection").unwrap()
        );
        assert!(application_surface_request_id(&json!(null), "connection").is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod work_evidence_mount_tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod lcm_claude_recall_tests;

mod project_host_admission_replay;

/// D7 (staleness UX) + D1/D4 (startup catch-up + sync-on-read) behavioural
/// tests. The pure-logic banner tests need no server; the server tests build
/// a real indexed `TraceDecay` over a temp git repo, mirroring the
/// `indexing.rs` test idiom.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod background_refresh_writer_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod freshness_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod hook_boundary_failure_matrix_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod host_admission_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod query_scope_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod staleness_banner_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod writer_test_support;
