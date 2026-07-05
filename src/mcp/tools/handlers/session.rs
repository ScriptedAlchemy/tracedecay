use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Map, Value};

use super::super::render::{self, truncated_json_envelope_with_handle, Md};
use super::support::{profile_root_for_global_db, project_registry_context, safe_profile_relpath};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{GlobalDb, ProjectRegistryContext};
use crate::mcp::response_handles::{
    observe_response_truncation, store_response_handle, RESPONSE_RETRIEVE_TOOL,
};
use crate::mcp::tools::{ToolResult, MAX_RESPONSE_CHARS};
use crate::sessions::cursor::HermesProfileDbReadOnly;
use crate::sessions::git_correlation::{GitRefFilter, GitScopeFilter, SessionsForQuery};
use crate::sessions::lcm::compression_decision::{self, AssemblyCapInput};
use crate::sessions::lcm::{
    LcmCleanConfig, LcmCompressionRequest, LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget,
    LcmExpandQueryRequest, LcmExpandRequest, LcmExpandTarget, LcmGcConfig, LcmGrepRequest,
    LcmGrepSort, LcmLoadSessionRequest, LcmPreflightRequest, LcmScope, LcmSessionBoundaryRequest,
    LcmSummarizerMode, LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT,
};
use crate::sessions::{
    ProviderScope, SessionSearchFilters, SessionSearchScope, SessionSearchTimeRange,
};
use crate::timeutil::SearchTimeBound;
use crate::tracedecay::{current_timestamp, TraceDecay};

const DEFAULT_LCM_CONTENT_LIMIT: usize = 4096;
const DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 32_000;
const MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 65_536;
const MAX_LCM_CONTENT_LIMIT: usize = 8192;
const MAX_LCM_LOAD_CONTENT_LIMIT: usize = 20_000;
const MAX_LCM_RESULT_LIMIT: usize = 100;
const MAX_LCM_EXPAND_QUERY_PROMPT_CHARS: usize = 2_048;
const MAX_LCM_EXPAND_QUERY_QUERY_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS: usize = 2_048;

fn tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    tool_json_with_md(project_root, args, value, || render::generic_md(value))
}

/// Like [`tool_json`] but renders the markdown (default-format) body with a
/// caller-supplied closure instead of the generic key/value renderer. The
/// `format:"json"` path is unaffected — it always serializes `value` compactly.
fn tool_json_with_md<F: FnOnce() -> String>(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
    md: F,
) -> ToolResult {
    let text = render::finalize(project_root, args, value, md);
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

const MESSAGE_SEARCH_SNIPPET_CHARS: usize = 240;

/// Renders `tracedecay_message_search` results as compact markdown. Each hit
/// shows provider, session (id + title), role, timestamp, and score with a
/// plain-text snippet of the message body — deliberately dropping the raw
/// `metadata_json`, `source_path`, and `transcript_path` blobs that the generic
/// renderer would dump verbatim into table cells. Pass `format:"json"` to get
/// the full structured records.
fn render_message_search_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Transcript Search");
    for key in ["query", "provider", "scope"] {
        let field = render::field_str(value, key);
        if !field.is_empty() {
            md.field(key, field);
        }
    }
    md.field("count", &render::field_i64(value, "count").to_string());
    if let Some(summary) = git_filter_summary(value) {
        md.field("git filter", &summary);
    }
    let results = value.get("results").and_then(Value::as_array);
    match results {
        Some(results) if !results.is_empty() => {
            md.blank();
            for hit in results {
                append_message_search_hit(&mut md, hit);
            }
        }
        _ => {
            md.blank().empty_note("No matching messages.");
        }
    }
    md.render()
}

/// One-line `branch=… worktree=… commit=…` summary of the applied git-scope
/// filter, or `None` when no filter was applied. Reads the `git_filter` object
/// echoed into the payload by the message-search / lcm-grep handlers.
fn git_filter_summary(value: &Value) -> Option<String> {
    if !value
        .get("git_filter_applied")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let filter = value.get("git_filter")?;
    let mut parts = Vec::new();
    for key in ["branch", "worktree", "commit"] {
        if let Some(field) = filter.get(key).and_then(Value::as_str) {
            parts.push(format!("{key}={field}"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn append_message_search_hit(md: &mut Md, hit: &Value) {
    let session = hit.get("session");
    let message = hit.get("message");
    let provider = message
        .and_then(|m| m.get("provider"))
        .or_else(|| session.and_then(|s| s.get("provider")))
        .and_then(Value::as_str)
        .unwrap_or("");
    let role = message
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    let session_id = session
        .and_then(|s| s.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = session
        .and_then(|s| s.get("title"))
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty());
    let timestamp = message
        .and_then(|m| m.get("timestamp"))
        .and_then(Value::as_i64);

    let mut header = format!("**{role}** · {provider} · score {score:.1}");
    if let Some(ts) = timestamp {
        let _ = write!(header, " · t={ts}");
    }
    md.bullet(&header);
    let mut locator = format!("session `{session_id}`");
    if let Some(title) = title {
        let _ = write!(locator, " — {title}");
    }
    md.line(&format!("  {locator}"));
    let text = message
        .and_then(|m| m.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let snippet = message_text_snippet(text, MESSAGE_SEARCH_SNIPPET_CHARS);
    if !snippet.is_empty() {
        md.line(&format!("  {snippet}"));
    }
}

/// Best-effort single-line plain-text snippet from a stored message body.
/// Message text is frequently itself JSON (`tool_use` / `tool_result` blocks), so
/// pull the human-readable fields out rather than showing an escaped blob.
fn message_text_snippet(text: &str, max_chars: usize) -> String {
    let readable = readable_message_text(text, max_chars.saturating_mul(8));
    let collapsed = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    let (snippet, truncated) = truncate_chars(&collapsed, max_chars);
    if truncated {
        format!("{snippet}…")
    } else {
        snippet
    }
}

fn readable_message_text(text: &str, budget: usize) -> String {
    let trimmed = text.trim_start();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            let mut out = String::new();
            collect_readable_text(&value, &mut out, budget);
            if !out.trim().is_empty() {
                return out;
            }
        }
    }
    text.to_string()
}

fn collect_readable_text(value: &Value, out: &mut String, budget: usize) {
    if out.len() >= budget {
        return;
    }
    match value {
        Value::String(s) if !s.is_empty() => {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(s);
        }
        Value::Array(arr) => {
            for item in arr {
                collect_readable_text(item, out, budget);
            }
        }
        Value::Object(map) => {
            // Prefer human-facing fields; ignore ids, kinds, and metadata blobs.
            for key in ["text", "content", "thinking", "input"] {
                if let Some(field) = map.get(key) {
                    collect_readable_text(field, out, budget);
                }
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
pub(super) struct LcmHandlerContext<'a> {
    project_root: Option<&'a Path>,
    project_session_db_path: Option<&'a Path>,
}

impl<'a> LcmHandlerContext<'a> {
    pub(super) fn active(cg: &'a TraceDecay) -> Self {
        Self {
            project_root: Some(cg.project_root()),
            project_session_db_path: Some(cg.store_layout().sessions_db_path.as_path()),
        }
    }

    pub(super) const fn projectless() -> Self {
        Self {
            project_root: None,
            project_session_db_path: None,
        }
    }
}

async fn selected_project_session_db_path(
    project_root: &Path,
    project_session_db_path: &Path,
    args: &Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<Option<(PathBuf, PathBuf)>> {
    let Some(context) = project_registry_context(
        args,
        &["project_path", "project_root"],
        global_db,
        allow_default_registry_fallback,
    )
    .await?
    else {
        return Ok(Some((
            project_session_db_path.to_path_buf(),
            project_root.to_path_buf(),
        )));
    };
    let profile_root = profile_root_for_global_db(global_db, allow_default_registry_fallback)?;
    let candidates = registry_session_db_candidates(&context, &profile_root)?;
    let target_root = PathBuf::from(context.project.display_root);
    for db_path in candidates {
        if db_path.is_file() {
            return Ok(Some((db_path, target_root)));
        }
    }
    Ok(None)
}

fn registry_session_db_candidates(
    context: &ProjectRegistryContext,
    profile_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    if let Some(artifact) = context
        .stores
        .iter()
        .flat_map(|store| store.artifacts.iter())
        .find(|artifact| artifact.artifact_kind == "sessions_db")
    {
        candidates.push(profile_root.join(safe_profile_relpath(&artifact.relpath)?));
    }
    if let Some(store) = context
        .stores
        .iter()
        .find(|store| store.store.storage_mode == "profile_sharded")
    {
        candidates.push(
            profile_root
                .join(safe_profile_relpath(&store.store.store_relpath)?)
                .join(crate::storage::SESSIONS_DB_FILENAME),
        );
    }
    Ok(candidates)
}

fn lcm_preflight_tool_json(project_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    if !render::wants_json(args) {
        // Markdown default: route through the normal renderer so an oversized
        // preflight payload is truncated *with* a retrieval handle. Passing the
        // project root is what lets `truncated_markdown_with_handle` store the
        // full body — without it the truncation would be irreversible.
        return tool_json(project_root, args, value);
    }
    let formatted = serde_json::to_string(value).unwrap_or_default();
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else {
        let started = std::time::Instant::now();
        let compact = compact_lcm_preflight_payload(value, formatted.len(), 8, 512);
        let compact_text = serde_json::to_string(&compact).unwrap_or_default();
        let text = if compact_text.len() <= MAX_RESPONSE_CHARS {
            compact_text
        } else {
            let minimal = compact_lcm_preflight_payload(value, formatted.len(), 4, 256);
            let minimal_text = serde_json::to_string(&minimal).unwrap_or_default();
            if minimal_text.len() <= MAX_RESPONSE_CHARS {
                minimal_text
            } else {
                let floor = compact_lcm_preflight_payload(value, formatted.len(), 1, 64);
                bounded_lcm_contract_text(&floor)
            }
        };
        // Contract-preserving compaction drops data without storing a handle,
        // so record it as an irreversible truncation for telemetry parity with
        // the render-layer truncation paths.
        observe_response_truncation(
            formatted.len(),
            text.len(),
            false,
            current_timestamp(),
            "compacted_no_handle",
            started.elapsed(),
        );
        text
    };
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

fn compact_lcm_preflight_payload(
    value: &Value,
    original_chars: usize,
    replay_limit: usize,
    replay_content_chars: usize,
) -> Value {
    let mut object = Map::new();
    for key in [
        "status",
        "provider",
        "session_id",
        "should_compress",
        "reason",
    ] {
        if let Some(field) = value.get(key) {
            object.insert(key.to_string(), field.clone());
        }
    }
    let (replay_messages, replay_truncated, replay_compacted) = compact_messages_for_mcp(
        value.get("replay_messages"),
        replay_limit,
        replay_content_chars,
    );
    object.insert("replay_messages".to_string(), replay_messages);
    object.insert(
        "replay_messages_truncated_for_mcp".to_string(),
        json!(replay_truncated),
    );
    object.insert(
        "replay_messages_compacted_for_mcp".to_string(),
        json!(replay_compacted),
    );
    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(original_chars),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!("lcm-preflight response compacted to preserve Hermes bridge contract"),
    );
    Value::Object(object)
}

fn compact_messages_for_mcp(
    value: Option<&Value>,
    limit: usize,
    content_chars: usize,
) -> (Value, bool, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false, false);
    };
    let mut truncated = array.len() > limit;
    let mut compacted = false;
    let messages = array
        .iter()
        .take(limit)
        .map(|item| {
            let mut object = Map::new();
            if let Some(map) = item.as_object() {
                for (key, field) in map {
                    if key == "content" {
                        let content_text = field.as_str().map_or_else(
                            || serde_json::to_string(field).unwrap_or_default(),
                            str::to_string,
                        );
                        let (content, content_truncated) =
                            truncate_chars(&content_text, content_chars);
                        object.insert(key.clone(), json!(content));
                        object.insert(
                            "content_truncated_for_mcp".to_string(),
                            json!(content_truncated),
                        );
                        if !field.is_string() {
                            object.insert("content_serialized_for_mcp".to_string(), json!(true));
                            compacted = true;
                        }
                        truncated |= content_truncated;
                    } else {
                        object.insert(key.clone(), field.clone());
                    }
                }
            }
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    (Value::Array(messages), truncated, compacted || truncated)
}

fn bounded_lcm_contract_text(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    if text.len() <= MAX_RESPONSE_CHARS {
        return text;
    }
    serde_json::to_string(&json!({
        "status": value.get("status").cloned().unwrap_or_else(|| json!("ok")),
        "reason": value.get("reason").cloned().unwrap_or_else(|| json!("mcp_contract_floor_over_budget")),
        "mcp_response_truncated": true,
        "contract_truncated": true,
        "mcp_truncation_reason": "lcm response exceeded minimum Hermes bridge contract budget",
        "replay_messages": [],
        "replay_messages_truncated_for_mcp": true,
        "replay_messages_compacted_for_mcp": true,
    }))
    .unwrap_or_default()
}

fn lcm_response_handle_root(project_root: Option<&Path>, args: &Value) -> Option<PathBuf> {
    if let Some(root) = project_root {
        return Some(root.to_path_buf());
    }
    for key in ["response_handle_project_root", "project_root"] {
        if let Some(root) = string_arg(args, key) {
            return Some(PathBuf::from(root));
        }
    }
    if string_arg(args, "storage_scope") == Some("hermes_profile") {
        return string_arg(args, "hermes_home").map(PathBuf::from);
    }
    None
}

fn lcm_expand_query_tool_json(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
) -> ToolResult {
    if !render::wants_json(args) {
        return tool_json(project_root, args, value);
    }
    let formatted = serde_json::to_string(value).unwrap_or_default();
    let needs_synthesis = value
        .get("needs_synthesis")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else if needs_synthesis {
        let started = std::time::Instant::now();
        let compact =
            compact_lcm_expand_query_payload(value, formatted.len(), CompactTier::Standard);
        let compact_text = serde_json::to_string(&compact).unwrap_or_default();
        let (text, handle_status) = if compact_text.len() <= MAX_RESPONSE_CHARS {
            (compact_text, "compacted_no_handle")
        } else {
            let fallback = compact_lcm_expand_query_payload(
                value,
                formatted.len(),
                CompactTier::Minimal {
                    compact_chars: compact_text.len(),
                },
            );
            let fallback_text = serde_json::to_string(&fallback).unwrap_or_default();
            if fallback_text.len() <= MAX_RESPONSE_CHARS {
                (fallback_text, "compacted_no_handle")
            } else {
                // Even the Minimal tier overflowed (e.g. oversized cloned
                // pagination or match metadata). Enforce a hard floor that
                // stays valid JSON and keeps the Hermes synthesis contract
                // keys, storing the full payload behind a handle when we can.
                bounded_lcm_expand_query_floor_text(project_root, value, &formatted)
            }
        };
        // The synthesis contract path shrinks the payload in place instead of
        // going through the render-layer envelope, so record the truncation
        // explicitly. It is reversible only when the floor stored a handle.
        observe_response_truncation(
            formatted.len(),
            text.len(),
            handle_status == "stored",
            current_timestamp(),
            handle_status,
            started.elapsed(),
        );
        text
    } else {
        truncated_json_envelope_with_handle(project_root, &formatted)
    };
    // Safety net: every branch above is already bounded (the floor guarantees
    // it for needs_synthesis), but never emit an unbounded body regardless.
    let text = if text.len() <= MAX_RESPONSE_CHARS {
        text
    } else {
        truncated_json_envelope_with_handle(project_root, &text)
    };
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

/// Hard floor for a `needs_synthesis` expand-query payload that is still over
/// [`MAX_RESPONSE_CHARS`] after [`CompactTier::Minimal`] compaction. Emits a
/// bounded JSON object that preserves the Hermes bridge synthesis contract
/// (`status`, `needs_synthesis`, `synthesis_prompt`, bounded scalars) while
/// dropping the unbounded arrays (`context_blocks`, `matches`, `node_ids`,
/// `context_pagination`). When a project root is available the full original
/// payload is stored behind a retrieval handle so nothing is lost; the handle
/// is surfaced as `response_handle` (a key the Hermes plugin recognizes).
///
/// Returns the serialized text plus the telemetry handle status
/// (`"stored"` when the full payload was cached, `"compacted_no_handle"`
/// otherwise).
fn bounded_lcm_expand_query_floor_text(
    project_root: Option<&Path>,
    value: &Value,
    formatted: &str,
) -> (String, &'static str) {
    const FLOOR_SCALAR_CHARS: usize = 512;
    const FLOOR_AUX_JSON_CHARS: usize = 2_048;

    let handle = project_root
        .and_then(|root| store_response_handle(root, formatted, current_timestamp()).ok());
    let handle_status: &'static str = if handle.is_some() {
        "stored"
    } else {
        "compacted_no_handle"
    };

    let mut object = Map::new();
    for key in [
        "status",
        "provider",
        "session_id",
        "storage_scope",
        "answer",
    ] {
        insert_bounded_scalar_field(&mut object, value, key, FLOOR_SCALAR_CHARS);
    }
    for key in [
        "needs_synthesis",
        "max_tokens",
        "context_max_tokens",
        "context_budget",
        "context_truncated",
    ] {
        if let Some(field) = value.get(key) {
            object.insert(key.to_string(), field.clone());
        }
    }
    insert_bounded_text_field(&mut object, value, "prompt", FLOOR_SCALAR_CHARS);
    insert_bounded_text_field(&mut object, value, "query", FLOOR_SCALAR_CHARS);
    // Contract-adjacent recovery metadata survives only when it is itself
    // small; anything larger is recoverable via the response handle.
    for key in ["context_recovery_hint", "summary_request"] {
        if let Some(field) = value.get(key) {
            let serialized_len = serde_json::to_string(field).map_or(usize::MAX, |s| s.len());
            if serialized_len <= FLOOR_AUX_JSON_CHARS {
                object.insert(key.to_string(), field.clone());
            }
        }
    }

    // Drop the unbounded arrays entirely; the synthesis prompt below tells the
    // bridge the context was elided and pagination/node ids are recoverable.
    for key in [
        "context_blocks",
        "matches",
        "node_ids",
        "context_pagination",
    ] {
        object.insert(key.to_string(), json!([]));
        object.insert(format!("{key}_truncated_for_mcp"), json!(true));
    }
    object.insert(
        "synthesis_prompt".to_string(),
        compact_synthesis_prompt_with_limits(
            value,
            &json!([]),
            FLOOR_SCALAR_CHARS,
            FLOOR_SCALAR_CHARS,
        ),
    );

    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(formatted.len()),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!(
            "expand-query response exceeded the minimal synthesis contract budget; unbounded context arrays were dropped"
        ),
    );
    if let Some(record) = &handle {
        object.insert("response_handle".to_string(), json!(record.handle));
        object.insert("retrieve_tool".to_string(), json!(RESPONSE_RETRIEVE_TOOL));
        object.insert("retrieve_expires_at".to_string(), json!(record.expires_at));
        object.insert(
            "retrieve_instruction".to_string(),
            json!(format!(
                "The full expand-query response ({} chars) was stored locally and expires at {}. Call `{RESPONSE_RETRIEVE_TOOL}` with handle `{}` to recover the dropped context_blocks, matches, node_ids, and context_pagination.",
                formatted.len(),
                record.expires_at,
                record.handle
            )),
        );
    }

    let text = serde_json::to_string(&Value::Object(object)).unwrap_or_default();
    if text.len() <= MAX_RESPONSE_CHARS {
        return (text, handle_status);
    }
    // Absolute floor: every retained field above is bounded, so this branch is
    // effectively unreachable, but never emit an unbounded body.
    (
        serde_json::to_string(&json!({
            "status": value.get("status").cloned().unwrap_or_else(|| json!("ok")),
            "needs_synthesis": value
                .get("needs_synthesis")
                .cloned()
                .unwrap_or(json!(true)),
            "context_blocks": [],
            "matches": [],
            "mcp_response_truncated": true,
            "contract_truncated": true,
            "mcp_truncation_reason":
                "expand-query response exceeded the minimum synthesis contract budget",
        }))
        .unwrap_or_default(),
        handle_status,
    )
}

#[derive(Copy, Clone)]
enum CompactTier {
    Standard,
    Minimal { compact_chars: usize },
}

fn compact_lcm_expand_query_payload(
    value: &Value,
    original_chars: usize,
    tier: CompactTier,
) -> Value {
    let limits = match tier {
        CompactTier::Standard => LcmExpandQueryCompactLimits {
            max_context_blocks: 3,
            max_context_block_chars: 600,
            max_matches: 10,
            max_match_snippet_chars: 160,
            max_node_ids: 50,
            max_node_id_chars: 160,
            max_pagination_items: 50,
            max_scalar_chars: None,
            max_prompt_chars: MAX_LCM_EXPAND_QUERY_PROMPT_CHARS,
            max_query_chars: MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
            max_synthesis_system_chars: MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS,
            max_synthesis_prompt_chars: MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS,
            compact_chars: None,
            truncation_reason: "expand-query response compacted to preserve synthesis contract fields",
        },
        CompactTier::Minimal { compact_chars } => LcmExpandQueryCompactLimits {
            max_context_blocks: 1,
            max_context_block_chars: 240,
            max_matches: 5,
            max_match_snippet_chars: 80,
            max_node_ids: 25,
            max_node_id_chars: 120,
            max_pagination_items: 10,
            max_scalar_chars: Some(512),
            max_prompt_chars: 512,
            max_query_chars: 512,
            max_synthesis_system_chars: 512,
            max_synthesis_prompt_chars: 512,
            compact_chars: Some(compact_chars),
            truncation_reason: "expand-query response reduced to minimal synthesis contract after compact payload overflow",
        },
    };

    let mut object = Map::new();
    if let Some(max_scalar_chars) = limits.max_scalar_chars {
        for key in [
            "status",
            "provider",
            "session_id",
            "storage_scope",
            "answer",
        ] {
            insert_bounded_scalar_field(&mut object, value, key, max_scalar_chars);
        }
        for key in [
            "needs_synthesis",
            "max_tokens",
            "context_max_tokens",
            "context_budget",
            "context_truncated",
        ] {
            if let Some(field) = value.get(key) {
                object.insert(key.to_string(), field.clone());
            }
        }
        insert_bounded_text_field(&mut object, value, "prompt", limits.max_prompt_chars);
        insert_bounded_text_field(&mut object, value, "query", limits.max_query_chars);
    } else {
        for key in [
            "status",
            "provider",
            "session_id",
            "storage_scope",
            "answer",
            "needs_synthesis",
            "max_tokens",
            "context_max_tokens",
            "context_budget",
            "context_truncated",
        ] {
            if let Some(field) = value.get(key) {
                object.insert(key.to_string(), field.clone());
            }
        }
        insert_bounded_text_field(&mut object, value, "prompt", limits.max_prompt_chars);
        insert_bounded_text_field(&mut object, value, "query", limits.max_query_chars);
        object.insert("mcp_response_truncated".to_string(), json!(true));
        object.insert("contract_truncated".to_string(), json!(true));
        object.insert(
            "mcp_original_response_chars".to_string(),
            json!(original_chars),
        );
        object.insert(
            "mcp_truncation_reason".to_string(),
            json!(limits.truncation_reason),
        );
    }

    let (context_blocks, context_blocks_truncated) = compact_context_blocks(
        value.get("context_blocks"),
        limits.max_context_blocks,
        limits.max_context_block_chars,
    );
    let (matches, matches_truncated) = compact_matches(
        value.get("matches"),
        limits.max_matches,
        limits.max_match_snippet_chars,
    );
    let (node_ids, node_ids_truncated) = compact_string_array(
        value.get("node_ids"),
        limits.max_node_ids,
        limits.max_node_id_chars,
    );
    let (context_pagination, pagination_truncated) =
        compact_array(value.get("context_pagination"), limits.max_pagination_items);

    object.insert("context_blocks".to_string(), context_blocks.clone());
    object.insert(
        "context_blocks_truncated_for_mcp".to_string(),
        json!(context_blocks_truncated),
    );
    object.insert("matches".to_string(), matches);
    object.insert(
        "matches_truncated_for_mcp".to_string(),
        json!(matches_truncated),
    );
    object.insert("node_ids".to_string(), node_ids);
    object.insert(
        "node_ids_truncated_for_mcp".to_string(),
        json!(node_ids_truncated),
    );
    object.insert("context_pagination".to_string(), context_pagination);
    object.insert(
        "context_pagination_truncated_for_mcp".to_string(),
        json!(pagination_truncated),
    );
    object.insert(
        "synthesis_prompt".to_string(),
        compact_synthesis_prompt_with_limits(
            value,
            &context_blocks,
            limits.max_synthesis_system_chars,
            limits.max_synthesis_prompt_chars,
        ),
    );

    if limits.max_scalar_chars.is_some() {
        object.insert("mcp_response_truncated".to_string(), json!(true));
        object.insert("contract_truncated".to_string(), json!(true));
        object.insert(
            "mcp_original_response_chars".to_string(),
            json!(original_chars),
        );
        if let Some(compact_chars) = limits.compact_chars {
            object.insert(
                "mcp_compact_response_chars".to_string(),
                json!(compact_chars),
            );
        }
        object.insert(
            "mcp_truncation_reason".to_string(),
            json!(limits.truncation_reason),
        );
    }

    Value::Object(object)
}

struct LcmExpandQueryCompactLimits {
    max_context_blocks: usize,
    max_context_block_chars: usize,
    max_matches: usize,
    max_match_snippet_chars: usize,
    max_node_ids: usize,
    max_node_id_chars: usize,
    max_pagination_items: usize,
    max_scalar_chars: Option<usize>,
    max_prompt_chars: usize,
    max_query_chars: usize,
    max_synthesis_system_chars: usize,
    max_synthesis_prompt_chars: usize,
    compact_chars: Option<usize>,
    truncation_reason: &'static str,
}

fn compact_array(value: Option<&Value>, limit: usize) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    (
        Value::Array(array.iter().take(limit).cloned().collect()),
        array.len() > limit,
    )
}

fn compact_matches(value: Option<&Value>, limit: usize, snippet_chars: usize) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let matches = array
        .iter()
        .take(limit)
        .map(|item| {
            let mut object = Map::new();
            for key in ["kind", "node_id", "store_id"] {
                if let Some(field) = item.get(key) {
                    object.insert(key.to_string(), field.clone());
                }
            }
            if let Some(snippet) = item.get("snippet").and_then(Value::as_str) {
                let (snippet, truncated) = truncate_chars(snippet, snippet_chars);
                object.insert("snippet".to_string(), json!(snippet));
                object.insert("snippet_truncated_for_mcp".to_string(), json!(truncated));
            }
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    (Value::Array(matches), array.len() > limit)
}

fn compact_string_array(value: Option<&Value>, limit: usize, item_chars: usize) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let mut truncated = array.len() > limit;
    let values = array
        .iter()
        .take(limit)
        .filter_map(|item| item.as_str())
        .map(|item| {
            let (item, item_truncated) = truncate_chars(item, item_chars);
            truncated |= item_truncated;
            json!(item)
        })
        .collect::<Vec<_>>();
    (Value::Array(values), truncated)
}

fn compact_context_blocks(
    value: Option<&Value>,
    limit: usize,
    content_chars: usize,
) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let mut truncated = array.len() > limit;
    let blocks = array
        .iter()
        .take(limit)
        .map(|item| {
            let mut object = Map::new();
            for key in ["kind", "node_id", "source_ref", "content_range"] {
                if let Some(field) = item.get(key) {
                    object.insert(key.to_string(), field.clone());
                }
            }
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (content, content_truncated) = truncate_chars(content, content_chars);
            truncated |= content_truncated;
            object.insert("content".to_string(), json!(content));
            object.insert(
                "content_truncated_for_mcp".to_string(),
                json!(content_truncated),
            );
            object.insert("raw_message".to_string(), Value::Null);
            object.insert("summary_node".to_string(), Value::Null);
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    (Value::Array(blocks), truncated)
}

fn compact_synthesis_prompt_with_limits(
    value: &Value,
    context_blocks: &Value,
    system_chars: usize,
    prompt_chars: usize,
) -> Value {
    let default_system = LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT;
    let system = value
        .get("synthesis_prompt")
        .and_then(|prompt| prompt.get("system"))
        .and_then(Value::as_str)
        .unwrap_or(default_system);
    let (system, system_truncated) = truncate_chars(system, system_chars);
    let prompt = value
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (prompt, prompt_truncated) = truncate_chars(prompt, prompt_chars);
    let context_json = serde_json::to_string(context_blocks).unwrap_or_else(|_| "[]".into());
    let truncation_note = if value
        .get("context_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "\n\nNOTE: Some LCM context was truncated before MCP response compaction; pagination metadata is included in the tool response."
    } else {
        ""
    };
    let prompt_truncation_note = if prompt_truncated {
        "\n\nNOTE: The original question was truncated in this MCP response; synthesize from the bounded question preview and returned context, or state that the response degraded because the prompt exceeded the MCP response budget."
    } else {
        ""
    };
    json!({
        "system": system,
        "system_truncated_for_mcp": system_truncated,
        "user_prompt_truncated_for_mcp": prompt_truncated,
        "user": format!(
            "QUESTION:\n{prompt}\n\nCOMPACT EXPANDED CONTEXT:\n{context_json}{truncation_note}{prompt_truncation_note}\n\nNOTE: The MCP response was compacted to preserve the synthesis contract. Use node_ids and context_pagination for follow-up expansion if more context is needed."
        ),
    })
}

fn insert_bounded_text_field(
    object: &mut Map<String, Value>,
    value: &Value,
    key: &str,
    max_chars: usize,
) {
    let truncated_key = format!("{key}_truncated_for_mcp");
    match value.get(key) {
        Some(Value::String(text)) => {
            let (text, truncated) = truncate_chars(text, max_chars);
            object.insert(key.to_string(), json!(text));
            object.insert(truncated_key, json!(truncated));
        }
        Some(Value::Null) => {
            object.insert(key.to_string(), Value::Null);
            object.insert(truncated_key, json!(false));
        }
        Some(field) => {
            object.insert(key.to_string(), field.clone());
            object.insert(truncated_key, json!(false));
        }
        None => {}
    }
}

fn insert_bounded_scalar_field(
    object: &mut Map<String, Value>,
    value: &Value,
    key: &str,
    max_chars: usize,
) {
    match value.get(key) {
        Some(Value::String(text)) => {
            let (text, truncated) = truncate_chars(text, max_chars);
            object.insert(key.to_string(), json!(text));
            object.insert(format!("{key}_truncated_for_mcp"), json!(truncated));
        }
        Some(Value::Bool(_) | Value::Number(_) | Value::Null) => {
            object.insert(key.to_string(), value[key].clone());
        }
        _ => {}
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    let truncated = value.chars().nth(max_chars).is_some();
    let text = value.chars().take(max_chars).collect::<String>();
    (text, truncated)
}

fn string_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    string_arg(args, name).ok_or_else(|| TraceDecayError::Config {
        message: format!("missing required parameter: {name}"),
    })
}

fn argument_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn bounded_usize_arg(args: &Value, name: &str, min: usize, max: usize) -> Result<Option<usize>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let Some(integer) = value.as_i64() else {
        return Err(argument_error(format!("{name} must be an integer")));
    };
    if integer < 0 {
        return Err(argument_error(format!("{name} must be >= {min}")));
    }
    let integer =
        usize::try_from(integer).map_err(|_| argument_error(format!("{name} must be <= {max}")))?;
    if integer < min {
        return Err(argument_error(format!("{name} must be >= {min}")));
    }
    if integer > max {
        return Err(argument_error(format!("{name} must be <= {max}")));
    }
    Ok(Some(integer))
}

fn non_negative_i64_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let Some(integer) = value.as_i64() else {
        return Err(argument_error(format!("{name} must be an integer")));
    };
    if integer < 0 {
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    Ok(Some(integer))
}

fn signed_i64_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_i64()
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be an integer")))
}

fn bool_arg(args: &Value, name: &str) -> Result<Option<bool>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a boolean")))
}

fn non_negative_i64_arg_alias(args: &Value, primary: &str, alias: &str) -> Result<Option<i64>> {
    match non_negative_i64_arg(args, primary)? {
        Some(value) => Ok(Some(value)),
        None => non_negative_i64_arg(args, alias),
    }
}

fn non_negative_timestamp_arg_aliases(
    args: &Value,
    names: &[&str],
    bound: SearchTimeBound,
) -> Result<Option<i64>> {
    for name in names {
        if args.get(name).is_some() {
            return non_negative_timestamp_arg(args, name, bound);
        }
    }
    Ok(None)
}

fn non_negative_timestamp_arg(
    args: &Value,
    name: &str,
    bound: SearchTimeBound,
) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let timestamp = match value {
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| timestamp_argument_error(name))?,
        Value::String(text) => parse_timestamp_string(text, name, bound)?,
        _ => return Err(timestamp_argument_error(name)),
    };
    if timestamp < 0 {
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    Ok(Some(timestamp))
}

fn parse_timestamp_string(value: &str, name: &str, bound: SearchTimeBound) -> Result<i64> {
    let text = value.trim();
    if text.is_empty() {
        return Err(argument_error(format!("{name} must not be empty")));
    }
    if let Ok(timestamp) = text.parse::<i64>() {
        if timestamp >= 0 {
            return Ok(timestamp);
        }
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    let now = crate::tracedecay::current_timestamp();
    crate::timeutil::parse_search_time_filter_bound(text, now, bound)
        .ok_or_else(|| timestamp_argument_error(name))
}

fn message_search_time_range(args: &Value) -> Result<SessionSearchTimeRange> {
    Ok(SessionSearchTimeRange {
        start_time: non_negative_timestamp_arg_aliases(
            args,
            &["since", "start_time", "time_from"],
            SearchTimeBound::Start,
        )?,
        end_time: non_negative_timestamp_arg_aliases(
            args,
            &["until", "end_time", "time_to"],
            SearchTimeBound::End,
        )?,
    })
}

fn timestamp_argument_error(name: &str) -> TraceDecayError {
    argument_error(format!(
        "{name} must be a non-negative Unix timestamp, timezone-aware ISO/RFC3339 string, YYYY-MM-DD date, or relative time like 'last hour'"
    ))
}

fn provider_or_all_arg(args: &Value) -> &str {
    optional_search_provider_arg(args).unwrap_or("all")
}

fn required_specific_provider_arg(args: &Value) -> Result<&str> {
    match string_arg(args, "provider") {
        Some("all") => Err(argument_error(
            "provider must name a specific provider for this tool",
        )),
        Some(provider) => Ok(provider),
        None => Err(argument_error("provider is required for this tool")),
    }
}

fn optional_search_provider_arg(args: &Value) -> Option<&str> {
    string_arg(args, "provider")
        .map(str::trim)
        .filter(|provider| !provider.is_empty() && *provider != "all")
}

fn messages_arg(args: &Value) -> Result<Vec<Value>> {
    let Some(messages) = args.get("messages") else {
        return Ok(Vec::new());
    };
    let Some(messages) = messages.as_array() else {
        return Err(argument_error("messages must be an array"));
    };
    Ok(messages.clone())
}

fn string_array_arg(args: &Value, name: &str) -> Result<Vec<String>> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(argument_error(format!("{name} must be an array")));
    };
    values
        .iter()
        .map(|value| {
            if let Some(text) = value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                return Ok(text.to_string());
            }
            if let Some(integer) = value.as_i64() {
                if integer >= 0 {
                    return Ok(integer.to_string());
                }
            }
            Err(argument_error(format!(
                "{name} must contain only non-empty strings or non-negative integers"
            )))
        })
        .collect()
}

fn summarizer_arg(args: &Value) -> Result<LcmSummarizerMode> {
    let mode = match args.get("summarizer") {
        Some(summarizer) => {
            serde_json::from_value(summarizer.clone()).map_err(|err| TraceDecayError::Config {
                message: format!("invalid summarizer: {err}"),
            })?
        }
        None => LcmSummarizerMode::HermesAuxiliary,
    };
    if matches!(mode, LcmSummarizerMode::Noop) && hard_compression_pressure(args)? {
        Ok(LcmSummarizerMode::HermesAuxiliary)
    } else {
        Ok(mode)
    }
}

fn hard_compression_pressure(args: &Value) -> Result<bool> {
    let Some(current_tokens) = non_negative_i64_arg(args, "current_tokens")? else {
        return Ok(false);
    };
    if non_negative_i64_arg(args, "threshold_tokens")?
        .is_some_and(|threshold| threshold > 0 && current_tokens >= threshold)
    {
        return Ok(true);
    }
    let assembly_cap = compression_decision::effective_assembly_token_cap(AssemblyCapInput {
        max_assembly_tokens: non_negative_i64_arg(args, "max_assembly_tokens")?,
        context_length: non_negative_i64_arg(args, "context_length")?,
        reserve_tokens_floor: non_negative_i64_arg(args, "reserve_tokens_floor")?,
    });
    Ok(assembly_cap.is_some_and(|cap| current_tokens >= cap))
}

fn lcm_content_slice(args: &Value) -> Result<LcmContentSlice> {
    Ok(LcmContentSlice {
        offset: bounded_usize_arg(args, "content_offset", 0, usize::MAX)?.unwrap_or(0),
        limit: bounded_usize_arg(args, "content_limit", 1, MAX_LCM_CONTENT_LIMIT)?
            .unwrap_or(DEFAULT_LCM_CONTENT_LIMIT),
    })
}

fn lcm_load_content_slice(args: &Value) -> Result<(LcmContentSlice, Option<usize>)> {
    let offset = bounded_usize_arg(args, "content_offset", 0, usize::MAX)?.unwrap_or(0);
    let requested_limit = match args.get("content_limit") {
        Some(value) => {
            let Some(integer) = value.as_i64() else {
                return Err(argument_error("content_limit must be an integer"));
            };
            if integer <= 0 {
                return Err(argument_error("content_limit must be >= 1"));
            }
            usize::try_from(integer).map_err(|_| {
                argument_error(format!(
                    "content_limit must be <= {MAX_LCM_LOAD_CONTENT_LIMIT}"
                ))
            })?
        }
        None => DEFAULT_LCM_CONTENT_LIMIT,
    };
    let limit = requested_limit.min(MAX_LCM_LOAD_CONTENT_LIMIT);
    let clamped_from = (requested_limit > limit).then_some(requested_limit);
    Ok((LcmContentSlice { offset, limit }, clamped_from))
}

fn lcm_doctor_mode(args: &Value) -> Result<&str> {
    let mode = string_arg(args, "mode").unwrap_or("diagnose");
    match mode {
        "diagnose" | "repair" | "retention" | "clean" | "gc" => Ok(mode),
        _ => Err(argument_error(
            "mode must be one of diagnose, repair, retention, clean, gc",
        )),
    }
}

fn lcm_doctor_clean_apply_enabled(args: &Value) -> Result<bool> {
    match args.get("doctor_clean_apply_enabled") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| argument_error("doctor_clean_apply_enabled must be a boolean")),
        None => Ok(crate::global_db::env_flag("LCM_DOCTOR_CLEAN_APPLY_ENABLED")),
    }
}

fn lcm_gc_apply_enabled(args: &Value) -> Result<bool> {
    match args.get("lcm_gc_apply_enabled") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| argument_error("lcm_gc_apply_enabled must be a boolean")),
        None => Ok(crate::global_db::env_flag("LCM_GC_APPLY_ENABLED")),
    }
}

fn lcm_clean_config(args: &Value) -> Result<LcmCleanConfig> {
    Ok(LcmCleanConfig {
        ignore_session_patterns: string_array_arg(args, "ignore_session_patterns")?,
        stateless_session_patterns: string_array_arg(args, "stateless_session_patterns")?,
        ignore_message_patterns: string_array_arg(args, "ignore_message_patterns")?,
    })
}

fn lcm_gc_config(args: &Value) -> Result<LcmGcConfig> {
    match args.get("gc_config") {
        Some(value) => serde_json::from_value::<LcmGcConfig>(value.clone()).map_err(|err| {
            argument_error(format!(
                "gc_config must be a valid LcmGcConfig object: {err}"
            ))
        }),
        None => Ok(LcmGcConfig::default()),
    }
}

// By-value so it can be used point-free as a `map_err` adapter.
#[allow(clippy::needless_pass_by_value)]
fn lcm_error(err: crate::sessions::lcm::LcmError) -> TraceDecayError {
    TraceDecayError::Config {
        message: err.to_string(),
    }
}

fn lcm_unavailable(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "message": "could not open active project tracedecay session database",
        }),
    )
}

/// Returned by pure-read tools when the sessions.db file has not been
/// created yet (nothing has been ingested). Distinct from "unavailable"
/// so callers can tell "no data yet" apart from "open failed".
/// The `store_exists: false` field is the machine-readable discriminator;
/// other fields are backward-compatible additions.
fn lcm_not_yet_ingested(args: &Value, storage_scope: &str) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "not_ingested",
            "store_exists": false,
            "storage_scope": storage_scope,
            "message": "session store does not exist yet — nothing has been ingested",
        }),
    )
}

fn lcm_scoped_unavailable(
    args: &Value,
    storage_scope: &str,
    message: impl Into<String>,
) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "storage_scope": storage_scope,
            "message": message.into(),
        }),
    )
}

fn lcm_storage_scope_unavailable(args: &Value, storage_scope: &str) -> ToolResult {
    lcm_scoped_unavailable(
        args,
        storage_scope,
        format!(
            "{storage_scope} LCM status storage is not available from the active project handler"
        ),
    )
}

fn project_local_storage_without_project(args: &Value) -> ToolResult {
    lcm_scoped_unavailable(
        args,
        "project_local",
        "project_local LCM storage requires an initialized TraceDecay project root",
    )
}

struct LcmStorage {
    db: GlobalDb,
    scope: &'static str,
}

fn available_lcm_storage(db: GlobalDb, scope: &'static str) -> LcmStorageResolution {
    LcmStorageResolution::Available(Box::new(LcmStorage { db, scope }))
}

/// Database paths whose schema (sessions DDL + LCM migrations) has already
/// been ensured by this process. In `tracedecay serve`, every LCM tool call
/// re-opens the project session DB; once `GlobalDb::open_at` has ensured the
/// schema for a path, later opens skip the DDL batch and the LCM version
/// gate entirely via `open_at_assuming_schema`. The schema only ever grows
/// and lives in the file itself, so a concurrent process cannot invalidate
/// the flag; the `is_file` check below covers the file being deleted
/// underneath a long-lived server. One-shot CLI invocations open each path
/// once, so their behavior is unchanged.
///
/// Connections are deliberately NOT cached: each call still opens a fresh
/// libsql local connection. Sharing a long-lived handle across tool calls
/// would have to prove cross-process WAL safety and stale-handle recovery
/// (other processes checkpoint and write the same file), which is not worth
/// the risk for a per-call open that is cheap once the DDL is skipped.
static ENSURED_SCHEMA_DB_PATHS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn schema_already_ensured(db_path: &Path) -> bool {
    db_path.is_file()
        && ENSURED_SCHEMA_DB_PATHS
            .lock()
            .is_ok_and(|paths| paths.contains(db_path))
}

fn mark_schema_ensured(db_path: &Path) {
    if let Ok(mut paths) = ENSURED_SCHEMA_DB_PATHS.lock() {
        paths.insert(db_path.to_path_buf());
    }
}

/// Opens a writable session DB, ensuring the schema at most once per
/// process per path (see [`ENSURED_SCHEMA_DB_PATHS`]).
async fn open_session_db_with_cached_ensure(db_path: &Path) -> Option<GlobalDb> {
    if schema_already_ensured(db_path) {
        if let Some(db) = GlobalDb::open_at_assuming_schema(db_path).await {
            return Some(db);
        }
        // Fast path failed (e.g. file replaced mid-session): fall through to
        // a full ensure rather than failing the tool call.
    }
    let db = GlobalDb::open_at(db_path).await?;
    mark_schema_ensured(db_path);
    Some(db)
}

enum LcmStorageResolution {
    Available(Box<LcmStorage>),
    Unavailable(ToolResult),
}

fn invalid_hermes_profile_home(args: &Value, message: impl Into<String>) -> ToolResult {
    lcm_scoped_unavailable(args, "hermes_profile", message)
}

fn hermes_profile_home(args: &Value) -> std::result::Result<PathBuf, ToolResult> {
    let Some(hermes_home) = string_arg(args, "hermes_home") else {
        return Err(invalid_hermes_profile_home(
            args,
            "hermes_profile LCM storage requires an explicit absolute hermes_home",
        ));
    };
    let path = PathBuf::from(hermes_home);
    if !path.is_absolute() {
        return Err(invalid_hermes_profile_home(
            args,
            "hermes_profile LCM storage requires an absolute hermes_home",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid_hermes_profile_home(
            args,
            "hermes_profile LCM storage requires a normalized absolute hermes_home",
        ));
    }
    let Ok(canonical) = std::fs::canonicalize(&path) else {
        return Err(invalid_hermes_profile_home(
            args,
            format!(
                "hermes_home does not exist or is not a directory: {}",
                path.display()
            ),
        ));
    };
    if !canonical.is_dir() {
        return Err(invalid_hermes_profile_home(
            args,
            format!(
                "hermes_home does not exist or is not a directory: {}",
                path.display()
            ),
        ));
    }
    Ok(canonical)
}

/// How an LCM storage open treats the backing sessions.db.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LcmOpenMode {
    /// Writable open: creates the store and ensures schema as needed.
    Writable,
    /// Read-only: a missing store is a hard error.
    ReadOnlyExisting,
    /// Read-only: a missing store is a distinguishable `not_ingested`
    /// result, without creating the file. Use this for every `readOnlyHint`
    /// LCM handler so "nothing ingested yet" never looks like "ok, 0 rows"
    /// (and the tool never ghost-creates an empty sessions.db).
    ReadOnlyOrMissing,
}

async fn open_lcm_db_at(db_path: &Path, mode: LcmOpenMode) -> Option<GlobalDb> {
    match mode {
        LcmOpenMode::Writable => open_session_db_with_cached_ensure(db_path).await,
        LcmOpenMode::ReadOnlyExisting | LcmOpenMode::ReadOnlyOrMissing => {
            GlobalDb::open_read_only_at(db_path).await
        }
    }
}

macro_rules! lcm_open_storage {
    ($context:expr, $args:expr) => {
        match open_lcm_storage($context, $args, LcmOpenMode::Writable).await {
            LcmStorageResolution::Available(storage) => storage,
            LcmStorageResolution::Unavailable(result) => return Ok(result),
        }
    };
}

/// Like `lcm_open_storage!` but with [`LcmOpenMode::ReadOnlyOrMissing`]
/// semantics for `readOnlyHint` handlers.
macro_rules! lcm_open_storage_ro {
    ($context:expr, $args:expr) => {
        match open_lcm_storage($context, $args, LcmOpenMode::ReadOnlyOrMissing).await {
            LcmStorageResolution::Available(storage) => storage,
            LcmStorageResolution::Unavailable(result) => return Ok(result),
        }
    };
}

async fn open_lcm_storage(
    context: LcmHandlerContext<'_>,
    args: &Value,
    mode: LcmOpenMode,
) -> LcmStorageResolution {
    let storage_scope = string_arg(args, "storage_scope").unwrap_or("project_local");
    match storage_scope {
        "project_local" => {
            if context.project_root.is_none() {
                return LcmStorageResolution::Unavailable(project_local_storage_without_project(
                    args,
                ));
            }
            let Some(db_path) = context.project_session_db_path else {
                return LcmStorageResolution::Unavailable(project_local_storage_without_project(
                    args,
                ));
            };
            let db_path = db_path.to_path_buf();
            if mode == LcmOpenMode::ReadOnlyOrMissing && !db_path.is_file() {
                return LcmStorageResolution::Unavailable(lcm_not_yet_ingested(
                    args,
                    "project_local",
                ));
            }
            let Some(db) = open_lcm_db_at(&db_path, mode).await else {
                return LcmStorageResolution::Unavailable(lcm_unavailable(args));
            };
            available_lcm_storage(db, "project_local")
        }
        "hermes_profile" => {
            let hermes_home = match hermes_profile_home(args) {
                Ok(hermes_home) => hermes_home,
                Err(result) => return LcmStorageResolution::Unavailable(result),
            };
            let db_path = match mode {
                LcmOpenMode::Writable => {
                    match crate::sessions::cursor::resolve_hermes_profile_session_db_path(
                        &hermes_home,
                    ) {
                        Ok(db_path) => db_path,
                        Err(message) => {
                            return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                                args, message,
                            ));
                        }
                    }
                }
                LcmOpenMode::ReadOnlyExisting | LcmOpenMode::ReadOnlyOrMissing => {
                    match crate::sessions::cursor::resolve_hermes_profile_session_db_readonly(
                        &hermes_home,
                    ) {
                        HermesProfileDbReadOnly::Exists(db_path) => db_path,
                        HermesProfileDbReadOnly::NotIngested(db_path) => {
                            return LcmStorageResolution::Unavailable(match mode {
                                LcmOpenMode::ReadOnlyOrMissing => {
                                    lcm_not_yet_ingested(args, "hermes_profile")
                                }
                                _ => invalid_hermes_profile_home(
                                    args,
                                    format!(
                                        "hermes_profile LCM storage requires an existing session database: {}",
                                        db_path.display()
                                    ),
                                ),
                            });
                        }
                        HermesProfileDbReadOnly::ConfigError(msg) => {
                            return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                                args, msg,
                            ));
                        }
                    }
                }
            };
            let Some(db) = open_lcm_db_at(&db_path, mode).await else {
                return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                    args,
                    "could not open hermes_profile tracedecay session database",
                ));
            };
            available_lcm_storage(db, "hermes_profile")
        }
        other => LcmStorageResolution::Unavailable(lcm_storage_scope_unavailable(args, other)),
    }
}

fn parse_lcm_scope(args: &Value) -> Result<LcmScope> {
    let Some(value) = args.get("scope") else {
        return Ok(LcmScope::All);
    };
    let Some(scope) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(argument_error("scope must be one of current, session, all"));
    };
    match scope {
        "current" => Ok(LcmScope::Current),
        "session" => Ok(LcmScope::Session),
        "all" => Ok(LcmScope::All),
        _ => Err(argument_error("scope must be one of current, session, all")),
    }
}

fn lcm_grep_provider_arg(args: &Value) -> &str {
    if let Some(provider) = optional_search_provider_arg(args) {
        return provider;
    }
    "all"
}

fn parse_lcm_grep_sort(args: &Value) -> Result<LcmGrepSort> {
    let Some(sort) = string_arg(args, "sort") else {
        return Ok(LcmGrepSort::Recency);
    };
    sort.parse::<LcmGrepSort>()
        .map_err(|()| argument_error("sort must be one of recency, relevance, hybrid"))
}

fn parse_lcm_summary_node_id(target: &Value) -> Result<String> {
    required_string_arg(target, "node_id")
        .map(str::to_string)
        .map_err(|_| TraceDecayError::Config {
            message: "target.node_id is required when target.kind is summary_node".to_string(),
        })
}

fn parse_lcm_external_payload_ref(target: &Value) -> Result<String> {
    required_string_arg(target, "payload_ref")
        .map(str::to_string)
        .map_err(|_| TraceDecayError::Config {
            message: "target.payload_ref is required when target.kind is external_payload"
                .to_string(),
        })
}

fn parse_lcm_describe_target(args: &Value) -> Result<LcmDescribeTarget> {
    let Some(target) = args.get("target") else {
        return Ok(LcmDescribeTarget::Session);
    };
    match string_arg(target, "kind").unwrap_or_default() {
        "summary_node" => Ok(LcmDescribeTarget::SummaryNode {
            node_id: parse_lcm_summary_node_id(target)?,
        }),
        "external_payload" => Ok(LcmDescribeTarget::ExternalPayload {
            payload_ref: parse_lcm_external_payload_ref(target)?,
        }),
        "session" => Ok(LcmDescribeTarget::Session),
        _ => Err(TraceDecayError::Config {
            message: "target.kind must be one of session, summary_node, external_payload"
                .to_string(),
        }),
    }
}

fn parse_lcm_expand_target(args: &Value) -> Result<LcmExpandTarget> {
    let target = args.get("target").ok_or_else(|| TraceDecayError::Config {
        message: "missing required parameter: target".to_string(),
    })?;
    match string_arg(target, "kind").unwrap_or_default() {
        "raw_message" => {
            let store_id = non_negative_i64_arg(target, "store_id")?.ok_or_else(|| {
                TraceDecayError::Config {
                    message: "target.store_id is required when target.kind is raw_message"
                        .to_string(),
                }
            })?;
            Ok(LcmExpandTarget::RawMessage { store_id })
        }
        "summary_node" => Ok(LcmExpandTarget::SummaryNode {
            node_id: parse_lcm_summary_node_id(target)?,
        }),
        "external_payload" => Ok(LcmExpandTarget::ExternalPayload {
            payload_ref: parse_lcm_external_payload_ref(target)?,
        }),
        _ => Err(TraceDecayError::Config {
            message: "target.kind must be one of raw_message, summary_node, external_payload"
                .to_string(),
        }),
    }
}

/// Parses the `scope` argument for `tracedecay_message_search`. Like
/// [`parse_lcm_scope`], invalid values are a hard error naming the valid set —
/// never silently broadened to `all`.
fn parse_message_search_scope(args: &Value) -> Result<SessionSearchScope> {
    let Some(value) = args.get("scope") else {
        return Ok(SessionSearchScope::All);
    };
    match value.as_str().map(str::trim) {
        Some("all") => Ok(SessionSearchScope::All),
        Some("parents_only") => Ok(SessionSearchScope::ParentsOnly),
        Some("subagents_only") => Ok(SessionSearchScope::SubagentsOnly),
        _ => Err(argument_error(
            "scope must be one of all, parents_only, subagents_only",
        )),
    }
}

fn parse_message_search_provider_scope(args: &Value) -> Result<ProviderScope> {
    ProviderScope::parse_optional(string_arg(args, "provider")).map_err(argument_error)
}

/// Parses the optional `branch` / `worktree` / `commit` git-scope filter
/// arguments shared by `tracedecay_message_search` and `tracedecay_lcm_grep`.
fn parse_git_scope_filter(args: &Value) -> Result<GitScopeFilter> {
    GitScopeFilter::from_args(
        string_arg(args, "branch"),
        string_arg(args, "worktree"),
        string_arg(args, "commit"),
    )
    .map_err(|err| argument_error(err.to_string()))
}

const DEFAULT_SESSIONS_FOR_LIMIT: usize = 20;

/// Renders `tracedecay_sessions_for` results as compact markdown: one bullet
/// per correlated session with its activity window or commit attribution.
fn render_sessions_for_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Sessions For Git Ref");
    for key in ["git_ref", "value"] {
        let field = render::field_str(value, key);
        if !field.is_empty() {
            md.field(key, field);
        }
    }
    md.field("count", &render::field_i64(value, "count").to_string());
    let results = value.get("results").and_then(Value::as_array);
    match results {
        Some(results) if !results.is_empty() => {
            md.blank();
            for hit in results {
                append_sessions_for_hit(&mut md, hit);
            }
        }
        _ => {
            md.blank()
                .empty_note("No correlated sessions recorded for this git ref.");
        }
    }
    md.render()
}

fn append_sessions_for_hit(md: &mut Md, hit: &Value) {
    let session_id = render::field_str(hit, "session_id");
    let provider = render::field_str(hit, "provider");
    let mut header = format!("session `{session_id}`");
    if !provider.is_empty() {
        let _ = write!(header, " · {provider}");
    }
    md.bullet(&header);
    let mut detail = String::new();
    if let Some(branch) = hit.get("branch").and_then(Value::as_str) {
        let _ = write!(detail, "branch `{branch}`");
    }
    if let Some(worktree) = hit.get("worktree").and_then(Value::as_str) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(detail, "worktree `{worktree}`");
    }
    if let (Some(first), Some(last)) = (
        hit.get("first_ts").and_then(Value::as_i64),
        hit.get("last_ts").and_then(Value::as_i64),
    ) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(
            detail,
            "active {} .. {}",
            crate::timeutil::humanize_unix_secs(first),
            crate::timeutil::humanize_unix_secs(last)
        );
    }
    if let Some(sha) = hit.get("commit_sha").and_then(Value::as_str) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let short = sha.get(..12).unwrap_or(sha);
        let _ = write!(detail, "commit `{short}`");
        if let Some(committed_at) = hit.get("committed_at").and_then(Value::as_i64) {
            let _ = write!(
                detail,
                " at {}",
                crate::timeutil::humanize_unix_secs(committed_at)
            );
        }
    }
    if !detail.is_empty() {
        md.line(&format!("  {detail}"));
    }
}

pub(super) async fn handle_sessions_for(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let kind = required_string_arg(&args, "git_ref")?;
    let value = required_string_arg(&args, "value")?;
    let git_ref =
        GitRefFilter::parse(kind, value).map_err(|err| argument_error(err.to_string()))?;
    let since = non_negative_timestamp_arg(&args, "since", SearchTimeBound::Start)?;
    let until = non_negative_timestamp_arg(&args, "until", SearchTimeBound::End)?;
    let limit = bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?
        .unwrap_or(DEFAULT_SESSIONS_FOR_LIMIT);
    let query = SessionsForQuery {
        git_ref,
        since,
        until,
        limit,
    };

    // Read-only lookup against the project session store; a missing store
    // means nothing was ever recorded, which is a valid empty result (the
    // tool never ghost-creates an empty sessions.db).
    let db_path = cg.store_layout().sessions_db_path.clone();
    let results = if db_path.is_file() {
        let Some(db) = GlobalDb::open_read_only_at(&db_path).await else {
            return Ok(tool_json(
                Some(cg.project_root()),
                &args,
                &json!({
                    "status": "unavailable",
                    "message": "could not open project tracedecay session database",
                    "results": [],
                    "count": 0
                }),
            ));
        };
        db.git_sessions_for(&query)
            .await
            .map_err(|err| TraceDecayError::Config {
                message: err.to_string(),
            })?
    } else {
        Vec::new()
    };

    let payload = json!({
        "status": "ok",
        "git_ref": query.git_ref.kind(),
        "value": query.git_ref.value(),
        "since": since,
        "until": until,
        "count": results.len(),
        "results": results,
    });
    Ok(tool_json_with_md(
        Some(cg.project_root()),
        &args,
        &payload,
        || render_sessions_for_md(&payload),
    ))
}

pub(super) async fn handle_message_search(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?;
    let provider_scope = parse_message_search_provider_scope(&args)?;
    let requested_provider = provider_scope.provider_id();
    let project_key = args
        .get("project_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project_key| !project_key.is_empty());
    let parent_session_id = args
        .get("parent_session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|parent_session_id| !parent_session_id.is_empty());
    let include_subagents = args
        .get("include_subagents")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let catch_up = args
        .get("catch_up")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut scope = parse_message_search_scope(&args)?;
    if !include_subagents && matches!(scope, SessionSearchScope::All) {
        scope = SessionSearchScope::ParentsOnly;
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 50) as usize;
    let git_filter = parse_git_scope_filter(&args)?;
    let git_filter_applied = !git_filter.is_empty();
    let time_range = message_search_time_range(&args)?;

    let Some((db_path, target_root)) = selected_project_session_db_path(
        cg.project_root(),
        &cg.store_layout().sessions_db_path,
        &args,
        global_db,
        allow_default_registry_fallback,
    )
    .await?
    else {
        return Ok(tool_json(
            Some(cg.project_root()),
            &args,
            &json!({
                "status": "unavailable",
                "message": "could not resolve selected project tracedecay session database",
                "results": [],
                "count": 0
            }),
        ));
    };
    let Some(db) = open_session_db_with_cached_ensure(&db_path).await else {
        return Ok(tool_json(
            Some(cg.project_root()),
            &args,
            &json!({
                "status": "unavailable",
                "message": "could not open selected project tracedecay session database",
                "results": [],
                "count": 0
            }),
        ));
    };
    let catch_up_performed = catch_up;
    if catch_up_performed {
        let _ = crate::sessions::ingest_global_sources_for_provider(
            &db,
            &target_root,
            provider_scope.provider(),
        )
        .await;
    }
    let results = if git_filter_applied {
        db.search_session_messages_git_scoped(
            requested_provider,
            project_key,
            query,
            limit,
            SessionSearchFilters {
                scope,
                parent_session_id,
                time_range,
            },
            &git_filter,
        )
        .await
    } else if let Some(provider) = requested_provider {
        db.search_session_messages_filtered(
            provider,
            project_key,
            query,
            limit,
            SessionSearchFilters {
                scope,
                parent_session_id,
                time_range,
            },
        )
        .await
    } else {
        db.search_session_messages_all_providers_filtered(
            project_key,
            query,
            limit,
            SessionSearchFilters {
                scope,
                parent_session_id,
                time_range,
            },
        )
        .await
    };

    let mut payload = json!({
        "status": "ok",
        "provider": requested_provider.unwrap_or("all"),
        "requested_provider": requested_provider,
        "selected_project_root": target_root,
        "project_key": project_key,
        "parent_session_id": parent_session_id,
        "include_subagents": include_subagents,
        "catch_up": catch_up,
        "catch_up_performed": catch_up_performed,
        "catch_up_provider": provider_scope.response_label(),
        "scope": match scope {
            SessionSearchScope::All => "all",
            SessionSearchScope::ParentsOnly => "parents_only",
            SessionSearchScope::SubagentsOnly => "subagents_only",
        },
        "since": time_range.start_time,
        "until": time_range.end_time,
        "query": query,
        "count": results.len(),
        "results": results,
    });
    if git_filter_applied {
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                "git_filter".to_string(),
                serde_json::to_value(&git_filter).unwrap_or(Value::Null),
            );
            map.insert("git_filter_applied".to_string(), Value::Bool(true));
        }
    }
    Ok(tool_json_with_md(
        Some(&target_root),
        &args,
        &payload,
        || render_message_search_md(&payload),
    ))
}

pub(super) async fn handle_lcm_status(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_or_all_arg(&args);
    let session_id = string_arg(&args, "session_id");
    let deep = bool_arg(&args, "deep")?.unwrap_or(false);
    let gc_config = lcm_gc_config(&args)?;
    let storage = lcm_open_storage_ro!(context, &args);
    let mut status = storage
        .db
        .lcm_status_with_options(provider, session_id, deep, &gc_config)
        .await
        .map_err(lcm_error)?;
    status.storage_scope = Some(storage.scope.to_string());
    Ok(tool_json(
        context.project_root,
        &args,
        &json!({
            "status": "ok",
            "provider": provider,
            "session_id": session_id,
            "deep": deep,
            "lcm": status,
        }),
    ))
}

pub(super) async fn handle_lcm_doctor(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = string_arg(&args, "session_id");
    let mode = lcm_doctor_mode(&args)?;
    let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
    let clean_apply_enabled = lcm_doctor_clean_apply_enabled(&args)?;
    let gc_apply_enabled = lcm_gc_apply_enabled(&args)?;
    if mode == "clean" && apply && !clean_apply_enabled {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "denied",
                "provider": provider,
                "session_id": session_id,
                "mode": mode,
                "dry_run": false,
                "apply": true,
                "error": "destructive cleanup is disabled by default",
                "note": "set LCM_DOCTOR_CLEAN_APPLY_ENABLED=true only in trusted operator environments",
                "repairs": {
                    "planned_actions": [],
                    "applied_actions": [],
                    "backup": Value::Null,
                    "unsafe_actions_skipped": [
                        {
                            "kind": "clean_lcm_noise",
                            "safe": false,
                            "reason": "doctor_clean_apply_disabled"
                        }
                    ]
                }
            }),
        ));
    }
    if mode == "gc" && apply && !gc_apply_enabled {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "denied",
                "provider": provider,
                "session_id": session_id,
                "mode": mode,
                "dry_run": false,
                "apply": true,
                "error": "payload GC apply is disabled by default",
                "note": "set LCM_GC_APPLY_ENABLED=true only in trusted operator environments",
                "repairs": {
                    "planned_actions": [],
                    "applied_actions": [],
                    "backup": Value::Null,
                    "unsafe_actions_skipped": [
                        {
                            "kind": "payload_gc",
                            "safe": false,
                            "reason": "lcm_gc_apply_disabled"
                        }
                    ]
                }
            }),
        ));
    }
    let clean_config = lcm_clean_config(&args)?;
    let gc_config = lcm_gc_config(&args)?;
    let open_mode = if matches!(mode, "repair" | "clean" | "gc") && apply {
        LcmOpenMode::Writable
    } else {
        LcmOpenMode::ReadOnlyExisting
    };
    let storage = match open_lcm_storage(context, &args, open_mode).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let mut payload = storage
        .db
        .lcm_doctor(provider, session_id, mode, apply, clean_config, gc_config)
        .await
        .map_err(lcm_error)?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("storage_scope".to_string(), json!(storage.scope));
        if let Some(diagnostics) = object
            .get_mut("diagnostics")
            .and_then(serde_json::Value::as_object_mut)
        {
            diagnostics.insert(
                "ast_grep".to_string(),
                super::super::definitions::ast_grep_diagnostics_json(),
            );
        }
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

pub(super) async fn handle_lcm_load_session(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_or_all_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let (content_slice, content_limit_clamped_from) = lcm_load_content_slice(&args)?;
    let storage = lcm_open_storage_ro!(context, &args);
    let page = storage
        .db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            after_store_id: non_negative_i64_arg(&args, "after_store_id")?,
            limit: bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(50),
            roles: {
                let mut roles = string_array_arg(&args, "roles")?;
                if roles.is_empty() {
                    if let Some(role) = string_arg(&args, "role") {
                        roles.push(role.to_string());
                    }
                }
                roles
            },
            start_time: non_negative_i64_arg_alias(&args, "start_time", "time_from")?,
            end_time: non_negative_i64_arg_alias(&args, "end_time", "time_to")?,
            content_slice: Some(content_slice),
        })
        .await
        .map_err(lcm_error)?;
    let mut payload = json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "messages": page.messages,
        "next_cursor": page.next_cursor,
        "content_limit": content_slice.limit,
    });
    if let Some(clamped_from) = content_limit_clamped_from {
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "content_limit_clamped_from".to_string(),
                json!(clamped_from),
            );
        }
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

pub(super) async fn handle_lcm_grep(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let query = required_string_arg(&args, "query")?;
    // Validate scope before opening storage so argument errors are reported
    // even when the sessions DB does not exist yet.
    let scope = parse_lcm_scope(&args)?;
    let provider = lcm_grep_provider_arg(&args);
    let git_filter = parse_git_scope_filter(&args)?;
    let git_filter_applied = !git_filter.is_empty();
    let storage = lcm_open_storage_ro!(context, &args);
    let hits = storage
        .db
        .lcm_grep(LcmGrepRequest {
            provider: provider.to_string(),
            query: query.to_string(),
            scope,
            session_id: string_arg(&args, "session_id").map(str::to_string),
            include_summaries: args
                .get("include_summaries")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            limit: bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(10),
            sort: parse_lcm_grep_sort(&args)?,
            source: string_arg(&args, "source").map(str::to_string),
            role: string_arg(&args, "role").map(str::to_string),
            start_time: non_negative_timestamp_arg_aliases(
                &args,
                &["since", "start_time", "time_from"],
                SearchTimeBound::Start,
            )?,
            end_time: non_negative_timestamp_arg_aliases(
                &args,
                &["until", "end_time", "time_to"],
                SearchTimeBound::End,
            )?,
            git_filter: git_filter.clone(),
        })
        .await
        .map_err(lcm_error)?;
    let mut payload = json!({
        "status": "ok",
        "provider": provider,
        "query": query,
        "count": hits.len(),
        "hits": hits,
        "sort": string_arg(&args, "sort").unwrap_or("recency"),
    });
    if git_filter_applied {
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                "git_filter".to_string(),
                serde_json::to_value(&git_filter).unwrap_or(Value::Null),
            );
            map.insert("git_filter_applied".to_string(), Value::Bool(true));
        }
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

pub(super) async fn handle_lcm_describe(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    // Validate target before opening storage so argument errors are reported
    // even when the sessions DB does not exist yet.
    let target = parse_lcm_describe_target(&args)?;
    let storage = lcm_open_storage_ro!(context, &args);
    let description = storage
        .db
        .lcm_describe(LcmDescribeRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            target,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        context.project_root,
        &args,
        &json!({
            "status": "ok",
            "provider": provider,
            "session_id": session_id,
            "description": description,
        }),
    ))
}

pub(super) async fn handle_lcm_expand(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let target = parse_lcm_expand_target(&args)?;
    let storage = lcm_open_storage_ro!(context, &args);
    let expansion = storage
        .db
        .lcm_expand(LcmExpandRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            target,
            content_slice: Some(lcm_content_slice(&args)?),
            source_offset: bounded_usize_arg(&args, "source_offset", 0, usize::MAX)?.unwrap_or(0),
            source_limit: bounded_usize_arg(&args, "source_limit", 1, usize::MAX)?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        context.project_root,
        &args,
        &json!({
            "status": "ok",
            "provider": provider,
            "session_id": session_id,
            "expansion": expansion,
        }),
    ))
}

pub(super) async fn handle_lcm_expand_query(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let prompt = required_string_arg(&args, "prompt")?;
    let max_results =
        bounded_usize_arg(&args, "max_results", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(5);
    let max_tokens =
        bounded_usize_arg(&args, "max_tokens", 1, MAX_LCM_CONTENT_LIMIT)?.unwrap_or(2000);
    // `context_max_tokens` is the retrieval context budget (how much LCM
    // material is assembled before host synthesis). It is orthogonal to
    // `max_tokens` (the synthesis *output* budget): max_tokens ≤ 8 192
    // while context_max_tokens lives in [32 000, 65 536], so a clamp of
    // the form `max_tokens.clamp(32_000, 65_536)` always evaluates to
    // 32_000 — making max_tokens dead. The default is therefore a fixed
    // constant; pass `context_max_tokens` explicitly when a larger budget
    // is wanted.
    let context_max_tokens = bounded_usize_arg(
        &args,
        "context_max_tokens",
        1,
        MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT,
    )?
    .unwrap_or(DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT);
    let storage = lcm_open_storage_ro!(context, &args);
    let response = storage
        .db
        .lcm_expand_query(LcmExpandQueryRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            prompt: prompt.to_string(),
            query: string_arg(&args, "query").map(str::to_string),
            node_ids: string_array_arg(&args, "node_ids")?,
            max_results,
            max_tokens,
            context_max_tokens,
        })
        .await
        .map_err(lcm_error)?;
    let mut payload = serde_json::to_value(response).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize expand-query response: {err}"),
    })?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("status".to_string(), json!("ok"));
        object.insert("provider".to_string(), json!(provider));
        object.insert("session_id".to_string(), json!(session_id));
        object.insert("storage_scope".to_string(), json!(storage.scope));
    }
    Ok(lcm_expand_query_tool_json(
        context.project_root,
        &args,
        &payload,
    ))
}

pub(super) async fn handle_lcm_session_boundary(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = lcm_open_storage!(context, &args);
    let response = storage
        .db
        .lcm_session_boundary(LcmSessionBoundaryRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            old_session_id: string_arg(&args, "old_session_id").map(str::to_string),
            boundary_reason: string_arg(&args, "boundary_reason").map(str::to_string),
            bound_session_id: string_arg(&args, "bound_session_id").map(str::to_string),
            boundary_skip_at: None,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        context.project_root,
        &args,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "recorded": response.recorded,
            "reason": response.reason,
        }),
    ))
}

pub(super) async fn handle_lcm_preflight(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = lcm_open_storage!(context, &args);
    let response = storage
        .db
        .lcm_preflight(LcmPreflightRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            threshold_tokens: non_negative_i64_arg(&args, "threshold_tokens")?,
            max_assembly_tokens: non_negative_i64_arg(&args, "max_assembly_tokens")?,
            leaf_chunk_tokens: non_negative_i64_arg(&args, "leaf_chunk_tokens")?,
            max_source_messages: bounded_usize_arg(&args, "max_source_messages", 1, usize::MAX)?,
            summary_fan_in: bounded_usize_arg(&args, "summary_fan_in", 2, usize::MAX)?,
            incremental_max_depth: signed_i64_arg(&args, "incremental_max_depth")?,
            fresh_tail_count: bounded_usize_arg(&args, "fresh_tail_count", 0, usize::MAX)?,
            dynamic_leaf_chunk_enabled: bool_arg(&args, "dynamic_leaf_chunk_enabled")?,
            dynamic_leaf_chunk_max: non_negative_i64_arg(&args, "dynamic_leaf_chunk_max")?,
            context_length: non_negative_i64_arg(&args, "context_length")?,
            reserve_tokens_floor: non_negative_i64_arg(&args, "reserve_tokens_floor")?,
            ignore_session_patterns: string_array_arg(&args, "ignore_session_patterns")?,
            stateless_session_patterns: string_array_arg(&args, "stateless_session_patterns")?,
            ignore_message_patterns: string_array_arg(&args, "ignore_message_patterns")?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(lcm_preflight_tool_json(
        context.project_root,
        &args,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "should_compress": response.should_compress,
            "reason": response.reason,
            "replay_messages": response.replay_messages,
        }),
    ))
}

pub(super) async fn handle_lcm_compress(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let response_handle_root = lcm_response_handle_root(context.project_root, &args);
    let storage = lcm_open_storage!(context, &args);
    let response = storage
        .db
        .lcm_compress(LcmCompressionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            focus_topic: string_arg(&args, "focus_topic").map(str::to_string),
            ignore_session_patterns: string_array_arg(&args, "ignore_session_patterns")?,
            stateless_session_patterns: string_array_arg(&args, "stateless_session_patterns")?,
            ignore_message_patterns: string_array_arg(&args, "ignore_message_patterns")?,
            expected_current_frontier_store_id: non_negative_i64_arg(
                &args,
                "expected_current_frontier_store_id",
            )?,
            threshold_tokens: non_negative_i64_arg(&args, "threshold_tokens")?,
            max_assembly_tokens: non_negative_i64_arg(&args, "max_assembly_tokens")?,
            leaf_chunk_tokens: non_negative_i64_arg(&args, "leaf_chunk_tokens")?,
            max_source_messages: bounded_usize_arg(&args, "max_source_messages", 1, usize::MAX)?,
            summary_fan_in: bounded_usize_arg(&args, "summary_fan_in", 2, usize::MAX)?,
            incremental_max_depth: signed_i64_arg(&args, "incremental_max_depth")?,
            fresh_tail_count: bounded_usize_arg(&args, "fresh_tail_count", 0, usize::MAX)?,
            dynamic_leaf_chunk_enabled: bool_arg(&args, "dynamic_leaf_chunk_enabled")?,
            dynamic_leaf_chunk_max: non_negative_i64_arg(&args, "dynamic_leaf_chunk_max")?,
            context_length: non_negative_i64_arg(&args, "context_length")?,
            reserve_tokens_floor: non_negative_i64_arg(&args, "reserve_tokens_floor")?,
            summarizer: summarizer_arg(&args)?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        response_handle_root.as_deref(),
        &args,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "reason": response.reason,
            "summary_nodes_created": response.summary_nodes_created,
            "summary_nodes": response.summary_nodes,
            "replay_messages": response.replay_messages,
            "replay_token_estimate": response.replay_token_estimate,
            "replay_over_budget": response.replay_over_budget,
            "compression_attempts": response.compression_attempts,
            "fallback_used": response.fallback_used,
            "context_recovery_hint": response.context_recovery_hint,
            "retry_status": response.retry_status,
            "frontier": response.frontier,
            "summary_request": response.summary_request,
        }),
    ))
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
