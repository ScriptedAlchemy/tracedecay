//! Request routing and handlers: per-method JSON-RPC dispatch,
//! handshake handling, resources, and `tools/call` execution.

use super::*;
use crate::mcp::ToolResult;
use tracedecay_sessions::WorkflowIndexReadPort;

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

struct ApplicationCancellationRegistration<'a> {
    registry: &'a std::sync::Mutex<HashMap<String, tracedecay_application::CancellationSignal>>,
    request_id: Option<String>,
}

impl Drop for ApplicationCancellationRegistration<'_> {
    fn drop(&mut self) {
        if let Some(request_id) = self.request_id.as_deref() {
            recover_lock(self.registry).remove(request_id);
        }
    }
}

/// Application-surface plumbing prepared once per dispatch: the typed request
/// id, deadline, cancellation signal, daemon invocation client, and the RAII
/// cancellation registration that must outlive the dispatch await.
struct ApplicationSurfaceDispatch<'a> {
    request_id: Option<tracedecay_application::RequestId>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    invocation_executor: Option<&'a dyn crate::daemon_client::DaemonInvocationExecutor>,
    _registration: ApplicationCancellationRegistration<'a>,
}

/// Retained name for this module's call sites; the saturating clamp is the one
/// shared definition so MCP cannot stamp "now" differently from the daemon.
pub(super) fn mcp_now_micros() -> tracedecay_domain::UtcMicros {
    tracedecay_application::clock::now_micros()
}

fn is_source_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_str_replace"
            | "tracedecay_multi_str_replace"
            | "tracedecay_insert_at"
            | "tracedecay_ast_grep_rewrite"
            | "tracedecay_replace_symbol"
            | "tracedecay_insert_at_symbol"
            | "tracedecay_move_symbol"
            | "tracedecay_api_migration_apply"
            | "tracedecay_source_edit_reconcile"
    )
}

/// Reads that walk a git tree or the whole code graph, and so must not run
/// without a horizon.
///
/// The catalog-owned git reads are recognised by their surface operation; every
/// other git-walking tool is recognised through the canonical MCP binding table
/// rather than a second hand-maintained name list, so a newly bound git tool
/// inherits the bound instead of silently running unbounded.
fn is_controlled_read_tool(tool_name: &str) -> bool {
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

fn dispatch_deadline_horizon_micros(
    application_surface: bool,
    thirty_second_operation: bool,
) -> Option<i64> {
    if thirty_second_operation {
        Some(30_000_000)
    } else if application_surface {
        Some(10_000_000)
    } else {
        None
    }
}

/// Hand-maintained schema documentation for the `tracedecay://schema` resource.
/// Mirrors `src/db/migrations.rs::create_schema`. Update both together.
const SCHEMA_MARKDOWN: &str = r"# tracedecay SQLite schema

The active project database lives in the user-level TraceDecay profile store
(`~/.tracedecay/projects/<project_id>/tracedecay.db` by default), scoped to the
current project. Per-branch variants live beside it under the same store. All
tables are plain SQLite; safe to query with any client. WAL mode is used, so
readers do not block writers.

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

### `vectors` — optional embeddings (semantic search backend)
- `node_id` PRIMARY KEY FK → `nodes.id`
- `embedding` BLOB
- `model` TEXT, `created_at` INTEGER

### `metadata` — key/value store
Common keys: `tokens_saved`, schema-version markers.

### `node_fingerprints` — redundancy cache
- `node_id` PRIMARY KEY FK → `nodes.id`
- `ast_hash`, `cfg_hash`, `call_seq_hash`, `shingles`
- `body_tokens`, `source_hash`

### `read_cache` — rendered `tracedecay_read` responses
- primary key: `(project_id, session_id, file_path, mode, args_hash)`
- stores `mtime_ns`, `digest`, rendered `body` BLOB, token count, and `created_at`

### v11: `memory_facts`, `memory_entities`, `memory_fact_entities`, `memory_banks`, `memory_feedback_events`
The holographic fact store replaces narrow decision rows with durable facts
linked to named entities:

- `memory_facts` — numeric `fact_id`, unique fact content, category, source,
  tags JSON, computed trust score, retrieval/feedback counts, timestamps, and
  structured metadata.
- `memory_entities` — normalized recall keys for symbols, files,
  directories, branches, people, subsystems, and concepts. Facts can attach
  multiple entities so recall can start from code or natural-language names.
- `memory_fact_entities` — many-to-many join table linking facts to entities
  with cascade deletes.
- `memory_banks` — optional holographic memory-bank vectors by category or
  bank name (`bank_name`, `vector`, `hrr_algebra`, `hrr_dim`, `fact_count`,
  `updated_at`).
- `memory_feedback_events` — append-only `helpful`/`unhelpful` audit events
  keyed by numeric `fact_id`, with source, note, old/new trust, and trust delta.

Older `memory_decisions` / `memory_code_areas` tables are migration-only inputs:
v11 backfills them into `memory_facts` and then drops the legacy tables.

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
        recover_lock(&self.application_surface_cancellations)
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
        let Ok(mut connection) = self.new_connection_route_state() else {
            return request.id.clone().map(|id| {
                JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    "MCP connection identity is unavailable".to_owned(),
                )
            });
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
    pub(crate) fn new_connection_route_state(
        &self,
    ) -> std::result::Result<ConnectionRouteState, crate::request_identity::RequestIdentityError>
    {
        // One request-correlation scope per client connection: envelope ids
        // are client-chosen and connection-local, so the daemon widens them
        // before they become persisted application request identities.
        let memory_request_scope = self.connection_identity.establish_connection_scope()?;
        Ok(ConnectionRouteState::new(
            memory_request_scope,
            self.hook_project_routes.snapshot(),
        ))
    }

    pub(crate) async fn handle_request_for_connection(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        connection: &mut ConnectionRouteState,
        pre_cancelled: bool,
    ) -> Option<JsonRpcResponse> {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        *recover_lock(&self.method_call_counts)
            .entry(request.method.clone())
            .or_insert(0) += 1;
        if matches!(classify_mcp_method(&request.method), McpMethod::HookEvent) {
            Box::pin(self.handle_hook_event_notification(
                request.params.as_ref(),
                &mut connection.route_cache,
            ))
            .await;
            return None;
        }
        if matches!(classify_mcp_method(&request.method), McpMethod::Cancelled) {
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
        self.hook_project_routes
            .refresh_into(&mut connection.route_cache);
        let id = request.id.clone()?;

        let result = match classify_mcp_method(&request.method) {
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
                    &connection.route_cache,
                    connection.implicit_project_path(),
                    connection.memory_request_scope(),
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

        if let Some(ref resp) = result
            && resp.error.is_some()
        {
            self.stats.errors.fetch_add(1, Ordering::Relaxed);
        }

        result
    }

    pub(crate) async fn handle_hook_event_notification(
        &self,
        params: Option<&Value>,
        route_cache: &mut HookProjectRouteCache,
    ) -> HostAdmissionOutcome {
        let Some(event) = hook_events::parse_hook_event(params) else {
            let outcome = HostAdmissionOutcome::degraded("malformed_event");
            Self::report_host_admission_outcome(outcome);
            return outcome;
        };
        // R4: one branch resolution for this notification — the drift check
        // below and the hook-plan branch label both read it.
        let (cg, live_branch) = self.reopen_if_branch_drifted_memoized().await;
        let root = cg.project_root().to_path_buf();
        // Live-activity tap: a host hook arriving here IS an agent working in
        // this project, so publish it at the observation point carrying this
        // project's own registered id. The application lane retains it even
        // without a connected dashboard; the SSE adapter coalesces the burst.
        let activity_project_id =
            crate::application::event_lane::enabled(self.session_db.as_deref())
                .then(|| cg.store_layout().identity.project_id.clone())
                .flatten();
        if let Some(activity_db) = self.session_db.as_deref() {
            crate::application::event_lane::publish(
                activity_db,
                crate::application::event_lane::ActivityFamilyV1::Hook,
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
            && let Some(sink) = &self.code_index_hook_sink
        {
            // A `true` return means the paths really entered a mounted
            // worktree's incremental queue — the exact moment indexing work is
            // created for this project, and the only condition worth lighting.
            if sink(root.clone(), event.rel_paths.clone()).await
                && let Some(activity_db) = self.session_db.as_deref()
            {
                crate::application::event_lane::publish(
                    activity_db,
                    crate::application::event_lane::ActivityFamilyV1::CodeIndex,
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
        // Connection routing is identity for subsequent tool calls on this
        // connection: publish it before durable admission, even when effect
        // processing is deferred, retained, or the spool is unavailable. A
        // spool outage must never leave follow-up reads silently pinned to
        // the active project — that would present wrong-project data as
        // success instead of a typed unavailable outcome.
        if let Err(error) = self.update_hook_workspace_route(&event, route_cache).await {
            tracing::warn!(error = %error, "hook project registry route resolution failed");
            let outcome =
                HostAdmissionOutcome::retained_unavailable("project_registry_route_unavailable");
            Self::report_host_admission_outcome(outcome);
            return outcome;
        }
        let admission_source = event.admission_source();
        let admitted = match self.host_admission_broker.as_ref() {
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
        let outcome = Box::pin(self.replay_host_admission(Some(admitted.seq))).await;
        // Route analytics + span observations are side writes: only after the
        // durable spool record is authoritatively committed (Committed or
        // ExactDuplicate). Failures are best-effort and must never change the
        // admission outcome already decided above.
        if matches!(
            outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ) {
            if let Some(wake) = &self.project_session_refresh_wake {
                wake.wake();
            }
            if let Some(wake) = &self.user_session_refresh_wake {
                wake.wake();
            }
            self.record_hook_route_analytics(
                &root,
                &event,
                current_branch.as_deref(),
                admitted.seq,
            );
            self.record_hook_span_observation(&event).await;
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
        let node_count = self
            .cg_snapshot()
            .await
            .get_stats()
            .await
            .map_or(0, |s| s.node_count);
        let budget = explore_call_budget(node_count);
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
        match get_catalog_filtered_tool_definitions_with_budget(
            node_count,
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
    fn resource_contents(id: Value, uri: &str, mime: &str, text: &str) -> JsonRpcResponse {
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

    /// Returns graph statistics as a JSON resource.
    pub(crate) async fn read_resource_status(&self, id: Value) -> JsonRpcResponse {
        let cg = self.reopen_if_branch_drifted().await;
        match cg.get_stats().await {
            Ok(stats) => {
                let mut output = serde_json::to_value(&stats).unwrap_or(json!({}));
                output["branch_diagnostics"] =
                    serde_json::to_value(cg.branch_diagnostics()).unwrap_or(json!({}));
                let text = serde_json::to_string_pretty(&output).unwrap_or_default();
                Self::resource_contents(id, "tracedecay://status", "application/json", &text)
            }
            Err(e) => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                format!("failed to read graph stats: {e}"),
            ),
        }
    }

    /// Returns the file list as a text resource (grouped by directory).
    pub(crate) async fn read_resource_files(&self, id: Value) -> JsonRpcResponse {
        match self.cg_snapshot().await.get_all_files().await {
            Ok(mut files) => {
                files.sort_by(|a, b| a.path.cmp(&b.path));
                let mut groups: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for f in &files {
                    let dir = f.path.rfind('/').map_or(".", |i| &f.path[..i]).to_string();
                    let name = f
                        .path
                        .rfind('/')
                        .map_or(f.path.as_str(), |i| &f.path[i + 1..]);
                    groups
                        .entry(dir)
                        .or_default()
                        .push(format!("{} ({} symbols)", name, f.node_count));
                }
                let mut lines = Vec::new();
                lines.push(format!("{} indexed files", files.len()));
                for (dir, entries) in &groups {
                    lines.push(format!("\n{}/ ({} files)", dir, entries.len()));
                    for entry in entries {
                        lines.push(format!("  {entry}"));
                    }
                }
                let text = lines.join("\n");
                Self::resource_contents(id, "tracedecay://files", "text/plain", &text)
            }
            Err(e) => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                format!("failed to read file list: {e}"),
            ),
        }
    }

    /// Returns a high-level project overview as a text resource.
    pub(crate) async fn read_resource_overview(&self, id: Value) -> JsonRpcResponse {
        let cg = self.cg_snapshot().await;
        let stats = match cg.get_stats().await {
            Ok(s) => s,
            Err(e) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    format!("failed to read graph stats: {e}"),
                );
            }
        };

        let mut lines = Vec::new();
        lines.push(format!("Project: {}", cg.project_root().display()));
        lines.push(format!(
            "Graph: {} nodes, {} edges, {} files",
            stats.node_count, stats.edge_count, stats.file_count
        ));

        // Language distribution
        if !stats.files_by_language.is_empty() {
            lines.push("\nLanguages:".to_string());
            let mut langs: Vec<_> = stats.files_by_language.iter().collect();
            langs.sort_by(|a, b| b.1.cmp(a.1));
            for (lang, count) in &langs {
                lines.push(format!("  {lang} ({count} files)"));
            }
        }

        // Node kind distribution (top 10)
        if !stats.nodes_by_kind.is_empty() {
            lines.push("\nSymbol kinds:".to_string());
            let mut kinds: Vec<_> = stats.nodes_by_kind.iter().collect();
            kinds.sort_by(|a, b| b.1.cmp(a.1));
            for (kind, count) in kinds.iter().take(10) {
                lines.push(format!("  {kind} ({count})"));
            }
        }

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

    async fn route_tool_arguments(
        &self,
        id: &Value,
        tool_name: &str,
        arguments: Value,
        route_cache: &HookProjectRouteCache,
        memory_request_scope: &str,
    ) -> Result<RoutedToolCall> {
        let (mut handler_arguments, routed_project) =
            route_cache.route_tool_arguments(tool_name, arguments)?;
        if crate::analytics::is_skill_view_tool(tool_name)
            && let Some(request_id) = json_rpc_request_id_string(id)
            && let Some(map) = handler_arguments.as_object_mut()
        {
            map.insert("__mcp_request_id".to_string(), json!(request_id));
        }
        if tool_supports_live_cancellation(tool_name)
            && let Some(map) = handler_arguments.as_object_mut()
        {
            map.remove("__mcp_request_id");
            if let Some(request_id) = application_surface_request_id(id, memory_request_scope) {
                map.insert("__mcp_request_id".to_owned(), json!(request_id));
            }
        }
        let selected_project = match routed_project {
            Some(project) => Some(project),
            None => {
                crate::mcp::tools::handlers::selected_registered_project_reader(
                    tool_name.to_owned(),
                    handler_arguments.clone(),
                    self.registry_db.as_deref(),
                    self.retained_project_graph_resolver.clone(),
                )
                .await?
            }
        };
        Ok(RoutedToolCall {
            arguments: handler_arguments,
            selected_project,
        })
    }

    async fn execute_tool_dispatch(
        &self,
        cg: &TraceDecay,
        tool_name: &str,
        handler_arguments: Value,
        preselected_project_reader: bool,
        server_stats: Option<Value>,
        implicit_project_path: Option<&Path>,
        application_invocation_executor: Option<
            &dyn crate::daemon_client::DaemonInvocationExecutor,
        >,
        application_invocation_target: tracedecay_application::InvocationTarget,
        application_request_id: Option<tracedecay_application::RequestId>,
        application_deadline: Option<tracedecay_application::Deadline>,
        application_cancellation: Option<tracedecay_application::CancellationSignal>,
    ) -> Result<ToolResult> {
        let engine_identity = cg.db_path();
        let workflow_index_reads = self
            .registered_session_db
            .as_ref()
            .map(|database| DaemonWorkflowIndexReadService::new(Arc::clone(database)));
        let read_flight = tool_allows_identical_read_coalescing(tool_name).then(|| {
            self.identical_read_coalescer.claim(
                engine_identity.to_string_lossy().as_ref(),
                tool_name,
                &handler_arguments,
                self.scope_prefix(),
            )
        });
        let dispatch: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>,
        > = handle_tool_call_with_registry_and_implicit_project(
            cg,
            tool_name,
            handler_arguments,
            server_stats,
            self.scope_prefix(),
            ToolCallRegistryOptions {
                global_db: self.registry_db.as_ref(),
                project_registry_reads: self.project_registry_reads.as_deref(),
                workflow_index_reads: workflow_index_reads
                    .as_ref()
                    .map(|service| service as &dyn WorkflowIndexReadPort),
                accounting_db: self.accounting_db.as_deref(),
                registered_project_session_db: self.registered_session_db.clone(),
                registered_savings_db: self.accounting_db.clone(),
                profile_root: self.profile_root.as_deref(),
                implicit_project_path,
                automation_scheduler_reconciler: self.automation_scheduler_reconciler.clone(),
                automation_writer: self.dashboard_automation_writer.clone(),
                doctor_report_reader: self.dashboard_doctor_report_reader.clone(),
                doctor_remediation_dispatcher: self.dashboard_doctor_remediation_dispatcher.clone(),
                code_index_freshness_reader: self.dashboard_code_index_freshness_reader.clone(),
                feedback_status_reader: self.dashboard_feedback_status_reader.clone(),
                diagnostics_cache: Some(&self.diagnostics_cache),
                diagnostics_lsp: Some(Arc::clone(&self.diagnostics_lsp)),
                application_invocation_executor,
                application_invocation_target,
                dashboard_application_invocation_executor: self
                    .application_invocation_executor
                    .clone(),
                application_request_id,
                application_deadline,
                application_cancellation,
                code_index_publication_identity: self.code_index_publication_identity.clone(),
                code_index_search_executor: self.code_index_search_executor.clone(),
                source_edit_executor: self.source_edit_executor.get().cloned(),
                source_edit_reconciliation_executor: self
                    .source_edit_reconciliation_executor
                    .get()
                    .cloned(),
                code_index_search_authority: self.code_index_search_authority.clone(),
                retained_project_graph_resolver: self.retained_project_graph_resolver.clone(),
                preselected_project_reader,
                session_authorities: crate::mcp::tools::SessionAuthorities::new(
                    self.session_db.as_ref(),
                    self.user_session_db.as_ref(),
                )
                .with_profile_identity(self.profile_identity.as_ref())
                .with_registered_databases(
                    self.registered_session_db.as_ref(),
                    self.registered_user_session_db.as_ref(),
                )
                .with_refresh_services(
                    self.project_session_refresh_service.as_deref(),
                    self.user_session_refresh_service.as_deref(),
                )
                .with_retrieval_services(
                    self.project_session_retrieval_service.as_deref(),
                    self.user_session_retrieval_service.as_deref(),
                ),
            },
        );
        if let Some(read_flight) = read_flight {
            match read_flight {
                ReadFlightClaim::Leader(leader) => match dispatch.await {
                    Ok(result) => Ok((*leader.complete(result)).clone()),
                    Err(error) => Err(error),
                },
                ReadFlightClaim::Follower(follower) => match follower.wait().await {
                    Some(result) => Ok((*result).clone()),
                    None => dispatch.await,
                },
            }
        } else {
            dispatch.await
        }
    }

    #[cfg(feature = "test-transport")]
    #[doc(hidden)]
    pub async fn call_tool_for_test(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolResult> {
        let cg = self.cg().await;
        self.execute_tool_dispatch(
            cg.as_ref(),
            tool_name,
            arguments,
            false,
            None,
            None,
            None,
            tracedecay_application::InvocationTarget::CurrentProject,
            None,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_tool_call(
        &self,
        id: &Value,
        tool_name: &str,
        arguments: Value,
        timings_enabled: bool,
        route_cache: &HookProjectRouteCache,
        implicit_project_path: Option<&Path>,
        memory_request_scope: &str,
        pre_cancelled: bool,
        publish_activity: bool,
    ) -> DispatchedToolCall {
        // Branch-drift hot-swap: if the working tree switched branches since
        // the served instance opened, reopen onto the live branch's DB so
        // this call reads the right index. Cheap no-op check when no drift.
        let (active_cg, live_branch) = self.reopen_if_branch_drifted_memoized().await;
        let handler_start = timings_enabled.then(std::time::Instant::now);
        let routed = match self
            .route_tool_arguments(id, tool_name, arguments, route_cache, memory_request_scope)
            .await
        {
            Ok(routed) => routed,
            Err(error) => {
                return DispatchedToolCall {
                    cg: active_cg,
                    selected_owner: None,
                    selected_scope: None,
                    outcome: Err(error),
                    elapsed_us: handler_start.map(|started| started.elapsed().as_micros() as u64),
                };
            }
        };
        let selected_owner = routed
            .selected_project
            .as_ref()
            .map(|selected| selected.owner.clone());
        let selected_scope = routed
            .selected_project
            .as_ref()
            .map(|selected| selected.scope.clone());
        let cg = routed.selected_project.as_ref().map_or_else(
            || Arc::clone(&active_cg),
            |selected| Arc::clone(&selected.graph),
        );
        let project_reader_preselected = routed.selected_project.is_some();
        let application_invocation_target =
            invocation_target_for_route(routed.selected_project.as_ref());

        self.begin_tool_dispatch(
            tool_name,
            &cg,
            &live_branch,
            project_reader_preselected,
            publish_activity,
        )
        .await;

        let server_stats = if tool_name == "tracedecay_status" {
            Some(self.server_stats_json().await)
        } else {
            None
        };

        // `timings_enabled` was initialized from the server's pinned resolved
        // snapshot (or an explicit transport override). Do not synchronously
        // re-read legacy configuration for every tool call.
        let ApplicationSurfaceDispatch {
            request_id: application_request_id,
            deadline: application_deadline,
            cancellation: application_cancellation,
            invocation_executor: application_invocation_executor,
            _registration,
        } = self
            .prepare_application_surface_dispatch(
                &cg,
                id,
                tool_name,
                memory_request_scope,
                pre_cancelled,
            )
            .await;
        let outcome = self
            .execute_tool_dispatch(
                &cg,
                tool_name,
                routed.arguments,
                project_reader_preselected,
                server_stats,
                implicit_project_path,
                application_invocation_executor,
                application_invocation_target,
                application_request_id.clone(),
                application_deadline,
                application_cancellation,
            )
            .await;
        DispatchedToolCall {
            cg,
            selected_owner,
            selected_scope,
            outcome,
            elapsed_us: handler_start.map(|t| t.elapsed().as_micros() as u64),
        }
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
        id: &Value,
        tool_name: &str,
        memory_request_scope: &str,
        pre_cancelled: bool,
    ) -> ApplicationSurfaceDispatch<'a> {
        let application_surface =
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name);
        let source_edit = is_source_edit_tool(tool_name);
        let controlled_read = is_controlled_read_tool(tool_name);
        let request_id = tool_supports_live_cancellation(tool_name)
            .then(|| application_surface_request_id(id, memory_request_scope))
            .flatten()
            .and_then(|request_id| tracedecay_application::RequestId::new(request_id).ok());
        let cancellation = request_id.as_ref().and_then(|request_id| {
            // The signal is built before the lock is taken: nothing fallible
            // runs inside the critical section, so an unwind can never leave
            // the registry half-updated.
            let cancellation = tracedecay_application::CancellationSignal::active(format!(
                "cancellation.{}",
                request_id.as_str()
            ))
            .ok()?;
            if pre_cancelled {
                cancellation.cancel(mcp_now_micros());
            }
            recover_lock(&self.application_surface_cancellations)
                .insert(request_id.as_str().to_owned(), cancellation.clone());
            Some(cancellation)
        });
        let registration = ApplicationCancellationRegistration {
            registry: &self.application_surface_cancellations,
            request_id: request_id
                .as_ref()
                .map(|request_id| request_id.as_str().to_owned()),
        };
        let deadline = dispatch_deadline_horizon_micros(
            application_surface.is_some() || source_edit,
            controlled_read || source_edit,
        )
        .and_then(|horizon| {
            let now = mcp_now_micros().0;
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                now.saturating_add(horizon),
            ))
            .ok()
        });
        let invocation_executor = if application_surface.is_some() {
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
            request_id,
            deadline,
            cancellation,
            invocation_executor,
            _registration: registration,
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
        let registered = self.accounting_db.clone();
        let legacy = self.global_db.clone();
        if registered.is_some() || legacy.is_some() {
            let ToolTokenAccounting {
                raw_file_tokens,
                response_tokens,
                net_saved_tokens,
            } = accounting;
            let project_path_str =
                RegisteredGlobalDb::canonical_project_key(accounting_project_root);
            let tool_name_owned = tool_name.to_string();
            let ts = crate::tracedecay::current_timestamp();
            let client_name = self.client_name();
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
                client_name: client_name.as_deref(),
                mcp_instance_id: self.connection_identity.instance_id(),
                failure_reason: failure_reason.as_deref(),
            });
            self.spawn_observed_ledger_write(async move {
                if let Some(gdb) = registered {
                    gdb.record_savings(
                        &project_path_str,
                        &tool_name_owned,
                        raw_file_tokens,
                        response_tokens,
                        ts,
                    )
                    .await;
                    if let Err(e) = gdb.append_analytics_event(&analytics_event).await {
                        tracing::warn!(error = %e, "MCP analytics event insert failed");
                    }
                    return;
                }
                let Some(gdb) = legacy else {
                    return;
                };
                gdb.record_savings(
                    &project_path_str,
                    &tool_name_owned,
                    raw_file_tokens,
                    response_tokens,
                    ts,
                )
                .await;
                if let Err(e) = gdb.append_analytics_event(&analytics_event).await {
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
        );
    }

    async fn append_version_and_automation_notices(
        &self,
        cg: &TraceDecay,
        result: &mut ToolResult,
    ) {
        // Prepend version-update warning + queue logging notification.
        if let Some(warning) = self.check_version_update().await {
            if let Some(content) = result
                .value
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
            {
                content.insert(0, json!({"type": "text", "text": &warning}));
            }
            recover_lock(&self.pending_notifications).push(json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {
                    "level": "warning",
                    "logger": "tracedecay",
                    "data": warning
                }
            }));
        }

        // Staged-automation nudge (Hermes parity R5): when automation
        // runs have queued skill drafts for review, append a one-line
        // notice so the approval queue doesn't grow silently. Fact
        // proposal counts stay telemetry-only in `staged_notice`.
        if let Some(notice) = self.maybe_automation_staged_notice(cg).await
            && let Some(content) = result
                .value
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
        {
            content.push(json!({"type": "text", "text": format!("\n{notice}")}));
        }
    }

    async fn append_per_file_staleness_notice(&self, cg: &TraceDecay, result: &mut ToolResult) {
        // Per-file staleness banner (#428 design): files this response
        // referenced that are still pending after the in-line sync
        // attempt get a focused banner naming them with edit ages,
        // telling the agent to Read THOSE files directly while
        // treating the rest of the response as authoritative.
        // Replaces the previous all-or-nothing "STALE INDEX"
        // warning that made agents distrust the entire answer.
        if result.touched_files.is_empty() {
            return;
        }

        let stale_files = cg.check_file_staleness(&result.touched_files).await;
        if stale_files.is_empty() {
            return;
        }

        let still_stale = match cg.sync_if_stale(&stale_files).await {
            Ok(false) => false,        // sync completed; files now fresh
            Ok(true) | Err(_) => true, // still stale (lock contention / sync error)
        };
        if !still_stale {
            return;
        }

        let banner = format_per_file_staleness_banner(cg.project_root(), &stale_files);
        // Machine-readable marker. Same shape as before
        // so existing scrapers keep working.
        let stale_json = serde_json::to_string(&stale_files).unwrap_or_else(|_| "[]".to_string());
        let marker = format!("\ntracedecay_graph_stale: {stale_json}");
        debug_assert!(
            result.value.is_object(),
            "tool result must be a JSON object so graph_stale can be attached"
        );
        if let Some(obj) = result.value.as_object_mut() {
            obj.insert("graph_stale".to_string(), json!(stale_files));
        }
        if let Some(content) = result
            .value
            .get_mut("content")
            .and_then(|c| c.as_array_mut())
        {
            content.insert(0, json!({"type": "text", "text": &banner}));
            content.push(json!({"type": "text", "text": marker}));
        }
    }

    async fn prepend_index_warnings(
        &self,
        cg: &TraceDecay,
        include_connection_worktree_warning: bool,
        result: &mut ToolResult,
    ) {
        // Warn if serving from a fallback (ancestor) branch DB.
        if let Some(warning) = cg.fallback_warning() {
            let warning = format!("WARNING: {warning}");
            if let Some(content) = result
                .value
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
            {
                content.insert(0, json!({"type": "text", "text": &warning}));
            }
        }

        // Check overall index age (warn if older than 1 hour).
        // Uses `last_sync_timestamp` (sync execution time) not the
        // max file `indexed_at` — a no-change sync still updates the
        // sync metadata even though no file gets a fresh `indexed_at`,
        // so a per-file fallback fires the warning forever on quiet
        // repos (#86).
        //
        // D7 staleness-warning UX: with auto-sync on (the normal
        // case), a stale index self-heals — the D4 background refresh
        // above was already kicked for this read. So instead of the
        // old "Run `tracedecay sync`" nag, we emit an informational
        // "refresh in progress" note (or nothing at all if a refresh
        // just completed). The manual-sync instruction is reserved
        // for the cases where auto-repair genuinely can't help:
        //   - serving a read-only fallback/ancestor store, or
        //   - the user disabled both auto_watch and read_refresh.
        let last_time = cg.last_sync_timestamp().await;
        let now = crate::tracedecay::current_timestamp();
        let age_secs = now - last_time;
        if last_time > 0 && age_secs > 3600 {
            let refreshed_recently = {
                let done = self.last_background_refresh_done_at.load(Ordering::Acquire);
                done > 0 && now.saturating_sub(done) < self.sync_config.read_cooldown_secs as i64
            };
            let banner = staleness_banner(StalenessBannerInputs {
                age_secs,
                // Auto-sync is "on" when either the daemon watcher
                // or sync-on-read can repair this.
                auto_sync_on: self.sync_config.auto_watch || self.sync_config.read_refresh,
                // A read-only fallback store can never be written,
                // so no background refresh can heal it.
                fallback_store: cg.fallback_warning().is_some(),
                refresh_running: self.background_refresh_running.load(Ordering::Acquire),
                refreshed_recently,
            });

            if let Some(banner) = banner
                && let Some(content) = result
                    .value
                    .get_mut("content")
                    .and_then(|c| c.as_array_mut())
            {
                content.insert(0, json!({"type": "text", "text": &banner}));
            }
        }

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
                    )
                    .await;
                }
                self.append_version_and_automation_notices(&cg, &mut result)
                    .await;
                self.append_per_file_staleness_notice(&cg, &mut result)
                    .await;
                self.prepend_index_warnings(&cg, selected_owner.is_none(), &mut result)
                    .await;
                mark_semantic_tool_error(&mut result);
                if selected_owner.is_none() {
                    self.refresh_after_live_transcript_projection(
                        &tool_name,
                        &analytics_arguments,
                        &result,
                    )
                    .await;
                }
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
                });
                tool_error_response(id, &tool_name, &error)
            }
        }
    }

    async fn refresh_after_live_transcript_projection(
        &self,
        tool_name: &str,
        arguments: &Value,
        result: &ToolResult,
    ) {
        if tool_name != "tracedecay_lcm_preflight"
            || arguments
                .get("transcript_projection")
                .and_then(Value::as_bool)
                != Some(true)
            || tool_result_has_semantic_error(result)
        {
            return;
        }
        let user_scope = arguments.get("storage_scope").and_then(Value::as_str) == Some("user");
        let wake = if user_scope {
            self.user_session_refresh_wake.as_ref()
        } else {
            self.project_session_refresh_wake.as_ref()
        };
        if let Some(wake) = wake {
            let _ = wake
                .wake_and_wait_until_idle(std::time::Duration::from_secs(5))
                .await;
        }
    }

    async fn publish_tool_call_activity(&self, tool_name: &str, cg: &TraceDecay) {
        if !crate::application::event_lane::enabled(self.session_db.as_deref()) {
            return;
        }
        let Some(activity_db) = self.session_db.as_deref() else {
            return;
        };
        crate::application::event_lane::publish(
            activity_db,
            crate::application::event_lane::ActivityFamilyV1::ToolCall,
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
        self.project_server_live
            .as_ref()
            .is_some_and(|live| !live.load(Ordering::Acquire))
            .then(|| {
                JsonRpcResponse::error_with_data(
                    id.clone(),
                    ErrorCode::InternalError,
                    "tool project route failed: project server was revoked after health validation"
                        .to_owned(),
                    Some(json!({
                        "tool": tool_name,
                        "reason_code": "project_server_health_revoked",
                        "retryable": true,
                        "detail": "the retained project server failed post-open health validation; retry against a recovered owner",
                    })),
                )
            })
    }

    /// Handles the `tools/call` method, dispatching to the appropriate tool handler.
    pub(crate) async fn handle_tools_call(
        &self,
        id: Value,
        params: Option<&Value>,
        timings_enabled: bool,
        route_cache: &HookProjectRouteCache,
        implicit_project_path: Option<&Path>,
        memory_request_scope: &str,
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
        if let Some(response) = self.project_server_revoked_response(&id, &tool_name) {
            return response;
        }

        let fast_unavailable = self.message_search_worker_is_unavailable(&tool_name, &arguments);
        let dispatch = self
            .dispatch_tool_call(
                &id,
                &tool_name,
                arguments,
                timings_enabled,
                route_cache,
                implicit_project_path,
                memory_request_scope,
                pre_cancelled,
                !fast_unavailable,
            )
            .await;
        if let Some(response) = self.project_server_revoked_response(&id, &tool_name) {
            return response;
        }
        if fast_unavailable {
            return Self::finish_unavailable_tool_call(id, &tool_name, dispatch);
        }
        let response = self
            .complete_tool_call(
                id.clone(),
                tool_name.clone(),
                analytics_arguments,
                analytics_session_id,
                dispatch,
            )
            .await;
        if let Some(response) = self.project_server_revoked_response(&id, &tool_name) {
            return response;
        }
        response
    }
}

#[cfg(test)]
mod git_read_control_tests {
    use super::*;

    #[test]
    fn controlled_operations_receive_live_registration_and_bounded_deadlines() {
        assert!(tool_supports_live_cancellation("tracedecay_search"));
        assert!(tool_supports_live_cancellation(
            "tracedecay_run_affected_tests"
        ));
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
                dispatch_deadline_horizon_micros(application_surface.is_some(), controlled_read),
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
            "tracedecay_api_migration_apply",
            "tracedecay_source_edit_reconcile",
        ] {
            assert!(is_source_edit_tool(tool_name));
            assert!(tool_supports_live_cancellation(tool_name));
            assert_eq!(
                dispatch_deadline_horizon_micros(true, true),
                Some(30_000_000)
            );
        }

        let request_id = "request.git-read-controls".to_owned();
        let signal = tracedecay_application::CancellationSignal::active(
            "cancellation.request.git-read-controls",
        )
        .expect("signal");
        let registry = std::sync::Mutex::new(HashMap::from([(request_id.clone(), signal.clone())]));
        {
            let _registration = ApplicationCancellationRegistration {
                registry: &registry,
                request_id: Some(request_id.clone()),
            };
            signal.cancel(tracedecay_domain::UtcMicros(1));
            assert!(registry.lock().expect("registry").contains_key(&request_id));
        }
        assert!(!registry.lock().expect("registry").contains_key(&request_id));
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
                    false,
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
