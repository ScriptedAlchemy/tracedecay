// Rust guideline compliant 2025-10-17
//! MCP server that reads JSON-RPC 2.0 messages from stdin and writes
//! responses to stdout.
//!
//! The server exposes code graph tools via the Model Context Protocol,
//! allowing AI assistants to query the code graph interactively.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::mcp::project_route::{
    HookProjectRouteCache, SharedHookProjectRouteCache, mcp_analytics_session_id,
};
use crate::mcp::response_handles::{
    RESPONSE_RETRIEVE_TOOL, cleanup_expired_response_handles, response_handle_stats_json,
};
use crate::mcp::tool_analytics::{
    McpToolAnalyticsEvent, hook_route_analytics_event, mcp_tool_analytics_event,
};
use crate::path_tree::format_compact_annotated_path_list;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use super::hook_events::{self, HookAgent, HookEventPlan};
use super::tools::{
    ToolCallRegistryOptions, ToolResult, explore_call_budget, get_tool_definitions_with_budget,
    handle_tool_call_with_registry_and_implicit_project,
};
use super::transport::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

/// Every JSON-RPC method surface the MCP server understands. This is the
/// single source of truth for [`McpServer::handle_request`] dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpMethod {
    Initialize,
    /// `initialized` / `notifications/initialized` — compatibility no-ops.
    InitializedAck,
    ToolsList,
    ToolsCall,
    ResourcesList,
    ResourcesRead,
    /// `ping` / `logging/setLevel` — acknowledged with an empty result.
    TrivialAck,
    /// The daemon's internal hook-event notification.
    HookEvent,
    Unknown,
}

pub(crate) fn classify_mcp_method(method: &str) -> McpMethod {
    if method == crate::daemon::HOOK_EVENT_METHOD {
        return McpMethod::HookEvent;
    }
    match method {
        "initialize" => McpMethod::Initialize,
        "initialized" | "notifications/initialized" => McpMethod::InitializedAck,
        "tools/list" => McpMethod::ToolsList,
        "tools/call" => McpMethod::ToolsCall,
        "resources/list" => McpMethod::ResourcesList,
        "resources/read" => McpMethod::ResourcesRead,
        "ping" | "logging/setLevel" => McpMethod::TrivialAck,
        _ => McpMethod::Unknown,
    }
}

/// The steering instructions advertised from the `initialize` handshake of a
/// healthy server.
pub(crate) const SERVER_INSTRUCTIONS: &str = concat!(
    "tracedecay is a code-graph MCP server. \
    Start with tracedecay_context for any code exploration task \
    — it returns relevant symbols, relationships, and code \
    snippets for a natural-language query. Use tracedecay_search \
    to find specific symbols by name. Discovery and analysis \
    tools are read-only and safe to call in parallel. Edit \
    and session-memory tools can mutate local project state \
    and declare readOnlyHint=false. \
    Every tool is also available from the shell: ",
    crate::cli_fallback_args_invocation_lit!(),
    " \
    — run `tracedecay tool` to list tools, \
    `tracedecay tool <name> --help` for parameters). If an MCP \
    call errors, times out, or this server disconnects, fall \
    back to that CLI instead of querying .tracedecay databases \
    directly or abandoning tracedecay. \
    When a tool result contains a `tracedecay_metrics:` line, \
    report the savings to the user (e.g. 'TraceDecay\\'d ~N tokens')."
);

/// The `initialize` result payload.
pub(crate) fn initialize_result(instructions: &str) -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": true
            },
            "resources": {},
            "logging": {}
        },
        "serverInfo": {
            "name": "tracedecay",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": instructions,
    })
}

/// The `resources/list` result payload.
pub(crate) fn resources_list_result() -> Value {
    json!({
        "resources": [
            {
                "uri": "tracedecay://status",
                "name": "Graph Status",
                "description": "Code graph statistics: node/edge/file counts, languages, DB size, and index freshness.",
                "mimeType": "application/json"
            },
            {
                "uri": "tracedecay://files",
                "name": "File List",
                "description": "All indexed project files grouped by directory with symbol counts.",
                "mimeType": "text/plain"
            },
            {
                "uri": "tracedecay://overview",
                "name": "Project Overview",
                "description": "High-level project summary: language distribution, largest modules, and top entry points.",
                "mimeType": "text/plain"
            },
            {
                "uri": "tracedecay://branches",
                "name": "Tracked Branches",
                "description": "List of tracked branches with DB sizes, parent branch, and last sync time. Empty if multi-branch is not active.",
                "mimeType": "application/json"
            },
            {
                "uri": "tracedecay://schema",
                "name": "SQLite Schema",
                "description": "Documentation for the .tracedecay/tracedecay.db schema: tables, columns, indexes, and common query recipes. Use when MCP tools don't cover your query and you need to drop down to raw SQL.",
                "mimeType": "text/markdown"
            }
        ]
    })
}

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

#[derive(Default)]
struct ConnectionRouteState {
    implicit_project_path: Option<PathBuf>,
}

impl ConnectionRouteState {
    async fn observe_initialize(&mut self, params: Option<&Value>, registry_db: Option<&GlobalDb>) {
        self.implicit_project_path =
            resolve_initialize_roots_project_path(params, registry_db).await;
    }

    fn implicit_project_path(&self) -> Option<&Path> {
        self.implicit_project_path.as_deref()
    }
}

pub(crate) async fn resolve_initialize_roots_project_path(
    params: Option<&Value>,
    registry_db: Option<&GlobalDb>,
) -> Option<PathBuf> {
    let roots = initialize_root_paths(params);
    if roots.is_empty() {
        return None;
    }
    let registry_db = registry_db?;
    let projects = registry_db.search_code_projects("", usize::MAX).await;
    for root in roots {
        if let Some(project_path) = match_initialize_root_to_registered_project(&root, &projects) {
            return Some(project_path);
        }
    }
    None
}

pub(crate) fn initialize_root_paths(params: Option<&Value>) -> Vec<PathBuf> {
    params
        .and_then(|p| p.get("roots"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|root| {
            let uri = root.get("uri").and_then(Value::as_str)?;
            crate::serve::local_path_from_mcp_root_uri(uri)
        })
        .collect()
}

fn match_initialize_root_to_registered_project(
    root: &Path,
    projects: &[crate::global_db::CodeProjectRecord],
) -> Option<PathBuf> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut matches: Vec<_> = projects
        .iter()
        .filter_map(|project| {
            let project_path = PathBuf::from(&project.canonical_root);
            let project_path = project_path
                .canonicalize()
                .unwrap_or_else(|_| project_path.clone());
            (root == project_path || root.starts_with(&project_path))
                .then(|| (project_path.components().count(), project_path))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    matches.into_iter().map(|(_, path)| path).next()
}

/// Cache duration for version checks (15 minutes).
const VERSION_CHECK_INTERVAL: Duration = Duration::from_mins(15);

/// Upper bound for [`McpServer::ledger_writes_settled`]. Savings-ledger writes
/// are fire-and-forget `SQLite` appends that finish in well under a second on a
/// healthy machine, so 10 s is generous headroom; the point is that the wait
/// is *finite* — a wedged recorder task can never hang the caller (tests,
/// shutdown drains) indefinitely as the previous unbounded loop allowed.
const LEDGER_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

// Global accounting (savings ledger + worldwide-counter flushes) is enabled
// by default; see `crate::global_db::global_accounting_mode` for the env
// override precedence.

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

/// Build the per-file staleness banner inserted at the top of any tool
/// response that referenced files the in-line sync couldn't refresh.
///
/// The shape mimics codegraph's #428 banner: name each pending file with
/// its edit age (how long since the on-disk mtime), and direct the agent
/// to `Read` those specific files. The rest of the response is treated
/// as authoritative — distinct from the previous binary "STALE INDEX"
/// warning that asked the agent to distrust the whole answer.
fn format_per_file_staleness_banner(
    project_root: &std::path::Path,
    stale_files: &[String],
) -> String {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut lines = Vec::with_capacity(stale_files.len() + 2);
    lines.push(format!(
        "WARNING: {} file(s) referenced below were edited after the last sync. \
         Read these directly; the rest of this response reflects the current index:",
        stale_files.len()
    ));
    let annotated_paths = stale_files
        .iter()
        .map(|path| {
            let age = file_mtime_secs(project_root, path).map_or(0, |m| now_secs.saturating_sub(m));
            (path.as_str(), format!(" (edited {})", humanize_age(age)))
        })
        .collect::<Vec<_>>();
    let path_list = format_compact_annotated_path_list(annotated_paths, "  - ", "  ");
    if !path_list.is_empty() {
        lines.push(path_list);
    }
    lines.push("Run `tracedecay sync` to refresh the index.".to_string());
    lines.join("\n")
}

fn needs_lazy_sync_before_dispatch(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_ast_grep_rewrite"
            | "tracedecay_insert_at"
            | "tracedecay_insert_at_symbol"
            | "tracedecay_move_symbol"
            | "tracedecay_multi_str_replace"
            | "tracedecay_replace_symbol"
            | "tracedecay_str_replace"
    )
}

/// Read the on-disk mtime (UNIX seconds) for `relative_path` joined onto
/// `project_root`. Returns `None` when the file is missing or stat fails.
fn file_mtime_secs(project_root: &std::path::Path, relative_path: &str) -> Option<i64> {
    let abs = project_root.join(relative_path);
    let meta = std::fs::metadata(&abs).ok()?;
    let modified = meta.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(secs)
}

/// Render a duration in seconds as a compact phrase: `"5s ago"`,
/// `"3m ago"`, `"2h ago"`, `"4d ago"`. Used in the staleness banner so
/// the agent can judge how stale "still stale" actually is.
fn humanize_age(secs: i64) -> String {
    if secs < 60 {
        format!("{}s ago", secs.max(0))
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Inputs to the D7 overall-staleness banner decision, factored out so the
/// branch logic is unit-testable without a live server.
#[derive(Debug, Clone, Copy)]
struct StalenessBannerInputs {
    age_secs: i64,
    /// `SyncConfig.auto_watch || SyncConfig.read_refresh`.
    auto_sync_on: bool,
    /// Serving a read-only fallback/ancestor store (`fallback_warning().is_some()`).
    fallback_store: bool,
    /// A background refresh is currently in flight.
    refresh_running: bool,
    /// A background refresh completed within `read_cooldown_secs`.
    refreshed_recently: bool,
}

/// Decide the D7 overall-staleness banner. Returns `None` when no banner
/// should be emitted. The age guard (`> 3600s`) is applied by the caller.
///
/// Rules:
/// - Auto-sync on and not a fallback store: emit an informational "refresh in
///   progress / scheduled" note (or nothing if a refresh just completed);
///   NEVER instruct `tracedecay sync`.
/// - Auto-repair impossible (fallback store, or auto-sync fully disabled):
///   fall back to the manual `tracedecay sync` instruction.
fn staleness_banner(inputs: StalenessBannerInputs) -> Option<String> {
    let age_phrase = format_index_age_phrase(inputs.age_secs);
    let stale_mins = inputs.age_secs / 60;
    if inputs.auto_sync_on && !inputs.fallback_store {
        if inputs.refresh_running {
            Some(format!(
                "Note: index refresh in progress (was {stale_mins}m stale); \
                 very recent edits may not appear yet."
            ))
        } else if inputs.refreshed_recently {
            None
        } else {
            Some(format!(
                "Note: index refresh scheduled (was {stale_mins}m stale); \
                 very recent edits may not appear yet."
            ))
        }
    } else {
        Some(format!(
            "WARNING: Index last synced {age_phrase} ago. \
             Run `tracedecay sync` to update."
        ))
    }
}

/// Format the index-age phrase used by the overall-staleness banner (D7),
/// preserving the pre-existing `"Xd Yh"` / `"Xh Ym"` shape. `age_secs` is
/// assumed `> 3600` (the banner's guard); shorter ages still format sensibly.
fn format_index_age_phrase(age_secs: i64) -> String {
    let hours = age_secs / 3600;
    let mins = (age_secs % 3600) / 60;
    if hours >= 24 {
        format!("{}d {}h", hours / 24, hours % 24)
    } else {
        format!("{hours}h {mins}m")
    }
}

/// Whether an MCP tool result should be classified as a semantic failure for
/// analytics/`isError` purposes.
///
/// Handlers that build results structurally (e.g. edit tools, whose result
/// struct carries a `success: bool`) call
/// [`crate::mcp::tools::ToolResult::with_semantic_error`] to record the
/// outcome directly — that marker is authoritative and wins over the
/// rendered text. Handlers that have not been migrated to set the marker
/// leave it `None`, and this falls back to the pre-existing text-based
/// heuristic (`value_has_semantic_error`) that sniffs the rendered response
/// text for JSON failure shapes or known plain-text failure prefixes.
fn tool_result_has_semantic_error(result: &ToolResult) -> bool {
    result
        .semantic_error()
        .unwrap_or_else(|| value_has_semantic_error(&result.value))
}

fn value_has_semantic_error(value: &Value) -> bool {
    value
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|item| {
                let Some(text) = item.get("text").and_then(Value::as_str) else {
                    return false;
                };
                let trimmed = text.trim_start();
                if plain_text_tool_failure(trimmed) {
                    return true;
                }
                if !trimmed.starts_with('{') {
                    return false;
                }
                let Ok(payload) = serde_json::from_str::<Value>(trimmed) else {
                    return false;
                };
                payload.get("success").and_then(Value::as_bool) == Some(false)
                    || payload.get("error").is_some_and(|error| !error.is_null())
                    || payload
                        .get("failed")
                        .and_then(Value::as_u64)
                        .is_some_and(|failed| failed > 0)
                    || payload
                        .get("exit_code")
                        .is_some_and(|code| !code.is_null() && code.as_i64() != Some(0))
            })
        })
}

fn plain_text_tool_failure(text: &str) -> bool {
    text.starts_with("git error:") || text.starts_with("git diff failed:")
}

/// Reason to record for a semantically-failed tool result, for the
/// `failure_reason` analytics field. Prefers the handler-supplied structural
/// [`ToolResult::failure_message`] (e.g. an edit result's `message`, such as
/// "`old_str` not found"); falls back to the rendered response's first text
/// block for handlers that only signal failure via `value_has_semantic_error`
/// text heuristics. Callers must only invoke this once the result is already
/// known to be a semantic failure — it does not itself re-check that.
fn semantic_failure_reason(result: &ToolResult) -> Option<String> {
    if let Some(message) = result.failure_message() {
        return Some(message.to_string());
    }
    result
        .value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| {
            content
                .iter()
                .find_map(|item| item.get("text").and_then(Value::as_str))
        })
        .map(|text| text.trim_start().to_string())
}

fn mark_semantic_tool_error(result: &mut ToolResult) {
    if !tool_result_has_semantic_error(result) {
        return;
    }
    if let Some(obj) = result.value.as_object_mut() {
        obj.insert("isError".to_string(), json!(true));
    }
}

/// Map response-handle failures onto actionable JSON-RPC errors at the MCP
/// boundary so clients can distinguish bad input from cache/runtime problems.
fn tool_error_response(id: Value, tool_name: &str, error: &TraceDecayError) -> JsonRpcResponse {
    if tool_name == RESPONSE_RETRIEVE_TOOL {
        match error {
            TraceDecayError::Config { message }
                if message.starts_with("missing required parameter: handle") =>
            {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InvalidParams,
                    "tracedecay_retrieve requires the `handle` argument copied from a truncated MCP response envelope."
                        .to_string(),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "missing_handle_argument",
                        "retryable": false,
                        "retry_instruction": "Call `tracedecay_retrieve` again with the exact `handle` value emitted by the truncated response envelope."
                    })),
                );
            }
            TraceDecayError::Config { message }
                if message.starts_with("invalid response handle") =>
            {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InvalidParams,
                    message.clone(),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "invalid_handle",
                        "retryable": false,
                        "retry_instruction": "Pass the exact `handle` string from a truncated MCP response envelope; do not shorten or edit it."
                    })),
                );
            }
            TraceDecayError::Json(err) => {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InternalError,
                    format!(
                        "tool execution failed: cached response handle record is unreadable: {err}"
                    ),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "corrupt_handle_record",
                        "retryable": true,
                        "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle."
                    })),
                );
            }
            TraceDecayError::Io(err) => {
                return JsonRpcResponse::error_with_data(
                    id,
                    ErrorCode::InternalError,
                    format!("tool execution failed: failed to read cached response handle: {err}"),
                    Some(json!({
                        "tool": RESPONSE_RETRIEVE_TOOL,
                        "reason_code": "handle_read_failed",
                        "retryable": true,
                        "retry_instruction": "Fix the local project cache/filesystem issue, then re-run the original MCP tool to regenerate the full response and a fresh handle."
                    })),
                );
            }
            _ => {}
        }
    }

    let cli_name = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
    JsonRpcResponse::error_with_data(
        id,
        ErrorCode::InternalError,
        format!("tool execution failed: {error}"),
        Some(json!({
            "tool": tool_name,
            "cli_fallback": format!(
                "This tool is also available from the shell: `tracedecay tool {cli_name} ...` \
                 (`tracedecay tool {cli_name} --help` for parameters). If MCP calls keep \
                 failing or timing out, fall back to that CLI instead of querying \
                 .tracedecay databases directly."
            ),
        })),
    )
}

fn hardcoded_internal_error_response(id: &Value, detail: &str) -> String {
    let id_json = serde_json::to_string(id).unwrap_or_else(|_| "null".to_string());
    let detail_json = serde_json::to_string(detail)
        .unwrap_or_else(|_| "\"response serialization failed\"".to_string());
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id_json},\"error\":{{\"code\":-32603,\"message\":\"failed to serialize JSON-RPC response\",\"data\":{{\"reason_code\":\"response_serialization_failed\",\"detail\":{detail_json}}}}}}}"
    )
}

fn serialize_response_line(resp: &JsonRpcResponse) -> String {
    match serde_json::to_string(resp) {
        Ok(line) => line,
        Err(e) => {
            eprintln!("failed to serialize response: {e}");
            let fallback = JsonRpcResponse::error_with_data(
                resp.id.clone(),
                ErrorCode::InternalError,
                "failed to serialize JSON-RPC response".to_string(),
                Some(json!({
                    "reason_code": "response_serialization_failed",
                    "detail": e.to_string(),
                })),
            );
            serde_json::to_string(&fallback).unwrap_or_else(|fallback_err| {
                hardcoded_internal_error_response(&resp.id, &fallback_err.to_string())
            })
        }
    }
}

/// Cached result of a latest-version check against GitHub releases.
struct VersionCheckState {
    latest: Option<String>,
    checked_at: Option<Instant>,
}

/// Updates daemon ownership routing after this server changes physical graph DB.
/// Implementations must not call back into this `McpServer`: reconciliation is
/// awaited while the graph write guard is held so readers see the swap and
/// registry rekey atomically.
pub(crate) type DatabaseOwnerReconciler = Arc<
    dyn Fn(Arc<TraceDecay>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
>;

/// Complete hook-driven branch write requested by the MCP server.
///
/// `incremental_sync_agent` is set only for a routed worktree operation. In
/// that case the writer must keep any opened [`TraceDecay`] handle inside the
/// returned future until the conditional incremental sync has completed.
#[derive(Debug, Clone)]
pub(crate) struct HookBranchWriteRequest {
    pub(crate) root: PathBuf,
    pub(crate) branch: String,
    pub(crate) open_options: TraceDecayOpenOptions,
    pub(crate) incremental_sync_agent: Option<HookAgent>,
}

/// Metadata returned after the complete hook branch write has settled. No
/// writable store handle crosses the capability boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HookBranchWriteResult {
    pub(crate) branch_outcome: crate::branch::BranchAddOutcome,
    pub(crate) refresh_file_token_map: bool,
}

/// Injectable ownership boundary for hook-driven branch writes.
///
/// Daemon construction can wrap [`execute_hook_branch_write_direct`] in its
/// store-administration coordinator so the permit covers branch creation and,
/// for routed worktrees, the subsequent open and incremental sync.
pub(crate) type HookBranchWriter = Arc<
    dyn Fn(
            HookBranchWriteRequest,
        ) -> Pin<Box<dyn Future<Output = Result<HookBranchWriteResult>> + Send>>
        + Send
        + Sync
        + 'static,
>;

fn direct_hook_branch_writer() -> HookBranchWriter {
    Arc::new(|request| Box::pin(execute_hook_branch_write_direct(request)))
}

pub(crate) async fn execute_hook_branch_write_direct(
    request: HookBranchWriteRequest,
) -> Result<HookBranchWriteResult> {
    let branch_outcome = TraceDecay::add_branch_tracking_with_options(
        &request.root,
        &request.branch,
        request.open_options.clone(),
    )
    .await?;
    let mut refresh_file_token_map = false;

    if branch_outcome == crate::branch::BranchAddOutcome::AlreadyTracked
        && let Some(agent) = request.incremental_sync_agent
    {
        let worktree_cg =
            TraceDecay::open_with_options(&request.root, request.open_options).await?;
        refresh_file_token_map = run_hook_incremental_sync_direct(&worktree_cg, agent).await?;
    }

    Ok(HookBranchWriteResult {
        branch_outcome,
        refresh_file_token_map,
    })
}

async fn run_hook_incremental_sync_direct(cg: &TraceDecay, agent: HookAgent) -> Result<bool> {
    let marker = hook_events::sync_marker_path(&cg.store_layout().data_root, agent);
    let now = crate::tracedecay::current_timestamp();
    if !hook_events::should_run_sync(&marker, now, 3) {
        return Ok(false);
    }
    match cg.sync().await {
        Ok(_) | Err(TraceDecayError::SyncLock { .. }) => {
            hook_events::write_sync_marker(&marker, now);
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

/// Complete detached read-refresh write requested by the MCP server.
#[derive(Debug, Clone)]
pub(crate) struct BackgroundRefreshRequest {
    pub(crate) project_root: PathBuf,
    pub(crate) open_options: TraceDecayOpenOptions,
    pub(crate) full_sync_escalation_files: usize,
}

/// Injectable ownership boundary for a detached read refresh. The returned
/// token map is the only state allowed to cross back into the server; any
/// temporary [`TraceDecay`] handle must remain inside the callback future.
pub(crate) type BackgroundRefreshWriter = Arc<
    dyn Fn(
            BackgroundRefreshRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Option<HashMap<String, u64>>>> + Send>>
        + Send
        + Sync
        + 'static,
>;

fn direct_background_refresh_writer() -> BackgroundRefreshWriter {
    Arc::new(|request| Box::pin(execute_background_refresh_direct(request)))
}

pub(crate) async fn execute_background_refresh_direct(
    request: BackgroundRefreshRequest,
) -> Result<Option<HashMap<String, u64>>> {
    let cg = TraceDecay::open_with_options(&request.project_root, request.open_options).await?;
    let scoped = match cg.last_synced_commit().await {
        Some(base) => cg.stale_files_since_commit(&base, request.full_sync_escalation_files),
        None => None,
    };
    let result = if let Some(files) = scoped {
        if files.is_empty() {
            Ok(())
        } else {
            cg.sync_if_stale_silent(&files).await
        }
    } else {
        let stale = cg.find_stale_files().await;
        if stale.is_empty() {
            Ok(())
        } else {
            cg.sync_if_stale_silent(&stale).await
        }
    };
    if let Err(error) = result {
        eprintln!("[tracedecay] background read refresh failed: {error}");
    }
    Ok(cg.get_file_token_map().await.ok())
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
    stats: ServerStats,
    method_call_counts: std::sync::Mutex<HashMap<String, u64>>,
    resource_read_counts: std::sync::Mutex<HashMap<String, u64>>,
    tool_call_counts: std::sync::Mutex<HashMap<String, u64>>,
    diagnostics_cache: crate::diagnostics::DiagnosticsCache,
    diagnostics_lsp: tokio::sync::Mutex<crate::diagnostics::lsp::broker::DiagnosticBroker>,
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
    global_db: Option<Arc<GlobalDb>>,
    /// Registry used for project-selector reads. This remains available even
    /// when global accounting is disabled so daemon clients do not fall back
    /// to the daemon process profile for selector resolution.
    registry_db: Option<Arc<GlobalDb>>,
    allow_default_registry_fallback: bool,
    automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
    database_owner_reconciler: Option<DatabaseOwnerReconciler>,
    dashboard_automation_writer: crate::dashboard::DashboardAutomationWriter,
    hook_branch_writer: HookBranchWriter,
    background_refresh_writer: BackgroundRefreshWriter,
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
    /// Flipped to `true` once the *synchronous* portion of
    /// [`Self::run_startup_catch_up_sync`] finishes — i.e. the file-tree
    /// walk and index sync. The detached transcript-ingest spawn is tracked
    /// separately by [`Self::transcript_ingest_done`].
    startup_catch_up_done: AtomicBool,
    /// Guards the one-shot startup catch-up spawn (D1). `compare_exchange`d
    /// from `false` to `true` by the first caller so the catch-up runs at
    /// most once per server even if two `new_with_dbs` paths race. Distinct
    /// from [`startup_catch_up_done`](Self::startup_catch_up_done), which
    /// tracks *completion* rather than *dispatch*.
    startup_catch_up_started: AtomicBool,
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
    /// Flipped to `true` when the detached transcript-ingest task spawned
    /// inside [`Self::run_startup_catch_up_sync`] completes (success or
    /// timeout). Stored as `Arc<AtomicBool>` so the spawned task can hold a
    /// cheap clone and signal completion without a raw-pointer round-trip.
    transcript_ingest_done: Arc<AtomicBool>,
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
    /// Stable per-process instance id from
    /// [`crate::runtime_identity::process_run_id`] (a random hex token minted
    /// once per process). Recorded in every analytics event's
    /// `metadata.mcp_instance_id`. Hoisting it to `runtime_identity` lets the
    /// daemon later stamp the *same* id on its own events, grouping one process
    /// lifetime across both the MCP server and the daemon.
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
    mcp_instance_id: String,
}

impl McpServer {
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
        let registry_db = GlobalDb::open().await.map(Arc::new);
        let global_db: Option<Arc<GlobalDb>> = if crate::global_db::global_accounting_enabled() {
            registry_db.clone()
        } else {
            None
        };
        Self::new_with_dbs(cg, scope_prefix, global_db, registry_db, true).await
    }

    pub async fn new_with_global_db(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<GlobalDb>>,
    ) -> Arc<Self> {
        Self::new_with_dbs(cg, scope_prefix, global_db.clone(), global_db, true).await
    }

    pub async fn new_with_dbs(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<GlobalDb>>,
        registry_db: Option<Arc<GlobalDb>>,
        allow_default_registry_fallback: bool,
    ) -> Arc<Self> {
        Self::new_with_dbs_and_automation_reconciler(
            cg,
            scope_prefix,
            global_db,
            registry_db,
            allow_default_registry_fallback,
            None,
        )
        .await
    }

    pub(crate) async fn new_with_dbs_and_automation_reconciler(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<GlobalDb>>,
        registry_db: Option<Arc<GlobalDb>>,
        allow_default_registry_fallback: bool,
        automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
    ) -> Arc<Self> {
        Self::new_with_dbs_and_reconcilers(
            cg,
            scope_prefix,
            global_db,
            registry_db,
            allow_default_registry_fallback,
            automation_scheduler_reconciler,
            None,
        )
        .await
    }

    pub(crate) async fn new_with_dbs_and_reconcilers(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<GlobalDb>>,
        registry_db: Option<Arc<GlobalDb>>,
        allow_default_registry_fallback: bool,
        automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
        database_owner_reconciler: Option<DatabaseOwnerReconciler>,
    ) -> Arc<Self> {
        Self::new_with_dbs_and_reconcilers_and_writers(
            cg,
            scope_prefix,
            global_db,
            registry_db,
            allow_default_registry_fallback,
            automation_scheduler_reconciler,
            database_owner_reconciler,
            crate::dashboard::direct_dashboard_automation_writer(),
            direct_hook_branch_writer(),
            direct_background_refresh_writer(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new_with_dbs_and_reconcilers_and_writers(
        cg: TraceDecay,
        scope_prefix: Option<String>,
        global_db: Option<Arc<GlobalDb>>,
        registry_db: Option<Arc<GlobalDb>>,
        allow_default_registry_fallback: bool,
        automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
        database_owner_reconciler: Option<DatabaseOwnerReconciler>,
        dashboard_automation_writer: crate::dashboard::DashboardAutomationWriter,
        hook_branch_writer: HookBranchWriter,
        background_refresh_writer: BackgroundRefreshWriter,
    ) -> Arc<Self> {
        let file_token_map = cg.get_file_token_map().await.unwrap_or_default();
        let persisted = cg.get_tokens_saved().await.unwrap_or(0);
        let response_handle_project_root = cg.project_root().to_path_buf();
        // Register this project in the global DB with its current tokens
        if let Some(ref gdb) = global_db {
            gdb.upsert(cg.project_root(), persisted).await;
        }

        // Detect borrowed-worktree index once at startup so every read
        // tool can cheaply prefix a heads-up. Two git rev-parse spawns
        // worst case (#312). spawn_blocking because the underlying
        // `Command::output()` can sit on slow disks.
        let worktree_mismatch = {
            let project_root = cg.project_root().to_path_buf();
            tokio::task::spawn_blocking(move || {
                let cwd = std::env::current_dir().ok()?;
                crate::worktree::detect_worktree_index_mismatch(&cwd, &project_root)
            })
            .await
            .ok()
            .flatten()
        };

        // Resolve the [sync] config once (D1/D4/D7 all read it). Loading it
        // here keeps the per-call read path free of config-file IO.
        let sync_config = crate::config::load_sync_config(cg.project_root());
        let telemetry_config = crate::config::load_telemetry_config(cg.project_root());
        let code_diagnostics_settings =
            crate::diagnostics::lsp::settings::load_settings(&cg.store_layout().dashboard_root)
                .await
                .unwrap_or_default();
        let diagnostics_lsp = crate::dashboard::code_diagnostics_broker(
            cg.project_root().to_path_buf(),
            code_diagnostics_settings,
        );

        let server = Arc::new(Self {
            cg: tokio::sync::RwLock::new(Arc::new(cg)),
            stats: ServerStats::new(),
            method_call_counts: std::sync::Mutex::new(HashMap::new()),
            resource_read_counts: std::sync::Mutex::new(HashMap::new()),
            tool_call_counts: std::sync::Mutex::new(HashMap::new()),
            diagnostics_cache: crate::diagnostics::DiagnosticsCache::default(),
            diagnostics_lsp: tokio::sync::Mutex::new(diagnostics_lsp),
            file_token_map: Arc::new(std::sync::Mutex::new(file_token_map)),
            tokens_saved: AtomicU64::new(persisted),
            last_flushed_tokens: AtomicU64::new(persisted),
            last_flush_at: AtomicI64::new(0),
            global_db,
            registry_db,
            allow_default_registry_fallback,
            automation_scheduler_reconciler,
            database_owner_reconciler,
            dashboard_automation_writer,
            hook_branch_writer,
            background_refresh_writer,
            initialize_root_routing_enabled: AtomicBool::new(true),
            hook_project_routes: SharedHookProjectRouteCache::default(),
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
            startup_catch_up_done: AtomicBool::new(true),
            startup_catch_up_started: AtomicBool::new(false),
            background_refresh_running: Arc::new(AtomicBool::new(false)),
            last_background_refresh_at: AtomicI64::new(0),
            last_background_refresh_done_at: Arc::new(AtomicI64::new(0)),
            sync_config,
            transcript_ingest_done: Arc::new(AtomicBool::new(true)),
            ledger_writes_started: Arc::new(AtomicU64::new(0)),
            ledger_writes_finished: Arc::new(AtomicU64::new(0)),
            ledger_write_notify: Arc::new(tokio::sync::Notify::new()),
            span_observation_debounce: std::sync::Mutex::new(
                crate::sessions::git_correlation::SpanObservationDebounce::new(),
            ),
            client_name: std::sync::Mutex::new(None),
            // Shared process run id: the daemon should stamp this same id on its
            // own events so one process lifetime groups across both surfaces.
            // Field/metadata key stay `mcp_instance_id` (no analytics schema
            // churn) even though the id now lives in `runtime_identity`.
            mcp_instance_id: crate::runtime_identity::process_run_id().to_string(),
        });

        tokio::task::spawn_blocking(move || {
            let _ = cleanup_expired_response_handles(
                &response_handle_project_root,
                crate::tracedecay::current_timestamp(),
            );
        });

        // D1: startup catch-up sync. Reconciles changes made while the server
        // was down (terminal `git pull`, IDE edits before launch, another
        // tool's writes) so read-only sessions start fresh instead of serving
        // a stale index forever. `run_startup_catch_up_sync` is non-blocking-
        // safe (detached transcript ingest, flags flipped on every exit path),
        // so we spawn it detached and return immediately.
        //
        // Gated on `SyncConfig.session_start_sync` (default true) and single-
        // flighted by `startup_catch_up_started` so it runs at most once per
        // server even if two `new_with_dbs` paths overlap.
        if server.sync_config.session_start_sync
            && server
                .startup_catch_up_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            // Clear the completion flags *before* spawning. They default to
            // `true` (so the no-sync path reports "done" immediately), and
            // `run_startup_catch_up_sync` only flips them to `false` once it
            // actually starts running. Without this pre-clear there is a
            // window between the spawn and the task's first instruction where
            // both flags still read `true`, so a caller that reaches
            // `wait_for_startup_catch_up` in that window observes "done" and
            // returns immediately — then the detached catch-up sync runs
            // concurrently with the caller's own work (e.g. racing it to index
            // a just-written file). Clearing here closes that window so the
            // wait blocks until the catch-up sync genuinely completes.
            server.startup_catch_up_done.store(false, Ordering::Release);
            server
                .transcript_ingest_done
                .store(false, Ordering::Release);
            let s = Arc::clone(&server);
            tokio::spawn(async move {
                s.run_startup_catch_up_sync().await;
            });
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

    /// Clones out the currently served `TraceDecay` instance. The lock is
    /// held only for the clone, never across an await on the instance.
    async fn cg_snapshot(&self) -> Arc<TraceDecay> {
        self.cg.read().await.clone()
    }

    async fn update_hook_workspace_route(
        &self,
        event: &hook_events::HookEvent,
        route_cache: &mut HookProjectRouteCache,
    ) {
        let route_cwd = HookProjectRouteCache::route_cwd(event);
        let project_path = match route_cwd {
            Some(cwd) => self.registered_project_containing_path(cwd).await,
            None => None,
        };
        route_cache.observe_hook_event(event, project_path);
        self.hook_project_routes.store(route_cache);
    }

    async fn registered_project_containing_path(&self, cwd: &Path) -> Option<String> {
        let registry = self.registry_db.as_deref()?;
        let mut candidate = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        loop {
            if let Some(context) = registry.project_registry_context_by_alias(&candidate).await {
                return Some(context.project.canonical_root);
            }
            if !candidate.pop() {
                return None;
            }
        }
    }

    /// Resolves the registered project for a hook route `cwd`, first by the
    /// parent-directory alias walk (see
    /// [`Self::registered_project_containing_path`]) and, when that misses,
    /// by git-common-dir identity. A linked worktree lives at a sibling path,
    /// so the alias walk never reaches its main checkout; identity resolution
    /// maps it back to the registered project through the shared common dir.
    async fn registered_project_for_route_cwd(&self, cwd: &Path) -> Option<String> {
        if let Some(root) = self.registered_project_containing_path(cwd).await {
            return Some(root);
        }
        let registry = self.registry_db.as_deref()?;
        let git_common_dir = crate::worktree::git_common_dir(cwd);
        let context = registry
            .project_registry_context_by_identity(cwd, git_common_dir.as_deref())
            .await?;
        Some(context.project.canonical_root)
    }

    /// Detects mid-session branch drift and reopens the served instance
    /// onto the live branch's DB, returning the instance the caller should
    /// use for this request.
    ///
    /// Fast path: one cheap `branch_drifted` check (gix HEAD read) on the
    /// current snapshot. On drift, the write lock serializes the swap and
    /// the drift check is repeated under it so concurrent calls reopen at
    /// most once. If reopening fails the previous instance is kept — the
    /// drift guards in [`TraceDecay::ensure_branch_writable`] and
    /// [`Self::maybe_sync_if_stale`] still protect writes, exactly as
    /// before this hot-swap existed.
    async fn reopen_if_branch_drifted(&self) -> Arc<TraceDecay> {
        let current = self.cg_snapshot().await;
        if !current.branch_drifted() {
            return current;
        }
        let snapshot = {
            let mut guard = self.cg.write().await;
            if !guard.branch_drifted() {
                // A concurrent call already swapped (or the user switched back).
                return guard.clone();
            }
            match guard.reopen_for_current_branch().await {
                Ok(fresh) => {
                    eprintln!(
                        "[tracedecay] branch changed to '{}' — reopened the index for it",
                        fresh.active_branch().unwrap_or("<detached>")
                    );
                    *guard = Arc::new(fresh);
                    let fresh = guard.clone();
                    if let Some(reconcile) = &self.database_owner_reconciler {
                        reconcile(fresh.clone()).await;
                    }
                    fresh
                }
                Err(e) => {
                    eprintln!(
                        "[tracedecay] branch drift detected but reopen failed: {e}; \
                         continuing to serve branch '{}'",
                        guard.serving_branch().unwrap_or("<none>")
                    );
                    return guard.clone();
                }
            }
        };
        // New branch DB ⇒ new file set; refresh the token accounting map.
        self.refresh_file_token_map().await;
        snapshot
    }

    async fn reopen_after_branch_tracking_added(&self) {
        let reopened = {
            let mut guard = self.cg.write().await;
            match guard.reopen_for_current_branch().await {
                Ok(fresh) => {
                    eprintln!(
                        "[tracedecay] branch tracking added for '{}' — reopened the index for it",
                        fresh.active_branch().unwrap_or("<detached>")
                    );
                    *guard = Arc::new(fresh);
                    let fresh = guard.clone();
                    if let Some(reconcile) = &self.database_owner_reconciler {
                        reconcile(fresh.clone()).await;
                    }
                    Some(fresh)
                }
                Err(e) => {
                    eprintln!(
                        "[tracedecay] hook branch tracking added but reopen failed: {e}; \
                         continuing to serve branch '{}'",
                        guard.serving_branch().unwrap_or("<none>")
                    );
                    None
                }
            }
        };
        if reopened.is_some() {
            self.refresh_file_token_map().await;
        }
    }

    /// Estimates the raw-file token cost ("before") for the given file
    /// paths from the cached file-token map (indexed file bytes / 4).
    /// Pure lookup — persists nothing.
    fn estimate_raw_file_tokens(&self, file_paths: &[String]) -> u64 {
        if file_paths.is_empty() {
            return 0;
        }
        debug_assert!(
            file_paths.iter().all(|p| !p.is_empty()),
            "estimate_raw_file_tokens received empty file path"
        );
        let Ok(map) = self.file_token_map.lock() else {
            return 0;
        };
        file_paths
            .iter()
            .filter_map(|path| map.get(path.as_str()))
            .sum()
    }

    /// Adds `delta` saved tokens to the running counter and persists it.
    ///
    /// `delta` must already be the *net* saving for one call
    /// (`before.saturating_sub(after)`), not the gross raw-file estimate:
    /// crediting the full "before" would count a full-file read whose
    /// response contains the entire file as 100% saved.
    async fn persist_saved_tokens(&self, delta: u64) {
        if delta == 0 {
            return;
        }
        let new_total = self.tokens_saved.fetch_add(delta, Ordering::Relaxed) + delta;
        let cg = self.cg_snapshot().await;
        // Persist to DB (best-effort, don't block on failure)
        let _ = cg.set_tokens_saved(new_total).await;
        // Also increment the resettable local counter
        let _ = cg.add_local_counter(delta).await;
        // Best-effort update to global DB
        if let Some(ref gdb) = self.global_db {
            gdb.upsert(cg.project_root(), new_total).await;
        }
    }

    /// Resolves once every savings-ledger write spawned so far has
    /// completed (immediately when none are pending — including when global
    /// accounting is disabled and no writes are ever spawned).
    ///
    /// Test-only observability for the fire-and-forget ledger recorder:
    /// production code never calls this, so the request path stays
    /// non-blocking, while tests can await durability deterministically
    /// instead of polling the DB against a wall-clock deadline.
    ///
    /// Bounded by [`LEDGER_SETTLE_TIMEOUT`] so a spawned write that wedges
    /// (a stuck DB handle, a task that never resolves) can never hang the
    /// caller forever — the earlier unbounded loop made a wedged write
    /// manifest as an un-observable, indefinitely-hung integration test.
    pub async fn ledger_writes_settled(&self) {
        self.ledger_writes_settled_within(LEDGER_SETTLE_TIMEOUT)
            .await;
    }

    /// Like [`Self::ledger_writes_settled`] but bounded by an explicit
    /// `timeout`. Returns `true` when every spawned ledger write settled
    /// within the bound, `false` when the bound elapsed with writes still
    /// pending. A timeout is never silent: it logs a warning naming how many
    /// writes were still outstanding so a wedged recorder is diagnosable.
    pub async fn ledger_writes_settled_within(&self, timeout: std::time::Duration) -> bool {
        let wait = async {
            loop {
                // Register interest *before* re-checking so a completion
                // between the check and the await cannot be missed.
                let notified = self.ledger_write_notify.notified();
                let started = self.ledger_writes_started.load(Ordering::SeqCst);
                let finished = self.ledger_writes_finished.load(Ordering::SeqCst);
                if finished >= started {
                    return;
                }
                notified.await;
            }
        };
        if tokio::time::timeout(timeout, wait).await.is_ok() {
            return true;
        }
        let started = self.ledger_writes_started.load(Ordering::SeqCst);
        let finished = self.ledger_writes_finished.load(Ordering::SeqCst);
        let pending = started.saturating_sub(finished);
        eprintln!(
            "[tracedecay] ledger_writes_settled timed out after {timeout:?} with \
             {pending} savings-ledger write(s) still pending"
        );
        false
    }

    /// Test-only hook: spawn an observed ledger write that never completes, so
    /// tests can prove [`Self::ledger_writes_settled`] stays bounded when a
    /// recorder task wedges. Uses the same [`Self::spawn_observed_ledger_write`]
    /// accounting the production path uses, so the counters advance identically.
    #[cfg(test)]
    pub(crate) fn spawn_wedged_ledger_write_for_test(&self) {
        self.spawn_observed_ledger_write(std::future::pending::<()>());
    }

    /// Re-read the file-to-token-count map from the DB and swap it into the
    /// cached `file_token_map`. Called after each lazy sync triggered by
    /// [`maybe_sync_if_stale`](Self::maybe_sync_if_stale) so the accounting
    /// tracks newly indexed / removed files.
    pub async fn refresh_file_token_map(&self) {
        // best-effort; leave stale map in place if the DB read fails
        let Ok(fresh) = self.cg_snapshot().await.get_file_token_map().await else {
            return;
        };
        if let Ok(mut guard) = self.file_token_map.lock() {
            *guard = fresh;
        }
    }

    /// Catch-up sync helper for tests and explicit callers. Bypasses the 30 s
    /// cooldown in [`Self::maybe_sync_if_stale`] so changes made while the
    /// server was down — a terminal `git pull`, IDE edits before the agent
    /// launched, files touched by another tool — can be reconciled before
    /// assertions or source-editing work. The staleness-check stamp is updated
    /// on the way out so the next lazy sync doesn't immediately re-walk the
    /// tree.
    ///
    /// The completion flag is flipped on every exit path (including
    /// errors) so [`Self::wait_for_startup_catch_up`] never hangs.
    pub async fn run_startup_catch_up_sync(&self) {
        self.startup_catch_up_done.store(false, Ordering::Release);
        self.transcript_ingest_done.store(false, Ordering::Release);

        let cg = self.cg_snapshot().await;
        let stale = cg.find_stale_files().await;
        if !stale.is_empty() {
            if let Err(e) = cg.sync_if_stale_silent(&stale).await {
                eprintln!("[tracedecay] startup catch-up sync failed: {e}");
                self.startup_catch_up_done.store(true, Ordering::Release);
                self.transcript_ingest_done.store(true, Ordering::Release);
                return;
            }
        }
        self.refresh_file_token_map().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.last_staleness_check_at.store(now, Ordering::Release);

        // Best-effort transcript ingestion sweep for hookless agents (Claude,
        // Codex, Gemini). Cursor ingests via its own end-of-turn hook; these
        // agents register no hook, so their transcripts are reconciled here.
        // Detached + timeout-guarded so it never delays MCP readiness.
        // `transcript_ingest_done` is flipped inside the spawn (via an Arc
        // clone) so tests that assert on LCM store content can wait for both
        // flags via `wait_for_startup_catch_up`.
        {
            let project_root = cg.project_root().to_path_buf();
            let session_db_path = cg.store_layout().sessions_db_path.clone();
            let ingest_done_flag = Arc::clone(&self.transcript_ingest_done);
            let analytics_db = self.global_db.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(20), async move {
                    if let Some(db) = GlobalDb::open_at(&session_db_path).await {
                        let _ = crate::sessions::ingest_global_sources_for_startup(
                            &db,
                            &project_root,
                        )
                        .await;
                        // Historical git-span correlation is only ever written by
                        // live hook events (which never fire for stdio/daemonless
                        // deployments) or a manual CLI backfill. Neither runs for
                        // most projects, leaving `session_git_spans` empty so
                        // `sessions_for` silently returns nothing. Drain that
                        // history here — one bounded, watermarked pass per startup
                        // — so correlation self-heals without a manual invocation.
                        let git = crate::sessions::git_correlation::SystemGit;
                        let _ = db
                            .git_run_incremental_backfill(
                                &git,
                                crate::sessions::git_correlation::DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS,
                            )
                            .await;
                        // With transcripts freshly ingested into `db`'s
                        // session_messages, close the hint-efficacy loop: import
                        // any new hook telemetry into the durable analytics store
                        // and correlate emitted hints against the tool activity
                        // that followed them. Best-effort and idempotent (own
                        // parse cursors + hint_outcome watermark), so it never
                        // blocks readiness and re-runs safely each startup.
                        if let Some(analytics_db) = analytics_db.as_deref() {
                            let sources =
                                crate::analytics_bridge::hook_import_sources(Some(&project_root));
                            let _ = crate::analytics_bridge::import_hook_analytics(
                                analytics_db,
                                &sources,
                            )
                            .await;
                            let project_id = GlobalDb::canonical_project_key(&project_root);
                            let now = crate::tracedecay::current_timestamp();
                            let _ = crate::hooks::hint_outcomes::correlate_hint_outcomes(
                                analytics_db,
                                &db,
                                &project_id,
                                now,
                            )
                            .await;
                        }
                    }
                })
                .await;
                ingest_done_flag.store(true, Ordering::Release);
            });
        }

        self.startup_catch_up_done.store(true, Ordering::Release);
    }

    /// Returns `true` once the *synchronous* portion of
    /// [`Self::run_startup_catch_up_sync`] has finished (the file-tree walk
    /// and index sync). See [`Self::transcript_ingest_done`] for the
    /// detached ingest task.
    pub fn startup_catch_up_done(&self) -> bool {
        self.startup_catch_up_done.load(Ordering::Acquire)
    }

    /// Returns `true` once the detached transcript-ingest task spawned by
    /// [`Self::run_startup_catch_up_sync`] has completed (success, error,
    /// or 20 s timeout).
    pub fn transcript_ingest_done(&self) -> bool {
        self.transcript_ingest_done.load(Ordering::Acquire)
    }

    /// Polls until both the synchronous catch-up sync *and* the detached
    /// transcript-ingest task have completed, or until `timeout` elapses.
    /// Returns `true` if both completed within the budget.
    ///
    /// Tests use this so neither the index walk nor the transcript ingest
    /// races against later DB assertions.
    pub async fn wait_for_startup_catch_up(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while !self.startup_catch_up_done() || !self.transcript_ingest_done() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        true
    }

    /// Walk the project tree, sync any stale files, and refresh the
    /// file-to-token-count map — but only if at least 30 s have passed
    /// since the last successful sync. The cooldown is the gate: while
    /// it holds, this returns immediately, so dropping it into every
    /// `tools/call` handler is cheap.
    ///
    /// Concurrent callers are serialized via
    /// [`Self::last_staleness_check_at`]: the first caller stamps `now`
    /// into the field with `compare_exchange`; later callers within the
    /// same window see the stamp and bail. If the actual sync work
    /// fails, the stamp still advances — failure to walk the tree
    /// should not cause every subsequent tool call to retry.
    pub async fn maybe_sync_if_stale(&self) {
        let cg = self.cg_snapshot().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let previous = self.last_staleness_check_at.load(Ordering::Acquire);
        let last_sync = cg.last_sync_timestamp().await;
        if previous != 0 && now.saturating_sub(last_sync) < 30 {
            return;
        }

        if now.saturating_sub(previous) < 30 {
            return;
        }
        if self
            .last_staleness_check_at
            .compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        // Branch-drift guard (#2): if the working tree switched branches since
        // this snapshot opened, the cached DB belongs to the old branch. Skip
        // the lazy sync — `find_stale_files` would diff the new branch's files
        // against the old branch's DB, and `ensure_branch_writable` would
        // reject the write anyway. `tools/call` reopens onto the live branch
        // via [`Self::reopen_if_branch_drifted`] *before* invoking this, so
        // the guard only fires on a checkout racing the current call.
        if cg.branch_drifted() {
            return;
        }

        let stale = cg.find_stale_files().await;
        if !stale.is_empty() {
            if let Err(e) = cg.sync_if_stale_silent(&stale).await {
                eprintln!("[tracedecay] lazy sync failed: {e}");
                return;
            }
        }
        // Always refresh: a sibling MCP peer may have synced the DB
        // between our cooldown windows, in which case `stale` is empty
        // here but our in-memory `file_token_map` is still pre-sync.
        self.refresh_file_token_map().await;
    }

    /// D4: sync-on-read entry point for read (non-edit) tools. NEVER blocks.
    ///
    /// If read-refresh is enabled and the read cooldown has elapsed since the
    /// last background spawn, this `compare_exchange`s
    /// [`background_refresh_running`](Self::background_refresh_running) to
    /// `true` and spawns a detached refresh, then returns immediately so the
    /// caller serves the current answer with zero added latency. The *next*
    /// read observes the freshly synced index.
    ///
    /// Single-flighted three ways: the `read_cooldown_secs` stamp, the
    /// `background_refresh_running` flag, and the underlying cross-process
    /// sync lock. At most one refresh runs at a time.
    fn maybe_spawn_read_refresh(&self, cg: &Arc<TraceDecay>) {
        if !self.sync_config.read_refresh {
            return;
        }
        // A checkout racing this call would diff the new branch against the
        // old branch's DB; `tools/call` reopens onto the live branch before
        // dispatch, so this only fires on an in-flight race. Skip it — the
        // next call runs on the reopened snapshot.
        if cg.branch_drifted() {
            return;
        }

        let now = crate::tracedecay::current_timestamp();
        let cooldown = self.sync_config.read_cooldown_secs as i64;
        let previous = self.last_background_refresh_at.load(Ordering::Acquire);
        if previous != 0 && now.saturating_sub(previous) < cooldown {
            return;
        }
        // Reserve the cooldown slot. If another read call won the race, bail.
        if self
            .last_background_refresh_at
            .compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Reserve the single-flight slot. If a refresh is already running
        // (e.g. a slow prior spawn that outlived its cooldown), don't stack.
        if self
            .background_refresh_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        self.spawn_read_refresh_task(cg, self.sync_config.full_sync_escalation_files);
    }

    /// Spawns the detached D4 refresh task. The task owns cheap `Arc` clones
    /// of the background-refresh flag, the completion stamp, and the shared
    /// file-token map, so no `Arc<Self>` receiver is needed. Prefers diff-
    /// scoping off `last_synced_commit`; falls back to the full tree walk
    /// when no base commit is stamped or the diff escalates past the limit.
    ///
    /// The caller MUST have already set `background_refresh_running` to
    /// `true`; this task clears it on completion.
    fn spawn_read_refresh_task(&self, cg: &Arc<TraceDecay>, escalation: usize) {
        let running = Arc::clone(&self.background_refresh_running);
        let done_at = Arc::clone(&self.last_background_refresh_done_at);
        let token_map = Arc::clone(&self.file_token_map);
        let refresh = Arc::clone(&self.background_refresh_writer);
        let request = BackgroundRefreshRequest {
            project_root: cg.project_root().to_path_buf(),
            open_options: cg.open_options(),
            full_sync_escalation_files: escalation,
        };
        tokio::spawn(async move {
            match refresh(request).await {
                Ok(Some(fresh)) => {
                    if let Ok(mut guard) = token_map.lock() {
                        *guard = fresh;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[tracedecay] background read refresh could not reopen project: {e}");
                }
            }
            done_at.store(crate::tracedecay::current_timestamp(), Ordering::Release);
            running.store(false, Ordering::Release);
        });
    }

    /// D1/daemon hook: refresh the index if this cached project server's last
    /// sync is older than `threshold_secs`. Called by the daemon on a
    /// `project_server` cache hit so a long-lived cached server heals like a
    /// freshly launched one. Non-blocking: it kicks the same detached D4
    /// refresh and returns immediately.
    pub async fn refresh_if_session_stale(&self, threshold_secs: u64) {
        if !self.sync_config.read_refresh && !self.sync_config.auto_watch {
            return;
        }
        let cg = self.cg_snapshot().await;
        if cg.branch_drifted() {
            return;
        }
        let now = crate::tracedecay::current_timestamp();
        let last_sync = cg.last_sync_timestamp().await;
        if last_sync != 0 && now.saturating_sub(last_sync) < threshold_secs as i64 {
            return;
        }
        // Single-flight against the read-refresh machinery.
        if self
            .background_refresh_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.last_background_refresh_at
            .store(now, Ordering::Release);
        self.spawn_read_refresh_task(&cg, self.sync_config.full_sync_escalation_files);
    }

    /// Returns a compact one-line notice when automation runs have staged
    /// managed-skill output awaiting review that the user hasn't been told
    /// about yet. Fact proposal counts remain telemetry-only.
    ///
    /// Cheap by construction: a 60 s `compare_exchange` cooldown gates the
    /// check, and the underlying dedupe state
    /// ([`crate::automation::staged_notice`]) fires at most once per new
    /// batch (latest run id or pending-count change), so dropping this into
    /// every `tools/call` response is safe.
    async fn maybe_automation_staged_notice(&self, cg: &TraceDecay) -> Option<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let previous = self.last_automation_notice_check_at.load(Ordering::Acquire);
        if now.saturating_sub(previous) < 60 {
            return None;
        }
        if self
            .last_automation_notice_check_at
            .compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let profile_root = crate::storage::default_profile_root().ok()?;
        crate::automation::staged_notice::maybe_automation_staged_notice(
            &cg.store_layout().dashboard_root,
            &profile_root,
        )
        .await
    }

    /// Internal: snapshot of the current `file_token_map`. Exposed for
    /// integration tests only; not part of the stable public API.
    #[doc(hidden)]
    pub fn file_token_map_snapshot(&self) -> HashMap<String, u64> {
        self.file_token_map
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Flushes pending tokens to the worldwide counter if at least 30 seconds
    /// have elapsed since the last flush. Best-effort, never blocks for long.
    async fn maybe_flush_worldwide(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let last = self.last_flush_at.load(Ordering::Relaxed);
        if now - last < 30 {
            return;
        }
        // Mark as attempted immediately to prevent re-entry.
        self.last_flush_at.store(now, Ordering::Relaxed);

        let current = self.tokens_saved.load(Ordering::Relaxed);
        let last_flushed = self.last_flushed_tokens.load(Ordering::Relaxed);
        if current <= last_flushed {
            return;
        }
        let delta = current - last_flushed;

        if self.global_db.is_none() {
            return;
        }

        let success = tokio::task::spawn_blocking(move || {
            let mut config = crate::user_config::UserConfig::load();
            config.pending_upload += delta;
            if config.upload_enabled && crate::cloud::flush_pending(config.pending_upload).is_some()
            {
                config.pending_upload = 0;
                config.last_upload_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if let Err(err) = config.save() {
                    eprintln!("[tracedecay] warning: could not save config: {err}");
                }
                return true;
            }
            if let Err(err) = config.save() {
                eprintln!("[tracedecay] warning: could not save config: {err}");
            }
            false
        })
        .await
        .unwrap_or(false);

        if success {
            self.last_flushed_tokens.store(current, Ordering::Relaxed);
        }
    }

    /// Returns a version-update warning if a newer release is available.
    /// Results are cached for `VERSION_CHECK_INTERVAL` (15 minutes).
    async fn check_version_update(&self) -> Option<String> {
        let current = env!("CARGO_PKG_VERSION");

        // Fast path: serve from cache if still fresh.
        {
            let cache = self.version_cache.lock().ok()?;
            if let Some(checked_at) = cache.checked_at {
                if checked_at.elapsed() < VERSION_CHECK_INTERVAL {
                    let latest = cache.latest.as_deref()?;
                    return if crate::cloud::is_newer_minor_version(current, latest) {
                        Some(format!(
                            "⚠️ tracedecay v{current} is installed, but v{latest} is available. \
                             Run `tracedecay upgrade` to update."
                        ))
                    } else {
                        None
                    };
                }
            }
        }

        // Cache miss or expired – fetch from GitHub (best-effort, 1 s timeout).
        let latest = tokio::task::spawn_blocking(crate::cloud::fetch_latest_version)
            .await
            .ok()
            .flatten();

        // Update cache regardless of fetch outcome so we don't retry immediately.
        if let Ok(mut cache) = self.version_cache.lock() {
            cache.latest.clone_from(&latest);
            cache.checked_at = Some(Instant::now());
        }

        let latest = latest?;
        if crate::cloud::is_newer_minor_version(current, &latest) {
            Some(format!(
                "⚠️ tracedecay v{current} is installed, but v{latest} is available. \
                 Run `tracedecay upgrade` to update."
            ))
        } else {
            None
        }
    }

    /// Process a single raw JSON-RPC line and write the response.
    /// Used to replay a peeked `initialize` message that was consumed before
    /// the server's main loop started.
    pub async fn handle_and_write(
        &self,
        line: &str,
        transport: &mut impl super::transport::McpTransport,
    ) -> Result<()> {
        let parsed: std::result::Result<super::transport::JsonRpcRequest, _> =
            serde_json::from_str(line);
        let response = match parsed {
            Ok(request) => Box::pin(self.handle_request(&request)).await,
            Err(e) => Some(super::transport::JsonRpcResponse::error(
                Value::Null,
                super::transport::ErrorCode::ParseError,
                format!("failed to parse JSON-RPC request: {e}"),
            )),
        };
        if let Some(resp) = response {
            let mut json_str = serialize_response_line(&resp);
            json_str.push('\n');
            transport.write_line(&json_str).await?;
            transport.flush().await?;
        }
        Ok(())
    }

    /// Runs the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout. Runs until stdin is closed or a shutdown signal
    /// (SIGINT/SIGTERM) is received, then performs graceful cleanup.
    pub async fn run(&self, transport: &mut impl super::transport::McpTransport) -> Result<()> {
        self.run_with_shutdown_policy(transport, true, true, None, None)
            .await
    }

    /// Runs one client connection without shutting down the server when that
    /// connection closes. Daemon-owned servers use this so the engine remains
    /// shared across independent clients.
    pub async fn run_connection(
        &self,
        transport: &mut impl super::transport::McpTransport,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, None, None)
            .await
    }

    /// Runs one daemon client connection using connection-local timing
    /// settings. The shared server's default timing flag remains unchanged.
    pub async fn run_connection_with_timings(
        &self,
        transport: &mut impl super::transport::McpTransport,
        timings_enabled: bool,
    ) -> Result<()> {
        self.run_with_shutdown_policy(transport, false, false, Some(timings_enabled), None)
            .await
    }

    pub(crate) async fn run_daemon_connection_with_timings(
        &self,
        transport: &mut impl super::transport::McpTransport,
        timings_enabled: bool,
        lifecycle: &crate::daemon::DaemonLifecycle,
    ) -> Result<()> {
        self.run_with_shutdown_policy(
            transport,
            false,
            false,
            Some(timings_enabled),
            Some(lifecycle),
        )
        .await
    }

    async fn run_with_shutdown_policy(
        &self,
        transport: &mut impl super::transport::McpTransport,
        shutdown_on_exit: bool,
        listen_for_process_signals: bool,
        timings_override: Option<bool>,
        request_lifecycle: Option<&crate::daemon::DaemonLifecycle>,
    ) -> Result<()> {
        let mut route_cache = self.hook_project_routes.snapshot();

        // Register the SIGTERM listener once before entering the loop so
        // there is no window between iterations where a SIGTERM is delivered
        // but no handler is installed (which would cause silent loss of the
        // signal and skip the shutdown() flush).
        #[cfg(unix)]
        #[allow(clippy::expect_used)]
        let mut sigterm = listen_for_process_signals.then(|| {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler")
        });

        let mut connection_route = ConnectionRouteState::default();

        loop {
            let line: String = {
                #[cfg(unix)]
                {
                    if let Some(sigterm) = sigterm.as_mut() {
                        tokio::select! {
                            result = transport.read_line() => {
                                match result {
                                    Ok(Some(line)) => line,
                                    Ok(None) => break,
                                    Err(e) => {
                                        self.shutdown_if(shutdown_on_exit).await;
                                        return Err(e.into());
                                    }
                                }
                            }
                            _ = tokio::signal::ctrl_c() => break,
                            _ = sigterm.recv() => break,
                        }
                    } else if let Some(lifecycle) = request_lifecycle {
                        tokio::select! {
                            result = transport.read_line() => {
                                match result {
                                    Ok(Some(line)) => line,
                                    Ok(None) => break,
                                    Err(e) => {
                                        self.shutdown_if(shutdown_on_exit).await;
                                        return Err(e.into());
                                    }
                                }
                            }
                            () = lifecycle.wait_for_draining() => break,
                        }
                    } else {
                        match transport.read_line().await {
                            Ok(Some(line)) => line,
                            Ok(None) => break,
                            Err(e) => {
                                self.shutdown_if(shutdown_on_exit).await;
                                return Err(e.into());
                            }
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    if listen_for_process_signals {
                        tokio::select! {
                            result = transport.read_line() => {
                                match result {
                                    Ok(Some(line)) => line,
                                    Ok(None) => break,
                                    Err(e) => {
                                        self.shutdown_if(shutdown_on_exit).await;
                                        return Err(e.into());
                                    }
                                }
                            }
                            _ = tokio::signal::ctrl_c() => break,
                        }
                    } else {
                        match transport.read_line().await {
                            Ok(Some(line)) => line,
                            Ok(None) => break,
                            Err(e) => {
                                self.shutdown_if(shutdown_on_exit).await;
                                return Err(e.into());
                            }
                        }
                    }
                }
            };

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            // Parse the incoming JSON
            let parsed: std::result::Result<JsonRpcRequest, _> = serde_json::from_str(&line);
            let request_activity = request_lifecycle.and_then(|lifecycle| lifecycle.try_enter());
            let rejecting_for_drain = request_lifecycle.is_some() && request_activity.is_none();

            let response = if rejecting_for_drain {
                parsed.as_ref().ok().and_then(|request| {
                    request.id.clone().map(|id| {
                        JsonRpcResponse::error(
                            id,
                            ErrorCode::InternalError,
                            "TraceDecay daemon is draining for upgrade; retry the request"
                                .to_string(),
                        )
                    })
                })
            } else {
                match parsed {
                    Ok(request) => {
                        if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                            && self.initialize_root_routing_enabled.load(Ordering::Relaxed)
                        {
                            connection_route
                                .observe_initialize(
                                    request.params.as_ref(),
                                    self.registry_db.as_deref(),
                                )
                                .await;
                        }
                        Box::pin(self.handle_request_with_timings_and_implicit_project(
                            &request,
                            timings_override.unwrap_or_else(|| self.timings_enabled()),
                            &mut route_cache,
                            connection_route.implicit_project_path(),
                        ))
                        .await
                    }
                    Err(e) => Some(JsonRpcResponse::error(
                        Value::Null,
                        ErrorCode::ParseError,
                        format!("failed to parse JSON-RPC request: {e}"),
                    )),
                }
            };

            // Drain and write any pending notifications (e.g., version warnings).
            {
                let notifications: Vec<Value> = self
                    .pending_notifications
                    .lock()
                    .map(|mut p| p.drain(..).collect())
                    .unwrap_or_default();
                for notification in notifications {
                    if let Ok(s) = serde_json::to_string(&notification) {
                        if let Err(e) = transport.write_line(&format!("{s}\n")).await {
                            self.shutdown_if(shutdown_on_exit).await;
                            return Err(e.into());
                        }
                        if let Err(e) = transport.flush().await {
                            self.shutdown_if(shutdown_on_exit).await;
                            return Err(e.into());
                        }
                    }
                }
            }

            // Write response (if any) as a single line to stdout
            if let Some(resp) = response {
                let json_line = serialize_response_line(&resp);
                let output = format!("{json_line}\n");
                if let Err(e) = transport.write_line(&output).await {
                    eprintln!("failed to write response: {e}");
                    self.shutdown_if(shutdown_on_exit).await;
                    return Err(e.into());
                }
                if let Err(e) = transport.flush().await {
                    eprintln!("failed to flush stdout: {e}");
                    self.shutdown_if(shutdown_on_exit).await;
                    return Err(e.into());
                }
            }
            drop(request_activity);
            if rejecting_for_drain
                || request_lifecycle.is_some_and(|lifecycle| !lifecycle.accepting())
            {
                break;
            }
        }

        self.shutdown_if(shutdown_on_exit).await;
        Ok(())
    }

    async fn shutdown_if(&self, enabled: bool) {
        if enabled {
            self.shutdown().await;
        }
    }

    /// Persists the tokens-saved counter, flushes pending tokens to the
    /// worldwide counter, checkpoints the WAL, and logs a session summary.
    ///
    /// Idempotent — safe to call multiple times. `run` invokes it once when
    /// its main loop exits; callers (e.g. `main.rs`, tests) may invoke it
    /// explicitly afterwards without re-running the persistence logic.
    pub async fn shutdown(&self) {
        // Idempotency guard: only run the persistence path once.
        if self.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }

        let uptime = self.stats.started_at.elapsed();
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let tokens_saved = self.tokens_saved.load(Ordering::Relaxed);

        let cg = self.cg_snapshot().await;
        // Persist final tokens-saved value
        if let Err(e) = cg.set_tokens_saved(tokens_saved).await {
            eprintln!("[tracedecay] warning: failed to persist tokens_saved on shutdown: {e}");
        }

        // Update global DB with final count and checkpoint it
        if let Some(ref gdb) = self.global_db {
            gdb.upsert(cg.project_root(), tokens_saved).await;
            gdb.checkpoint().await;
        }

        // Flush remaining delta to worldwide counter (what periodic flushes missed)
        let last_flushed = self.last_flushed_tokens.load(Ordering::Relaxed);
        if self.global_db.is_some() && tokens_saved > last_flushed {
            let delta = tokens_saved - last_flushed;
            let mut config = crate::user_config::UserConfig::load();
            config.pending_upload += delta;
            if config.upload_enabled {
                if let Some(_total) = crate::cloud::flush_pending(config.pending_upload) {
                    config.pending_upload = 0;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    config.last_upload_at = now;
                }
            }
            if let Err(err) = config.save() {
                eprintln!("[tracedecay] warning: could not save config: {err}");
            }
        }

        // Checkpoint WAL to merge it into the main database file
        if let Err(e) = cg.checkpoint().await {
            eprintln!("[tracedecay] warning: failed to checkpoint WAL on shutdown: {e}");
        }

        eprintln!(
            "[tracedecay] shutdown: {} tool calls, ~{} tokens saved, uptime {}s",
            tool_calls,
            tokens_saved,
            uptime.as_secs()
        );
    }

    /// Dispatches a parsed JSON-RPC request to the appropriate handler.
    ///
    /// Returns `None` for notifications (requests without an `id`).
    pub(crate) async fn handle_request(&self, request: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        let mut route_cache = self.hook_project_routes.snapshot();
        Box::pin(self.handle_request_with_timings(
            request,
            self.timings_enabled(),
            &mut route_cache,
        ))
        .await
    }

    async fn handle_request_with_timings(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        route_cache: &mut HookProjectRouteCache,
    ) -> Option<JsonRpcResponse> {
        Box::pin(self.handle_request_with_timings_and_implicit_project(
            request,
            timings_enabled,
            route_cache,
            None,
        ))
        .await
    }

    async fn handle_request_with_timings_and_implicit_project(
        &self,
        request: &JsonRpcRequest,
        timings_enabled: bool,
        route_cache: &mut HookProjectRouteCache,
        implicit_project_path: Option<&Path>,
    ) -> Option<JsonRpcResponse> {
        debug_assert!(
            !request.method.is_empty(),
            "handle_request called with empty method"
        );
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut counts) = self.method_call_counts.lock() {
            *counts.entry(request.method.clone()).or_insert(0) += 1;
        }
        if matches!(classify_mcp_method(&request.method), McpMethod::HookEvent) {
            Box::pin(self.handle_hook_event_notification(request.params.as_ref(), route_cache))
                .await;
            return None;
        }
        self.hook_project_routes.refresh_into(route_cache);
        let id = request.id.clone()?;

        let result = match classify_mcp_method(&request.method) {
            McpMethod::Initialize => Some(self.handle_initialize(id, request.params.as_ref())),
            // Some clients send the initialized notification with an id (or
            // via the alternate method name); both stay compatibility no-ops.
            // Hook events were consumed by the early notification dispatch
            // above and can never reach this match with a response due.
            McpMethod::InitializedAck | McpMethod::HookEvent => None,
            McpMethod::ToolsList => Some(self.handle_tools_list(id).await),
            // Boxed because the V2 domain contracts push this future past the
            // large-future lint threshold; the other arms stay small enough
            // to hold inline in the dispatch future.
            McpMethod::ToolsCall => Some(
                Box::pin(self.handle_tools_call(
                    id,
                    request.params.as_ref(),
                    timings_enabled,
                    route_cache,
                    implicit_project_path,
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

        // Track errors
        if let Some(ref resp) = result {
            if resp.error.is_some() {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }

        result
    }

    async fn handle_hook_event_notification(
        &self,
        params: Option<&Value>,
        route_cache: &mut HookProjectRouteCache,
    ) {
        let Some(event) = hook_events::parse_hook_event(params) else {
            return;
        };
        let cg = self.reopen_if_branch_drifted().await;
        let root = cg.project_root().to_path_buf();
        self.update_hook_workspace_route(&event, route_cache).await;
        let current_branch = crate::branch::current_branch(&root);
        self.record_hook_route_analytics(&root, &event, current_branch.as_deref());
        self.record_hook_span_observation(&event).await;
        let plan = hook_events::plan_hook_event(&event, &root, current_branch.as_deref());
        self.run_hook_event_plan(cg, &root, plan).await;
    }

    async fn run_hook_event_plan(&self, cg: Arc<TraceDecay>, root: &Path, plan: HookEventPlan) {
        match plan {
            HookEventPlan::SyncFiles(rel_paths) => {
                match cg.sync_if_stale_silent(&rel_paths).await {
                    Ok(()) | Err(TraceDecayError::SyncLock { .. }) => {
                        self.refresh_file_token_map().await;
                    }
                    Err(e) => eprintln!("[tracedecay] hook file sync failed: {e}"),
                }
            }
            HookEventPlan::AddBranch(branch) => {
                let request = HookBranchWriteRequest {
                    root: root.to_path_buf(),
                    branch,
                    open_options: cg.open_options(),
                    incremental_sync_agent: None,
                };
                match (self.hook_branch_writer)(request).await {
                    Ok(result) => match result.branch_outcome {
                        crate::branch::BranchAddOutcome::Added => {
                            self.reopen_after_branch_tracking_added().await;
                        }
                        crate::branch::BranchAddOutcome::AlreadyTracked => {
                            self.refresh_file_token_map().await;
                        }
                        crate::branch::BranchAddOutcome::Deferred
                        | crate::branch::BranchAddOutcome::NotIndexed => {}
                    },
                    Err(e) => eprintln!("[tracedecay] hook branch tracking failed: {e}"),
                }
            }
            HookEventPlan::AddBranchAt {
                root,
                branch,
                agent,
            } => {
                let request = HookBranchWriteRequest {
                    root,
                    branch,
                    open_options: cg.open_options(),
                    incremental_sync_agent: Some(agent),
                };
                match (self.hook_branch_writer)(request).await {
                    Ok(result) if result.refresh_file_token_map => {
                        self.refresh_file_token_map().await;
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("[tracedecay] hook worktree branch write failed: {e}"),
                }
            }
            HookEventPlan::SyncCurrentBranch { branch, agent } => {
                let request = HookBranchWriteRequest {
                    root: root.to_path_buf(),
                    branch,
                    open_options: cg.open_options(),
                    incremental_sync_agent: Some(agent),
                };
                match (self.hook_branch_writer)(request).await {
                    Ok(result) => match result.branch_outcome {
                        crate::branch::BranchAddOutcome::Added => {
                            self.reopen_after_branch_tracking_added().await;
                        }
                        crate::branch::BranchAddOutcome::AlreadyTracked => {
                            if result.refresh_file_token_map {
                                self.refresh_file_token_map().await;
                            }
                        }
                        crate::branch::BranchAddOutcome::Deferred
                        | crate::branch::BranchAddOutcome::NotIndexed => {}
                    },
                    Err(e) => {
                        eprintln!("[tracedecay] hook current branch tracking failed: {e}");
                    }
                }
            }
            HookEventPlan::DebouncedIncrementalSync(agent) => {
                self.run_hook_incremental_sync(cg, agent).await;
            }
            HookEventPlan::RecordTerminalReceipt { route, receipt } => {
                match crate::automation::host_receipts::record(
                    &cg.store_layout().dashboard_root,
                    route,
                    receipt,
                )
                .await
                {
                    Ok(true) => {
                        if let Some(reconcile) = &self.automation_scheduler_reconciler {
                            reconcile();
                        }
                    }
                    Ok(false) => {}
                    Err(error) => eprintln!("[tracedecay] host receipt record failed: {error}"),
                }
            }
            HookEventPlan::MarkTurnIngested {
                route,
                transcript_watermark,
            } => {
                match crate::automation::host_receipts::mark_turn_ingested(
                    &cg.store_layout().dashboard_root,
                    route,
                    &transcript_watermark,
                )
                .await
                {
                    Ok(()) => {
                        if let Some(reconcile) = &self.automation_scheduler_reconciler {
                            reconcile();
                        }
                    }
                    Err(error) => eprintln!("[tracedecay] turn ingest receipt failed: {error}"),
                }
            }
            HookEventPlan::Noop => {}
        }
    }

    async fn run_hook_incremental_sync(&self, cg: Arc<TraceDecay>, agent: HookAgent) {
        match run_hook_incremental_sync_direct(&cg, agent).await {
            Ok(true) => self.refresh_file_token_map().await,
            Ok(false) => {}
            Err(e) => eprintln!("[tracedecay] hook incremental sync failed: {e}"),
        }
    }

    /// Handles the `initialize` method, returning server capabilities.
    ///
    /// Also records the caller's negotiated `clientInfo.name` (e.g.
    /// `"claude-code"`, `"codex"`, `"cursor"`) so subsequent `tools/call`
    /// analytics events can attribute per-host adoption instead of every
    /// call recording the same opaque `provider="mcp"`. Only the short
    /// name field is retained — never the full `clientInfo` payload.
    fn handle_initialize(&self, id: Value, params: Option<&Value>) -> JsonRpcResponse {
        let client_name = params
            .and_then(|p| p.get("clientInfo"))
            .and_then(|ci| ci.get("name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if client_name.is_some() {
            if let Ok(mut slot) = self.client_name.lock() {
                *slot = client_name;
            }
        }
        JsonRpcResponse::success(id, initialize_result(SERVER_INSTRUCTIONS))
    }

    /// The negotiated MCP client name recorded by the most recent
    /// `initialize` handshake, if any (see [`Self::client_name`] field doc).
    fn client_name(&self) -> Option<String> {
        self.client_name.lock().ok().and_then(|slot| slot.clone())
    }

    /// Handles the `tools/list` method, returning all available tool definitions.
    async fn handle_tools_list(&self, id: Value) -> JsonRpcResponse {
        let node_count = self
            .cg_snapshot()
            .await
            .get_stats()
            .await
            .map_or(0, |s| s.node_count);
        let budget = explore_call_budget(node_count);
        let tools = get_tool_definitions_with_budget(node_count, budget);
        JsonRpcResponse::success(id, json!({ "tools": tools }))
    }

    /// Handles the `resources/list` method, returning available resources.
    fn handle_resources_list(id: Value) -> JsonRpcResponse {
        JsonRpcResponse::success(id, resources_list_result())
    }

    /// Handles the `resources/read` method, returning resource contents.
    async fn handle_resources_read(&self, id: Value, params: Option<&Value>) -> JsonRpcResponse {
        let uri = params.and_then(|p| p.get("uri")).and_then(|v| v.as_str());

        let Some(uri) = uri else {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "missing 'uri' in resources/read params".to_string(),
            );
        };
        if let Ok(mut counts) = self.resource_read_counts.lock() {
            *counts.entry(uri.to_string()).or_insert(0) += 1;
        }

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

    /// Returns the `SQLite` schema documentation as a markdown resource.
    /// Sourced from `src/db/migrations.rs::create_schema` — keep in sync.
    fn read_resource_schema(id: Value) -> JsonRpcResponse {
        JsonRpcResponse::success(
            id,
            json!({
                "contents": [{
                    "uri": "tracedecay://schema",
                    "mimeType": "text/markdown",
                    "text": SCHEMA_MARKDOWN
                }]
            }),
        )
    }

    /// Returns graph statistics as a JSON resource.
    async fn read_resource_status(&self, id: Value) -> JsonRpcResponse {
        let cg = self.reopen_if_branch_drifted().await;
        match cg.get_stats().await {
            Ok(stats) => {
                let mut output = serde_json::to_value(&stats).unwrap_or(json!({}));
                output["branch_diagnostics"] =
                    serde_json::to_value(cg.branch_diagnostics()).unwrap_or(json!({}));
                let text = serde_json::to_string_pretty(&output).unwrap_or_default();
                JsonRpcResponse::success(
                    id,
                    json!({
                        "contents": [{
                            "uri": "tracedecay://status",
                            "mimeType": "application/json",
                            "text": text
                        }]
                    }),
                )
            }
            Err(e) => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                format!("failed to read graph stats: {e}"),
            ),
        }
    }

    /// Returns the file list as a text resource (grouped by directory).
    async fn read_resource_files(&self, id: Value) -> JsonRpcResponse {
        match self.cg_snapshot().await.get_all_files().await {
            Ok(mut files) => {
                files.sort_by(|a, b| a.path.cmp(&b.path));
                let mut groups: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for f in &files {
                    let dir = f.path.rfind('/').map_or(".", |i| &f.path[..i]).to_string();
                    #[allow(clippy::map_unwrap_or)]
                    let name = f
                        .path
                        .rfind('/')
                        .map(|i| &f.path[i + 1..])
                        .unwrap_or(&f.path);
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
                JsonRpcResponse::success(
                    id,
                    json!({
                        "contents": [{
                            "uri": "tracedecay://files",
                            "mimeType": "text/plain",
                            "text": text
                        }]
                    }),
                )
            }
            Err(e) => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                format!("failed to read file list: {e}"),
            ),
        }
    }

    /// Returns a high-level project overview as a text resource.
    async fn read_resource_overview(&self, id: Value) -> JsonRpcResponse {
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
        JsonRpcResponse::success(
            id,
            json!({
                "contents": [{
                    "uri": "tracedecay://overview",
                    "mimeType": "text/plain",
                    "text": text
                }]
            }),
        )
    }

    async fn read_resource_branches(&self, id: Value) -> JsonRpcResponse {
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
        JsonRpcResponse::success(
            id,
            json!({
                "contents": [{
                    "uri": "tracedecay://branches",
                    "mimeType": "application/json",
                    "text": text
                }]
            }),
        )
    }

    /// Handles the `tools/call` method, dispatching to the appropriate tool handler.
    async fn handle_tools_call(
        &self,
        id: Value,
        params: Option<&Value>,
        timings_enabled: bool,
        route_cache: &HookProjectRouteCache,
        implicit_project_path: Option<&Path>,
    ) -> JsonRpcResponse {
        let Some(params) = params else {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "missing params for tools/call".to_string(),
            );
        };

        let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
            return JsonRpcResponse::error(
                id,
                ErrorCode::InvalidParams,
                "missing 'name' in tools/call params".to_string(),
            );
        };

        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let analytics_arguments = arguments.clone();
        let analytics_session_id = mcp_analytics_session_id(&arguments);

        // Branch-drift hot-swap: if the working tree switched branches since
        // the served instance opened, reopen onto the live branch's DB so
        // this call reads the right index. Cheap no-op check when no drift.
        let cg = self.reopen_if_branch_drifted().await;

        // Notification-free freshness is useful before tools that edit source
        // files in the index. Read-only graph queries should not block behind
        // a full project walk; on very large indexes (especially when
        // node_modules was intentionally included) that turns diagnostics and
        // search into sync operations.
        if needs_lazy_sync_before_dispatch(tool_name) {
            self.maybe_sync_if_stale().await;
        } else {
            // D4: sync-on-read (never blocking). Read tools serve the current
            // answer IMMEDIATELY and, when the read-refresh cooldown has
            // elapsed, kick a single-flighted background refresh so the *next*
            // read sees fresh data. This heals read-only sessions that never
            // touch an edit tool without ever making a query wait behind a
            // project walk.
            self.maybe_spawn_read_refresh(&cg);
        }

        self.stats.tool_calls.fetch_add(1, Ordering::Relaxed);
        eprintln!("[tracedecay] tool call: {tool_name}");
        if let Ok(mut counts) = self.tool_call_counts.lock() {
            *counts.entry(tool_name.to_string()).or_insert(0) += 1;
        }

        let server_stats = if tool_name == "tracedecay_status" {
            Some(self.server_stats_json().await)
        } else {
            None
        };

        let timings_active =
            timings_enabled && crate::config::load_telemetry_config(cg.project_root()).timings;
        let handler_start = if timings_active {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let mut handler_arguments = route_cache.apply_to_tool_arguments(tool_name, arguments);
        if crate::analytics::is_skill_view_tool(tool_name) {
            if let Some(request_id) = json_rpc_request_id_string(&id) {
                if let Some(map) = handler_arguments.as_object_mut() {
                    map.insert("__mcp_request_id".to_string(), json!(request_id));
                }
            }
        }

        let dispatch_outcome = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            tool_name,
            handler_arguments,
            server_stats,
            self.scope_prefix(),
            ToolCallRegistryOptions {
                global_db: self.registry_db.as_deref(),
                allow_default_registry_fallback: self.allow_default_registry_fallback,
                implicit_project_path,
                automation_scheduler_reconciler: self.automation_scheduler_reconciler.clone(),
                automation_writer: self.dashboard_automation_writer.clone(),
                diagnostics_cache: Some(&self.diagnostics_cache),
                diagnostics_lsp: Some(&self.diagnostics_lsp),
            },
        )
        .await;
        let handler_elapsed_us = handler_start.map(|t| t.elapsed().as_micros() as u64);
        let request_id = id.clone();
        match dispatch_outcome {
            Ok(mut result) => {
                if let Some(us) = handler_elapsed_us {
                    let obj = result.value.as_object_mut();
                    if let Some(map) = obj {
                        let meta = map.entry("_meta").or_insert_with(|| json!({}));
                        if let Some(meta_obj) = meta.as_object_mut() {
                            meta_obj.insert("duration_us".to_string(), json!(us));
                        }
                    }
                }
                // Estimate approximate token count of the graph response
                // ("after"), before any banners/metrics lines are appended.
                let response_tokens: u64 = result
                    .value
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map_or(0, |arr| {
                        let total_chars: usize = arr
                            .iter()
                            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                            .map(str::len)
                            .sum();
                        (total_chars / 4) as u64
                    });

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
                if raw_file_tokens > 0 {
                    if let Some(content) = result
                        .value
                        .get_mut("content")
                        .and_then(|c| c.as_array_mut())
                    {
                        content.push(json!({"type": "text", "text": format!(
                            "\ntracedecay_metrics: before={raw_file_tokens} after={response_tokens}"
                        )}));
                    }
                }
                let analytics_outcome = if tool_result_has_semantic_error(&result) {
                    "error"
                } else {
                    "success"
                };

                // Persist to the cross-project savings ledger (best-effort, non-blocking).
                // Clone the Arc — no new connection is opened. The counters
                // and notify make the write's completion observable to
                // [`Self::ledger_writes_settled`] without making it awaited
                // anywhere on the request path.
                if let Some(gdb) = self.global_db.clone() {
                    let project_path_str = GlobalDb::canonical_project_key(cg.project_root());
                    let tool_name_owned = tool_name.to_string();
                    let ts = crate::tracedecay::current_timestamp();
                    let client_name = self.client_name();
                    let failure_reason = (analytics_outcome == "error")
                        .then(|| semantic_failure_reason(&result))
                        .flatten();
                    let analytics_event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
                        project_root: cg.project_root(),
                        session_id: analytics_session_id.clone(),
                        tool_name,
                        outcome: analytics_outcome,
                        raw_file_tokens,
                        response_tokens,
                        net_saved_tokens,
                        duration_us: handler_elapsed_us,
                        timestamp: ts,
                        request_id: &request_id,
                        arguments: &analytics_arguments,
                        internal_analytics: result.internal_analytics(),
                        client_name: client_name.as_deref(),
                        mcp_instance_id: Some(self.mcp_instance_id.as_str()),
                        failure_reason: failure_reason.as_deref(),
                    });
                    self.spawn_observed_ledger_write(async move {
                        gdb.record_savings(
                            &project_path_str,
                            &tool_name_owned,
                            raw_file_tokens,
                            response_tokens,
                            ts,
                        )
                        .await;
                        if let Err(e) = gdb.append_analytics_event(&analytics_event).await {
                            eprintln!("[tracedecay] analytics_events insert failed: {e}");
                        }
                    });
                }

                // Prepend version-update warning + queue logging notification.
                if let Some(warning) = self.check_version_update().await {
                    if let Some(content) = result
                        .value
                        .get_mut("content")
                        .and_then(|c| c.as_array_mut())
                    {
                        content.insert(0, json!({"type": "text", "text": &warning}));
                    }
                    if let Ok(mut pending) = self.pending_notifications.lock() {
                        pending.push(json!({
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

                // Staged-automation nudge (Hermes parity R5): when automation
                // runs have queued skill drafts for review, append a one-line
                // notice so the approval queue doesn't grow silently. Fact
                // proposal counts stay telemetry-only in `staged_notice`.
                if let Some(notice) = self.maybe_automation_staged_notice(&cg).await {
                    if let Some(content) = result
                        .value
                        .get_mut("content")
                        .and_then(|c| c.as_array_mut())
                    {
                        content.push(json!({"type": "text", "text": format!("\n{notice}")}));
                    }
                }

                // Per-file staleness banner (#428 design): files this response
                // referenced that are still pending after the in-line sync
                // attempt get a focused banner naming them with edit ages,
                // telling the agent to Read THOSE files directly while
                // treating the rest of the response as authoritative.
                // Replaces the previous all-or-nothing "STALE INDEX"
                // warning that made agents distrust the entire answer.
                if !result.touched_files.is_empty() {
                    let stale_files = cg.check_file_staleness(&result.touched_files).await;
                    if !stale_files.is_empty() {
                        let still_stale = match cg.sync_if_stale(&stale_files).await {
                            Ok(false) => false,        // sync completed; files now fresh
                            Ok(true) | Err(_) => true, // still stale (lock contention / sync error)
                        };
                        if still_stale {
                            let banner =
                                format_per_file_staleness_banner(cg.project_root(), &stale_files);
                            // Machine-readable marker. Same shape as before
                            // so existing scrapers keep working.
                            let stale_json = serde_json::to_string(&stale_files)
                                .unwrap_or_else(|_| "[]".to_string());
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
                    }
                }

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
                {
                    let last_time = cg.last_sync_timestamp().await;
                    let now = crate::tracedecay::current_timestamp();
                    let age_secs = now - last_time;
                    if last_time > 0 && age_secs > 3600 {
                        let refreshed_recently = {
                            let done = self.last_background_refresh_done_at.load(Ordering::Acquire);
                            done > 0
                                && now.saturating_sub(done)
                                    < self.sync_config.read_cooldown_secs as i64
                        };
                        let banner = staleness_banner(StalenessBannerInputs {
                            age_secs,
                            // Auto-sync is "on" when either the daemon watcher
                            // or sync-on-read can repair this.
                            auto_sync_on: self.sync_config.auto_watch
                                || self.sync_config.read_refresh,
                            // A read-only fallback store can never be written,
                            // so no background refresh can heal it.
                            fallback_store: cg.fallback_warning().is_some(),
                            refresh_running: self
                                .background_refresh_running
                                .load(Ordering::Acquire),
                            refreshed_recently,
                        });

                        if let Some(banner) = banner {
                            if let Some(content) = result
                                .value
                                .get_mut("content")
                                .and_then(|c| c.as_array_mut())
                            {
                                content.insert(0, json!({"type": "text", "text": &banner}));
                            }
                        }
                    }
                }

                // Borrowed-worktree heads-up (#312). Inserted LAST so it
                // appears FIRST in the response — the index serving the
                // wrong branch is the most serious of these warnings to
                // surface to the agent.
                if let Some(ref m) = self.worktree_mismatch {
                    let notice = crate::worktree::worktree_mismatch_notice(m);
                    if let Some(content) = result
                        .value
                        .get_mut("content")
                        .and_then(|c| c.as_array_mut())
                    {
                        content.insert(0, json!({"type": "text", "text": notice}));
                    }
                }

                mark_semantic_tool_error(&mut result);
                JsonRpcResponse::success(id, result.value)
            }
            Err(e) => {
                self.record_mcp_tool_error_analytics(
                    cg.project_root(),
                    analytics_session_id,
                    tool_name,
                    &request_id,
                    &analytics_arguments,
                    handler_elapsed_us,
                    &e,
                );
                tool_error_response(id, tool_name, &e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // internal fan-in of independent request/analytics fields
    fn record_mcp_tool_error_analytics(
        &self,
        project_root: &std::path::Path,
        session_id: Option<String>,
        tool_name: &str,
        request_id: &Value,
        arguments: &Value,
        duration_us: Option<u64>,
        error: &TraceDecayError,
    ) {
        let Some(gdb) = self.global_db.clone() else {
            return;
        };
        let client_name = self.client_name();
        // `TraceDecayError`'s `Display` (via `thiserror`) already carries a
        // variant-classified, human-readable message (e.g. "config error:
        // missing required parameter: handle"); bounded truncation happens
        // in `mcp_tool_analytics_event`.
        let failure_reason = error.to_string();
        let event = mcp_tool_analytics_event(McpToolAnalyticsEvent {
            project_root,
            session_id,
            tool_name,
            outcome: "error",
            raw_file_tokens: 0,
            response_tokens: 0,
            net_saved_tokens: 0,
            duration_us,
            timestamp: crate::tracedecay::current_timestamp(),
            request_id,
            arguments,
            internal_analytics: None,
            client_name: client_name.as_deref(),
            mcp_instance_id: Some(self.mcp_instance_id.as_str()),
            failure_reason: Some(&failure_reason),
        });
        self.spawn_observed_ledger_write(async move {
            if let Err(e) = gdb.append_analytics_event(&event).await {
                eprintln!("[tracedecay] analytics_events insert failed: {e}");
            }
        });
    }

    fn record_hook_route_analytics(
        &self,
        project_root: &std::path::Path,
        event: &hook_events::HookEvent,
        current_branch: Option<&str>,
    ) {
        let Some(event) = hook_route_analytics_event(
            project_root,
            event,
            current_branch,
            crate::tracedecay::current_timestamp(),
        ) else {
            return;
        };
        let Some(gdb) = self.global_db.clone() else {
            return;
        };
        self.spawn_observed_ledger_write(async move {
            if let Err(e) = gdb.append_analytics_event(&event).await {
                eprintln!("[tracedecay] hook route analytics insert failed: {e}");
            }
        });
    }

    /// Records a live session↔git span from one hook route notification.
    ///
    /// Route metadata carries `(session_id, thread_id, cwd, worktree,
    /// branch)`; when the route names a session and resolves to a registered
    /// project, this folds one [`SpanObservation`] into that project's
    /// `sessions.db` span table (see [`crate::sessions::git_correlation`]).
    /// Mid-session branch/worktree switches are handled by the span table
    /// itself — the observation always carries the *current* branch.
    ///
    /// Fail-open like [`Self::update_hook_workspace_route`]: any resolution or
    /// DB error is dropped. An in-process debounce keyed by
    /// `(provider, session, branch, worktree)` collapses a burst of tool-use
    /// events to one write per
    /// [`DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS`](crate::sessions::git_correlation::DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS)
    /// so the notification hot path never blocks on repeated writes (spans
    /// merge regardless, so a dropped observation only widens a span slightly
    /// less).
    async fn record_hook_span_observation(&self, event: &hook_events::HookEvent) {
        use crate::sessions::git_correlation::{
            self as gc, DEFAULT_SPAN_MERGE_GAP_SECS, DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS,
            SpanObservation, SpanSource,
        };

        let Some(route) = event.route.as_ref() else {
            return;
        };
        let Some(session_id) = route
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        // Resolve the project for this route the same way route caching does;
        // spans belong to that project's store, not this server's checkout.
        let route_cwd = route.cwd.as_deref().or(event.cwd.as_deref());
        let Some(cwd) = route_cwd else {
            return;
        };
        let Some(project_root) = self.registered_project_for_route_cwd(cwd).await else {
            return;
        };
        let project_root = PathBuf::from(project_root);

        // Worktree preference: the routed worktree, else the cwd's git
        // worktree root, else the resolved project root. Never fabricated.
        let worktree_raw = route
            .worktree
            .as_deref()
            .map(Path::to_path_buf)
            .or_else(|| crate::worktree::git_worktree_root(cwd))
            .unwrap_or_else(|| project_root.clone());
        let worktree = gc::normalize_worktree(&worktree_raw.to_string_lossy());

        let branch = route
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let thread_id = route
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let ts = crate::tracedecay::current_timestamp();

        // Hook routes are provider-agnostic: leave provider empty.
        let key = gc::span_debounce_key("", session_id, branch.as_deref(), &worktree);
        let should_record = self
            .span_observation_debounce
            .lock()
            .map_or(true, |mut debounce| {
                debounce.should_record(&key, ts, DEFAULT_SPAN_OBSERVATION_DEBOUNCE_SECS)
            });
        if !should_record {
            return;
        }

        let observation = SpanObservation {
            provider: String::new(),
            session_id: session_id.to_string(),
            thread_id,
            branch,
            worktree,
            ts,
            source: SpanSource::HookRoute,
        };
        self.spawn_observed_ledger_write(async move {
            let Ok(db_path) = crate::storage::resolve_project_session_db_path(&project_root) else {
                return;
            };
            let Some(db) = GlobalDb::open_at(&db_path).await else {
                return;
            };
            if let Err(e) = db
                .git_record_span_observation(&observation, DEFAULT_SPAN_MERGE_GAP_SECS)
                .await
            {
                eprintln!("[tracedecay] hook route span record failed: {e}");
            }
        });
    }

    fn spawn_observed_ledger_write<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.ledger_writes_started.fetch_add(1, Ordering::SeqCst);
        let finished = self.ledger_writes_finished.clone();
        let notify = self.ledger_write_notify.clone();
        tokio::spawn(async move {
            future.await;
            finished.fetch_add(1, Ordering::SeqCst);
            notify.notify_waiters();
        });
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
            "ratios": {
                "tool_calls_per_jsonrpc_message": ratio(tool_calls),
                "errors_per_jsonrpc_message": ratio(errors),
            },
            "approx_tokens_saved": self.tokens_saved.load(Ordering::Relaxed),
        });

        if let Some(ref gdb) = self.global_db {
            if let Some(global_total) = gdb.global_tokens_saved().await {
                let local = self.tokens_saved.load(Ordering::Relaxed);
                stats["global_tokens_saved"] = json!(global_total.saturating_sub(local));
            }
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

fn json_rpc_request_id_string(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}
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
mod hook_branch_writer_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod staleness_banner_tests;
