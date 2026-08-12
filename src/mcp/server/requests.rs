//! Request routing and handlers: per-method JSON-RPC dispatch,
//! handshake handling, resources, and `tools/call` execution.

use super::dispatch_settlement::{
    ApplicationCancellationRegistration, DispatchControl, PreparedDispatchControl,
};
use super::*;
use crate::mcp::ToolResult;

mod tool_dispatch;

struct PreparedToolCall {
    tool_name: String,
    arguments: Value,
    analytics_arguments: Value,
    analytics_session_id: Option<String>,
}

struct DispatchedToolCall {
    cg: Arc<TraceDecay>,
    selected_owner: Option<crate::global_db::ProjectRegistryContext>,
    selected_scope: Option<tracedecay_application::ResolvedScope>,
    outcome: Result<ToolResult>,
    elapsed_us: Option<u64>,
}

struct RoutedToolCall {
    arguments: Value,
    selected_project: Option<crate::mcp::project_route::ResolvedProjectRoute>,
    selected_server: Option<Arc<McpServer>>,
}

struct ToolTokenAccounting {
    raw_file_tokens: u64,
    response_tokens: u64,
    net_saved_tokens: u64,
}

pub(super) fn invocation_target_for_route(
    route: Option<&crate::mcp::project_route::ResolvedProjectRoute>,
) -> tracedecay_application::InvocationTarget {
    route.map_or(
        tracedecay_application::InvocationTarget::CurrentProject,
        |route| tracedecay_application::InvocationTarget::Resolved(route.scope.clone()),
    )
}

pub(super) fn accounting_project_root<'a>(
    active_root: &'a Path,
    selected_owner: Option<&'a crate::global_db::ProjectRegistryContext>,
    selected_scope: Option<&tracedecay_application::ResolvedScope>,
) -> Option<&'a Path> {
    match (selected_owner, selected_scope) {
        (None, None) => Some(active_root),
        (Some(owner), Some(scope)) if owner.project.project_id == scope.project_id.as_str() => {
            Some(Path::new(&owner.project.canonical_root))
        }
        _ => None,
    }
}

/// Locks a server-side `std::sync::Mutex`, recovering from poisoning.
///
/// A panic anywhere in a client task poisons every `Mutex` a guard was alive
/// for. Treating poison as "skip this work" would be permanent: request
/// counters stop counting and, worse, the cancellation registry stops
/// registering, so shutdown can no longer cancel in-flight requests and the
/// drain hangs. None of the guarded state can be left torn by an unwind (each
/// critical section is a single map or counter update), so recovering the
/// value is the correct response.
pub(super) fn recover_lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Application-surface plumbing prepared once per dispatch: the typed request
/// invocation executor, retained independently from dispatch settlement.
struct ApplicationSurfaceDispatch<'a> {
    invocation_executor: Option<&'a dyn crate::daemon_client::DaemonInvocationExecutor>,
}

/// Whether a tool reaches the typed daemon invocation boundary.
///
/// Canonical application and retained operations require the daemon invocation
/// executor before the request is admitted to its typed owner.
fn requires_application_invocation_executor(tool_name: &str) -> bool {
    crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name).is_some()
        || crate::mcp::tools::binding::work_operation_for_tool(tool_name).is_some()
        || tracedecay_application::RetainedSurfaceOperation::from_tool_name(tool_name).is_some()
}

/// Retained name for this module's call sites; the saturating clamp is the one
/// shared definition so MCP cannot stamp "now" differently from the daemon.
pub(super) fn mcp_now_micros() -> tracedecay_domain::UtcMicros {
    tracedecay_application::clock::now_micros()
}

pub(super) fn is_source_edit_tool(tool_name: &str) -> bool {
    crate::mcp::tools::tool_dispatches_source_edit_effect(tool_name)
}

/// Reads that walk a git tree or the whole code graph, and so must not run
/// without a horizon.
///
/// The catalog-owned git reads are recognised by their surface operation; every
/// other git-walking tool is recognised through the canonical MCP binding table
/// rather than a second hand-maintained name list, so a newly bound git tool
/// inherits the bound instead of silently running unbounded.
pub(super) fn is_controlled_read_tool(tool_name: &str) -> bool {
    matches!(
        crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name),
        Some(
            crate::application_surface::ApplicationSurfaceOperation::GitStatus
                | crate::application_surface::ApplicationSurfaceOperation::GitDiff
                | crate::application_surface::ApplicationSurfaceOperation::GitHistory
                | crate::application_surface::ApplicationSurfaceOperation::GitBlame
                | crate::application_surface::ApplicationSurfaceOperation::GitHunks
        )
    ) || crate::mcp::tools::handlers::tool_dispatches_git_reads(tool_name)
        || tool_name == "tracedecay_search"
}

pub(super) fn tool_supports_live_cancellation(tool_name: &str) -> bool {
    crate::mcp::tools::tool_supports_live_cancellation(tool_name)
}

pub(super) fn dispatch_deadline_horizon_micros(bounded_operation: bool) -> Option<i64> {
    bounded_operation.then_some(30_000_000)
}

/// Hand-maintained schema documentation for the `tracedecay://schema` resource.
/// Mirrors `src/db/migrations.rs::create_schema`. Update both together.
const SCHEMA_MARKDOWN: &str = r"# tracedecay SQLite schema

The active project database lives in the user-level TraceDecay profile store
(`~/.tracedecay/projects/<project_id>/tracedecay.db` by default), scoped to the
current project. Linked worktrees share this durable project store. All tables
are plain SQLite; safe to query with any client. WAL mode is used, so readers
do not block writers.

## Tables

### `nodes` — every indexed symbol
- `id` TEXT PRIMARY KEY — content-hashed identifier (changes when symbol moves or renames)
- `kind` TEXT — e.g. `function`, `struct`, `trait`, `impl`, `method`, `module`, `file`
- `name` TEXT — local identifier
- `qualified_name` TEXT — language-style path (e.g. `crate::module::Type::method`)
- `file_path` TEXT — relative to the project root
- `start_line`, `end_line` INTEGER — 1-based inclusive line range of the symbol
- `start_column`, `end_column` INTEGER — 0-based column range
- `attrs_start_line` INTEGER — first line of leading doc-comments / attributes (or `start_line` if none)
- `signature` TEXT NULL — extracted source-level signature
- `docstring` TEXT NULL — leading doc-comment
- `visibility` TEXT — one of `public`, `pub_crate`, `pub_super`, `private`
- `is_async` INTEGER (0/1)
- `branches`, `loops`, `returns`, `max_nesting`, `unsafe_blocks`, `unchecked_calls`, `assertions` INTEGER — complexity metrics
- `updated_at` INTEGER — UNIX epoch seconds

Indexes: `kind`, `name`, `qualified_name`, `file_path`, `(file_path,start_line)`, `lower(name)`.

### `edges` — directed relationships between nodes
- `id` INTEGER PRIMARY KEY AUTOINCREMENT
- `source` TEXT — FK → `nodes.id` (CASCADE DELETE)
- `target` TEXT — FK → `nodes.id` (CASCADE DELETE)
- `kind` TEXT — one of `contains`, `calls`, `returns`, `type_of`, `uses`, `implements`, `extends`, `annotates`, `derives_macro`, `receives`
- `line` INTEGER NULL — source line of the relationship

Unique constraint: `(source, target, kind, COALESCE(line, -1))`. Indexes on `source`, `target`, `kind`, `(source,kind)`, `(target,kind)`.

### `files` — index bookkeeping
- `path` TEXT PRIMARY KEY
- `content_hash` TEXT — sha256 of file contents at index time
- `size` INTEGER — file size in bytes
- `modified_at`, `indexed_at` INTEGER — UNIX epoch seconds
- `node_count` INTEGER — number of nodes extracted from this file

### `unresolved_refs` — references the resolver could not bind
- `from_node_id` FK → `nodes.id`
- `reference_name` TEXT
- `reference_kind` TEXT
- `line`, `col` INTEGER
- `file_path` TEXT

### `metadata` — key/value store
Common keys: `tokens_saved`, schema-version markers.

### `node_fingerprints` — redundancy cache
- `node_id` PRIMARY KEY FK → `nodes.id`
- `ast_hash`, `cfg_hash`, `call_seq_hash`, `shingles`
- `body_tokens`, `source_hash`

### `read_cache` — rendered `tracedecay_read` responses
- primary key: `(project_id, session_id, file_path, mode, args_hash)`
- stores `mtime_ns`, `digest`, rendered `body` BLOB, token count, and `created_at`

## Recipes

### Find every impl block of a trait
```sql
SELECT n.id, n.qualified_name, n.file_path, n.start_line
FROM nodes n
JOIN edges e ON e.source = n.id
WHERE e.kind = 'implements'
  AND e.target IN (SELECT id FROM nodes WHERE qualified_name = ?1);
```

### Top callers of a node
```sql
SELECT n.qualified_name, COUNT(*) AS call_count
FROM edges e
JOIN nodes n ON n.id = e.source
WHERE e.target = ?1 AND e.kind = 'calls'
GROUP BY n.qualified_name
ORDER BY call_count DESC
LIMIT 20;
```

### Files modified since last index
Compare `files.modified_at` against the live filesystem mtime — `tracedecay_affected` does this with extra git plumbing.

### Largest functions by line span
```sql
SELECT qualified_name, file_path, end_line - start_line + 1 AS lines
FROM nodes
WHERE kind IN ('function', 'method')
ORDER BY lines DESC
LIMIT 20;
```

## Gotchas
- `nodes.id` is a content hash, so it changes when the symbol moves. For cross-run lookups use `qualified_name` (or `tracedecay_by_qualified_name`).
- `edges.kind = 'calls'` may reference a *trait method* node rather than the resolved concrete impl — trait dispatch is not currently rewritten.
- `derives_macro` edges record `#[derive(...)]` usage but generated impls are not in the graph.
";

impl McpServer {
    pub(crate) fn cancel_application_surface_request(
        &self,
        id: &Value,
        connection_scope: &str,
    ) -> bool {
        let Some(cancelled_id) = application_surface_request_id(id, connection_scope) else {
            return false;
        };
        recover_lock(self.dispatch_authority.cancellations())
            .get(&cancelled_id)
            .cloned()
            .is_some_and(|cancellation| cancellation.cancel(mcp_now_micros()))
    }

    /// Dispatches a parsed JSON-RPC request to the appropriate handler.
    ///
    /// Returns `None` for notifications (requests without an `id`).
    pub(crate) async fn handle_request(&self, request: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        // The initialize-replay entry point builds its own per-connection
        // context so replay dispatches carry a real memory-request scope,
        // exactly like the live connection loop. These callers never dispatch
        // memory tools, but the scope is unconditionally present rather than an
        // absent-scope special case.
        let mut connection = match self.new_connection_route_state() {
            Ok(connection) => connection,
            Err(error) => {
                return request
                    .id
                    .clone()
                    .map(|id| tool_error_response(id, &request.method, &error));
            }
        };
        Box::pin(self.handle_request_for_connection(
            request,
            self.timings_enabled(),
            &mut connection,
            false,
        ))
        .await
    }

    /// Builds a fresh per-connection routing/identity context. Each call
    /// allocates a new connection scope sequence, so the derived
    /// `{mcp_instance_id}-c{seq}` prefix is unique across connections and
    /// initialize-replay dispatches alike.
    pub(crate) fn new_connection_route_state(&self) -> Result<ConnectionRouteState> {
        // One request-correlation scope per client connection: envelope ids
        // are client-chosen and connection-local, so the daemon widens them
        // before they become persisted application request identities.
        let memory_request_scope = self
            .connection_identity
            .establish_connection_scope()
            .map_err(|error| TraceDecayError::Config {
                message: format!("MCP connection identity unavailable: {error}"),
            })?;
        Ok(ConnectionRouteState::new(
            memory_request_scope,
            self.hook_project_routes.snapshot()?,
        ))
    }

    fn record_request_accounting(&self, method: &str, response_is_error: bool) {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        *recover_lock(&self.method_call_counts)
            .entry(method.to_owned())
            .or_insert(0) += 1;
        if response_is_error {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) async fn handle_request_for_connection(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        pre_cancelled: bool,
    ) -> Option<JsonRpcResponse> {
        // A response lease belongs to exactly one request. Production
        // transports take it before writing; direct callers drop it with this
        // connection state after observing the returned response.
        connection.clear_selected_response_lease();
        connection.clear_selected_request_server();
        if matches!(classify_mcp_method(&request.method), McpMethod::HookEvent) {
            Box::pin(self.handle_hook_event_notification(
                request.params.as_ref(),
                &mut connection.route_cache,
            ))
            .await;
            return None;
        }
        if matches!(classify_mcp_method(&request.method), McpMethod::Cancelled) {
            self.record_request_accounting(&request.method, false);
            if let Some(cancelled_id) = request
                .params
                .as_ref()
                .and_then(|params| params.get("requestId"))
            {
                let _ = self.cancel_application_surface_request(
                    cancelled_id,
                    connection.memory_request_scope(),
                );
            }
            return None;
        }
        if let Err(error) = self
            .hook_project_routes
            .refresh_into(&mut connection.route_cache)
        {
            self.record_request_accounting(&request.method, true);
            return request
                .id
                .clone()
                .map(|id| tool_error_response(id, &request.method, &error));
        }
        let method = classify_mcp_method(&request.method);
        if matches!(&method, McpMethod::Initialize)
            && self.initialize_root_routing_enabled.load(Ordering::Relaxed)
        {
            connection
                .observe_initialize(
                    request.params.as_ref(),
                    self.registry_db.as_deref(),
                    self.retained_project_server_resolver.clone(),
                )
                .await;
        }
        let Some(id) = request.id.clone() else {
            self.record_request_accounting(&request.method, false);
            return None;
        };

        let result = match method {
            McpMethod::Initialize => Some(self.handle_initialize(id, request.params.as_ref())),
            // Some clients send the initialized notification with an id (or
            // via the alternate method name); both stay compatibility no-ops.
            // Hook events were consumed by the early notification dispatch
            // above and can never reach this match with a response due.
            McpMethod::InitializedAck | McpMethod::HookEvent | McpMethod::Cancelled => None,
            McpMethod::ToolsList => Some(self.handle_tools_list(id).await),
            McpMethod::ToolsCall => Some(
                Box::pin(self.handle_tools_call(
                    id,
                    request.params.as_ref(),
                    timings_enabled,
                    connection,
                    pre_cancelled,
                ))
                .await,
            ),
            McpMethod::ResourcesList => Some(Self::handle_resources_list(id)),
            McpMethod::ResourcesRead => Some(
                self.handle_resources_read(id, request.params.as_ref())
                    .await,
            ),
            McpMethod::TrivialAck => Some(JsonRpcResponse::success(id, json!({}))),
            McpMethod::Unknown => Some(JsonRpcResponse::error(
                id,
                ErrorCode::MethodNotFound,
                format!("method not found: {}", request.method),
            )),
        };

        let response_is_error = result
            .as_ref()
            .is_some_and(|response| response.error.is_some());
        if let Some(server) = connection.take_selected_request_server() {
            server.record_request_accounting(&request.method, response_is_error);
        } else {
            self.record_request_accounting(&request.method, response_is_error);
        }

        result
    }

    pub(crate) async fn handle_hook_event_notification(
        &self,
        params: Option<&Value>,
        route_cache: &mut HookProjectRouteCache,
    ) -> HostAdmissionOutcome {
        let Some(event) = hook_events::parse_hook_event(params) else {
            self.record_request_accounting("tracedecay/hookEvent", false);
            let outcome = HostAdmissionOutcome::degraded("malformed_event");
            Self::report_host_admission_outcome(outcome);
            return outcome;
        };
        // Resolve the hook's exact project before opening a graph, publishing
        // activity, touching an indexing sink, or admitting durable work. The
        // connection server owns only transport/cache state; the selected
        // retained server owns every project-scoped authority below.
        let hook_route = match self.update_hook_workspace_route(&event, route_cache).await {
            Ok(route) => route,
            Err(error) => {
                self.record_request_accounting("tracedecay/hookEvent", false);
                tracing::warn!(error = %error, "hook project registry route resolution failed");
                let outcome = HostAdmissionOutcome::retained_unavailable(
                    "project_registry_route_unavailable",
                );
                Self::report_host_admission_outcome(outcome);
                return outcome;
            }
        };
        let dispatch_server = match hook_route.retained_server() {
            Ok(server)
                if server.project_route_live() != Some(false)
                    && !server
                        .project_server_lifecycle
                        .response_revoked()
                        .is_cancelled() =>
            {
                server
            }
            Ok(_) | Err(_) => {
                self.record_request_accounting("tracedecay/hookEvent", false);
                let outcome =
                    HostAdmissionOutcome::retained_unavailable("project_route_unavailable");
                Self::report_host_admission_outcome(outcome);
                return outcome;
            }
        };
        dispatch_server.record_request_accounting("tracedecay/hookEvent", false);
        // R4: one branch resolution for this notification — the drift check
        // below and the hook-plan branch label both read it.
        let (cg, live_branch) = dispatch_server.reopen_if_branch_drifted_memoized().await;
        let root = cg.project_root().to_path_buf();
        // Live-activity tap: a host hook arriving here IS an agent working in
        // this project, so publish it at the observation point carrying this
        // project's own registered id. The application lane retains it even
        // without a connected dashboard; the SSE adapter coalesces the burst.
        let activity_project_id =
            tracedecay_usecases::event_lane::enabled(dispatch_server.session_db.as_deref())
                .then(|| cg.store_layout().identity.project_id.clone())
                .flatten();
        if let Some(activity_db) = dispatch_server.session_db.as_deref() {
            tracedecay_usecases::event_lane::publish(
                activity_db,
                tracedecay_usecases::event_lane::ActivityFamilyV1::Hook,
                &root,
                activity_project_id.as_deref(),
                1,
                Some(event.kind.as_key()),
            )
            .await;
        }
        // Primary incremental-index hint: deliver the exact touched paths into
        // the daemon-owned code-index scheduler queue as soon as the routing
        // event is observed. Independent of host-admission durability so an
        // after-edit reaches indexing even when effect processing is deferred.
        // Best-effort: a `false` return (no mounted worktree) is not an error.
        if !event.rel_paths.is_empty()
            && let Some(sink) = &dispatch_server.code_index_hook_sink
        {
            // A `true` return means the paths really entered a mounted
            // worktree's incremental queue — the exact moment indexing work is
            // created for this project, and the only condition worth lighting.
            if sink(root.clone(), event.rel_paths.clone()).await
                && let Some(activity_db) = dispatch_server.session_db.as_deref()
            {
                tracedecay_usecases::event_lane::publish(
                    activity_db,
                    tracedecay_usecases::event_lane::ActivityFamilyV1::CodeIndex,
                    &root,
                    activity_project_id.as_deref(),
                    event.rel_paths.len() as u64,
                    Some(event.kind.as_key()),
                )
                .await;
            }
        }
        let current_branch = live_branch.resolve_for(&root);
        let plan = hook_events::plan_hook_event(&event, &root, current_branch.as_deref());
        let Ok(payload) = hook_events::encode_durable_hook_event_plan(&plan) else {
            let outcome = HostAdmissionOutcome::degraded("invalid_host_event_plan");
            Self::report_host_admission_outcome(outcome);
            return outcome;
        };
        let admission_source = event.admission_source();
        let admitted = match dispatch_server.host_admission_broker.as_ref() {
            Some(broker) => broker.admit(&admission_source, &payload).await,
            None => Err(HostAdmissionOutcome::retained_unavailable(
                "spool_unavailable",
            )),
        };
        let admitted = match admitted {
            Ok(admitted) => admitted,
            Err(outcome) => {
                Self::report_host_admission_outcome(outcome);
                return outcome;
            }
        };
        let outcome = Box::pin(dispatch_server.replay_host_admission(Some(admitted.seq))).await;
        // Route analytics + span observations are side writes: only after the
        // durable spool record is authoritatively committed (Committed or
        // ExactDuplicate). Failures are best-effort and must never change the
        // admission outcome already decided above.
        if matches!(
            outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ) {
            if let Some(wake) = &dispatch_server.project_session_refresh_wake {
                wake.wake();
            }
            if let Some(wake) = &dispatch_server.user_session_refresh_wake {
                wake.wake();
            }
            dispatch_server.record_hook_route_analytics(
                &root,
                &event,
                current_branch.as_deref(),
                admitted.seq,
            );
            dispatch_server
                .record_hook_span_observation(&event, &hook_route)
                .await;
        }
        Self::report_host_admission_outcome(outcome);
        outcome
    }

    /// Handles the `initialize` method, returning server capabilities.
    ///
    /// Also records the caller's negotiated `clientInfo.name` (e.g.
    /// `"claude-code"`, `"codex"`, `"cursor"`) so subsequent `tools/call`
    /// analytics events can attribute per-host adoption instead of every
    /// call recording the same opaque `provider="mcp"`. Only the short
    /// name field is retained — never the full `clientInfo` payload.
    pub(crate) fn handle_initialize(&self, id: Value, params: Option<&Value>) -> JsonRpcResponse {
        let client_name = params
            .and_then(|p| p.get("clientInfo"))
            .and_then(|ci| ci.get("name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if client_name.is_some() {
            *recover_lock(&self.client_name) = client_name;
        }
        JsonRpcResponse::success(id, initialize_result(SERVER_INSTRUCTIONS))
    }

    /// The negotiated MCP client name recorded by the most recent
    /// `initialize` handshake, if any (see [`Self::client_name`] field doc).
    pub(crate) fn client_name(&self) -> Option<String> {
        recover_lock(&self.client_name).clone()
    }

    /// Handles the `tools/list` method, returning all available tool definitions.
    pub(crate) async fn handle_tools_list(&self, id: Value) -> JsonRpcResponse {
        let budget = explore_call_budget(0);
        let profile_id = match tracedecay_tool_catalog::ProfileId::new(
            tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
        ) {
            Ok(profile_id) => profile_id,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    format!("invalid MCP discovery profile: {error}"),
                );
            }
        };
        let authority = match default_catalog_discovery_authority() {
            Ok(authority) => authority,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    format!("MCP catalog discovery unavailable: {error}"),
                );
            }
        };
        match crate::mcp::tools::get_catalog_filtered_tool_definitions_with_warming_budget(
            budget,
            &profile_id,
            &authority,
            &project_catalog_discovery_scope(),
            ToolRegistryMode::HostAvailable,
        ) {
            Ok(tools) => JsonRpcResponse::success(id, json!({ "tools": tools })),
            Err(error) => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                format!("MCP catalog discovery unavailable: {error}"),
            ),
        }
    }

    /// Handles the `resources/list` method, returning available resources.
    pub(crate) fn handle_resources_list(id: Value) -> JsonRpcResponse {
        JsonRpcResponse::success(id, resources_list_result())
    }

    /// Handles the `resources/read` method, returning resource contents.
    pub(crate) async fn handle_resources_read(
        &self,
        id: Value,
        params: Option<&Value>,
    ) -> JsonRpcResponse {
        let uri = params.and_then(|p| p.get("uri")).and_then(|v| v.as_str());

        let Some(uri) = uri else {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "missing 'uri' in resources/read params".to_string(),
            );
        };
        *recover_lock(&self.resource_read_counts)
            .entry(uri.to_string())
            .or_insert(0) += 1;

        match uri {
            "tracedecay://status" => self.read_resource_status(id).await,
            "tracedecay://files" => self.read_resource_files(id).await,
            "tracedecay://overview" => self.read_resource_overview(id).await,
            "tracedecay://branches" => self.read_resource_branches(id).await,
            "tracedecay://schema" => Self::read_resource_schema(id),
            _ => JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                format!("unknown resource URI: {uri}"),
            ),
        }
    }

    /// Wraps a single resource body in the `resources/read` contents envelope.
    pub(super) fn resource_contents(
        id: Value,
        uri: &str,
        mime: &str,
        text: &str,
    ) -> JsonRpcResponse {
        JsonRpcResponse::success(
            id,
            json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": mime,
                    "text": text
                }]
            }),
        )
    }

    /// Returns the `SQLite` schema documentation as a markdown resource.
    /// Sourced from `src/db/migrations.rs::create_schema` — keep in sync.
    pub(crate) fn read_resource_schema(id: Value) -> JsonRpcResponse {
        Self::resource_contents(id, "tracedecay://schema", "text/markdown", SCHEMA_MARKDOWN)
    }

    /// Returns typed file-inventory availability for the active project.
    ///
    /// MCP resources do not carry the application operation, request identity,
    /// deadline, and cancellation proof required to open the verified code
    /// graph. Until that resource-specific admission exists, exposing files
    /// from another store would make an unverified or stale inventory look
    /// authoritative.
    pub(crate) async fn read_resource_files(&self, id: Value) -> JsonRpcResponse {
        Self::resource_contents(
            id,
            "tracedecay://files",
            "text/plain",
            "status: unavailable\nreason: verified_generation_file_inventory_not_admitted",
        )
    }

    /// Returns a high-level project overview as a text resource.
    pub(crate) async fn read_resource_overview(&self, id: Value) -> JsonRpcResponse {
        let cg = self.cg_snapshot().await;
        let mut lines = Vec::new();
        lines.push(format!("Project: {}", cg.project_root().display()));
        lines.push(
            "Graph statistics: unavailable (sealed generation statistics are not published)"
                .to_owned(),
        );

        let text = lines.join("\n");
        Self::resource_contents(id, "tracedecay://overview", "text/plain", &text)
    }

    pub(crate) async fn read_resource_branches(&self, id: Value) -> JsonRpcResponse {
        let cg = self.cg_snapshot().await;
        let tracedecay_dir = &cg.store_layout().data_root;
        let current = cg.active_branch();

        let branches: Vec<Value> = match crate::branch_meta::load_branch_meta(tracedecay_dir) {
            Some(meta) => meta
                .branches
                .iter()
                .map(|(name, entry)| {
                    let db_path = tracedecay_dir.join(&entry.db_file);
                    let size_bytes = db_path.metadata().map_or(0, |m| m.len());
                    json!({
                        "name": name,
                        "db_file": entry.db_file,
                        "parent": entry.parent,
                        "size_bytes": size_bytes,
                        "last_synced_at": entry.last_synced_at,
                        "is_current": current == Some(name.as_str()),
                        "is_default": name == &meta.default_branch,
                    })
                })
                .collect(),
            None => vec![],
        };

        let output = json!({
            "branch_count": branches.len(),
            "branches": branches,
        });
        let text = serde_json::to_string_pretty(&output).unwrap_or_default();
        Self::resource_contents(id, "tracedecay://branches", "application/json", &text)
    }

    #[allow(clippy::result_large_err)]
    fn prepare_tool_call(
        id: &Value,
        params: Option<&Value>,
    ) -> std::result::Result<PreparedToolCall, JsonRpcResponse> {
        let Some(params) = params else {
            return Err(JsonRpcResponse::error(
                id.clone(),
                ErrorCode::InvalidParams,
                "missing params for tools/call".to_string(),
            ));
        };

        let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
            return Err(JsonRpcResponse::error(
                id.clone(),
                ErrorCode::InvalidParams,
                "missing 'name' in tools/call params".to_string(),
            ));
        };

        let mut arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        if crate::mcp::project_route::protect_tool_structural_ids(&mut arguments).is_err() {
            return Err(JsonRpcResponse::error(
                id.clone(),
                ErrorCode::InvalidParams,
                "invalid structural identifier".to_string(),
            ));
        }

        Ok(PreparedToolCall {
            tool_name: tool_name.to_string(),
            analytics_arguments: arguments.clone(),
            analytics_session_id: mcp_analytics_session_id(&arguments),
            arguments,
        })
    }

    /// Applies the pre-dispatch freshness policy and records the call in the
    /// server counters and the activity lane.
    async fn begin_tool_dispatch(
        &self,
        tool_name: &str,
        cg: &Arc<TraceDecay>,
        live_branch: &crate::branch::BranchMemo,
        project_reader_preselected: bool,
        publish_activity: bool,
    ) {
        // Notification-free freshness is useful before tools that edit source
        // files in the index. Read-only graph queries should not block behind
        // a full project walk; on very large indexes (especially when
        // node_modules was intentionally included) that turns diagnostics and
        // search into sync operations.
        if !project_reader_preselected && needs_lazy_sync_before_dispatch(tool_name) {
            self.maybe_sync_if_stale().await;
        } else if !project_reader_preselected {
            // D4: sync-on-read (never blocking). Read tools serve the current
            // answer IMMEDIATELY and, when the read-refresh cooldown has
            // elapsed, kick a single-flighted background refresh so the *next*
            // read sees fresh data. This heals read-only sessions that never
            // touch an edit tool without ever making a query wait behind a
            // project walk.
            self.maybe_spawn_read_refresh(cg, live_branch);
        }

        self.stats.tool_calls.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(tool_name, "dispatching MCP tool call");
        *recover_lock(&self.tool_call_counts)
            .entry(tool_name.to_string())
            .or_insert(0) += 1;
        if publish_activity {
            self.publish_tool_call_activity(tool_name, cg).await;
        }
    }

    /// Prepare the application-surface plumbing for a single dispatch. Returns
    /// the typed request id, deadline, cancellation, and daemon invocation
    /// executor, plus the RAII registration guard that must outlive the dispatch.
    async fn prepare_application_surface_dispatch<'a>(
        &'a self,
        cg: &TraceDecay,
        tool_name: &str,
    ) -> ApplicationSurfaceDispatch<'a> {
        let invocation_executor = if requires_application_invocation_executor(tool_name) {
            match self.application_invocation_executor.as_deref() {
                Some(executor) => Some(executor),
                None => self
                    .application_surface_client
                    .get_or_try_init(|| async {
                        let handshake = crate::daemon::DaemonHandshake::for_current_client(
                            Some(cg.project_root().to_path_buf()),
                            self.scope_prefix.clone(),
                            false,
                            false,
                        )?;
                        crate::daemon_client::DaemonInvocationClient::for_current(handshake)
                    })
                    .await
                    .ok()
                    .map(|client| client as &dyn crate::daemon_client::DaemonInvocationExecutor),
            }
        } else {
            None
        };
        ApplicationSurfaceDispatch {
            invocation_executor,
        }
    }

    fn attach_tool_timing(result: &mut ToolResult, elapsed_us: Option<u64>) {
        if let Some(us) = elapsed_us
            && let Some(map) = result.value.as_object_mut()
        {
            let meta = map.entry("_meta").or_insert_with(|| json!({}));
            if let Some(meta_obj) = meta.as_object_mut() {
                meta_obj.insert("duration_us".to_string(), json!(us));
            }
        }
    }

    fn response_token_count(result: &ToolResult) -> u64 {
        result
            .value
            .get("content")
            .and_then(|content| content.as_array())
            .map_or(0, |content| {
                let total_chars: usize = content
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
                    .map(str::len)
                    .sum();
                (total_chars / 4) as u64
            })
    }

    async fn apply_token_accounting(
        &self,
        cg: &TraceDecay,
        tool_name: &str,
        result: &mut ToolResult,
    ) -> ToolTokenAccounting {
        // Estimate approximate token count of the graph response
        // ("after"), before any banners/metrics lines are appended.
        let response_tokens = Self::response_token_count(result);
        // "Before" counterfactual: reading every referenced file raw,
        // in full. Counters credit only the net saving per call —
        // before minus what this response actually delivered.
        let raw_file_tokens = self.estimate_raw_file_tokens(&result.touched_files);
        let net_saved_tokens = raw_file_tokens.saturating_sub(response_tokens);
        self.persist_saved_tokens(net_saved_tokens).await;
        crate::monitor::write_entry(
            cg.project_root(),
            "tracedecay",
            tool_name,
            net_saved_tokens,
            raw_file_tokens,
        );
        self.maybe_flush_worldwide().await;

        // Append per-call token savings to the response content.
        if raw_file_tokens > 0
            && let Some(content) = result
                .value
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
        {
            content.push(json!({"type": "text", "text": format!(
                "\ntracedecay_metrics: before={raw_file_tokens} after={response_tokens}"
            )}));
        }

        ToolTokenAccounting {
            raw_file_tokens,
            response_tokens,
            net_saved_tokens,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_success_analytics(
        &self,
        accounting_project_root: &Path,
        tool_name: &str,
        request_id: &Value,
        analytics_arguments: &Value,
        analytics_session_id: &Option<String>,
        elapsed_us: Option<u64>,
        accounting: ToolTokenAccounting,
        result: &ToolResult,
        connection_client_name: Option<&str>,
        connection_instance_id: Option<&str>,
    ) {
        let analytics_outcome = if tool_result_has_semantic_error(result) {
            "error"
        } else {
            "success"
        };

        // Persist to the cross-project savings ledger (best-effort, non-blocking).
        // Clone the Arc — no new connection is opened. The counters
        // and notify make the write's completion observable to
        // [`Self::ledger_writes_settled`] without making it awaited
        // anywhere on the request path.
        if let Some(registered) = self.accounting_db.clone() {
            let ToolTokenAccounting {
                raw_file_tokens,
                response_tokens,
                net_saved_tokens,
            } = accounting;
            let project_path_str =
                RegisteredGlobalDb::canonical_project_key(accounting_project_root);
            let tool_name_owned = tool_name.to_string();
            let ts = crate::tracedecay::current_timestamp();
            let failure_reason = (analytics_outcome == "error")
                .then(|| semantic_failure_reason(result))
                .flatten();
            let analytics_event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
                project_root: accounting_project_root,
                session_id: analytics_session_id.clone(),
                tool_name,
                outcome: analytics_outcome,
                raw_file_tokens,
                response_tokens,
                net_saved_tokens,
                duration_us: elapsed_us,
                timestamp: ts,
                request_id,
                arguments: analytics_arguments,
                internal_analytics: result.internal_analytics(),
                client_name: connection_client_name,
                mcp_instance_id: connection_instance_id,
                failure_reason: failure_reason.as_deref(),
            });
            self.spawn_observed_ledger_write(async move {
                registered
                    .record_savings(
                        &project_path_str,
                        &tool_name_owned,
                        raw_file_tokens,
                        response_tokens,
                        ts,
                    )
                    .await;
                if let Err(e) = registered.append_analytics_event(&analytics_event).await {
                    tracing::warn!(error = %e, "MCP analytics event insert failed");
                }
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_success_accounting(
        &self,
        cg: &TraceDecay,
        accounting_project_root: &Path,
        tool_name: &str,
        request_id: &Value,
        analytics_arguments: &Value,
        analytics_session_id: &Option<String>,
        elapsed_us: Option<u64>,
        result: &mut ToolResult,
        connection_client_name: Option<&str>,
        connection_instance_id: Option<&str>,
    ) {
        let accounting = self.apply_token_accounting(cg, tool_name, result).await;
        self.spawn_success_analytics(
            accounting_project_root,
            tool_name,
            request_id,
            analytics_arguments,
            analytics_session_id,
            elapsed_us,
            accounting,
            result,
            connection_client_name,
            connection_instance_id,
        );
    }

    async fn append_version_notice(
        &self,
        result: &mut ToolResult,
        connection_notifications: &std::sync::Mutex<Vec<Value>>,
    ) {
        // Prepend the version-update warning and queue the corresponding
        // protocol notification.
        if let Some(warning) = self.check_version_update().await {
            if let Some(content) = result
                .value
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
            {
                content.insert(0, json!({"type": "text", "text": &warning}));
            }
            recover_lock(connection_notifications).push(json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {
                    "level": "warning",
                    "logger": "tracedecay",
                    "data": warning
                }
            }));
        }
    }

    async fn prepend_index_warnings(
        &self,
        include_connection_worktree_warning: bool,
        result: &mut ToolResult,
    ) {
        // Borrowed-worktree heads-up (#312). Inserted LAST so it
        // appears FIRST in the response — the index serving the
        // wrong branch is the most serious of these warnings to
        // surface to the agent.
        if include_connection_worktree_warning && let Some(ref m) = self.worktree_mismatch {
            let notice = crate::worktree::worktree_mismatch_notice(m);
            if let Some(content) = result
                .value
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
            {
                content.insert(0, json!({"type": "text", "text": notice}));
            }
        }
    }

    async fn complete_tool_call(
        &self,
        id: Value,
        tool_name: String,
        analytics_arguments: Value,
        analytics_session_id: Option<String>,
        dispatch: DispatchedToolCall,
        connection_client_name: Option<&str>,
        connection_instance_id: Option<&str>,
        connection_notifications: &std::sync::Mutex<Vec<Value>>,
    ) -> JsonRpcResponse {
        let DispatchedToolCall {
            cg,
            selected_owner,
            selected_scope,
            outcome,
            elapsed_us,
        } = dispatch;
        let request_id = id.clone();

        match outcome {
            Ok(mut result) => {
                Self::attach_tool_timing(&mut result, elapsed_us);
                mark_semantic_tool_error(&mut result);
                if !tool_result_has_semantic_error(&result)
                    && let Err(error) =
                        super::live_transcript_refresh::join_required_live_transcript_refresh(
                            &tool_name,
                            &analytics_arguments,
                            selected_owner.is_some(),
                            self.project_session_refresh_wake.as_ref(),
                            self.user_session_refresh_wake.as_ref(),
                        )
                        .await
                {
                    self.record_mcp_tool_error_analytics(McpToolErrorAnalyticsRequest {
                        project_root: cg.project_root(),
                        session_id: analytics_session_id,
                        tool_name: &tool_name,
                        request_id: &request_id,
                        arguments: &analytics_arguments,
                        duration_us: elapsed_us,
                        error: &error,
                        connection_client_name,
                        connection_instance_id,
                    });
                    return tool_error_response(id, &tool_name, &error);
                }
                let accounting_project_root = accounting_project_root(
                    cg.project_root(),
                    selected_owner.as_ref(),
                    selected_scope.as_ref(),
                );
                if let Some(accounting_project_root) = accounting_project_root {
                    self.record_success_accounting(
                        &cg,
                        accounting_project_root,
                        &tool_name,
                        &request_id,
                        &analytics_arguments,
                        &analytics_session_id,
                        elapsed_us,
                        &mut result,
                        connection_client_name,
                        connection_instance_id,
                    )
                    .await;
                }
                self.append_version_notice(&mut result, connection_notifications)
                    .await;
                self.prepend_index_warnings(selected_owner.is_none(), &mut result)
                    .await;
                JsonRpcResponse::success(id, result.value)
            }
            Err(error) => {
                self.record_mcp_tool_error_analytics(McpToolErrorAnalyticsRequest {
                    project_root: cg.project_root(),
                    session_id: analytics_session_id,
                    tool_name: &tool_name,
                    request_id: &request_id,
                    arguments: &analytics_arguments,
                    duration_us: elapsed_us,
                    error: &error,
                    connection_client_name,
                    connection_instance_id,
                });
                tool_error_response(id, &tool_name, &error)
            }
        }
    }

    async fn publish_tool_call_activity(&self, tool_name: &str, cg: &TraceDecay) {
        if !tracedecay_usecases::event_lane::enabled(self.session_db.as_deref()) {
            return;
        }
        let Some(activity_db) = self.session_db.as_deref() else {
            return;
        };
        tracedecay_usecases::event_lane::publish(
            activity_db,
            tracedecay_usecases::event_lane::ActivityFamilyV1::ToolCall,
            cg.project_root(),
            cg.store_layout().identity.project_id.as_deref(),
            1,
            Some(tool_name),
        )
        .await;
    }

    fn message_search_worker_is_unavailable(&self, tool_name: &str, arguments: &Value) -> bool {
        if tool_name != "tracedecay_message_search"
            || arguments.get("catch_up").and_then(Value::as_bool) != Some(true)
        {
            return false;
        }
        let user_scope = arguments.get("storage_scope").and_then(Value::as_str) == Some("user");
        let wake = if user_scope {
            self.user_session_refresh_wake.as_ref()
        } else {
            self.project_session_refresh_wake.as_ref()
        };
        wake.is_some_and(|wake| wake.status().unavailable_reason.is_some())
    }

    /// Finishes a dispatch that bypassed success accounting because the backing
    /// worker was already known to be unavailable.
    fn finish_unavailable_tool_call(
        id: Value,
        tool_name: &str,
        dispatch: DispatchedToolCall,
    ) -> JsonRpcResponse {
        let DispatchedToolCall {
            outcome,
            elapsed_us,
            ..
        } = dispatch;
        match outcome {
            Ok(mut result) => {
                Self::attach_tool_timing(&mut result, elapsed_us);
                mark_semantic_tool_error(&mut result);
                JsonRpcResponse::success(id, result.value)
            }
            Err(error) => tool_error_response(id, tool_name, &error),
        }
    }

    pub(super) fn project_server_revoked_response(
        &self,
        id: &Value,
        tool_name: &str,
    ) -> Option<JsonRpcResponse> {
        let (reason_code, detail) = if self
            .project_server_lifecycle
            .response_revoked()
            .is_cancelled()
        {
            (
                "project_server_response_revoked",
                "the retained project server was retired before response completion",
            )
        } else if self
            .project_server_live
            .as_ref()
            .is_some_and(|live| !live.load(Ordering::Acquire))
        {
            (
                "project_server_health_revoked",
                "the retained project server failed post-open health validation; retry against a recovered owner",
            )
        } else {
            return None;
        };
        Some(JsonRpcResponse::error_with_data(
            id.clone(),
            ErrorCode::InternalError,
            format!("tool project route failed: {detail}"),
            Some(json!({
                "tool": tool_name,
                "reason_code": reason_code,
                "retryable": true,
                "detail": detail,
            })),
        ))
    }

    /// Handles the `tools/call` method, dispatching to the appropriate tool handler.
    pub(crate) async fn handle_tools_call(
        &self,
        id: Value,
        params: Option<&Value>,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        pre_cancelled: bool,
    ) -> JsonRpcResponse {
        let PreparedToolCall {
            tool_name,
            arguments,
            analytics_arguments,
            analytics_session_id,
        } = match Self::prepare_tool_call(&id, params) {
            Ok(call) => call,
            Err(response) => return response,
        };
        let memory_request_scope = connection.memory_request_scope().to_owned();
        // Resolve the exact execution server before creating cancellation,
        // deadline, settlement, or accounting state. A failed/ambiguous route
        // therefore cannot leave request authority on the active server.
        let routed = match self
            .route_tool_arguments(
                &id,
                &tool_name,
                arguments,
                &connection.route_cache,
                connection.initialize_route(),
                &memory_request_scope,
            )
            .await
        {
            Ok(routed) => routed,
            Err(error) => return tool_error_response(id, &tool_name, &error),
        };
        let dispatch_server = match routed.selected_server.as_ref() {
            Some(selected) => Arc::clone(selected),
            None => match self.dispatch_authority.server().upgrade() {
                Some(server) => server,
                None => {
                    let error = TraceDecayError::project_route(
                        "tool_dispatch_shutdown",
                        true,
                        "MCP server was released before retained dispatch admission",
                    );
                    return tool_error_response(id, &tool_name, &error);
                }
            },
        };
        connection.install_selected_request_server(Arc::clone(&dispatch_server));
        if let Some(response) = dispatch_server.project_server_revoked_response(&id, &tool_name) {
            return response;
        }

        // Transport cancellation is owned by the connection server, then the
        // same signal is mirrored into the already-selected target below.
        let PreparedDispatchControl {
            request_id: application_request_id,
            control,
            _registration,
        } = match self.prepare_dispatch_control(
            &id,
            &tool_name,
            &memory_request_scope,
            pre_cancelled,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return tool_error_response(id, &tool_name, &error),
        };

        // Acquire exactly one response lease from the execution server. The
        // connection carries it through emission; the caller server owns only
        // transport cancellation, so cross-project calls never nest project
        // lifecycle locks or fall back to the caller's response authority.
        let response_lifecycle = dispatch_server.project_server_lifecycle.clone();
        let response_gate = Arc::clone(response_lifecycle.response_gate());
        let response_guard = response_gate.read_owned().await;
        if response_lifecycle.response_revoked().is_cancelled() {
            return dispatch_server
                .project_server_revoked_response(&id, &tool_name)
                .unwrap_or_else(|| {
                    let error = TraceDecayError::project_route(
                        "project_server_response_revoked",
                        true,
                        "the retained project server was retired before response admission",
                    );
                    tool_error_response(id, &tool_name, &error)
                });
        }
        connection.install_selected_response_lease(
            super::routing::SelectedProjectResponseLease::new(
                response_guard,
                response_lifecycle.response_revoked().clone(),
            ),
        );

        let fast_unavailable =
            dispatch_server.message_search_worker_is_unavailable(&tool_name, &routed.arguments);
        let target_request_id = application_request_id
            .as_ref()
            .filter(|_| {
                !std::ptr::eq(self, dispatch_server.as_ref())
                    && tool_supports_live_cancellation(&tool_name)
            })
            .map(|request_id| request_id.as_str().to_owned());
        // The connection and target registries share one cancellation signal:
        // transport teardown reaches the selected worker, while target
        // shutdown still owns and joins its admitted task.
        if let Some(request_id) = target_request_id.as_ref() {
            recover_lock(dispatch_server.dispatch_authority.cancellations())
                .insert(request_id.clone(), control.cancellation());
        }
        let _target_cancellation_registration = ApplicationCancellationRegistration::new(
            dispatch_server.dispatch_authority.cancellations(),
            target_request_id,
        );
        let worker_server = dispatch_server.dispatch_authority.server();
        let worker_tool_name = tool_name.clone();
        let worker_control = control.clone();
        let retained = control
            .run_retained(dispatch_server.dispatch_authority.registry(), async move {
                let server = worker_server.upgrade().ok_or_else(|| {
                    TraceDecayError::project_route(
                        "tool_dispatch_shutdown",
                        true,
                        "MCP server was released before retained dispatch admission",
                    )
                })?;
                Ok(server
                    .dispatch_routed_tool_call(
                        &worker_tool_name,
                        routed,
                        timings_enabled,
                        !fast_unavailable,
                        application_request_id,
                        worker_control,
                    )
                    .await)
            })
            .await;
        tracing::trace!(
            tool_name,
            settlement = ?retained.settlement(),
            "MCP tool dispatch settled"
        );
        let dispatch = match retained.result {
            Ok(dispatch) => dispatch,
            Err(failure) => {
                connection.clear_selected_response_lease();
                return tool_error_response(id, &tool_name, failure.error());
            }
        };
        if let Some(response) = dispatch_server.project_server_revoked_response(&id, &tool_name) {
            connection.clear_selected_response_lease();
            return response;
        }
        if fast_unavailable {
            return Self::finish_unavailable_tool_call(id, &tool_name, dispatch);
        }
        let connection_client_name = self.client_name();
        let connection_instance_id = self.connection_identity.instance_id();
        let response = dispatch_server
            .complete_tool_call(
                id.clone(),
                tool_name.clone(),
                analytics_arguments,
                analytics_session_id,
                dispatch,
                connection_client_name.as_deref(),
                connection_instance_id,
                &self.pending_notifications,
            )
            .await;
        if let Some(response) = dispatch_server.project_server_revoked_response(&id, &tool_name) {
            connection.clear_selected_response_lease();
            return response;
        }
        if response.error.is_some() {
            // Typed failures contain no selected-project payload and must be
            // deliverable even if retirement begins after their authority was
            // decided. Successful/semantic payloads keep the lease to write.
            connection.clear_selected_response_lease();
        }
        response
    }
}

#[cfg(test)]
mod git_read_control_tests {
    use super::*;
    use crate::mcp::server::dispatch_settlement::ApplicationCancellationRegistration;

    #[test]
    fn controlled_operations_receive_live_registration_and_bounded_deadlines() {
        assert!(tool_supports_live_cancellation("tracedecay_search"));
        assert!(tool_supports_live_cancellation(
            "tracedecay_run_affected_tests"
        ));
        assert!(tool_supports_live_cancellation("tracedecay_admin_cli"));
        assert!(tool_supports_live_cancellation("tracedecay_pr_context"));
        for tool_name in [
            "tracedecay_dead_code",
            "tracedecay_circular",
            "tracedecay_affected",
            "tracedecay_simplify_scan",
            "tracedecay_dependency_depth",
            "tracedecay_health",
            "tracedecay_dsm",
        ] {
            assert!(
                tool_supports_live_cancellation(tool_name),
                "{tool_name} must carry the caller cancellation signal into the verified graph"
            );
        }
        assert!(!tool_supports_live_cancellation("tracedecay_outline"));
        for tool_name in [
            "tracedecay_git_status",
            "tracedecay_git_diff",
            "tracedecay_git_history",
            "tracedecay_git_blame",
            "tracedecay_git_hunks",
        ] {
            assert!(tool_supports_live_cancellation(tool_name));
            let application_surface =
                crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name);
            assert!(
                application_surface.is_some(),
                "Git reads must enter the catalog-owned application surface",
            );
            let controlled_read = is_controlled_read_tool(tool_name);
            assert!(controlled_read);
            assert_eq!(
                dispatch_deadline_horizon_micros(controlled_read),
                Some(30_000_000)
            );
        }
        for tool_name in [
            "tracedecay_str_replace",
            "tracedecay_multi_str_replace",
            "tracedecay_insert_at",
            "tracedecay_ast_grep_rewrite",
            "tracedecay_replace_symbol",
            "tracedecay_insert_at_symbol",
            "tracedecay_move_symbol",
            "tracedecay_source_edit_reconcile",
            "tracedecay_source_edit_rollback",
        ] {
            assert!(is_source_edit_tool(tool_name));
            assert!(tool_supports_live_cancellation(tool_name));
            assert_eq!(dispatch_deadline_horizon_micros(true), Some(30_000_000));
        }

        let request_id = "request.git-read-controls".to_owned();
        let signal = tracedecay_application::CancellationSignal::active(
            "cancellation.request.git-read-controls",
        )
        .expect("signal");
        let registry = std::sync::Mutex::new(HashMap::from([(request_id.clone(), signal.clone())]));
        {
            let _registration =
                ApplicationCancellationRegistration::new(&registry, Some(request_id.clone()));
            signal.cancel(tracedecay_domain::UtcMicros(1));
            assert!(registry.lock().expect("registry").contains_key(&request_id));
        }
        assert!(!registry.lock().expect("registry").contains_key(&request_id));
    }

    #[test]
    fn all_retained_tools_request_the_daemon_invocation_executor() {
        for operation in tracedecay_application::RetainedSurfaceOperation::CALLABLE {
            let tool_name = format!("tracedecay_{}", operation.as_str());
            assert!(
                requires_application_invocation_executor(&tool_name),
                "{tool_name} must use the mounted retained owner",
            );
        }
    }

    /// These tools walk git trees but are not application-surface operations
    /// and are not source edits, so the horizon predicate used to return `None`
    /// for them: they dispatched with no deadline at all while the cheaper
    /// `tracedecay_git_status` was bounded at thirty seconds.
    #[test]
    fn git_reading_tools_receive_a_bounded_deadline() {
        for tool_name in [
            "tracedecay_admin_branch_add",
            "tracedecay_affected",
            "tracedecay_diff_context",
            "tracedecay_changelog",
            "tracedecay_commit_context",
            "tracedecay_pr_context",
            "tracedecay_branch_search",
            "tracedecay_branch_diff",
            "tracedecay_branch_list",
        ] {
            assert!(
                crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name)
                    .is_none(),
                "{tool_name} is not an application-surface operation, so only the \
                 git-dispatch predicate can bound it",
            );
            assert!(!is_source_edit_tool(tool_name));
            assert!(
                is_controlled_read_tool(tool_name),
                "{tool_name} walks a git tree and must be a controlled read",
            );
            assert_eq!(
                dispatch_deadline_horizon_micros(
                    is_controlled_read_tool(tool_name) || is_source_edit_tool(tool_name),
                ),
                Some(30_000_000),
                "{tool_name} must dispatch with a bounded horizon",
            );
        }
    }

    /// The horizon predicate reads the canonical binding table, so it must not
    /// sweep in reads from other dispatch families.
    #[test]
    fn non_git_reads_stay_outside_the_controlled_read_horizon() {
        for tool_name in [
            "tracedecay_outline",
            "tracedecay_body",
            "tracedecay_dead_code",
            "tracedecay_health",
            "tracedecay_context",
        ] {
            assert!(
                !is_controlled_read_tool(tool_name),
                "{tool_name} is not a git-walking read",
            );
        }
        assert!(is_controlled_read_tool("tracedecay_search"));
    }
}
