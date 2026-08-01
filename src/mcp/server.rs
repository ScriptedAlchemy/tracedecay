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

use crate::application::host_admission::{
    HostAdmissionOutcome, HostAdmissionStatus, TerminalReason, is_wire_oversized_io_error,
};
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
use crate::sessions::git_correlation::{
    self as git_correlation, DEFAULT_SPAN_MERGE_GAP_SECS, DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS,
    SpanObservation, SpanSource,
};
use crate::tracedecay::TraceDecay;

use super::hook_events::{self, HookAgent, HookEventPlan};
use super::tools::{
    ProjectRegistryReadPort, SessionRefreshServicePort, SessionRetrievalServicePort,
    ToolCallRegistryOptions, ToolRegistryMode, default_catalog_discovery_authority,
    explore_call_budget, get_catalog_filtered_tool_definitions_with_budget,
    handle_tool_call_with_registry_and_implicit_project, project_catalog_discovery_scope,
};
use super::transport::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

mod connection;
mod construction;
mod hook_dispatch;
mod hook_writes;
mod ledger;
mod lifecycle;
mod project_registry;
mod protocol;
mod read_coalescing;
mod requests;
mod rmcp;
mod routing;
mod session_refresh;
mod session_retrieval;
mod staleness;
mod tool_errors;
mod workflow_index;

pub(crate) use project_registry::DaemonProjectRegistryReadService;
pub(crate) use workflow_index::DaemonWorkflowIndexReadService;

pub(crate) use construction::*;
pub(crate) use hook_writes::*;
pub(crate) use ledger::McpToolErrorAnalyticsRequest;
pub(crate) use lifecycle::{
    ProjectServerResponseLifecycle, StartupCatchUpMachineV1, VersionCheckState,
};
pub(crate) use protocol::*;
use read_coalescing::*;
pub(crate) use rmcp::{RmcpConnectionAdapter, RmcpInitializeResponseDecorator};
pub(crate) use routing::*;
pub(crate) use session_refresh::*;
pub(crate) use session_retrieval::*;
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

/// Type-erased bridge from a tool handler to the daemon-owned code-index
/// generation authority. The daemon constructs this from its cloneable
/// `CodeIndexSchedulerRegistryV1`; direct (non-daemon) servers leave it `None`,
/// and a producer without it publishes nothing rather than minting its own file
/// identity.
pub(crate) type CodeIndexPublicationIdentityResolver =
    Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1 + 'static>;

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
                    crate::application::edit::SourceEditApplicationResult,
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

/// Concrete read bridge to a graph already mounted by the daemon. MCP project
/// selectors retain the root graph type because routed handlers require the
/// complete [`TraceDecay`] runtime.
pub(crate) use tracedecay_dashboard_api::project_graph::RetainedProjectGraphRequest;
pub(crate) type RetainedProjectGraphFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<Arc<TraceDecay>>>> + Send + 'static>,
>;
pub(crate) type RetainedProjectGraphResolver =
    Arc<dyn Fn(RetainedProjectGraphRequest) -> RetainedProjectGraphFuture + Send + Sync + 'static>;

/// Dashboard admission erases the concrete graph only at its consumer
/// boundary.
pub(crate) type DashboardRetainedProjectGraphResolver =
    tracedecay_dashboard_api::project_graph::RetainedProjectGraphResolver;

pub(crate) fn dashboard_retained_project_graph_resolver(
    resolver: RetainedProjectGraphResolver,
) -> DashboardRetainedProjectGraphResolver {
    Arc::new(move |request| {
        let resolver = Arc::clone(&resolver);
        Box::pin(async move {
            resolver(request).await.map(|graph| {
                graph.map(|graph| {
                    graph as Arc<dyn tracedecay_dashboard_api::DashboardProjectRuntime>
                })
            })
        })
    })
}

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
    cg: tokio::sync::RwLock<Arc<TraceDecay>>,
    /// Serializes branch reopen work without hiding the last complete graph
    /// behind the `cg` write lock while the replacement is prepared.
    branch_reopen: tokio::sync::Mutex<()>,
    stats: ServerStats,
    method_call_counts: std::sync::Mutex<HashMap<String, u64>>,
    resource_read_counts: std::sync::Mutex<HashMap<String, u64>>,
    tool_call_counts: std::sync::Mutex<HashMap<String, u64>>,
    identical_read_coalescer: IdenticalReadCoalescer,
    diagnostics_cache: crate::diagnostics::DiagnosticsCache,
    diagnostics_lsp: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    /// Approximate token count per indexed file (`file_path` -> tokens).
    /// `Arc` so the detached D4 background-refresh task can hold a cheap
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
    transcript_source_home: Option<PathBuf>,
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
    host_admission_broker: Option<crate::application::host_admission::SharedHostAdmissionBroker>,
    project_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    user_session_refresh_wake:
        Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    project_session_refresh_service: Option<Arc<dyn SessionRefreshServicePort>>,
    user_session_refresh_service: Option<Arc<dyn SessionRefreshServicePort>>,
    project_session_retrieval_service: Option<Arc<dyn SessionRetrievalServicePort>>,
    user_session_retrieval_service: Option<Arc<dyn SessionRetrievalServicePort>>,
    #[cfg(test)]
    project_session_retrieval_calls: Arc<AtomicU64>,
    #[cfg(test)]
    user_session_retrieval_calls: Arc<AtomicU64>,
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
    dashboard_doctor_remediation_dispatcher:
        Option<crate::dashboard::DoctorRemediationDispatcherV1>,
    dashboard_code_index_freshness_reader:
        Option<crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader>,
    dashboard_feedback_status_reader: Option<crate::dashboard::feedback_api::FeedbackStatusReader>,
    hook_branch_writer: HookBranchWriter,
    background_refresh_writer: BackgroundRefreshWriter,
    /// Bridge delivering after-edit hook paths into the daemon-owned code-index
    /// scheduler queue. `None` for direct servers with no scheduler registry.
    code_index_hook_sink: Option<CodeIndexHookSink>,
    /// Daemon-owned bridge to the code-index generation authority, the single
    /// mint for `file.daemon.<digest>` file identity and the generation every
    /// diagnostic producer must publish under. `None` for direct servers.
    code_index_publication_identity: Option<CodeIndexPublicationIdentityResolver>,
    /// Daemon-owned, authority-gated search bridge.
    code_index_search_executor: Option<CodeIndexSearchExecutor>,
    /// Installed only after project-open has resolved current source-edit
    /// authority. Direct servers remain fail-closed.
    source_edit_executor: tokio::sync::OnceCell<SourceEditExecutor>,
    source_edit_reconciliation_executor: tokio::sync::OnceCell<SourceEditReconciliationExecutor>,
    /// Admission supplied by an authenticated daemon application route. It is
    /// deliberately absent until such a route/grant is available.
    code_index_search_authority: Option<CodeIndexSearchAuthorityV1>,
    retained_project_graph_resolver: Option<RetainedProjectGraphResolver>,
    #[cfg(any(test, feature = "test-transport"))]
    _host_admission_test_runtime:
        Option<Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>>,
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
    /// Set to `true` after `shutdown` runs once; makes shutdown idempotent so
    /// callers can invoke it explicitly after `run` returns without re-running
    /// persistence logic.
    shutdown_done: AtomicBool,
    /// When true, every `tools/call` response gains a `_meta.duration_us`
    /// field measuring the handler's pure execution time. Toggled by
    /// `tracedecay serve --timings`. Off by default to keep responses clean.
    timings_enabled: AtomicBool,
    /// UNIX timestamp (secs) of the most recent staleness check started by
    /// the server. Read-modify-update via `compare_exchange` in
    /// [`maybe_sync_if_stale`](Self::maybe_sync_if_stale) so concurrent
    /// tool calls don't pile on the same walk.
    last_staleness_check_at: AtomicI64,
    /// UNIX timestamp (secs) of the most recent staged-automation notice
    /// check. Same `compare_exchange` cooldown pattern as
    /// [`last_staleness_check_at`](Self::last_staleness_check_at) so the
    /// pending-review stores are re-read at most once per window no matter
    /// how many tool calls fire.
    last_automation_notice_check_at: AtomicI64,
    /// Cached worktree-vs-index mismatch detection for this session. `None`
    /// when no mismatch exists (the common case) or detection was skipped
    /// (not a git repo / git missing). Computed once at startup so we
    /// spawn at most one pair of `git rev-parse` per session no matter how
    /// many tool calls fire. See [`crate::worktree`] and #312.
    worktree_mismatch: Option<crate::worktree::WorktreeIndexMismatch>,
    /// The whole startup catch-up lifecycle (D1): dispatch claim, index-sync
    /// and transcript-ingest phases, both retained task handles, and the
    /// ingest cancellation — one typed state behind one lock. See
    /// [`StartupCatchUpStateV1`] for the phases and the ordering hazard the
    /// previous flag soup carried. `Arc` so the detached ingest task can
    /// settle the same machine that waiters and shutdown read.
    startup_catch_up: Arc<StartupCatchUpMachineV1>,
    /// `true` while a detached sync-on-read refresh (D4) is in flight.
    /// Single-flights the background refresh: `compare_exchange`d to `true`
    /// before spawning and cleared on completion. Also read by the D7
    /// staleness banner so an in-progress refresh emits the informational
    /// "refresh in progress" note instead of the manual-sync warning.
    /// `Arc` so the detached refresh task holds a cheap clone to clear it on
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
    /// the detached refresh task can stamp it on completion.
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
    /// per [`crate::sessions::git_correlation::DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS`].
    span_observation_debounce:
        std::sync::Mutex<crate::sessions::git_correlation::SpanObservationDebounce>,
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
    /// Daemon-owned route liveness. A failed post-open health check revokes
    /// every tool on retained transports before cache retirement can await.
    project_server_live: Option<Arc<AtomicBool>>,
    /// The transport-visible response lifecycle for a retained project route.
    project_server_lifecycle: ProjectServerResponseLifecycle,
    /// Live MCP cancellation tokens keyed by canonical application request id.
    application_surface_cancellations:
        std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>>,
}

impl McpServer {
    pub(crate) fn doctor_report_ready(&self) -> bool {
        self.dashboard_doctor_report_reader.is_some()
            && self.doctor_report_published.load(Ordering::Acquire)
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

    #[cfg(all(test, unix))]
    pub(crate) async fn new_with_global_db(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<RegisteredGlobalDb>>,
    ) -> Arc<Self> {
        Self::new_with_dbs(cg, scope_prefix, global_db.clone(), global_db, true).await
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

    #[cfg(feature = "test-transport")]
    #[doc(hidden)]
    pub fn has_project_session_retrieval_service_for_test(&self) -> bool {
        self.project_session_retrieval_service.is_some()
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub fn host_admission_test_runtime_for_test(
        &self,
    ) -> Option<&crate::application::host_admission::HostAdmissionTestRuntimeV1> {
        self._host_admission_test_runtime.as_deref()
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub async fn new_with_host_admission_test_runtime_for_test(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        runtime: crate::application::host_admission::ProjectScopedTestRuntimeV1,
    ) -> crate::errors::Result<Arc<Self>> {
        Self::new_with_retained_test_graphs_for_test(cg, scope_prefix, runtime, Vec::new()).await
    }

    /// As [`Self::new_with_host_admission_test_runtime_for_test`], plus graphs
    /// for projects other than `cg`'s.
    ///
    /// Tools that name another project reach it only through the retained
    /// resolver (see `handlers::selected_registered_project_reader`), and a
    /// test runtime is scoped to a single project, so a cross-project fixture
    /// has to open each additional graph through its own scoped runtime and
    /// hand the result in here.
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub async fn new_with_retained_test_graphs_for_test(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        runtime: crate::application::host_admission::ProjectScopedTestRuntimeV1,
        retained_graphs: Vec<Arc<TraceDecay>>,
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
            let (admission_runtime, _) = tokio::task::spawn_blocking(move || {
                crate::application::host_admission::HostAdmissionRuntime::open_for_database(
                    &database_path,
                )
            })
            .await
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!("test server host-admission task failed: {error}"),
            })?
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!("test server host-admission spool failed: {error:?}"),
            })?;
            context.host_admission_broker = Some(Arc::new(
                crate::application::host_admission::HostAdmissionBroker::new(admission_runtime),
            ));
        }
        Self::new_with_registered_test_context(context, retained_graphs).await
    }

    /// Mounts the daemon-equivalent retained project-graph resolver on an
    /// already-assembled registered test context, then constructs the server.
    ///
    /// Path-selector reads — including the hook workspace route resolved
    /// before durable host admission — go through
    /// `handlers::selected_registered_project_reader`, which needs both the
    /// registry database the test runtime supplies and a resolver that can
    /// mount the selected project. A context without the resolver makes every
    /// hook notification fail closed as `project_registry_route_unavailable`
    /// before it ever reaches the spool, which is a fixture gap rather than
    /// the daemon's behaviour.
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub(crate) async fn new_with_registered_test_context(
        mut context: McpServerConstructionContext,
        retained_graphs: Vec<Arc<TraceDecay>>,
    ) -> crate::errors::Result<Arc<Self>> {
        let runtime = Arc::clone(context.host_admission_test_runtime.as_ref().ok_or_else(
            || crate::errors::TraceDecayError::Config {
                message: "registered test context is missing its host-admission runtime".to_owned(),
            },
        )?);
        let retained_root = context.cg.project_root().to_path_buf();
        let profile_root = runtime.profile_root_for_test().to_path_buf();
        let mut retained: Vec<(PathBuf, Arc<TraceDecay>)> = retained_graphs
            .into_iter()
            .map(|graph| (canonical_or_original(graph.project_root()), graph))
            .collect();
        // The runtime's resolver only mounts stores inside its own profile
        // root. A runtime supplied purely as an injected registry for a graph
        // in another profile has no graph here to retain; retaining
        // unconditionally instead resolved a store path never created.
        if graph_lives_under_profile(&context.cg, &profile_root) {
            let active = runtime
                .open_project_graph_for_test(
                    &retained_root,
                    crate::tracedecay::TraceDecayOpenOptions {
                        global_db_path: Some(profile_root.join("global.db")),
                        profile_root: Some(profile_root),
                    },
                )
                .await?;
            retained.push((canonical_or_original(&retained_root), Arc::new(active)));
        }
        // The daemon always has the active project mounted, so its resolver can
        // serve it like any other registered project. Mirror that here through
        // a late-bound slot: the server's own graph is only available after
        // construction, so the resolver captures the slot now and the slot is
        // filled from the finished server below. Without this fallback a
        // repo-local fixture (whose graph never enters `retained` above) makes
        // every path-selector read — including hook route resolution — report
        // the active project as unmounted.
        let active_graph_slot: Arc<std::sync::OnceLock<Arc<TraceDecay>>> =
            Arc::new(std::sync::OnceLock::new());
        let resolver_slot = Arc::clone(&active_graph_slot);
        let active_root = canonical_or_original(&retained_root);
        let resolver: RetainedProjectGraphResolver = Arc::new(move |request| {
            let requested = canonical_or_original(&request.requested_worktree_root);
            let registered = canonical_or_original(&request.registered_root);
            let project_id = request
                .owner
                .as_ref()
                .map(|owner| owner.project.project_id.as_str());
            let identity_matches = |graph: &Arc<TraceDecay>| {
                project_id.is_none_or(|project_id| {
                    graph.store_layout().identity.project_id.as_deref() == Some(project_id)
                })
            };
            let mut matches = retained.iter().filter(|(root, graph)| {
                (*root == requested || *root == registered) && identity_matches(graph)
            });
            let graph = matches.next().map(|(_, graph)| Arc::clone(graph));
            let graph = matches.next().is_none().then_some(graph).flatten();
            let graph = graph.or_else(|| {
                ((active_root == requested || active_root == registered)
                    && resolver_slot.get().is_some_and(identity_matches))
                .then(|| resolver_slot.get().map(Arc::clone))
                .flatten()
            });
            Box::pin(async move { Ok(graph) })
        });
        context = context.with_retained_project_graph_resolver(resolver);
        let server = Self::new_with_context(context).await;
        let _ = active_graph_slot.set(server.cg_snapshot().await);
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
            transcript_source_home,
            global_db,
            accounting_db,
            registry_db,
            session_db,
            user_session_db,
            registered_session_db,
            registered_user_session_db,
            host_admission_broker,
            project_session_refresh_wake,
            user_session_refresh_wake,
            own_project_host_admission_replay,
            startup_catch_up_enabled,
            automation_scheduler_reconciler,
            database_owner_reconciler,
            dashboard_automation_writer,
            dashboard_doctor_report_reader,
            dashboard_doctor_remediation_dispatcher,
            dashboard_code_index_freshness_reader,
            dashboard_feedback_status_reader,
            diagnostics_lsp,
            hook_branch_writer,
            background_refresh_writer,
            code_index_hook_sink,
            code_index_publication_identity,
            code_index_search_executor,
            code_index_search_authority,
            retained_project_graph_resolver,
            project_routes,
            application_invocation_executor,
            project_server_live,
            #[cfg(any(test, feature = "test-transport"))]
            host_admission_test_runtime,
        } = context;
        #[cfg(test)]
        assert!(
            !startup_catch_up_enabled
                || registered_session_db.is_none()
                || profile_identity.is_none()
                || transcript_source_home.is_some(),
            "test MCP servers with startup transcript authority require an isolated transcript-source home"
        );
        let file_token_map = cg.get_file_token_map().await.unwrap_or_default();
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
                crate::application::dashboard_diagnostics::open_diagnostic_broker(
                    cg.project_root().to_path_buf(),
                    &cg.store_layout().dashboard_root,
                )
                .await
            }
        };
        let active_project_id = cg.store_layout().identity.project_id.clone();
        let project_session_retrieval_root = match registry_db.as_deref() {
            Some(registry) => DaemonSessionRetrievalRoot::project(&cg, registry).await,
            None => None,
        };
        #[cfg(any(test, feature = "test-transport"))]
        let project_session_retrieval_root = project_session_retrieval_root.or_else(|| {
            session_db
                .as_ref()
                .map(|_| DaemonSessionRetrievalRoot::project_for_test(&cg))
        });
        let project_session_retrieval_root =
            project_session_retrieval_root.and_then(|root| match profile_identity.as_ref() {
                Some(identity) => root.with_project_runtime_shard(identity),
                None => Some(root),
            });
        let profile_session_retrieval_root =
            DaemonSessionRetrievalRoot::profile().and_then(|root| {
                match profile_identity.as_ref() {
                    Some(identity) => root.with_profile_runtime_shard(identity),
                    None => Some(root),
                }
            });
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
        let user_session_refresh_service = user_session_db
            .as_ref()
            .zip(user_session_refresh_wake.as_ref())
            .map(|(database, wake)| {
                Arc::new(DaemonSessionRefreshService::new(
                    Arc::clone(database),
                    wake.clone(),
                    None,
                )) as Arc<dyn SessionRefreshServicePort>
            });
        let project_registry_reads = registry_db.as_ref().map(|registry| {
            Arc::new(DaemonProjectRegistryReadService::new(Arc::clone(registry)))
                as Arc<dyn ProjectRegistryReadPort>
        });
        let project_session_retrieval_calls = Arc::new(AtomicU64::new(0));
        let user_session_retrieval_calls = Arc::new(AtomicU64::new(0));
        let project_session_retrieval_service = session_db
            .as_ref()
            .zip(project_session_retrieval_root)
            .and_then(|(database, root)| match registered_session_db.as_ref() {
                Some(registered) => DaemonSessionRetrievalService::new_registered(
                    Arc::clone(database),
                    Arc::clone(registered),
                    root,
                    Arc::clone(&project_session_retrieval_calls),
                    project_session_refresh_wake.clone(),
                ),
                None => DaemonSessionRetrievalService::new(
                    Arc::clone(database),
                    root,
                    Arc::clone(&project_session_retrieval_calls),
                    project_session_refresh_wake.clone(),
                ),
            })
            .map(|service| Arc::new(service) as Arc<dyn SessionRetrievalServicePort>);
        let user_session_retrieval_service = user_session_db
            .as_ref()
            .zip(profile_session_retrieval_root)
            .and_then(
                |(database, root)| match registered_user_session_db.as_ref() {
                    Some(registered) => DaemonSessionRetrievalService::new_registered(
                        Arc::clone(database),
                        Arc::clone(registered),
                        root,
                        Arc::clone(&user_session_retrieval_calls),
                        None,
                    ),
                    None => DaemonSessionRetrievalService::new(
                        Arc::clone(database),
                        root,
                        Arc::clone(&user_session_retrieval_calls),
                        None,
                    ),
                },
            )
            .map(|service| Arc::new(service) as Arc<dyn SessionRetrievalServicePort>);

        let server = Arc::new(Self {
            cg: tokio::sync::RwLock::new(cg),
            branch_reopen: tokio::sync::Mutex::new(()),
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
            transcript_source_home,
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
            user_session_refresh_service,
            project_session_retrieval_service,
            user_session_retrieval_service,
            #[cfg(test)]
            project_session_retrieval_calls,
            #[cfg(test)]
            user_session_retrieval_calls,
            project_host_admission_replay: tokio::sync::Mutex::new(None),
            automation_scheduler_reconciler,
            database_owner_reconciler,
            dashboard_automation_writer,
            dashboard_doctor_report_reader,
            doctor_report_published: AtomicBool::new(false),
            dashboard_doctor_remediation_dispatcher,
            dashboard_code_index_freshness_reader,
            dashboard_feedback_status_reader,
            hook_branch_writer,
            background_refresh_writer,
            code_index_hook_sink,
            code_index_publication_identity,
            code_index_search_executor,
            source_edit_executor: tokio::sync::OnceCell::new(),
            source_edit_reconciliation_executor: tokio::sync::OnceCell::new(),
            code_index_search_authority,
            retained_project_graph_resolver,
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
            shutdown_done: AtomicBool::new(false),
            timings_enabled: AtomicBool::new(telemetry_config.timings),
            last_staleness_check_at: AtomicI64::new(0),
            last_automation_notice_check_at: AtomicI64::new(0),
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
                crate::sessions::git_correlation::SpanObservationDebounce::new(),
            ),
            client_name: std::sync::Mutex::new(None),
            connection_identity: McpConnectionIdentityAuthority::from_os_entropy(),
            application_surface_client: tokio::sync::OnceCell::new(),
            application_invocation_executor,
            project_server_live,
            project_server_lifecycle: ProjectServerResponseLifecycle::default(),
            application_surface_cancellations: std::sync::Mutex::new(HashMap::new()),
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
        // a stale index forever. `run_startup_catch_up_sync` is non-blocking-
        // safe (detached transcript ingest, flags flipped on every exit path),
        // so we spawn it detached and return immediately.
        //
        // Gated on `SyncConfig.session_start_sync` (default true) and single-
        // flighted by the machine's dispatch claim so it runs at most once
        // per server even if two `new_with_dbs` paths overlap.
        //
        // Claiming dispatch *is* the transition into `Syncing`, which is what
        // used to require pre-clearing two default-`true` completion flags
        // before the spawn. Without that pre-clear there was a window between
        // the spawn and the task's first instruction where both flags still
        // read `true`, so a caller that reached `wait_for_startup_catch_up`
        // in that window observed "done" and returned immediately — then the
        // detached catch-up sync ran concurrently with the caller's own work
        // (e.g. racing it to index a just-written file). The window cannot
        // reopen now: no state exists in which a claimed dispatch reads as
        // settled.
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

    /// Installs the sole source-edit invocation owner resolved during
    /// project-open admission. Reinstallation is rejected so a later caller
    /// cannot replace the authority behind an already-serving MCP instance.
    pub(crate) fn install_source_edit_executor(
        &self,
        executor: SourceEditExecutor,
    ) -> std::result::Result<(), SourceEditExecutor> {
        self.source_edit_executor
            .set(executor)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(executor)
                | tokio::sync::SetError::InitializingError(executor) => executor,
            })
    }

    pub(crate) fn install_source_edit_reconciliation_executor(
        &self,
        executor: SourceEditReconciliationExecutor,
    ) -> std::result::Result<(), SourceEditReconciliationExecutor> {
        self.source_edit_reconciliation_executor
            .set(executor)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(executor)
                | tokio::sync::SetError::InitializingError(executor) => executor,
            })
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

    /// Clones out the currently served `TraceDecay` instance. The lock is
    /// held only for the clone, never across an await on the instance.
    async fn cg_snapshot(&self) -> Arc<TraceDecay> {
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

/// Whether `cg`'s graph store sits inside `profile_root`, which is the only
/// region a test runtime rooted there is allowed to mount.
#[cfg(any(test, feature = "test-transport"))]
fn graph_lives_under_profile(cg: &TraceDecay, profile_root: &Path) -> bool {
    canonical_or_original(&cg.store_layout().graph_db_path)
        .starts_with(canonical_or_original(profile_root))
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
mod message_search_cutover_tests;

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
mod hook_branch_writer_tests;
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
