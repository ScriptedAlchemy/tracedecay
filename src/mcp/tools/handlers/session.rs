use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::{Map, Value, json};

use super::super::render::{self, Md, truncated_json_envelope_with_handle};
use super::support::{
    argument_error, profile_root_for_global_db, project_registry_context, safe_profile_relpath,
    string_arg, tool_json, tool_json_with_md,
};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{
    GlobalDb, ParseOffset, ProjectRegistryContext, TranscriptBatch, WorkflowScopeFilter,
};
use crate::mcp::response_handles::{
    RESPONSE_RETRIEVE_TOOL, observe_response_truncation, store_response_handle,
};
use crate::mcp::tools::{MAX_RESPONSE_CHARS, ToolResult};
use crate::sessions::git_correlation::{
    CommitRelationFilter, GitRefFilter, GitScopeFilter, SessionsForQuery,
};
use crate::sessions::lcm::compression_decision::{self, AssemblyCapInput};
use crate::sessions::lcm::{
    LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT, LcmCleanConfig, LcmCompressionRequest,
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandQueryRequest,
    LcmExpandRequest, LcmExpandTarget, LcmGcConfig, LcmGrepFilters, LcmGrepRequest, LcmGrepSort,
    LcmLoadSessionRequest, LcmPreflightRequest, LcmScope, LcmSessionBoundaryRequest,
    LcmSummarizerMode,
};
use crate::sessions::shared::{content_storage_text_and_tools, preview_title};
use crate::sessions::{
    ProviderScope, SessionMessageRecord, SessionMessageSearchResult, SessionMessageType,
    SessionRecord, SessionSearchFilters, SessionSearchScope, SessionSearchTimeRange,
};
use crate::timeutil::SearchTimeBound;
use crate::tracedecay::{TraceDecay, current_timestamp};

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

const MESSAGE_SEARCH_SNIPPET_CHARS: usize = 240;

static MESSAGE_CATCH_UPS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::watch::Sender<bool>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct MessageCatchUpLeader {
    key: String,
    done: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Drop for MessageCatchUpLeader {
    fn drop(&mut self) {
        if let Ok(mut catch_ups) = MESSAGE_CATCH_UPS.lock()
            && catch_ups
                .get(&self.key)
                .is_some_and(|current| Arc::ptr_eq(current, &self.done))
        {
            catch_ups.remove(&self.key);
        }
        let _ = self.done.send(true);
    }
}

enum MessageCatchUpClaim {
    Leader(MessageCatchUpLeader),
    Wait(tokio::sync::watch::Receiver<bool>),
}

fn claim_message_catch_up(key: String) -> MessageCatchUpClaim {
    let mut catch_ups = MESSAGE_CATCH_UPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(done) = catch_ups.get(&key) {
        return MessageCatchUpClaim::Wait(done.subscribe());
    }
    let (done, _) = tokio::sync::watch::channel(false);
    let done = Arc::new(done);
    catch_ups.insert(key.clone(), Arc::clone(&done));
    MessageCatchUpClaim::Leader(MessageCatchUpLeader { key, done })
}

async fn wait_for_message_catch_up(mut done: tokio::sync::watch::Receiver<bool>) {
    while !*done.borrow_and_update() {
        if done.changed().await.is_err() {
            break;
        }
    }
}

/// Renders `tracedecay_message_search` results as compact markdown. Each hit
/// shows provider, session (id + title), role, timestamp, and score with a
/// plain-text snippet of the message body — deliberately dropping the raw
/// `metadata_json`, `source_path`, and `transcript_path` blobs that the generic
/// renderer would dump verbatim into table cells. Pass `format:"json"` to get
/// the full structured records.
fn render_message_search_md(value: &Value) -> String {
    let mut md = Md::new();
    let goals_mode = value.get("goals").and_then(Value::as_bool).unwrap_or(false);
    md.heading(
        2,
        if goals_mode {
            "Session Goals"
        } else {
            "Transcript Search"
        },
    );
    if goals_mode {
        md.field("mode", "goals (latest goal per session)");
    }
    for key in ["query", "provider", "scope"] {
        let field = render::field_str(value, key);
        if !field.is_empty() {
            md.field(key, field);
        }
    }
    md.field("count", &render::field_i64(value, "count").to_string());
    if let Some(scope) = value
        .get("project_scope")
        .and_then(Value::as_str)
        .filter(|scope| !scope.is_empty())
    {
        let searched = render::field_i64(value, "searched_project_count");
        let skipped = render::field_i64(value, "skipped_project_count");
        md.field(
            "project scope",
            &format!("{scope} (searched {searched}, skipped {skipped})"),
        );
    }
    if let Some(summary) = git_filter_summary(value) {
        md.field("git filter", &summary);
    }
    if let Some(summary) = workflow_filter_summary(value) {
        md.field("workflow filter", &summary);
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
            md.blank().empty_note(if goals_mode {
                "No goals recorded for this project."
            } else {
                "No matching messages."
            });
        }
    }
    md.render()
}

/// One-line `scoped to run wf_… agent …` summary of an applied workflow-run
/// filter, or `None` when none was applied. Reads the `workflow_run` /
/// `workflow_agent` keys echoed into the payload by the message-search handler.
fn workflow_filter_summary(value: &Value) -> Option<String> {
    if !value
        .get("workflow_filter_applied")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let run_id = value.get("workflow_run").and_then(Value::as_str)?;
    let mut summary = format!("scoped to run `{run_id}`");
    if let Some(agent) = value.get("workflow_agent").and_then(Value::as_str) {
        let _ = write!(summary, " agent `{agent}`");
    }
    Some(summary)
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
    // A `goal` row carries its lifecycle status in metadata; surface it so a
    // reader can tell whether the session's goal is still active.
    if let Some(goal_line) = goal_status_line(message) {
        md.line(&format!("  {goal_line}"));
    }
    let text = message
        .and_then(|m| m.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let snippet = message_text_snippet(text, MESSAGE_SEARCH_SNIPPET_CHARS);
    if !snippet.is_empty() {
        md.line(&format!("  {snippet}"));
    }
}

/// `goal [status]` prefix for a `kind = 'goal'` hit, reading `status` out of the
/// row's `metadata_json`. Returns `None` for non-goal rows (or goal rows with no
/// recorded status, which still render their objective as the snippet).
fn goal_status_line(message: Option<&Value>) -> Option<String> {
    let message = message?;
    if message.get("kind").and_then(Value::as_str) != Some("goal") {
        return None;
    }
    let status = message
        .get("metadata_json")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|meta| {
            meta.get("status")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    Some(match status {
        Some(status) => format!("goal [{status}]"),
        None => "goal".to_string(),
    })
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

    pub(super) fn user(sessions_db_path: &'a Path) -> Self {
        Self {
            project_root: None,
            project_session_db_path: Some(sessions_db_path),
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

fn message_search_has_registered_project_selector(args: &Value) -> bool {
    args.get("project_selector").is_some()
        || args.get("project_id").is_some()
        || args.get("project_path").is_some()
        || args.get("project_root").is_some()
}

struct MessageSearchRequest<'a> {
    query: &'a str,
    provider_scope: ProviderScope,
    requested_provider: Option<&'static str>,
    project_key: Option<&'a str>,
    parent_session_id: Option<&'a str>,
    workflow_run: Option<&'a str>,
    workflow_agent: Option<&'a str>,
    include_subagents: bool,
    catch_up: bool,
    scope: SessionSearchScope,
    message_type: SessionMessageType,
    limit: usize,
    git_filter: GitScopeFilter,
    time_range: SessionSearchTimeRange,
    workflow_scope: Option<WorkflowScopeFilter>,
    /// When true, ignore FTS and list each session's latest Codex goal
    /// (`kind = 'goal'`) instead. `query` is optional in this mode.
    goals: bool,
}

fn parse_message_search_request(args: &Value) -> Result<MessageSearchRequest<'_>> {
    let goals = args.get("goals").and_then(Value::as_bool).unwrap_or(false);
    let query = match args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
    {
        Some(query) => query,
        // In goals-listing mode the query is optional: the listing is not an
        // FTS search, so an absent query simply lists the most recent goals.
        None if goals => "",
        None => {
            return Err(TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            });
        }
    };
    let provider_scope = parse_message_search_provider_scope(args)?;
    let workflow_run = string_arg(args, "workflow_run");
    let workflow_agent = string_arg(args, "workflow_agent");
    let include_subagents = args
        .get("include_subagents")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut scope = parse_message_search_scope(args)?;
    if !include_subagents && matches!(scope, SessionSearchScope::All) {
        scope = SessionSearchScope::ParentsOnly;
    }
    Ok(MessageSearchRequest {
        query,
        provider_scope,
        requested_provider: provider_scope.provider_id(),
        project_key: args
            .get("project_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|project_key| !project_key.is_empty()),
        parent_session_id: args
            .get("parent_session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|parent_session_id| !parent_session_id.is_empty()),
        workflow_run,
        workflow_agent,
        include_subagents,
        catch_up: args
            .get("catch_up")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        scope,
        message_type: parse_session_message_type(args)?,
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 50) as usize,
        git_filter: parse_git_scope_filter(args)?,
        time_range: message_search_time_range(args)?,
        workflow_scope: workflow_run.map(|run_id| WorkflowScopeFilter {
            run_id: run_id.to_string(),
            agent_label: workflow_agent.map(str::to_string),
        }),
        goals,
    })
}

fn message_search_filters<'a>(request: &MessageSearchRequest<'a>) -> SessionSearchFilters<'a> {
    SessionSearchFilters {
        scope: request.scope,
        message_type: request.message_type,
        parent_session_id: request.parent_session_id,
        time_range: request.time_range,
    }
}

async fn search_session_messages_in_db(
    db: &GlobalDb,
    request: &MessageSearchRequest<'_>,
) -> Vec<SessionMessageSearchResult> {
    if let Some(workflow_filter) = &request.workflow_scope {
        db.search_session_messages_workflow_scoped(
            request.requested_provider,
            request.project_key,
            request.query,
            request.limit,
            message_search_filters(request),
            workflow_filter,
        )
        .await
    } else if !request.git_filter.is_empty() {
        db.search_session_messages_git_scoped(
            request.requested_provider,
            request.project_key,
            request.query,
            request.limit,
            message_search_filters(request),
            &request.git_filter,
        )
        .await
    } else if let Some(provider) = request.requested_provider {
        db.search_session_messages_filtered(
            provider,
            request.project_key,
            request.query,
            request.limit,
            message_search_filters(request),
        )
        .await
    } else {
        db.search_session_messages_all_providers_filtered(
            request.project_key,
            request.query,
            request.limit,
            message_search_filters(request),
        )
        .await
    }
}

/// Merge per-project shards into a single relevance-ordered top-K.
///
/// Each shard is truncated in SQL by BM25 relevance (`ORDER BY bm25(...) LIMIT
/// k`, see `search_session_messages_*` in `global_db`), so the merged set must
/// be re-sorted by the *same* key for the distributed top-K to be exact.
/// Sorting by recency here would drop a top-relevance row a shard kept while
/// surfacing lower-relevance-but-newer rows the shards never returned. Key:
/// inventory-last (transcript/branch inventory noise sinks below substantive
/// hits, matching the per-shard downrank in `search_session_messages_*` so the
/// merge does not resurrect noise the shards demoted), then score DESC
/// (relevance), then timestamp DESC, then a stable session/message id tie-break
/// so equal-score rows order deterministically. This matches the single-project
/// path, which returns DB rows already inventory-downranked in BM25 order
/// without a resort.
fn sort_and_truncate_message_results_by_relevance(
    results: &mut Vec<SessionMessageSearchResult>,
    limit: usize,
) {
    let mut decorated: Vec<(bool, SessionMessageSearchResult)> = results
        .drain(..)
        .map(|result| {
            let is_inventory =
                crate::sessions::message_noise::is_inventory_text(&result.message.text);
            (is_inventory, result)
        })
        .collect();
    decorated.sort_by(|(a_inventory, a), (b_inventory, b)| {
        a_inventory
            .cmp(b_inventory)
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| b.message.timestamp.cmp(&a.message.timestamp))
            .then_with(|| a.session.session_id.cmp(&b.session.session_id))
            .then_with(|| a.message.message_id.cmp(&b.message.message_id))
    });
    results.extend(decorated.into_iter().map(|(_, result)| result));
    results.truncate(limit);
}

fn message_search_payload(
    request: &MessageSearchRequest<'_>,
    results: &[SessionMessageSearchResult],
    catch_up_performed: bool,
) -> Value {
    let mut payload = json!({
        "status": "ok",
        "provider": request.requested_provider.unwrap_or("all"),
        "requested_provider": request.requested_provider,
        "project_key": request.project_key,
        "parent_session_id": request.parent_session_id,
        "include_subagents": request.include_subagents,
        "catch_up": request.catch_up,
        "catch_up_performed": catch_up_performed,
        "catch_up_provider": request.provider_scope.response_label(),
        "scope": request.scope.as_str(),
        "message_type": request.message_type.as_str(),
        "since": request.time_range.start_time,
        "until": request.time_range.end_time,
        "query": request.query,
        "goals": request.goals,
        "count": results.len(),
        "results": results,
    });
    if !request.git_filter.is_empty() {
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                "git_filter".to_string(),
                serde_json::to_value(&request.git_filter).unwrap_or(Value::Null),
            );
            map.insert("git_filter_applied".to_string(), Value::Bool(true));
        }
    }
    if request.workflow_scope.is_some() {
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                "workflow_run".to_string(),
                request
                    .workflow_run
                    .map_or(Value::Null, |run| Value::String(run.to_string())),
            );
            if let Some(label) = request.workflow_agent {
                map.insert(
                    "workflow_agent".to_string(),
                    Value::String(label.to_string()),
                );
            }
            map.insert("workflow_filter_applied".to_string(), Value::Bool(true));
        }
    }
    payload
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
    for key in ["status", "provider", "session_id", "answer"] {
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
        for key in ["status", "provider", "session_id", "answer"] {
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

fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    string_arg(args, name).ok_or_else(|| TraceDecayError::Config {
        message: format!("missing required parameter: {name}"),
    })
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
    if value.is_null() {
        return Ok(None);
    }
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
fn lcm_not_yet_ingested(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "not_ingested",
            "store_exists": false,
            "message": "session store does not exist yet — nothing has been ingested",
        }),
    )
}

fn project_local_storage_without_project(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "message": "LCM storage requires an initialized TraceDecay project root",
        }),
    )
}

struct LcmStorage {
    db: GlobalDb,
}

fn available_lcm_storage(db: GlobalDb) -> LcmStorageResolution {
    LcmStorageResolution::Available(Box::new(LcmStorage { db }))
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

async fn open_session_db_for_bulk_catch_up(db_path: &Path) -> Option<GlobalDb> {
    if schema_already_ensured(db_path) {
        if let Some(db) = GlobalDb::open_at_assuming_schema(db_path).await {
            return Some(db);
        }
    }
    let db = GlobalDb::open_at_without_structured_backfill(db_path).await?;
    mark_schema_ensured(db_path);
    Some(db)
}

enum LcmStorageResolution {
    Available(Box<LcmStorage>),
    Unavailable(ToolResult),
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
    let Some(db_path) = context.project_session_db_path else {
        return LcmStorageResolution::Unavailable(project_local_storage_without_project(args));
    };
    let db_path = db_path.to_path_buf();
    if mode == LcmOpenMode::ReadOnlyOrMissing && !db_path.is_file() {
        return LcmStorageResolution::Unavailable(lcm_not_yet_ingested(args));
    }
    let Some(db) = open_lcm_db_at(&db_path, mode).await else {
        return LcmStorageResolution::Unavailable(lcm_unavailable(args));
    };
    available_lcm_storage(db)
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
        // Term-bearing queries default to relevance (FTS rank primary, recency
        // as tiebreak) so distinct queries do not all collapse onto the same
        // few most-recent sessions. Pass `sort` explicitly for recency/hybrid.
        return Ok(LcmGrepSort::Relevance);
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
    value
        .as_str()
        .and_then(SessionSearchScope::parse)
        .ok_or_else(|| argument_error("scope must be one of all, parents_only, subagents_only"))
}

fn parse_session_message_type(args: &Value) -> Result<SessionMessageType> {
    let Some(value) = args.get("message_type") else {
        return Ok(SessionMessageType::All);
    };
    value
        .as_str()
        .and_then(SessionMessageType::parse)
        .ok_or_else(|| argument_error("message_type must be one of all, direct_user, tool_result"))
}

fn parse_lcm_relationship_scope(args: &Value) -> Result<SessionSearchScope> {
    let Some(value) = args.get("relationship_scope") else {
        return Ok(SessionSearchScope::All);
    };
    value
        .as_str()
        .and_then(SessionSearchScope::parse)
        .ok_or_else(|| {
            argument_error("relationship_scope must be one of all, parents_only, subagents_only")
        })
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
            let index_empty = value
                .get("index_empty")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            md.blank();
            if index_empty {
                if value.get("git_ref").and_then(Value::as_str) == Some("commit") {
                    md.empty_note(
                        "No commit evidence is indexed yet. Run `tracedecay sync` to ingest \
                         direct host/tool evidence; `tracedecay sessions git-backfill` adds \
                         weaker historical overlap evidence.",
                    );
                } else {
                    md.empty_note(
                        "Correlation index is empty — no git spans recorded yet. It will \
                         auto-backfill on the next MCP server startup, or run \
                         `tracedecay sessions git-backfill` to populate it now.",
                    );
                }
            } else {
                md.empty_note("No correlated sessions recorded for this git ref.");
            }
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
        if let Some(relation) = hit.get("relation").and_then(Value::as_str) {
            let _ = write!(detail, " · {relation}");
        }
        if let Some(evidence) = hit.get("evidence").and_then(Value::as_str) {
            let _ = write!(detail, " via {evidence}");
        }
        if let Some(confidence) = hit.get("confidence").and_then(Value::as_i64) {
            let _ = write!(detail, " ({confidence}/100)");
        }
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
    let relation = CommitRelationFilter::parse(string_arg(&args, "relation"))
        .map_err(|err| argument_error(err.to_string()))?;
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
    let (results, index_presence, observed_fallback) = if db_path.is_file() {
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
        // Query paths need only distinguish an empty row family from a
        // populated index with no match. Exact counts scan the entire
        // correlation tables on large stores, so use the bounded presence
        // probe here and leave exact health to diagnostics.
        let presence = db.git_correlation_index_presence().await.ok();
        let results = db
            .git_sessions_for_with_relation(&query, relation)
            .await
            .map_err(|err| TraceDecayError::Config {
                message: err.to_string(),
            })?;
        // A commit attributed only by time overlap (or a migrated v2 store) has
        // observed rows but no producer, so the producer-default query is
        // empty. That must not read as "no session touched this commit": look
        // up the observed sessions so the caller can be pointed at them.
        let observed_fallback = if results.is_empty()
            && matches!(query.git_ref, GitRefFilter::Commit(_))
            && relation == CommitRelationFilter::Produced
        {
            db.git_sessions_for_with_relation(&query, CommitRelationFilter::Observed)
                .await
                .ok()
                .filter(|hits| !hits.is_empty())
        } else {
            None
        };
        (results, presence, observed_fallback)
    } else {
        // No store file at all: the correlation index was never created.
        (Vec::new(), None, None)
    };

    // The index is "empty" when there is no store, the correlation tables are
    // absent, or the row family for this ref kind is empty (spans for
    // branch/worktree, commit rows for commit). That must not read as a genuine
    // "no sessions matched" result.
    let index_empty = index_presence
        .as_ref()
        .is_none_or(|presence| presence.is_empty_for(&query.git_ref));
    let mut payload = json!({
        "status": "ok",
        "git_ref": query.git_ref.kind(),
        "value": query.git_ref.value(),
        "since": since,
        "until": until,
        "relation": relation.as_str(),
        "count": results.len(),
        "results": results,
        "index_empty": index_empty,
    });
    if let Some(presence) = &index_presence {
        let mut index = json!({
            "tables_present": presence.tables_present,
            "spans_present": presence.spans_present,
            "commits_present": presence.commits_present,
            "span_count": Value::Null,
            "commit_count": Value::Null,
            "last_span_write": Value::Null,
            "backfill_watermark": Value::Null,
            "count_mode": "presence_only",
        });
        // Preserve the exact-zero signal for consumers without inventing a
        // count when the family is populated. Exact counts remain available
        // through diagnostics, where their full scan is explicit.
        if !presence.spans_present {
            index["span_count"] = json!(0);
        }
        if !presence.commits_present {
            index["commit_count"] = json!(0);
        }
        payload["index"] = index;
    }
    // When nothing matched, say *why*: an empty index self-heals via startup
    // auto-backfill (or a manual `tracedecay sessions git-backfill`), whereas a
    // populated index genuinely had no session on this ref.
    if results.is_empty() {
        if let Some(observed) = &observed_fallback {
            payload["observed_count"] = json!(observed.len());
            payload["observed_sessions"] = json!(observed);
            payload["message"] = json!(format!(
                "no producing sessions; {} session(s) observed this commit — pass relation=observed to list them",
                observed.len()
            ));
        } else {
            payload["message"] = json!(if index_empty {
                if matches!(&query.git_ref, GitRefFilter::Commit(_)) {
                    "no commit evidence indexed yet — run `tracedecay sync` to ingest direct host/tool evidence; `tracedecay sessions git-backfill` adds weaker historical overlap evidence"
                } else {
                    "correlation index empty (no git spans recorded yet) — it will auto-backfill on the next MCP server startup, or run `tracedecay sessions git-backfill` to populate it now"
                }
            } else {
                "no sessions matched this git ref"
            });
        }
    }
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
    let request = parse_message_search_request(&args)?;
    let project_scope = args
        .get("project_scope")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|scope| !scope.is_empty());

    if let Some(project_scope) = project_scope {
        if project_scope != "all_registered" {
            return Err(argument_error(
                "project_scope must be omitted or all_registered",
            ));
        }
        if message_search_has_registered_project_selector(&args) {
            return Err(argument_error(
                "project_scope cannot be combined with project_id, project_path, project_root, or project_selector",
            ));
        }
        let owned_global;
        let global = match global_db {
            Some(global) => global,
            None if allow_default_registry_fallback => {
                owned_global = match GlobalDb::open().await {
                    Some(global) => global,
                    None => {
                        return Ok(tool_json(
                            Some(cg.project_root()),
                            &args,
                            &json!({
                                "status": "unavailable",
                                "message": "registered project search requires the global project registry",
                                "project_scope": project_scope,
                                "results": [],
                                "count": 0
                            }),
                        ));
                    }
                };
                &owned_global
            }
            None => {
                return Err(TraceDecayError::Config {
                    message: "client project registry is unavailable for selector resolution"
                        .to_string(),
                });
            }
        };
        let profile_root = global
            .db_path()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| TraceDecayError::Config {
                message: "could not resolve tracedecay profile root".to_string(),
            })?;
        let catch_up_leader = if request.catch_up {
            let key = format!(
                "all-registered:{}:{}",
                profile_root.display(),
                request.provider_scope.response_label()
            );
            match claim_message_catch_up(key) {
                MessageCatchUpClaim::Wait(done) => {
                    wait_for_message_catch_up(done).await;
                    None
                }
                MessageCatchUpClaim::Leader(leader) => Some(leader),
            }
        } else {
            None
        };
        let perform_catch_up = catch_up_leader.is_some();
        let mut destinations = Vec::new();
        let mut skipped_project_count = 0usize;
        for project in global.list_code_projects(usize::MAX).await {
            let Some(context) = global
                .project_registry_context_by_id(&project.project_id)
                .await
            else {
                skipped_project_count += 1;
                continue;
            };
            // One project's malformed store relpath must not abort the whole
            // cross-project sweep; skip it like the neighboring missing-context
            // / missing-db / open-failure branches.
            let Ok(candidates) = registry_session_db_candidates(&context, &profile_root) else {
                skipped_project_count += 1;
                continue;
            };
            let Some(db_path) = candidates.into_iter().find(|path| path.is_file()) else {
                skipped_project_count += 1;
                continue;
            };
            let db = if perform_catch_up {
                open_session_db_for_bulk_catch_up(&db_path).await
            } else {
                GlobalDb::open_read_only_at(&db_path).await
            };
            let Some(db) = db else {
                skipped_project_count += 1;
                continue;
            };
            let display_root = Path::new(&context.project.display_root);
            let project_root = if display_root.is_absolute() {
                display_root.to_path_buf()
            } else {
                PathBuf::from(&context.project.canonical_root)
            };
            destinations.push((db, project_root));
        }
        if perform_catch_up {
            let provider = request.provider_scope.provider();
            let _ = crate::sessions::ingest_user_global_sources_for_provider(provider).await;
            if provider.is_none() || provider == Some(crate::sessions::SessionProvider::Hermes) {
                let hermes_destinations =
                    destinations
                        .iter()
                        .map(|(db, project_root)| {
                            crate::sessions::hermes::ProjectIngestDestination { db, project_root }
                        })
                        .collect::<Vec<_>>();
                let _ = crate::sessions::hermes::ingest_for_projects(&hermes_destinations).await;
            }
            if provider == Some(crate::sessions::SessionProvider::Hermes) {
                for (db, project_root) in &destinations {
                    crate::sessions::finalize_project_ingest(db, project_root).await;
                }
            } else {
                for (db, project_root) in &destinations {
                    let _ = crate::sessions::ingest_project_sources_for_provider(
                        db,
                        project_root,
                        provider,
                        false,
                    )
                    .await;
                }
            }
        }
        drop(catch_up_leader);
        let searched_project_count = destinations.len();
        let mut results = Vec::new();
        for (db, _) in &destinations {
            let mut project_results = search_session_messages_in_db(&db, &request).await;
            results.append(&mut project_results);
        }
        sort_and_truncate_message_results_by_relevance(&mut results, request.limit);
        let mut payload = message_search_payload(&request, &results, request.catch_up);
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                "project_scope".to_string(),
                Value::String(project_scope.to_string()),
            );
            map.insert(
                "searched_project_count".to_string(),
                json!(searched_project_count),
            );
            map.insert(
                "skipped_project_count".to_string(),
                json!(skipped_project_count),
            );
        }
        return Ok(tool_json_with_md(
            Some(cg.project_root()),
            &args,
            &payload,
            || render_message_search_md(&payload),
        ));
    }

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
    let catch_up_leader = if request.catch_up {
        let key = format!(
            "project:{}:{}",
            db_path.display(),
            request.provider_scope.response_label()
        );
        match claim_message_catch_up(key) {
            MessageCatchUpClaim::Wait(done) => {
                wait_for_message_catch_up(done).await;
                None
            }
            MessageCatchUpClaim::Leader(leader) => Some(leader),
        }
    } else {
        None
    };
    let perform_catch_up = catch_up_leader.is_some();
    let db = if request.catch_up && !perform_catch_up {
        GlobalDb::open_read_only_at(&db_path).await
    } else {
        open_session_db_with_cached_ensure(&db_path).await
    };
    let Some(db) = db else {
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
    let catch_up_performed = request.catch_up;
    if perform_catch_up {
        let _ = crate::sessions::ingest_global_sources_for_provider(
            &db,
            &target_root,
            request.provider_scope.provider(),
        )
        .await;
    }
    drop(catch_up_leader);
    // Build the workflow-run scope filter and, separately, resolve the run's
    // parent thread purely for the echoed `workflow_run_parent_session` field
    // (the scope itself is authoritative via the `workflow_agents` EXISTS
    // pushdown, so an unknown/orphan parent no longer needs a sentinel).
    let resolved_workflow_parent: Option<String> = match request.workflow_run {
        Some(run_id) => match db.workflow_run_for_id(run_id).await {
            Ok(Some(run)) if !run.parent_session_id.is_empty() => Some(run.parent_session_id),
            _ => None,
        },
        None => None,
    };
    let results = if request.goals {
        db.recent_session_goals(request.project_key, request.limit)
            .await
    } else {
        search_session_messages_in_db(&db, &request).await
    };
    let mut payload = message_search_payload(&request, &results, catch_up_performed);
    if let Some(map) = payload.as_object_mut() {
        map.insert("selected_project_root".to_string(), json!(target_root));
        if request.workflow_scope.is_some() {
            map.insert(
                "workflow_run_parent_session".to_string(),
                resolved_workflow_parent.map_or(Value::Null, Value::String),
            );
        }
    }
    Ok(tool_json_with_md(
        Some(&target_root),
        &args,
        &payload,
        || render_message_search_md(&payload),
    ))
}

pub(super) async fn handle_user_message_search(
    profile_root: &Path,
    args: Value,
) -> Result<ToolResult> {
    let request = parse_message_search_request(&args)?;
    let sessions_db_path = crate::sessions::user_sessions_db_path(profile_root);
    let catch_up_leader = if request.catch_up {
        let key = format!(
            "user:{}:{}",
            sessions_db_path.display(),
            request.provider_scope.response_label()
        );
        match claim_message_catch_up(key) {
            MessageCatchUpClaim::Wait(done) => {
                wait_for_message_catch_up(done).await;
                None
            }
            MessageCatchUpClaim::Leader(leader) => Some(leader),
        }
    } else {
        None
    };
    if catch_up_leader.is_some() {
        let _ = crate::sessions::ingest_user_global_sources_for_provider_at(
            profile_root,
            request.provider_scope.provider(),
        )
        .await;
    }
    drop(catch_up_leader);
    let Some(db) = GlobalDb::open_read_only_at(&sessions_db_path).await else {
        return Ok(tool_json(
            None,
            &args,
            &json!({
                "status": "unavailable",
                "message": "could not open user tracedecay session database",
                "results": [],
                "count": 0
            }),
        ));
    };
    let results = if request.goals {
        db.recent_session_goals(request.project_key, request.limit)
            .await
    } else {
        search_session_messages_in_db(&db, &request).await
    };
    let payload = message_search_payload(&request, &results, request.catch_up);
    Ok(tool_json_with_md(None, &args, &payload, || {
        render_message_search_md(&payload)
    }))
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
    let status = storage
        .db
        .lcm_status_with_options(provider, session_id, deep, &gc_config)
        .await
        .map_err(lcm_error)?;
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
    let relationship_scope = parse_lcm_relationship_scope(&args)?;
    let message_type = parse_session_message_type(&args)?;
    let provider = lcm_grep_provider_arg(&args);
    let git_filter = parse_git_scope_filter(&args)?;
    let git_filter_applied = !git_filter.is_empty();
    let storage = lcm_open_storage_ro!(context, &args);
    let hits = storage
        .db
        .lcm_grep_filtered(
            LcmGrepRequest {
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
            },
            LcmGrepFilters {
                relationship_scope,
                message_type,
            },
        )
        .await
        .map_err(lcm_error)?;
    let crate::sessions::lcm::LcmGrepOutcome {
        hits,
        capped_sessions,
    } = hits;
    let mut payload = json!({
        "status": "ok",
        "provider": provider,
        "query": query,
        "count": hits.len(),
        "hits": hits,
        "sort": string_arg(&args, "sort").unwrap_or("relevance"),
        "relationship_scope": string_arg(&args, "relationship_scope").unwrap_or("all"),
        "message_type": string_arg(&args, "message_type").unwrap_or("all"),
    });
    if !capped_sessions.is_empty() {
        if let Some(map) = payload.as_object_mut() {
            let dropped: usize = capped_sessions.values().sum();
            map.insert(
                "capped_sessions".to_string(),
                serde_json::to_value(&capped_sessions).unwrap_or(Value::Null),
            );
            map.insert(
                "note".to_string(),
                json!(format!(
                    "per-session cap dropped {dropped} additional hit(s) from {} session(s);                      rerun with scope=session and that session_id for complete results",
                    capped_sessions.len()
                )),
            );
        }
    }
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

/// Best-effort direct synthesis of the expand-query `answer` using the
/// configured automation backend. When the backend is `codex_app_server` and
/// available it runs one bounded synthesis call (reusing the automation retry
/// path) and, on non-empty output, populates `response.answer` and clears
/// `needs_synthesis`. Any failure (no backend, resolution error, backend error,
/// empty output) leaves the response untouched so the host can synthesize from
/// the raw context — preserving the existing `needs_synthesis:true` contract.
async fn maybe_synthesize_expand_query_answer(
    project_root: Option<&Path>,
    response: &mut crate::sessions::lcm::LcmExpandQueryResponse,
) {
    use crate::automation::backend::{
        BackendRetryPolicy, CodexAppServerBackend, backend_availability,
    };
    use crate::automation::config::AutomationBackend;

    if !response.needs_synthesis || response.context_blocks.is_empty() {
        return;
    }
    let Some(config) = resolve_expand_query_automation_config(project_root).await else {
        return;
    };
    if config.backend != AutomationBackend::CodexAppServer
        || !backend_availability(&config).available
    {
        return;
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let policy = BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let _ = synthesize_expand_query_answer(response, &backend, &policy).await;
}

/// Resolves the effective automation config (global user config layered with
/// any project override) for the active project. Best-effort: returns `None`
/// when the layout or project config cannot be resolved.
async fn resolve_expand_query_automation_config(
    project_root: Option<&Path>,
) -> Option<crate::automation::config::AutomationConfig> {
    use crate::automation::config::{effective_config, load_project_config};

    let global = crate::user_config::UserConfig::load().automation;
    let project = match project_root {
        Some(root) => {
            let layout = crate::storage::resolve_layout_for_current_profile(root).ok()?;
            load_project_config(&layout.dashboard_root)
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    effective_config(&global, project.as_ref()).ok()
}

/// Core synthesis step, isolated from backend construction and config
/// resolution so it can be unit tested with a fake backend. Runs one bounded
/// backend call built from the response's synthesis prompt and, on success,
/// records the answer. Returns `true` when an answer was synthesized.
async fn synthesize_expand_query_answer(
    response: &mut crate::sessions::lcm::LcmExpandQueryResponse,
    backend: &dyn crate::automation::backend::AgentTaskBackend,
    policy: &crate::automation::backend::BackendRetryPolicy,
) -> bool {
    use crate::automation::backend::{AgentTaskKind, AgentTaskRequest, run_agent_task_with_retry};

    if !response.needs_synthesis || response.context_blocks.is_empty() {
        return false;
    }
    let Some(synthesis_prompt) = response.synthesis_prompt.clone() else {
        return false;
    };
    let request = AgentTaskRequest::new(
        format!("lcm-expand-query-{}", current_timestamp()),
        AgentTaskKind::UserJob,
        synthesis_prompt.user,
        None,
        json!({ "system": synthesis_prompt.system }),
    );
    let Ok(task) = run_agent_task_with_retry(backend, &request, policy).await else {
        return false;
    };
    let answer = task.output_text.trim();
    if answer.is_empty() {
        return false;
    }
    response.answer = Some(answer.to_string());
    response.needs_synthesis = false;
    true
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
    let mut response = storage
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
    // Synthesize the answer directly when an automation backend is configured
    // and available; otherwise leave `needs_synthesis:true` for the host.
    maybe_synthesize_expand_query_answer(context.project_root, &mut response).await;
    let mut payload = serde_json::to_value(response).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize expand-query response: {err}"),
    })?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("status".to_string(), json!("ok"));
        object.insert("provider".to_string(), json!(provider));
        object.insert("session_id".to_string(), json!(session_id));
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
    let messages = messages_arg(&args)?;
    let response = storage
        .db
        .lcm_preflight(LcmPreflightRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages.clone(),
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
    if bool_arg(&args, "transcript_projection")? == Some(true) {
        upsert_live_transcript_projection(
            &storage.db,
            context.project_root,
            provider,
            session_id,
            &messages,
        )
        .await;
    }
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

async fn upsert_live_transcript_projection(
    db: &GlobalDb,
    project_root: Option<&Path>,
    provider: &str,
    session_id: &str,
    messages: &[Value],
) {
    let project = project_root
        .map(|root| root.to_string_lossy().to_string())
        .unwrap_or_else(|| "user".to_string());
    let storage_scope = if project_root.is_some() {
        "project"
    } else {
        "user"
    };
    let source_path = format!("live://{provider}/{session_id}");
    let mut projected = Vec::new();
    for (ordinal, message) in messages.iter().enumerate() {
        let Some(message_id) = message
            .get("id")
            .or_else(|| message.get("message_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let content = message.get("content").cloned().unwrap_or(Value::Null);
        let tool_calls = message.get("tool_calls");
        let (text, tool_names) = content_storage_text_and_tools(&content, tool_calls);
        if text.trim().is_empty() {
            continue;
        }
        let mut metadata = json!({
            "source": "lcm_preflight_live",
            "project_root": project,
            "storage_scope": storage_scope,
            "location_provenance": "host_live_route"
        });
        if let Some(roots) = message
            .get("associated_project_roots")
            .filter(|value| value.is_array())
        {
            metadata["associated_project_roots"] = roots.clone();
        }
        projected.push(SessionMessageRecord {
            provider: provider.to_string(),
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            role,
            timestamp: message
                .get("timestamp")
                .and_then(Value::as_f64)
                .map(|value| value as i64),
            ordinal: ordinal as i64,
            text,
            kind: Some("message".to_string()),
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
            source_path: Some(source_path.clone()),
            source_offset: Some(ordinal as i64),
            metadata_json: Some(metadata.to_string()),
        });
    }
    if projected.is_empty() {
        return;
    }
    let title = projected
        .iter()
        .find(|message| message.role == "user")
        .map(|message| preview_title(&message.text));
    let batch = TranscriptBatch {
        session: SessionRecord {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            project_key: project.clone(),
            project_path: project,
            title,
            started_at: projected
                .iter()
                .filter_map(|message| message.timestamp)
                .min(),
            ended_at: None,
            transcript_path: Some(source_path.clone()),
            metadata_json: Some(
                json!({
                    "source": "lcm_preflight_live",
                    "storage_scope": storage_scope,
                    "location_provenance": "host_live_route"
                })
                .to_string(),
            ),
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        },
        messages: projected,
    };
    let persisted = db
        .upsert_transcript_projection_batches(&[batch], &source_path, ParseOffset::default())
        .await;
    if !persisted {
        tracing::debug!(
            provider,
            session_id,
            "live transcript projection upsert failed"
        );
    }
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
