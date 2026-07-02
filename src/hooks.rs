//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! These functions are invoked by each agent's hook system to intercept tool
//! calls, redirect exploration work to tracedecay MCP tools, keep the index
//! fresh after edits / git state changes, and track per-session token savings.
//! Each agent sends its own event schema on stdin and expects its own output
//! shape, so the handlers are kept agent-specific rather than shared blindly.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub mod tool_hints;

use tool_hints::{decide_hint, HintAgent, HintCategory, ToolHint, ToolHintInput};

macro_rules! read_hook_event {
    () => {{
        match read_stdin_to_string() {
            Ok(event) => event,
            Err(e) => {
                eprintln!("tracedecay hook: failed to read stdin: {e}");
                return 1;
            }
        }
    }};
}

const TRACEDECAY_RESEARCH_BLOCK_REASON: &str = "STOP: Use tracedecay MCP tools \
(tracedecay_context, tracedecay_search, tracedecay_callees, tracedecay_callers, \
tracedecay_impact, tracedecay_files, tracedecay_affected) instead of agents for \
code research. TraceDecay is faster and more precise for symbol relationships, \
call paths, and code structure. Only use agents for code exploration if you \
have already tried tracedecay and it cannot answer the question.";

const CODEX_SUBAGENT_START_CONTEXT: &str = "tracedecay subagent context: this looks like a \
new/no-history subagent or code-research subagent. Use tracedecay MCP tools and the relevant TraceDecay skill/tool \
workflow before broad file reads: `tracedecay:searching-for-code` with `tracedecay_context` \
for code exploration, `tracedecay:reading-code-cheaply` with `tracedecay_outline` or \
`tracedecay_body` before whole-file reads, `tracedecay:tracing-functions` with \
`tracedecay_find_exact_symbol`, `tracedecay_callers`, and `tracedecay_callees` when \
asked to trace functions, find callers, or inspect setup/helper/fixture dependencies, \
`tracedecay:finding-impacted-areas` with `tracedecay_affected` and \
`tracedecay_test_map` before guessing affected tests, `tracedecay:recalling-project-memory` when \
project decisions/preferences matter, and `tracedecay:recalling-session-context` with \
`tracedecay_message_search`, `tracedecay_lcm_expand_query`, and `tracedecay_lcm_describe` \
when prior conversation context may be missing.";

const HOOK_ANALYTICS_FILENAME: &str = "hook_analytics.jsonl";

fn research_block_reason(hint: Option<ToolHint>) -> String {
    let base = crate::config::brand_env("RESEARCH_BLOCK_REASON")
        .unwrap_or_else(|| TRACEDECAY_RESEARCH_BLOCK_REASON.to_string());
    hint.map_or_else(
        || base.clone(),
        |hint| format!("{}\n\n{}", base, format_tool_hint(&hint)),
    )
}

fn record_hook_analytics(root: Option<&Path>, event: &str, mut fields: serde_json::Value) {
    let Some(path) = hook_analytics_path(root) else {
        return;
    };
    let Some(fields) = fields.as_object_mut() else {
        return;
    };
    fields.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    fields.insert(
        "ts_unix_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(now_unix_millis())),
    );
    let Ok(line) = serde_json::to_string(&fields) else {
        return;
    };
    append_private_jsonl(&path, &line);
}

fn hook_analytics_path(root: Option<&Path>) -> Option<PathBuf> {
    match root {
        Some(root) => crate::storage::resolve_layout_for_current_profile(root)
            .ok()
            .map(|layout| layout.data_root.join(HOOK_ANALYTICS_FILENAME)),
        None => crate::storage::default_profile_root()
            .ok()
            .map(|root| root.join(HOOK_ANALYTICS_FILENAME)),
    }
}

fn append_private_jsonl(path: &Path, line: &str) {
    let _ = crate::storage::PrivateStoreIo::append_line(path, line);
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn record_hook_invoked(root: Option<&Path>, agent: HintAgent, hook_name: &str, event_json: &str) {
    let parsed: Value = serde_json::from_str(event_json).unwrap_or(Value::Null);
    record_hook_analytics(
        root,
        "hook_invoked",
        serde_json::json!({
            "agent": agent.as_key(),
            "hook_name": hook_name,
            "hook_event_name": text_field(&parsed, &["hook_event_name", "hookEventName"]),
            "session_id": event_session_id(&parsed),
            "tool_name": text_field(&parsed, &["tool_name", "toolName", "name"]),
            "command": text_field(&parsed, &["command", "cmd", "shell_command"]),
            "prompt_category": inferred_prompt_category(&parsed),
        }),
    );
}

fn inferred_prompt_category(parsed: &Value) -> Option<&'static str> {
    let text = prompt_like_text(parsed)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }
    if is_code_research_prompt(&text) {
        Some("code_research")
    } else if text.contains("test") || text.contains("failing") || text.contains("ci") {
        Some("test_or_ci")
    } else if text.contains("dashboard") || text.contains("ui") || text.contains("frontend") {
        Some("dashboard_or_ui")
    } else if text.contains("bug") || text.contains("fix") || text.contains("error") {
        Some("debug_or_fix")
    } else {
        Some("general")
    }
}

fn record_hint_analytics(
    root: Option<&Path>,
    event: &str,
    agent: HintAgent,
    session_id: Option<&str>,
    hint: &ToolHint,
) {
    record_hook_analytics(
        root,
        event,
        serde_json::json!({
            "agent": agent.as_key(),
            "session_id": session_id,
            "category": hint.category.as_key(),
        }),
    );
}

fn record_workspace_status_analytics(
    root: Option<&Path>,
    status: HookWorkspaceStatus,
    session_id: Option<&str>,
) {
    record_hook_analytics(
        root,
        "workspace_status",
        serde_json::json!({
            "agent": HintAgent::Codex.as_key(),
            "session_id": session_id,
            "workspace_status": status.as_key(),
        }),
    );
}

fn record_hint_emitted(
    root: Option<&Path>,
    agent: HintAgent,
    session_id: Option<&str>,
    hint: &ToolHint,
) {
    if session_id.is_none() {
        record_hint_analytics(root, "missing_session", agent, None, hint);
    }
    record_hint_analytics(root, "hint_emitted", agent, session_id, hint);
}

/// `PreToolUse` hook handler for Claude Code's Agent tool matcher.
///
/// Reads the `TOOL_INPUT` environment variable (JSON), inspects the
/// `subagent_type` and `prompt` fields, and prints a JSON decision to
/// stdout. Blocks Explore agents and exploration-style prompts, directing
/// Claude to use tracedecay MCP tools instead.
pub fn hook_pre_tool_use() {
    let tool_input = std::env::var("TOOL_INPUT").unwrap_or_default();
    record_hook_invoked(None, HintAgent::Claude, "preToolUse", &tool_input);
    let decision = evaluate_hook_decision(&tool_input);
    if !decision.is_empty() {
        println!("{decision}");
    }
}

/// Pure decision logic for the `PreToolUse` hook.
///
/// Takes the raw `TOOL_INPUT` JSON string and returns the JSON decision
/// string to print to stdout.
pub fn evaluate_hook_decision(tool_input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(tool_input).unwrap_or_else(|_| serde_json::json!({}));
    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Claude,
        session_id: event_session_id(&parsed),
        tool_name: Some("Agent".to_string()),
        command: None,
        prompt: prompt_like_text(&parsed),
        subagent_type: parsed
            .get("subagent_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        file_path: None,
        hints_enabled: true,
    });
    let block_reason = research_block_reason(hint);
    let block_msg = || {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": block_reason
            }
        })
    };

    // Block Explore agents outright
    if parsed.get("subagent_type").and_then(|v| v.as_str()) == Some("Explore") {
        return block_msg().to_string();
    }

    // Check if the prompt is exploration/research work that tracedecay can handle
    if let Some(prompt) = parsed.get("prompt").and_then(|v| v.as_str()) {
        if is_code_research_prompt(prompt) {
            return block_msg().to_string();
        }
    }

    // Empty string = no output -> Claude Code implicitly allows the tool call
    String::new()
}

fn is_code_research_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    let exploration_patterns = [
        "explore",
        "codebase structure",
        "codebase architecture",
        "codebase overview",
        "source files contents",
        "read every",
        "full contents",
        "entire codebase",
        "architecture and structure",
        "call graph",
        "call path",
        "call chain",
        "symbol relat",
        "symbol lookup",
        "who calls",
        "callers of",
        "callees of",
    ];
    exploration_patterns.iter().any(|pat| lower.contains(pat))
}

/// Kiro `preToolUse` hook handler.
///
/// Kiro sends the hook event JSON on stdin. Returning exit code 2 blocks the
/// tool call and sends stderr back to the model. This is intentionally separate
/// from Claude's hook handler because Claude expects a JSON decision on stdout.
pub fn hook_kiro_pre_tool_use() -> i32 {
    let event = read_hook_event!();
    record_hook_invoked(None, HintAgent::Kiro, "preToolUse", &event);
    if let Some(reason) = evaluate_kiro_pre_tool_use(&event) {
        eprintln!("{reason}");
        2
    } else {
        0
    }
}

/// Cursor `subagentStart` hook handler.
///
/// Cursor sends hook event JSON on stdin and expects Cursor-shaped JSON on
/// stdout. This intentionally does not reuse the Claude hook output schema.
pub fn hook_cursor_subagent_start() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "subagentStart", &event);
    if let Some(decision) = evaluate_cursor_subagent_start(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `postToolUse` hook handler.
///
/// Emits soft `additional_context` hints steering exploration tools (Grep,
/// Glob, Read, semantic search, shell `rg`) toward tracedecay MCP tools.
/// Registered on `postToolUse` rather than `preToolUse` because Cursor's
/// documented `preToolUse` output schema has no context-injection field —
/// `additional_context` is only honored on `postToolUse`. The hook runs
/// unmatched (the docs enumerate no matcher value for Cursor's semantic
/// search tool) and irrelevant tools fail open with no output. Each hint
/// category is emitted at most once per session via [`ToolHintDedupe`]
/// persisted under `.tracedecay/`.
pub fn hook_cursor_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "postToolUse", &event);
    if let Some(decision) = cursor_post_tool_use_decision(&event) {
        println!("{decision}");
    }
    0
}

/// Cursor `beforeSubmitPrompt` hook handler.
///
/// Resets the project-local counter for a new prompt turn and does at most a
/// small, time-boxed *tail* ingest of newly-appended transcript lines (the bulk
/// catch-up lives on the lower-frequency `sessionStart` / `stop` hooks). The
/// output uses Cursor's documented `beforeSubmitPrompt` shape and never blocks
/// submission, even if the tail ingest times out.
pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "beforeSubmitPrompt",
        &event,
    );
    reset_counter_for_cursor_event(&event).await;
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_HOT_INGEST_MAX_BYTES),
        CURSOR_HOT_INGEST_BUDGET,
    )
    .await;
    // Cursor's documented `beforeSubmitPrompt` output is `continue` +
    // `user_message` only — `additional_context` is not part of this event's
    // contract, so no hint is emitted here (the postToolUse and sessionStart
    // hooks are the documented context channels).
    println!("{}", serde_json::json!({ "continue": true }));
    0
}

/// Cursor `sessionEnd` hook handler (fire-and-forget).
///
/// Final transcript-ingest flush when a conversation ends (including
/// `window_close` / `user_close`, which the end-of-turn `stop` hook can
/// miss). `sessionEnd` receives the common-schema `transcript_path`, so the
/// regular capped catch-up ingest applies. The response is logged but unused,
/// so an empty object is emitted. Fail-open.
pub async fn hook_cursor_session_end() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "sessionEnd", &event);
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_STOP_INGEST_BUDGET,
    )
    .await;
    println!("{}", serde_json::json!({}));
    0
}

/// Cursor `stop` hook handler (fire-and-forget).
///
/// Fires at the end of an agent turn and performs the primary transcript
/// ingest: a time-boxed incremental catch-up that picks up bounded transcript
/// tails appended during the turn. The `stop` output is informational only, so
/// we emit an empty object and never ask the agent to continue. Fail-open.
pub async fn hook_cursor_stop() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "stop", &event);
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_STOP_INGEST_BUDGET,
    )
    .await;
    println!("{}", serde_json::json!({}));
    0
}

/// Cursor `preCompact` hook handler.
///
/// Cursor's compaction event exposes pressure metadata but not Cursor's own
/// generated summary text. At the boundary, `TraceDecay` ingests the current
/// transcript tail, asks LCM for the compactable raw-message backlog, generates
/// a summary through `cursor-agent -p`, and stores that summary as a normal LCM
/// summary node. The hook is fail-open and emits Cursor's empty object shape.
pub async fn hook_cursor_pre_compact() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "preCompact", &event);
    if std::env::var(crate::sessions::cursor_agent::CURSOR_SUMMARY_CHILD_ENV).is_err() {
        let mut config = crate::sessions::cursor_agent::CursorAgentSummaryConfig::from_env();
        config.timeout = config.timeout.min(CURSOR_PRE_COMPACT_SUMMARY_BUDGET);
        let outcome = cursor_pre_compact_for_event_with_config(&event, &config).await;
        if outcome.status == "error" {
            eprintln!(
                "tracedecay Cursor preCompact summary failed: {}",
                outcome.reason
            );
        }
    }
    println!("{}", serde_json::json!({}));
    0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPreCompactOutcome {
    pub status: String,
    pub reason: String,
    pub summary_nodes_created: usize,
    pub summary_node_ids: Vec<String>,
}

impl CursorPreCompactOutcome {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: "skipped".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            reason: reason.into(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        }
    }
}

pub async fn cursor_pre_compact_for_event_with_config(
    event_json: &str,
    config: &crate::sessions::cursor_agent::CursorAgentSummaryConfig,
) -> CursorPreCompactOutcome {
    match tokio::time::timeout(
        CURSOR_PRE_COMPACT_BUDGET,
        cursor_pre_compact_for_event_inner(event_json, config),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => CursorPreCompactOutcome::error("timed out"),
    }
}

async fn cursor_pre_compact_for_event_inner(
    event_json: &str,
    config: &crate::sessions::cursor_agent::CursorAgentSummaryConfig,
) -> CursorPreCompactOutcome {
    if std::env::var(crate::sessions::cursor_agent::CURSOR_SUMMARY_CHILD_ENV).is_ok() {
        return CursorPreCompactOutcome::skipped("cursor summary child");
    }
    let parsed = match serde_json::from_str::<Value>(event_json) {
        Ok(parsed) => parsed,
        Err(err) => return CursorPreCompactOutcome::error(format!("invalid event JSON: {err}")),
    };
    let Some(project_root) = cursor_project_root_from_parsed_event(&parsed) else {
        return CursorPreCompactOutcome::skipped("no project root");
    };
    if !cursor_event_transcript_path_exists(&parsed) {
        return CursorPreCompactOutcome::skipped("no transcript path");
    }

    let caught_up =
        ingest_cursor_transcript_for_event(event_json, None, CURSOR_PRE_COMPACT_INGEST_BUDGET)
            .await;
    if !caught_up {
        return CursorPreCompactOutcome::skipped("transcript ingest did not complete");
    }

    let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
        return CursorPreCompactOutcome::skipped("session database unavailable");
    };
    let Some(session_id) = event_session_id(&parsed) else {
        return CursorPreCompactOutcome::skipped("no session id");
    };

    let messages_to_compact = event_usize(&parsed, &["messages_to_compact", "compact_count"]);
    if messages_to_compact == Some(0) {
        return CursorPreCompactOutcome::skipped("no messages to compact");
    }
    let fresh_tail_count = cursor_pre_compact_fresh_tail_count(&parsed, messages_to_compact);
    let current_tokens = event_i64(&parsed, &["context_tokens", "current_tokens", "tokens"]);
    let context_length = event_i64(&parsed, &["context_window_size", "context_length"]);

    let first = match db
        .lcm_compress(cursor_pre_compact_lcm_request(
            &session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::HermesAuxiliary,
            None,
        ))
        .await
    {
        Ok(response) => response,
        Err(err) => return CursorPreCompactOutcome::error(format!("LCM prepare failed: {err}")),
    };
    let Some(summary_request) = first.summary_request else {
        return CursorPreCompactOutcome::skipped(first.reason);
    };

    let summary = match crate::sessions::cursor_agent::summarize_with_cursor_agent(
        &summary_request,
        config,
    ) {
        Ok(summary) => summary,
        Err(err) => {
            return CursorPreCompactOutcome::error(format!("cursor-agent summary failed: {err}"))
        }
    };

    let second = match db
        .lcm_compress(cursor_pre_compact_lcm_request(
            &session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::Provided {
                summary_text: summary,
                route: Some("cursor_agent".to_string()),
            },
            first.frontier.current_frontier_store_id.or(Some(0)),
        ))
        .await
    {
        Ok(response) => response,
        Err(err) => return CursorPreCompactOutcome::error(format!("LCM persist failed: {err}")),
    };
    CursorPreCompactOutcome {
        status: second.status,
        reason: second.reason,
        summary_nodes_created: second.summary_nodes_created,
        summary_node_ids: second
            .summary_nodes
            .iter()
            .map(|node| node.node_id.clone())
            .collect(),
    }
}

/// Cursor `afterFileEdit` hook handler.
///
/// Keeps the graph fresh after Cursor Agent writes files by notifying the
/// daemon about the edited path(s). The daemon owns targeted sync scheduling
/// and the hook fails open when no daemon is available.
pub async fn hook_cursor_after_file_edit() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "afterFileEdit", &event);
    notify_cursor_after_file_edit(&event).await;
    0
}

/// Cursor `sessionStart` hook handler (fire-and-forget).
///
/// Emits Cursor's `sessionStart` output shape (`additional_context` + `env`)
/// steering the agent toward tracedecay MCP tools and reporting index freshness
/// for the resolved workspace. Never blocks session creation.
pub async fn hook_cursor_session_start() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "sessionStart", &event);
    // Catch-up ingest for resumed sessions whose transcript grew while no agent
    // was attached. No-op (no transcript_path) for brand-new sessions. Fail-open.
    ingest_cursor_transcript_for_event(
        &event,
        Some(CURSOR_CATCH_UP_INGEST_MAX_BYTES),
        CURSOR_SESSION_INGEST_BUDGET,
    )
    .await;
    let mut context = cursor_session_context_for_root(root.as_deref()).await;
    if session_start_from_compaction(&event) {
        append_context_recovery_hint(&mut context);
    }
    println!("{}", cursor_session_start_json(root.as_deref(), &context));
    0
}

/// Builds the lean Cursor `sessionStart` context for a resolved project root.
///
/// Deliberately complementary to (not duplicative of) the plugin's always-on
/// rule: the rule carries the tool-routing steering, so this only adds what
/// the rule cannot know — index freshness, the skill index, and the
/// tokens-saved counter.
async fn cursor_session_context_for_root(root: Option<&Path>) -> String {
    let (initialized, staleness, tokens_saved) = match root {
        Some(r) if crate::tracedecay::TraceDecay::has_initialized_store(r).await => {
            let (staleness, tokens_saved) = cursor_index_signals_for_root(r).await;
            (true, staleness, tokens_saved)
        }
        _ => (false, None, None),
    };
    build_cursor_session_context(initialized, staleness.as_deref(), tokens_saved)
}

/// Builds the tracedecay steering `additional_context` for Codex session/prompt
/// hooks. Unlike Cursor, Codex has no always-applied tracedecay rule, so this
/// context carries the full tool-routing steering plus index freshness.
async fn codex_session_context_for_event(event_json: &str) -> (String, HookWorkspaceStatus) {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let root = codex_project_root_from_parsed_event(&parsed);
    let cwd = event_cwd_from_parsed(&parsed);
    let session_id = event_session_id(&parsed);
    let status = codex_workspace_status(root.as_deref(), cwd.as_deref());
    record_workspace_status_analytics(root.as_deref(), status, session_id.as_deref());
    let staleness = match (status, root.as_deref()) {
        (HookWorkspaceStatus::Initialized, Some(r)) => {
            let (staleness, _) = cursor_index_signals_for_root(r).await;
            staleness
        }
        _ => None,
    };
    (
        build_codex_session_context_for_workspace(status, staleness.as_deref()),
        status,
    )
}

/// Cursor `afterShellExecution` hook handler.
///
/// Notifies the daemon after Cursor shell execution. The daemon decides whether
/// the command requires branch tracking or coalesced incremental sync.
pub async fn hook_cursor_after_shell() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(
        root.as_deref(),
        HintAgent::Cursor,
        "afterShellExecution",
        &event,
    );
    notify_cursor_after_shell_event(&event).await;
    0
}

/// Cursor `workspaceOpen` hook handler.
///
/// Notifies the daemon to run one-shot workspace catch-up when an indexed
/// workspace opens. We don't load plugins, so the output is an empty object.
/// Fail-open.
pub async fn hook_cursor_workspace_open() -> i32 {
    let event = read_hook_event!();
    let root = cursor_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Cursor, "workspaceOpen", &event);
    notify_cursor_workspace_open(&event).await;
    println!("{}", serde_json::json!({}));
    0
}

/// Pure decision logic for Cursor `subagentStart` hook events.
///
/// Cursor subagents must be allowed to start.
///
/// Earlier versions denied research/explore subagents in favor of tracedecay MCP
/// tools. In Cursor this can surface as a misleading "bubble creation" timeout,
/// and it prevents explicit user requests to use agents. Keep this handler
/// fail-open so stale installs that still register `subagentStart` do not block
/// subagent creation.
pub fn evaluate_cursor_subagent_start(event_json: &str) -> Option<String> {
    let _ = event_json;
    None
}

/// Pure decision logic for Cursor `postToolUse` hook events.
///
/// Returns a soft `additional_context` payload (Cursor's documented
/// `postToolUse` output shape) for exploration tools tracedecay can replace.
/// Invalid or unrelated tool events fail open with no output. Session-level
/// dedupe lives in [`cursor_post_tool_use_decision`]; this stays pure for
/// tests.
pub fn evaluate_cursor_post_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    Some(
        serde_json::json!({
            "additional_context": format_tool_hint(&hint),
        })
        .to_string(),
    )
}

/// Impure `postToolUse` path: [`evaluate_cursor_post_tool_use`] plus
/// per-session hint dedupe persisted under the project's `.tracedecay/` dir.
pub fn cursor_post_tool_use_decision(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed);
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Cursor,
        event_session_id(&parsed).as_deref(),
        &hint,
    );
    let hint = deduped_cursor_hint(event_json, hint)?;
    Some(
        serde_json::json!({
            "additional_context": format_tool_hint(&hint),
        })
        .to_string(),
    )
}

/// Suppresses hints that were already emitted for this session.
///
/// The `(session_id, category)` pairs are persisted in
/// `.tracedecay/tool_hints_seen.json` so each hint category surfaces at most
/// once per Cursor session across short-lived hook processes. Hints are also
/// suppressed entirely when the workspace has no tracedecay index (suggesting
/// tracedecay tools there would be misleading). When no session id is present
/// the hint is emitted as-is — dedupe is impossible but the hint is still
/// useful (fail-open).
fn deduped_cursor_hint(event_json: &str, hint: ToolHint) -> Option<ToolHint> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed)?;
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        record_hint_analytics(
            Some(&root),
            "suppressed_uninitialized",
            HintAgent::Cursor,
            event_session_id(&parsed).as_deref(),
            &hint,
        );
        return None;
    }
    deduped_project_hint(
        Some(root),
        HintAgent::Cursor,
        event_session_id(&parsed),
        hint,
    )
}

fn deduped_codex_hint(event_json: &str, parsed: &Value, hint: ToolHint) -> Option<ToolHint> {
    deduped_project_hint(
        codex_project_root_from_event(event_json),
        HintAgent::Codex,
        event_session_id(parsed),
        hint,
    )
}

fn deduped_project_hint(
    root: Option<PathBuf>,
    agent: HintAgent,
    session_id: Option<String>,
    hint: ToolHint,
) -> Option<ToolHint> {
    let Some(root) = root else {
        record_hint_emitted(None, agent, session_id.as_deref(), &hint);
        return Some(hint);
    };
    let Some(session_id) = session_id else {
        record_hint_emitted(Some(&root), agent, None, &hint);
        return Some(hint);
    };
    if !remember_hint_in_process(&root, agent, &session_id, hint.category) {
        record_hint_analytics(
            Some(&root),
            "suppressed_duplicate",
            agent,
            Some(&session_id),
            &hint,
        );
        return None;
    }
    let Ok(layout) = crate::storage::resolve_layout_for_current_profile(&root) else {
        record_hint_emitted(Some(&root), agent, Some(&session_id), &hint);
        return Some(hint);
    };
    if !layout.data_root.is_dir() {
        record_hint_emitted(Some(&root), agent, Some(&session_id), &hint);
        return Some(hint);
    }
    let path = layout.data_root.join("tool_hints_seen.json");
    let mut dedupe = tool_hints::ToolHintDedupe::load_or_default(&path);
    if !dedupe.should_emit(&session_id, hint.category) {
        record_hint_analytics(
            Some(&root),
            "suppressed_duplicate",
            agent,
            Some(&session_id),
            &hint,
        );
        return None;
    }
    let _ = dedupe.save(&path);
    record_hint_analytics(Some(&root), "hint_emitted", agent, Some(&session_id), &hint);
    Some(hint)
}

fn remember_hint_in_process(
    root: &Path,
    agent: HintAgent,
    session_id: &str,
    category: HintCategory,
) -> bool {
    static MEMORY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = format!(
        "{}\0{}\0{}\0{}",
        root.display(),
        agent.as_key(),
        session_id,
        category.as_key()
    );
    let Ok(mut memory) = MEMORY.get_or_init(|| Mutex::new(HashSet::new())).lock() else {
        return true;
    };
    memory.insert(key)
}

pub fn cursor_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_project_root_from_parsed_event(&parsed)
}

fn cursor_project_root_candidate_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    cursor_project_root_from_parsed_event(parsed).or_else(|| {
        cursor_event_candidates(parsed)
            .into_iter()
            .find_map(|candidate| nearest_project_like_root(&candidate))
    })
}

fn cursor_project_root_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    let resolved = cursor_event_candidates(parsed)
        .into_iter()
        .find_map(|candidate| crate::config::discover_project_root(&candidate));
    let cwd_root = cursor_event_cwd(parsed)
        .as_deref()
        .and_then(crate::config::discover_project_root);
    match (cwd_root, resolved) {
        // Prefer the root derived from cwd when available; this avoids routing
        // a root-B event into root A just because workspace_roots listed A first.
        (Some(cwd_root), Some(resolved)) if !paths_same(&cwd_root, &resolved) => Some(cwd_root),
        (Some(cwd_root), None) => Some(cwd_root),
        (_, other) => other,
    }
}

fn nearest_project_like_root(start: &Path) -> Option<PathBuf> {
    if let Some(root) = crate::worktree::git_worktree_root(start) {
        return Some(root);
    }
    let mut dir = start.to_path_buf();
    loop {
        if project_marker_exists(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn cursor_event_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|seen| seen == &candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(cwd) = cursor_event_cwd(event) {
        push_unique(cwd);
    }
    if let Some(project_root) = crate::config::brand_env("PROJECT_ROOT") {
        push_unique(PathBuf::from(project_root));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let path = Path::new(file_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let path = Path::new(transcript_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|s| !s.is_empty()) {
                push_unique(PathBuf::from(path));
            }
        }
    }
    candidates
}

fn cursor_event_cwd(event: &Value) -> Option<PathBuf> {
    event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Returns `true` when `command` is a git invocation that changes the working
/// tree / HEAD enough that a broad re-sync is warranted (checkout, switch,
/// pull, merge, rebase, reset, cherry-pick, `stash pop`/`stash apply`).
///
/// Read-only git commands (`status`, `log`, `diff`), `commit`/`add`, and
/// non-git commands return `false`. Only commands whose first token is `git`
/// match, so `echo git checkout` is ignored.
pub fn is_git_state_changing_command(command: &str) -> bool {
    let tokens = shell_words(command);
    let Some(sub_pos) = git_subcommand_pos(&tokens) else {
        return false;
    };
    let sub = tokens[sub_pos].to_ascii_lowercase();
    match sub.as_str() {
        "checkout" | "switch" | "pull" | "merge" | "rebase" | "reset" | "cherry-pick" => true,
        "stash" => {
            let after = tokens
                .iter()
                .skip(sub_pos + 1)
                .map(|t| t.to_ascii_lowercase())
                .find(|t| !t.starts_with('-'));
            matches!(after.as_deref(), Some("pop" | "apply"))
        }
        _ => false,
    }
}

/// The action a Cursor `afterShellExecution` hook should take for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorShellSyncPlan {
    /// Bootstrap/maintain branch tracking for the given branch (supersedes a
    /// plain sync; the branch-add path copies the parent DB and syncs).
    BranchAdd(String),
    /// Bootstrap/maintain branch tracking in a newly-created linked worktree.
    WorktreeBranchAdd {
        branch: String,
        worktree_path: String,
    },
    /// Run a full incremental sync (same-branch change set).
    IncrementalSync,
    /// Ensure the current branch is tracked, then sync it if it already was.
    CurrentBranchSync(String),
    /// Do nothing.
    Noop,
}

/// Classifies a shell command into the sync action a Cursor
/// `afterShellExecution` hook should take. Branch switches take precedence
/// over plain incremental syncs.
pub fn cursor_shell_sync_plan(command: &str) -> CursorShellSyncPlan {
    cursor_shell_sync_plan_with_current_branch(command, None)
}

/// Like [`cursor_shell_sync_plan`], but supplies the post-command current branch
/// for state-changing commands whose branch target is ambiguous or implicit.
pub fn cursor_shell_sync_plan_with_current_branch(
    command: &str,
    current_branch: Option<&str>,
) -> CursorShellSyncPlan {
    let raw = shell_words(command);
    if let Some(parts) = cursor_worktree_add_parts_from_tokens(&raw) {
        return CursorShellSyncPlan::WorktreeBranchAdd {
            branch: parts.branch,
            worktree_path: parts.worktree_path,
        };
    }
    if let Some(branch) = cursor_branch_switch_target_from_tokens(&raw) {
        return CursorShellSyncPlan::BranchAdd(branch);
    }
    if is_git_state_changing_command(command) {
        if let Some(branch) = current_branch.filter(|branch| !branch.is_empty()) {
            return CursorShellSyncPlan::CurrentBranchSync(branch.to_string());
        }
        return CursorShellSyncPlan::IncrementalSync;
    }
    CursorShellSyncPlan::Noop
}

/// Returns the target branch for a branch-changing git command:
/// `git checkout <branch>`, `git switch <branch>`, `git checkout -b <branch>`,
/// and `git switch -c <branch>`. Worktree creation is classified separately by
/// [`cursor_shell_sync_plan`], which owns `git worktree add` parsing.
///
/// Path checkouts (`git checkout -- <file>` or obvious file pathspecs), remote
/// tracking shortcuts such as `git switch --track origin/feature`, and
/// non-switch commands return `None`. Only commands whose first shell word is
/// `git` are considered.
pub fn cursor_branch_switch_target(command: &str) -> Option<String> {
    let raw = shell_words(command);
    cursor_branch_switch_target_from_tokens(&raw)
}

fn cursor_branch_switch_target_from_tokens(raw: &[String]) -> Option<String> {
    let sub_pos = git_subcommand_pos(raw)?;
    let sub = raw[sub_pos].to_ascii_lowercase();

    match sub.as_str() {
        "checkout" | "switch" => {
            let after = &raw[sub_pos + 1..];
            let mut i = 0;
            let mut uses_tracking_shortcut = false;
            while i < after.len() {
                let tok = &after[i];
                if tok == "--" {
                    return None;
                }
                if matches!(tok.as_str(), "-b" | "-B" | "-c" | "-C" | "--orphan") {
                    return after.get(i + 1).cloned();
                }
                if tok == "-t" || tok == "--track" || tok.starts_with("--track=") {
                    uses_tracking_shortcut = true;
                    i += 1;
                    continue;
                }
                if tok.starts_with('-') {
                    i += 1;
                    continue;
                }
                if uses_tracking_shortcut {
                    return None;
                }
                if is_obvious_checkout_pathspec(tok) {
                    return None;
                }
                return Some(tok.clone());
            }
            None
        }
        _ => None,
    }
}

fn cursor_worktree_add_parts_from_tokens(raw: &[String]) -> Option<WorktreeAddParts> {
    let sub_pos = git_subcommand_pos(raw)?;
    if raw.get(sub_pos)?.eq_ignore_ascii_case("worktree")
        && raw.get(sub_pos + 1)?.eq_ignore_ascii_case("add")
    {
        return cursor_worktree_add_parts(&raw[sub_pos + 2..]);
    }
    None
}

struct WorktreeAddParts {
    branch: String,
    worktree_path: String,
}

fn cursor_worktree_add_parts(after: &[String]) -> Option<WorktreeAddParts> {
    let mut i = 0;
    let mut positional = Vec::new();
    let mut detached = false;
    let mut new_branch = None;
    while i < after.len() {
        let tok = &after[i];
        if tok == "--" {
            positional.extend(after[i + 1..].iter().cloned());
            break;
        }
        if matches!(tok.as_str(), "-b" | "-B") {
            new_branch = after.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if tok == "-d" || tok == "--detach" {
            detached = true;
            i += 1;
            continue;
        }
        if tok == "--reason" {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        positional.push(tok.clone());
        i += 1;
    }
    if detached {
        return None;
    }
    let worktree_path = positional.first()?.clone();
    let branch = new_branch.or_else(|| positional.get(1).cloned())?;
    Some(WorktreeAddParts {
        branch,
        worktree_path,
    })
}

fn is_obvious_checkout_pathspec(token: &str) -> bool {
    token == "."
        || token == ":/"
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with(":/")
        || token
            .rsplit_once('.')
            .is_some_and(|(_, ext)| !ext.is_empty())
}

/// Splits a shell command line into words, honoring single/double quotes and
/// backslash escapes. Shared with `tool_hints` so search-command
/// classification sees the same tokens as the checkout/sync parsing here.
pub(crate) fn shell_words(command: &str) -> Vec<String> {
    shell_words_for_platform(command, cfg!(windows))
}

fn shell_words_for_platform(command: &str, windows: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for c in command.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => current.push(c),
            },
            _ => match c {
                '\'' | '"' => quote = Some(c),
                '\\' if windows => current.push(c),
                '\\' => escaped = true,
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            },
        }
    }

    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn git_subcommand_pos(tokens: &[String]) -> Option<usize> {
    if !tokens.first()?.eq_ignore_ascii_case("git") {
        return None;
    }

    let mut i = 1;
    while i < tokens.len() {
        let token = tokens[i].to_ascii_lowercase();
        match token.as_str() {
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                i += 2;
            }
            "--" => {
                i += 1;
            }
            _ if token.starts_with("--git-dir=")
                || token.starts_with("--work-tree=")
                || token.starts_with("--namespace=")
                || token.starts_with("--config-env=") =>
            {
                i += 1;
            }
            _ if token.starts_with('-') => {
                i += 1;
            }
            _ => return Some(i),
        }
    }
    None
}

pub fn cursor_shell_command_targets_project(
    command: &str,
    cwd: &Path,
    project_root: &Path,
) -> bool {
    let tokens = shell_words(command);
    if !tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("git"))
    {
        return true;
    }
    let Some(work_dir) = git_explicit_work_dir(&tokens, cwd) else {
        return true;
    };
    let target_root = crate::config::discover_project_root(&work_dir).unwrap_or(work_dir);
    paths_same(&target_root, project_root)
}

fn git_explicit_work_dir(tokens: &[String], cwd: &Path) -> Option<PathBuf> {
    let mut i = 1;
    let mut explicit_work_dir = None;
    while i < tokens.len() {
        let token = &tokens[i];
        match token.as_str() {
            "-C" | "--work-tree" => {
                let value = tokens.get(i + 1)?;
                explicit_work_dir = Some(resolve_shell_path(cwd, value));
                i += 2;
            }
            "-c" | "--git-dir" | "--namespace" | "--config-env" => i += 2,
            _ if token.starts_with("--work-tree=") => {
                let value = token.trim_start_matches("--work-tree=");
                explicit_work_dir = Some(resolve_shell_path(cwd, value));
                i += 1;
            }
            _ if token.starts_with("--git-dir=")
                || token.starts_with("--namespace=")
                || token.starts_with("--config-env=") =>
            {
                i += 1;
            }
            _ if token.starts_with('-') => i += 1,
            _ => break,
        }
    }
    explicit_work_dir
}

fn resolve_shell_path(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Resolves the filesystem root of the worktree created by a
/// `git worktree add` command. git resolves the worktree path against
/// `-C <dir>`/`--work-tree` overrides rather than the shell cwd, so those are
/// honored first. The result is canonicalized when the worktree exists (it
/// does by the time a post-shell hook fires) so symlinked components resolve
/// the way git resolved them, falling back to lexical `..` normalization.
pub fn resolve_worktree_add_root(command: &str, cwd: &Path, worktree_path: &str) -> PathBuf {
    let tokens = shell_words(command);
    let base = git_explicit_work_dir(&tokens, cwd).unwrap_or_else(|| cwd.to_path_buf());
    let joined = resolve_shell_path(&base, worktree_path);
    joined
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(&joined))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Extracts the repo-relative paths edited in a Cursor `afterFileEdit` event.
///
/// Cursor sends an absolute `file_path` (plus an `edits` array). We strip the
/// resolved `project_root` prefix and normalize to forward slashes so the hook
/// can notify the daemon about only the changed files. Paths outside the project
/// root are skipped.
pub fn cursor_after_file_edit_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };

    let mut abs_paths: Vec<String> = Vec::new();
    if let Some(p) = parsed
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        abs_paths.push(p.to_string());
    }
    // Defensive: some edit payloads may carry per-edit file paths.
    if let Some(edits) = parsed.get("edits").and_then(Value::as_array) {
        for edit in edits {
            if let Some(p) = edit
                .get("file_path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                abs_paths.push(p.to_string());
            }
        }
    }

    let mut rels: Vec<String> = Vec::new();
    for abs in abs_paths {
        if let Some(rel) = rel_under_root(project_root, Path::new(&abs)) {
            if !rels.contains(&rel) {
                rels.push(rel);
            }
        }
    }
    rels
}

fn rel_under_root(root: &Path, abs: &Path) -> Option<String> {
    let stripped = abs.strip_prefix(root).ok()?;
    if stripped.as_os_str().is_empty() {
        return None;
    }
    if stripped.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(stripped.to_string_lossy().replace('\\', "/"))
}

/// Returns `true` when a sync should run given the last marker time and a
/// debounce window. Used to coalesce back-to-back `afterShellExecution` syncs.
pub fn cursor_should_run_sync(now_secs: i64, last_secs: Option<i64>, debounce_secs: i64) -> bool {
    match last_secs {
        Some(last) => now_secs - last >= debounce_secs,
        None => true,
    }
}

/// Model-invocable workflow skills shipped in the tracedecay Cursor plugin's
/// `skills/` directory (slash dispatchers with `disable-model-invocation:
/// true` are excluded). Kept as one constant so the session steering context
/// and the bundle coverage test in `agents::cursor` stay in sync.
pub const CURSOR_PLUGIN_SKILLS: &[&str] = &[
    "architecture-overview",
    "assessing-test-coverage",
    "atomic-code-edits",
    "auditing-code-safety",
    "cleaning-up-dead-code",
    "code-health-report",
    "cross-branch-investigation",
    "curating-project-memory",
    "drafting-commit-and-pr",
    "exploring-types-and-traits",
    "finding-duplicate-logic",
    "finding-impacted-areas",
    "fixing-build-and-type-errors",
    "porting-code",
    "project-status",
    "reading-code-cheaply",
    "recalling-project-memory",
    "recalling-session-context",
    "refactoring-safely",
    "reviewing-a-diff",
    "running-impacted-tests",
    "searching-for-code",
    "tracing-functions",
    "tracking-session-health",
    "using-the-cli",
];

/// Builds the Cursor `sessionStart` `additional_context` text.
///
/// Intentionally lean: the always-applied plugin rule already carries the
/// tool-routing steering, so repeating it here would burn tokens every
/// session. This adds only the session-specific signals — index freshness,
/// the workflow-skill index, and the tokens-saved counter.
pub fn build_cursor_session_context(
    initialized: bool,
    staleness_hint: Option<&str>,
    tokens_saved: Option<u64>,
) -> String {
    let mut s = index_status_line(initialized, staleness_hint);
    if initialized {
        s.push_str("Workflow skills: tracedecay:");
        s.push_str(&CURSOR_PLUGIN_SKILLS.join(", "));
        s.push_str(" — each maps a common workflow to the right tracedecay tools.\n");
        if let Some(saved) = tokens_saved.filter(|saved| *saved > 0) {
            s.push_str("Tokens saved by tracedecay this session: ");
            s.push_str(&saved.to_string());
            s.push_str(".\n");
        }
    }
    s
}

/// One-line index freshness signal shared by the Cursor and Claude session
/// contexts. Both hosts carry the tool-routing steering in an always-applied
/// rule (Cursor plugin rule, CLAUDE.md), so their session hooks report only
/// session-specific signals.
fn index_status_line(initialized: bool, staleness_hint: Option<&str>) -> String {
    if initialized {
        match staleness_hint {
            Some(hint) => format!("tracedecay index status: {hint}.\n"),
            None => "tracedecay index status: initialized.\n".to_string(),
        }
    } else {
        "tracedecay index status: no project index found in this workspace — \
         run `tracedecay init` to enable tracedecay MCP tools.\n"
            .to_string()
    }
}

/// Builds the Codex session/prompt steering context. Codex has no
/// always-applied tracedecay rule, so the full tool-routing steering lives
/// here.
pub fn build_codex_session_context(initialized: bool, staleness_hint: Option<&str>) -> String {
    let status = if initialized {
        HookWorkspaceStatus::Initialized
    } else {
        HookWorkspaceStatus::UnindexedProject
    };
    build_codex_session_context_for_workspace(status, staleness_hint)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookWorkspaceStatus {
    Initialized,
    UnindexedProject,
    Generic,
}

impl HookWorkspaceStatus {
    fn as_key(self) -> &'static str {
        match self {
            HookWorkspaceStatus::Initialized => "initialized",
            HookWorkspaceStatus::UnindexedProject => "unindexed_project",
            HookWorkspaceStatus::Generic => "generic",
        }
    }
}

/// Builds the Codex session/prompt context for the detected workspace kind.
pub fn build_codex_session_context_for_workspace(
    status: HookWorkspaceStatus,
    staleness_hint: Option<&str>,
) -> String {
    let mut s = String::new();
    match status {
        HookWorkspaceStatus::Initialized | HookWorkspaceStatus::UnindexedProject => {
            s.push_str(
                "tracedecay is available via MCP. Prefer tracedecay MCP tools \
                 (tracedecay_context, tracedecay_search, tracedecay_callers, tracedecay_callees, \
                 tracedecay_impact, tracedecay_files, tracedecay_affected) over broad file reads \
                 or shell search for codebase exploration, symbol lookup, call graphs, and \
                 impact analysis. Fall back to file reads only when tracedecay cannot answer.\n\
                 If an MCP call errors, times out, or the server is disconnected, every tool \
                 is also a shell command: `tracedecay tool <name> --key value` (`tracedecay \
                 tool` lists tools, `tracedecay tool <name> --help` shows parameters). Use \
                 that CLI instead of querying .tracedecay databases directly or abandoning \
                 tracedecay.\n",
            );
            append_codex_recall_and_registry_guidance(&mut s);
            match status {
                HookWorkspaceStatus::Initialized => match staleness_hint {
                    Some(hint) => {
                        s.push_str("Index status: ");
                        s.push_str(hint);
                        s.push_str(".\n");
                    }
                    None => s.push_str("Index status: initialized.\n"),
                },
                HookWorkspaceStatus::UnindexedProject => s.push_str(
                    "Index status: no project index found in this code workspace — \
                     run `tracedecay init` to enable tracedecay code-graph tools.\n",
                ),
                HookWorkspaceStatus::Generic => {}
            }
        }
        HookWorkspaceStatus::Generic => {
            s.push_str(
                "TraceDecay session context is available via MCP. For prior conversation \
                 recovery, use tracedecay_lcm_expand_query, tracedecay_message_search, and \
                 tracedecay_lcm_describe before asking the user to repeat themselves. Use \
                 tracedecay_fact_store only for durable preferences, environment details, \
                 tool quirks, or decisions that will still matter later. Do not store task \
                 progress, temporary TODOs, or soon-stale session outcomes; recover those \
                 from transcripts instead.\n",
            );
            s.push_str("Workspace status: no active project workspace; no setup guidance needed for this prompt.\n");
        }
    }
    s
}

fn append_codex_recall_and_registry_guidance(s: &mut String) {
    s.push_str(
        "For other registered projects or sibling workspaces, check \
         tracedecay_project_list or tracedecay_project_search first; use \
         tracedecay_project_context to confirm the target and pass project_id or \
         project_path to tracedecay_context/search for cross-project code context before \
         scanning parent directories. When the user references prior conversation or \
         missing context, use tracedecay_message_search or tracedecay_lcm_expand_query \
         before asking the user to repeat themselves. Use tracedecay_fact_store only for \
         durable preferences, environment details, tool quirks, or decisions that will \
         still matter later. Do not store task progress, temporary TODOs, or soon-stale \
         session outcomes; recover those from transcripts instead.\n",
    );
}

fn append_context_recovery_hint(context: &mut String) {
    if !context.is_empty() && !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(COMPACTION_CONTEXT_RECOVERY_HINT);
    context.push('\n');
}

fn session_start_from_compaction(event_json: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return false;
    };
    ["source", "trigger", "reason", "boundary_reason"]
        .iter()
        .filter_map(|key| parsed.get(*key).and_then(Value::as_str))
        .any(matches_compaction_source)
}

fn matches_compaction_source(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "compact" | "compaction" | "contextcompacted" | "compression"
    )
}

/// Formats a short relative-age staleness hint from a sync age in seconds.
pub fn cursor_staleness_hint(age_secs: i64) -> String {
    let age = age_secs.max(0);
    if age < 60 {
        "last indexed just now".to_string()
    } else if age < 3_600 {
        format!("last indexed {}m ago", age / 60)
    } else if age < 86_400 {
        format!("last indexed {}h ago", age / 3_600)
    } else {
        format!("last indexed {}d ago", age / 86_400)
    }
}

/// Builds the Cursor `sessionStart` output JSON (`additional_context` + `env`).
/// When `project_root` is known, exposes it as `TRACEDECAY_PROJECT_ROOT` so
/// subsequent session hooks can reuse it.
pub fn cursor_session_start_json(project_root: Option<&Path>, additional_context: &str) -> String {
    let mut env = serde_json::Map::new();
    if let Some(root) = project_root {
        env.insert(
            "TRACEDECAY_PROJECT_ROOT".to_string(),
            Value::String(root.to_string_lossy().to_string()),
        );
    }
    serde_json::json!({
        "additional_context": additional_context,
        "env": Value::Object(env),
    })
    .to_string()
}

/// Opens the index once and reads both session-steering signals: the
/// staleness hint and the session tokens-saved counter.
async fn cursor_index_signals_for_root(root: &Path) -> (Option<String>, Option<u64>) {
    let Ok(cg) = crate::tracedecay::TraceDecay::open(root).await else {
        return (None, None);
    };
    let last = cg.last_sync_timestamp().await;
    let staleness = (last > 0).then(|| cursor_staleness_hint(now_unix_secs() - last));
    let tokens_saved = cg.get_tokens_saved().await.ok();
    (staleness, tokens_saved)
}

/// Best-effort daemon notification for Cursor `afterFileEdit`.
///
/// Resolves the edited repo-relative paths locally, then lets the daemon own
/// scheduling and sync execution. No-ops when no in-project paths were edited.
async fn notify_cursor_after_file_edit(event_json: &str) {
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }
    let rels = cursor_after_file_edit_rel_paths(event_json, &root);
    if rels.is_empty() {
        return;
    }
    crate::daemon::notify_hook_event(
        &root,
        crate::daemon::DaemonHookEvent::cursor_after_file_edit(rels),
    )
    .await;
}

/// Best-effort daemon notification for Cursor `afterShellExecution`.
async fn notify_cursor_after_shell_event(event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let command = parsed
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }
    let cwd = cursor_event_cwd(&parsed).unwrap_or_else(|| root.clone());
    crate::daemon::notify_hook_event(
        &root,
        crate::daemon::DaemonHookEvent::cursor_after_shell_execution(command.to_string(), cwd),
    )
    .await;
}

/// Best-effort daemon notification for Cursor `workspaceOpen`.
async fn notify_cursor_workspace_open(event_json: &str) {
    let Some(root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }
    crate::daemon::notify_hook_event(
        &root,
        crate::daemon::DaemonHookEvent::cursor_workspace_open(root.clone()),
    )
    .await;
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ---------------------------------------------------------------------------
// Claude Code lifecycle hook handlers (SessionStart / PostToolUse)
//
// Claude Code sends ONE JSON object on stdin with the same shared fields as
// Codex (session_id, transcript_path, cwd, hook_event_name, plus
// event-specific fields) and reads `hookSpecificOutput` JSON from stdout —
// Codex adopted Claude's hook schema, so these handlers share the Codex
// event/output helpers. The older Claude handlers (`hook_pre_tool_use`,
// `hook_prompt_submit`, `hook_stop`) predate that schema and keep their own
// input shapes.
// ---------------------------------------------------------------------------

/// Claude Code `SessionStart` hook handler (fail-open).
///
/// The CLAUDE.md prompt rules already carry the tool-routing steering, so
/// this emits only session-specific signals via
/// `hookSpecificOutput.additionalContext`: index freshness (or a
/// `tracedecay init` nudge in an unindexed project) plus the LCM
/// context-recovery hint when the session (re)starts from compaction.
pub async fn hook_claude_session_start() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Claude, "SessionStart", &event);
    let mut context = claude_session_context_for_event(&event).await;
    if session_start_from_compaction(&event) {
        append_context_recovery_hint(&mut context);
    }
    if context.is_empty() {
        println!("{}", serde_json::json!({}));
    } else {
        println!(
            "{}",
            codex_additional_context_json("SessionStart", &context)
        );
    }
    0
}

/// Builds the lean Claude `SessionStart` context: the index-status line for
/// the session's project, an init nudge for unindexed project-like
/// workspaces, and nothing at all outside code workspaces.
async fn claude_session_context_for_event(event_json: &str) -> String {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    // `discover_project_root` only resolves initialized tracedecay projects.
    match codex_project_root_from_parsed_event(&parsed) {
        Some(root) => {
            let (staleness, _) = cursor_index_signals_for_root(&root).await;
            index_status_line(true, staleness.as_deref())
        }
        None if event_cwd_from_parsed(&parsed)
            .as_deref()
            .is_some_and(is_project_like_workspace) =>
        {
            index_status_line(false, None)
        }
        None => String::new(),
    }
}

/// Claude Code `PostToolUse` hook handler used to keep the graph fresh after
/// writes.
///
/// For edit tools and shell commands this notifies the daemon, which owns
/// targeted sync, branch tracking, and coalescing. Fail-open and silent.
pub async fn hook_claude_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Claude, "PostToolUse", &event);
    claude_post_tool_use(&event).await;
    0
}

async fn claude_post_tool_use(event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let tool_name = parsed
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(cwd) = event_cwd_from_parsed(&parsed) else {
        return;
    };
    let Some(root) = crate::config::discover_project_root(&cwd)
        .or_else(|| crate::worktree::git_worktree_root(&cwd))
    else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }

    if is_claude_edit_tool(tool_name) {
        let rels = claude_edit_rel_paths(&parsed, &cwd, &root);
        if rels.is_empty() {
            return;
        }
        crate::daemon::notify_hook_event(
            &root,
            crate::daemon::DaemonHookEvent::claude_post_tool_use_edit(rels, cwd),
        )
        .await;
    } else if is_claude_bash_tool(tool_name) {
        let command = parsed
            .get("tool_input")
            .and_then(|ti| ti.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if command.is_empty() {
            return;
        }
        crate::daemon::notify_hook_event(
            &root,
            crate::daemon::DaemonHookEvent::claude_post_tool_use_shell(command.to_string(), cwd),
        )
        .await;
    }
}

fn is_claude_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "multiedit" | "notebookedit"
    )
}

fn is_claude_bash_tool(tool_name: &str) -> bool {
    tool_name.eq_ignore_ascii_case("bash")
}

/// Extracts the project-relative path edited by a Claude edit tool.
///
/// Claude's `Edit`/`Write`/`MultiEdit` put the target in
/// `tool_input.file_path`; `NotebookEdit` uses `tool_input.notebook_path`.
/// Paths are usually absolute but are resolved against the session `cwd`
/// when relative. Paths outside `project_root` are skipped.
fn claude_edit_rel_paths(parsed: &Value, cwd: &Path, project_root: &Path) -> Vec<String> {
    ["file_path", "notebook_path"]
        .iter()
        .filter_map(|key| {
            parsed
                .get("tool_input")
                .and_then(|ti| ti.get(*key))
                .and_then(Value::as_str)
        })
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| {
            let candidate = Path::new(raw);
            let abs = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                cwd.join(candidate)
            };
            rel_under_root(project_root, &abs)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Codex CLI hook handlers
//
// Codex sends ONE JSON object on stdin (shared fields: session_id,
// transcript_path, cwd, hook_event_name, model, plus event-specific fields)
// and reads a Codex-shaped JSON object from stdout. These handlers intentionally
// emit Codex's documented output schema (`hookSpecificOutput.additionalContext`
// for steering, `hookSpecificOutput.permissionDecision` for PreToolUse) rather
// than reusing the Claude / Cursor / Kiro output shapes.
// ---------------------------------------------------------------------------

/// Codex `SessionStart` hook handler (fire-and-forget).
///
/// Emits `hookSpecificOutput.additionalContext` steering the agent toward
/// tracedecay MCP tools and reporting index freshness for the session `cwd`.
pub async fn hook_codex_session_start() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "SessionStart", &event);
    let (mut context, _) = codex_session_context_for_event(&event).await;
    if session_start_from_compaction(&event) {
        append_context_recovery_hint(&mut context);
    }
    println!(
        "{}",
        codex_additional_context_json("SessionStart", &context)
    );
    0
}

/// Codex `UserPromptSubmit` hook handler.
///
/// Resets the per-project local counter for the new turn and injects the same
/// tracedecay steering context as `SessionStart`. Never blocks the prompt.
pub async fn hook_codex_user_prompt_submit() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(
        root.as_deref(),
        HintAgent::Codex,
        "UserPromptSubmit",
        &event,
    );
    reset_counter_for_codex_event(&event).await;
    let context = codex_user_prompt_submit_context_for_event(&event).await;
    println!(
        "{}",
        codex_additional_context_json("UserPromptSubmit", &context)
    );
    0
}

pub async fn codex_user_prompt_submit_context_for_event(event: &str) -> String {
    let (mut context, status) = codex_session_context_for_event(event).await;
    if !matches!(status, HookWorkspaceStatus::Generic) {
        if let Some(hint) = codex_prompt_hint(event) {
            append_tool_hint(&mut context, &hint);
        }
    }
    context
}

/// Codex `SubagentStart` hook handler.
///
/// Steers research/explore subagents toward tracedecay MCP tools. Codex cannot
/// hard-stop a subagent at start (`continue: false` is ignored for this event),
/// so this injects `additionalContext` instead of denying.
pub fn hook_codex_subagent_start() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "SubagentStart", &event);
    let count = record_codex_subagent_start(&event);
    let output = evaluate_codex_subagent_start(&event);
    eprintln!(
        "{}",
        codex_subagent_start_log_line(&event, count, output.is_some())
    );
    if let Some(output) = output {
        println!("{output}");
    }
    0
}

/// Codex `PostToolUse` hook handler used to keep the graph fresh after writes.
///
/// For edit tools and shell commands this notifies the daemon, which owns
/// targeted sync, branch tracking, and coalescing. Fail-open and silent.
pub async fn hook_codex_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostToolUse", &event);
    codex_post_tool_use(&event).await;
    0
}

/// Codex `PostCompact` hook handler.
///
/// Codex stores compacted context bodies encrypted in the transcript. This hook
/// uses the visible source messages already ingested into the LCM store, asks a
/// child Codex app-server turn to summarize them, and replaces the temporary
/// deterministic summary node. Fail-open: compaction must never block Codex.
pub async fn hook_codex_post_compact() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Codex, "PostCompact", &event);
    if std::env::var_os(crate::sessions::codex_app_server::CODEX_SUMMARY_CHILD_ENV).is_none() {
        codex_post_compact(&event).await;
    }
    println!("{}", serde_json::json!({}));
    0
}

/// Builds a Codex hook stdout payload that injects model-visible context via
/// `hookSpecificOutput.additionalContext`. Used by `SessionStart`,
/// `UserPromptSubmit`, and `SubagentStart`.
pub fn codex_additional_context_json(event_name: &str, additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

/// Pure decision logic for Codex `SubagentStart` events.
///
/// Returns a Codex `additionalContext` payload steering research/explore
/// or new/no-history subagents toward tracedecay MCP tools and compact memory
/// recall, or `None` for execution-style subagents that already have history.
/// Inspects `agent_type` (Codex's documented field), any prompt/task/description
/// text, and conservative history/newness fields when present.
pub fn evaluate_codex_subagent_start(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let task = parsed
        .get("prompt")
        .or_else(|| parsed.get("task"))
        .or_else(|| parsed.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: Some("SubagentStart".to_string()),
        command: None,
        prompt: (!task.is_empty()).then(|| task.to_string()),
        subagent_type: (!agent_type.is_empty()).then(|| agent_type.to_string()),
        file_path: None,
        hints_enabled: true,
    });
    let is_explore = agent_type.eq_ignore_ascii_case("explore");
    let is_research = is_explore || is_code_research_prompt(task);
    let needs_context = codex_subagent_needs_context(&parsed);
    if is_research || needs_context {
        let dedupe_hint = ToolHint {
            category: if is_research {
                HintCategory::ExploreSubagent
            } else {
                HintCategory::SubagentStartContext
            },
            message: "For Codex subagents, add compact TraceDecay context before isolated work."
                .to_string(),
            context: CODEX_SUBAGENT_START_CONTEXT.to_string(),
            nonblocking: true,
        };
        let root = codex_project_root_from_event(event_json);
        record_hint_analytics(
            root.as_deref(),
            "hint_candidate",
            HintAgent::Codex,
            event_session_id(&parsed).as_deref(),
            &dedupe_hint,
        );
        let _ = deduped_codex_hint(event_json, &parsed, dedupe_hint)?;
        let context = codex_subagent_start_context(hint, needs_context);
        return Some(codex_additional_context_json("SubagentStart", &context));
    }
    None
}

/// Records a Codex `SubagentStart` in the current project's profile-sharded
/// hook state and returns the session-local count. Fail-open: malformed events,
/// missing roots, and storage errors only disable counting.
pub fn record_codex_subagent_start(event_json: &str) -> Option<u64> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let root = codex_project_root_from_parsed_event(&parsed)?;
    let layout = crate::storage::resolve_layout_for_current_profile(&root).ok()?;
    let path = layout.data_root.join("codex_subagent_starts.json");
    let analytics_session_id = event_session_id(&parsed);
    let session_id = analytics_session_id
        .clone()
        .unwrap_or_else(|| "unknown-codex-session".to_string());
    let mut counts = read_codex_subagent_start_counts(&path);
    let count = counts.entry(session_id).or_insert(0);
    *count = count.saturating_add(1);
    let next = *count;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&counts) {
        let _ = std::fs::write(path, format!("{json}\n"));
    }
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    record_hook_analytics(
        Some(&root),
        "codex_subagent_start",
        serde_json::json!({
            "agent": HintAgent::Codex.as_key(),
            "session_id": analytics_session_id.as_deref(),
            "agent_type": agent_type,
            "count": next,
        }),
    );
    Some(next)
}

pub fn codex_subagent_start_log_line(
    event_json: &str,
    count: Option<u64>,
    emitted_context: bool,
) -> String {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let agent_type = parsed
        .get("agent_type")
        .or_else(|| parsed.get("subagent_type"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let session_id = event_session_id(&parsed).unwrap_or_else(|| "unknown".to_string());
    let count = count.map_or_else(|| "#?".to_string(), |value| format!("#{value}"));
    format!(
        "tracedecay Codex SubagentStart {count}: session_id={session_id} agent_type={agent_type} additional_context={emitted_context}"
    )
}

fn read_codex_subagent_start_counts(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn codex_subagent_start_context(hint: Option<ToolHint>, no_history: bool) -> String {
    let mut context = String::new();
    if no_history {
        context.push_str("new/no-history subagent: recover only relevant project memory or prior-session context before assuming missing decisions.\n");
    }
    context.push_str(CODEX_SUBAGENT_START_CONTEXT);
    context.push('\n');
    if let Some(hint) = hint {
        context.push('\n');
        context.push_str(&format_tool_hint(&hint));
        context.push('\n');
    }
    context
}

fn codex_subagent_needs_context(parsed: &Value) -> bool {
    bool_field(
        parsed,
        &["is_new", "new_subagent", "fresh_subagent", "no_history"],
    ) == Some(true)
        || bool_field(
            parsed,
            &[
                "has_history",
                "history_included",
                "receives_history",
                "conversation_history_included",
            ],
        ) == Some(false)
        || text_field(
            parsed,
            &[
                "history_mode",
                "context_mode",
                "conversation_history",
                "source",
                "reason",
            ],
        )
        .is_some_and(|value| matches_no_history_marker(&value))
        || empty_array_field(parsed, &["history", "messages", "conversation"])
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

fn empty_array_field(value: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        value
            .get(*key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    })
}

fn matches_no_history_marker(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "new" | "fresh" | "none" | "empty" | "nohistory" | "withoutconversationhistory"
    )
}

/// Resolves the tracedecay project root for a Codex event from its `cwd`.
pub fn codex_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    codex_project_root_from_parsed_event(&parsed)
}

fn codex_project_root_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    let cwd = event_cwd_from_parsed(parsed)?;
    crate::config::discover_project_root(&cwd)
}

fn codex_workspace_status(root: Option<&Path>, cwd: Option<&Path>) -> HookWorkspaceStatus {
    if root.is_some() {
        return HookWorkspaceStatus::Initialized;
    }
    if cwd.is_some_and(is_project_like_workspace) {
        HookWorkspaceStatus::UnindexedProject
    } else {
        HookWorkspaceStatus::Generic
    }
}

pub fn codex_workspace_status_from_event(event_json: &str) -> HookWorkspaceStatus {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let root = codex_project_root_from_parsed_event(&parsed);
    let cwd = event_cwd_from_parsed(&parsed);
    codex_workspace_status(root.as_deref(), cwd.as_deref())
}

fn is_project_like_workspace(cwd: &Path) -> bool {
    nearest_project_like_root(cwd).is_some()
}

fn project_marker_exists(dir: &Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "deno.json",
        "tsconfig.json",
    ];
    MARKERS.iter().any(|marker| dir.join(marker).exists())
}

/// Extracts the project-relative paths touched by a Codex `apply_patch` command.
///
/// Codex sends the patch text as `tool_input.command`. The `apply_patch` envelope
/// names each file with `*** Add File:`, `*** Update File:`, `*** Delete File:`,
/// or `*** Move to:` lines. Patch paths are relative to the session `cwd`
/// (which may be a subdirectory of the discovered project root), so we resolve
/// each against `cwd` and then make it relative to `project_root`. Absolute
/// paths outside the root are skipped. The result feeds the daemon's targeted
/// single-file sync event.
pub fn codex_apply_patch_rel_paths(command: &str, cwd: &Path, project_root: &Path) -> Vec<String> {
    const PREFIXES: [&str; 4] = [
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    let mut rels: Vec<String> = Vec::new();
    for line in command.lines() {
        let line = line.trim();
        for prefix in PREFIXES {
            if let Some(rest) = line.strip_prefix(prefix) {
                let raw = rest.trim();
                if raw.is_empty() {
                    continue;
                }
                let candidate = Path::new(raw);
                let abs = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    cwd.join(candidate)
                };
                if let Some(rel) = rel_under_root(project_root, &abs) {
                    if !rels.contains(&rel) {
                        rels.push(rel);
                    }
                }
            }
        }
    }
    rels
}

fn is_codex_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "apply_patch" | "edit" | "write"
    )
}

fn is_codex_bash_tool(tool_name: &str) -> bool {
    matches!(tool_name.to_ascii_lowercase().as_str(), "bash" | "shell")
}

async fn codex_post_tool_use(event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let tool_name = parsed
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let command = parsed
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(cwd) = event_cwd(event_json) else {
        return;
    };
    let Some(root) = crate::config::discover_project_root(&cwd)
        .or_else(|| crate::worktree::git_worktree_root(&cwd))
    else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }

    if is_codex_edit_tool(tool_name) {
        let rels = codex_apply_patch_rel_paths(command, &cwd, &root);
        if rels.is_empty() {
            return;
        }
        crate::daemon::notify_hook_event(
            &root,
            crate::daemon::DaemonHookEvent::codex_post_tool_use_edit(rels, cwd),
        )
        .await;
    } else if is_codex_bash_tool(tool_name) {
        crate::daemon::notify_hook_event(
            &root,
            crate::daemon::DaemonHookEvent::codex_post_tool_use_shell(command.to_string(), cwd),
        )
        .await;
    }
}

const CODEX_POST_COMPACT_BUDGET: Duration = Duration::from_secs(115);

async fn codex_post_compact(event_json: &str) {
    let work = async {
        let Some(project_root) = codex_project_root_from_event(event_json) else {
            return;
        };
        if !crate::tracedecay::TraceDecay::has_initialized_store(&project_root).await {
            return;
        }
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return;
        };
        if let Some(source) = crate::sessions::codex::CodexSource::new() {
            let _ = crate::sessions::source::ingest_source(&db, &source, &project_root, None).await;
        }
        let session_id = serde_json::from_str::<Value>(event_json)
            .ok()
            .and_then(|parsed| event_session_id(&parsed));
        let Ok(mut pending) = db
            .pending_codex_compaction_summary_requests(session_id.as_deref(), 1)
            .await
        else {
            return;
        };
        let Some(pending) = pending.pop() else {
            return;
        };
        let config = crate::sessions::codex_app_server::CodexAppServerSummaryConfig::from_env();
        let summary = match crate::sessions::codex_app_server::summarize_with_codex_app_server(
            &pending.request,
            &config,
        ) {
            Ok(summary) => summary,
            Err(err) => {
                eprintln!("tracedecay Codex PostCompact summary failed: {err}");
                return;
            }
        };
        if let Err(err) = db
            .replace_codex_compaction_summary(
                &pending.node_id,
                &summary.text,
                "codex_app_server",
                summary.model.as_deref().or(config.model.as_deref()),
            )
            .await
        {
            eprintln!("tracedecay Codex PostCompact summary replacement failed: {err}");
        }
    };
    let _ = tokio::time::timeout(CODEX_POST_COMPACT_BUDGET, work).await;
}

async fn reset_counter_for_codex_event(event_json: &str) {
    let Some(project_root) = codex_project_root_from_event(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Pure decision logic for Kiro `preToolUse` hook events.
///
/// Returns a block reason only for Kiro delegation/subagent tool calls whose
/// task text looks like codebase research that tracedecay MCP tools should
/// answer first.
pub fn evaluate_kiro_pre_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
    if !is_kiro_delegation_tool(tool_name) {
        return None;
    }

    let tool_input = parsed.get("tool_input").unwrap_or(&Value::Null);
    if let Some(prompt) = kiro_event_text(tool_input).filter(|text| is_code_research_prompt(text)) {
        let hint = decide_hint(&ToolHintInput {
            agent: HintAgent::Kiro,
            session_id: event_session_id(&parsed),
            tool_name: Some(tool_name.to_string()),
            command: None,
            prompt: Some(prompt),
            subagent_type: Some(tool_name.to_string()),
            file_path: None,
            hints_enabled: true,
        });
        Some(research_block_reason(hint))
    } else {
        None
    }
}

fn is_kiro_delegation_tool(tool_name: &str) -> bool {
    matches!(tool_name, "delegate" | "subagent" | "use_subagent")
}

fn kiro_event_text(value: &Value) -> Option<String> {
    let mut text = Vec::new();
    collect_kiro_task_strings(value, &mut text);
    if text.is_empty() {
        collect_strings(value, &mut text);
    }
    (!text.is_empty()).then(|| text.join("\n"))
}

fn collect_kiro_task_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let key = key.to_ascii_lowercase();
                if key.contains("prompt")
                    || key.contains("task")
                    || key.contains("query")
                    || key.contains("instruction")
                    || key.contains("message")
                    || key.contains("description")
                {
                    collect_strings(child, out);
                } else {
                    collect_kiro_task_strings(child, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_kiro_task_strings(item, out);
            }
        }
        Value::String(s) => out.push(s),
        _ => {}
    }
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                collect_strings(child, out);
            }
        }
        _ => {}
    }
}

/// `UserPromptSubmit` hook handler: resets the per-session local counter.
///
/// Token savings are now reported inline in each MCP tool response,
/// so this hook only needs to reset the counter for the new turn.
pub async fn hook_prompt_submit() {
    let project_path = crate::config::resolve_path(None);
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_path).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Kiro `userPromptSubmit` hook handler.
///
/// Kiro adds successful hook stdout to context, so this handler stays silent.
/// Resets the per-turn counter and runs a bounded catch-up ingest of Kiro IDE
/// transcripts for the resolved workspace.
pub async fn hook_kiro_prompt_submit() -> i32 {
    let event = read_hook_event!();
    record_hook_invoked(None, HintAgent::Kiro, "userPromptSubmit", &event);
    reset_counter_for_kiro_event(&event).await;
    ingest_kiro_transcript_for_event(
        &event,
        Some(KIRO_HOT_INGEST_MAX_BYTES),
        KIRO_HOT_INGEST_BUDGET,
    )
    .await;
    0
}

/// Kiro `postToolUse` hook handler used to keep the graph fresh after writes.
///
/// The installed Kiro agent maps this to `fs_write`. The hook discovers the
/// nearest tracedecay project from Kiro's `cwd` field and notifies the daemon,
/// which owns silent incremental sync scheduling. Missing daemon/index state is
/// fail-open.
pub async fn hook_kiro_post_tool_use() -> i32 {
    let event = read_hook_event!();
    record_hook_invoked(None, HintAgent::Kiro, "postToolUse", &event);
    notify_kiro_post_tool_use(&event).await;
    0
}

async fn reset_counter_for_kiro_event(event_json: &str) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Largest transcript tail the Kiro `userPromptSubmit` hook will read per call.
const KIRO_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Wall-clock budget for the Kiro prompt-submit catch-up ingest.
const KIRO_HOT_INGEST_BUDGET: std::time::Duration = std::time::Duration::from_millis(1_500);

/// Incrementally ingests Kiro IDE transcripts for the workspace referenced by
/// `event_json`. Always fails open.
async fn ingest_kiro_transcript_for_event(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: std::time::Duration,
) {
    let work = async {
        let Some(project_root) = kiro_project_root(event_json) else {
            return;
        };
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return;
        };
        let _ =
            crate::sessions::kiro::ingest_kiro_for_project(&db, &project_root, max_new_bytes).await;
    };
    let _ = tokio::time::timeout(budget, work).await;
}

async fn reset_counter_for_cursor_event(event_json: &str) {
    let Some(project_root) = cursor_project_root_from_event(event_json) else {
        return;
    };
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_root).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// Largest tail the `beforeSubmitPrompt` hot path will read in one call. Larger
/// backlogs are left for the `sessionStart` / `stop` catch-up ingests.
const CURSOR_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Largest transcript tail a low-priority Cursor catch-up hook will read.
/// Oversized backlogs stay queued instead of blocking hook execution. Public
/// so ingest-health reporting (`tracedecay_status`, doctor) can flag a backlog
/// the hooks will never drain on their own.
pub const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Hard wall-clock budget for the `beforeSubmitPrompt` tail ingest. Well under
/// Cursor's 5s hook timeout; on expiry we fail open and let heavier hooks catch up.
const CURSOR_HOT_INGEST_BUDGET: Duration = Duration::from_millis(1_500);
/// Budget for the `sessionStart` catch-up ingest (registered with a 5s timeout).
const CURSOR_SESSION_INGEST_BUDGET: Duration = Duration::from_secs(4);
/// Budget for the end-of-turn `stop` catch-up ingest (registered with a 30s timeout).
const CURSOR_STOP_INGEST_BUDGET: Duration = Duration::from_secs(25);
/// Budget for the transcript catch-up portion of the `preCompact` hook.
const CURSOR_PRE_COMPACT_INGEST_BUDGET: Duration = Duration::from_secs(30);
/// Budget for the auxiliary `cursor-agent` summary call inside the hook. Kept
/// below the registered Cursor hook timeout so the child can be killed/reaped
/// by `TraceDecay` rather than by Cursor killing the hook process. Sized so
/// the ingest budget plus this cap stay below the overall preCompact budget,
/// leaving slack for LCM prepare/persist and process overhead.
const CURSOR_PRE_COMPACT_SUMMARY_BUDGET: Duration = Duration::from_secs(75);
/// Overall budget for the `preCompact` hook (registered with a 120s timeout).
const CURSOR_PRE_COMPACT_BUDGET: Duration = Duration::from_secs(115);
const COMPACTION_CONTEXT_RECOVERY_HINT: &str = "Context was just compacted. If important prior-session context seems missing, query TraceDecay session context before assuming the compacted summary is complete. Start with `tracedecay_message_search` or `tracedecay_lcm_expand_query`; use `tracedecay_lcm_describe` and `tracedecay_lcm_expand` when you need the summary DAG sources.";

fn cursor_pre_compact_lcm_request(
    session_id: &str,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
    max_source_messages: Option<usize>,
    fresh_tail_count: Option<usize>,
    summarizer: crate::sessions::lcm::LcmSummarizerMode,
    expected_current_frontier_store_id: Option<i64>,
) -> crate::sessions::lcm::LcmCompressionRequest {
    crate::sessions::lcm::LcmCompressionRequest {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens,
        focus_topic: Some("Cursor context compaction".to_string()),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length,
        reserve_tokens_floor: None,
        summarizer,
    }
}

fn cursor_pre_compact_fresh_tail_count(
    parsed: &Value,
    messages_to_compact: Option<usize>,
) -> Option<usize> {
    let message_count = event_usize(parsed, &["message_count", "messages_count"])?;
    let messages_to_compact = messages_to_compact?;
    Some(message_count.saturating_sub(messages_to_compact))
}

fn event_i64(parsed: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = parsed.get(*key)?;
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str()?.parse::<i64>().ok())
    })
}

fn event_usize(parsed: &Value, keys: &[&str]) -> Option<usize> {
    event_i64(parsed, keys).and_then(|value| usize::try_from(value).ok())
}

fn cursor_event_transcript_path_exists(parsed: &Value) -> bool {
    parsed
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .is_some_and(|path| Path::new(path).exists())
}

/// Incrementally ingests the Cursor transcript referenced by `event_json` into
/// the resolved project session DB, bounded by `max_new_bytes` (the hot-path cap)
/// and an overall `budget`. Always fails open: a timeout, missing transcript, or
/// any error is swallowed so the calling hook never blocks the agent.
async fn ingest_cursor_transcript_for_event(
    event_json: &str,
    max_new_bytes: Option<u64>,
    budget: Duration,
) -> bool {
    let work = async {
        let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
            return false;
        };
        let Some(project_root) = cursor_project_root_from_parsed_event(&parsed) else {
            return false;
        };
        if let Some(cwd_root) = cursor_event_cwd(&parsed)
            .as_deref()
            .and_then(crate::config::discover_project_root)
        {
            if !paths_same(&cwd_root, &project_root) {
                return false;
            }
        }
        let Some(db) = crate::sessions::cursor::open_project_session_db(&project_root).await else {
            return false;
        };
        let _ = crate::sessions::cursor::ingest_cursor_transcript_event_capped(
            event_json,
            &db,
            max_new_bytes,
        )
        .await;
        true
    };
    // Short-lived CLI hook processes exit immediately, so the ingest must run
    // inline (not on a detached task); the timeout keeps it inside budget.
    tokio::time::timeout(budget, work).await.unwrap_or(false)
}

async fn notify_kiro_post_tool_use(event_json: &str) {
    let Some(project_root) = kiro_project_root(event_json) else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&project_root).await {
        return;
    }
    let rel_paths = kiro_post_tool_use_rel_paths(event_json, &project_root);
    crate::daemon::notify_hook_event(
        &project_root,
        crate::daemon::DaemonHookEvent::kiro_post_tool_use(rel_paths, event_cwd(event_json)),
    )
    .await;
}

pub fn kiro_post_tool_use_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };
    let cwd = event_cwd_from_parsed(&parsed).unwrap_or_else(|| project_root.to_path_buf());
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("input"))
        .unwrap_or(&Value::Null);

    let mut paths = Vec::new();
    collect_event_path_fields(&parsed, &mut paths);
    collect_event_path_fields(tool_input, &mut paths);

    let mut rels = Vec::new();
    for path in paths {
        let path = Path::new(&path);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if let Some(rel) = rel_under_root(project_root, &abs) {
            if !rels.contains(&rel) {
                rels.push(rel);
            }
        }
    }
    rels
}

fn collect_event_path_fields(value: &Value, out: &mut Vec<String>) {
    for key in ["file_path", "filePath", "path", "target_file", "targetFile"] {
        match value.get(key) {
            Some(Value::String(path)) if !path.is_empty() => out.push(path.clone()),
            Some(Value::Array(paths)) => {
                out.extend(
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .map(str::to_string),
                );
            }
            _ => {}
        }
    }
}

fn cursor_tool_hint_input(parsed: &Value) -> ToolHintInput {
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("input"))
        .unwrap_or(&Value::Null);
    ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(parsed),
        tool_name: text_field(parsed, &["tool_name", "toolName", "name"]),
        command: text_field(tool_input, &["command", "cmd"])
            .or_else(|| text_field(parsed, &["command", "cmd"])),
        prompt: text_field(
            tool_input,
            &["prompt", "query", "pattern", "task", "description"],
        )
        .or_else(|| {
            text_field(
                parsed,
                &["prompt", "query", "pattern", "task", "description"],
            )
        }),
        subagent_type: text_field(parsed, &["subagent_type", "subagentType", "agent_type"]),
        file_path: text_field(tool_input, &["file_path", "filePath", "path"])
            .or_else(|| text_field(parsed, &["file_path", "filePath", "path"])),
        hints_enabled: true,
    }
}

fn codex_prompt_hint(event_json: &str) -> Option<ToolHint> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    let hint = decide_hint(&ToolHintInput {
        agent: HintAgent::Codex,
        session_id: event_session_id(&parsed),
        tool_name: None,
        command: None,
        prompt: prompt_like_text(&parsed),
        subagent_type: None,
        file_path: None,
        hints_enabled: true,
    })?;
    let root = codex_project_root_from_event(event_json);
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Codex,
        event_session_id(&parsed).as_deref(),
        &hint,
    );
    deduped_codex_hint(event_json, &parsed, hint)
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn prompt_like_text(parsed: &Value) -> Option<String> {
    [
        "prompt",
        "user_prompt",
        "message",
        "input",
        "task",
        "description",
    ]
    .iter()
    .find_map(|key| parsed.get(*key).and_then(Value::as_str))
    .filter(|text| !text.is_empty())
    .map(str::to_string)
}

fn event_session_id(parsed: &Value) -> Option<String> {
    ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn format_tool_hint(hint: &ToolHint) -> String {
    format!("tracedecay hint: {}\n{}", hint.message, hint.context)
}

fn append_tool_hint(context: &mut String, hint: &ToolHint) {
    if !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(&format_tool_hint(hint));
    context.push('\n');
}

fn kiro_project_root(event_json: &str) -> Option<PathBuf> {
    let cwd = event_cwd(event_json).or_else(|| std::env::current_dir().ok())?;
    crate::config::discover_project_root(&cwd)
}

/// Reads the `cwd` string field from a hook event JSON payload. Shared by the
/// Kiro and Codex handlers, both of which send the session working directory.
fn event_cwd(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    event_cwd_from_parsed(&parsed)
}

fn event_cwd_from_parsed(parsed: &Value) -> Option<PathBuf> {
    let cwd = parsed.get("cwd").and_then(Value::as_str)?;
    let path = Path::new(cwd);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path.to_path_buf())
    }
}

fn read_stdin_to_string() -> std::io::Result<String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    Ok(input)
}

/// `Stop` hook handler: ingests new session data and prints a cost receipt.
///
/// Parses any new JSONL lines from Claude Code sessions, inserts them into
/// the global DB, and prints a one-line summary to stderr showing the
/// session cost, tokens saved, and efficiency ratio.
pub async fn hook_stop() {
    let Some(gdb) = crate::global_db::GlobalDb::open().await else {
        return;
    };

    let stats = crate::accounting::parser::ingest(&gdb).await;
    if stats.turns_inserted == 0 {
        return;
    }

    // Read tokens saved for efficiency calculation
    let project_path = crate::config::resolve_path(None);
    let tokens_saved = if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_path).await {
        cg.get_tokens_saved().await.unwrap_or(0)
    } else {
        0
    };

    let efficiency = if tokens_saved + stats.tokens_consumed > 0 {
        (tokens_saved as f64 / (tokens_saved + stats.tokens_consumed) as f64) * 100.0
    } else {
        0.0
    };

    let saved_str = crate::display::format_token_count(tokens_saved);

    // Print to stderr so it appears in the terminal but doesn't interfere
    // with stdout (which Claude Code may parse).
    if stats.cost_usd >= 0.001 {
        eprintln!(
            "\x1b[36mSession: ${:.2} spent | {saved_str} saved | {efficiency:.0}% efficiency\x1b[0m",
            stats.cost_usd
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::USER_DATA_DIR_ENV;
    use std::sync::{Mutex, OnceLock};

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn shell_words_preserves_unquoted_windows_paths() {
        assert_eq!(
            shell_words_for_platform(r"git --work-tree=C:\Users\me\repo pull", true),
            vec!["git", r"--work-tree=C:\Users\me\repo", "pull"]
        );
        assert_eq!(
            shell_words_for_platform(r"git --work-tree=C:\Users\me\repo pull", false),
            vec!["git", r"--work-tree=C:Usersmerepo", "pull"]
        );
    }

    #[test]
    fn git_work_tree_overrides_prior_c_directory() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("repo");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let command = format!(
            "git -C {} --git-dir={}/.git --work-tree={} pull",
            outside.display(),
            project.display(),
            project.display()
        );

        assert!(cursor_shell_command_targets_project(
            &command, &outside, &project
        ));
    }

    #[test]
    fn codex_prompt_hints_dedupe_by_session_and_category() {
        let _lock = env_lock().lock().unwrap();
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_hook_codex_prompt".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let layout = crate::storage::resolve_layout_for_current_profile(&project_root).unwrap();
        std::fs::create_dir_all(&layout.data_root).unwrap();
        let event = serde_json::json!({
            "session_id": "codex-session-1",
            "cwd": project_root,
            "prompt": "Please explain the impact of changing parse_user"
        })
        .to_string();

        let first = codex_prompt_hint(&event).unwrap();
        assert_eq!(first.category, tool_hints::HintCategory::Impact);

        assert!(
            codex_prompt_hint(&event).is_none(),
            "Codex should use shared per-session hint dedupe for prompt hints"
        );
    }

    #[test]
    fn compact_session_start_events_get_recovery_hint() {
        let event = serde_json::json!({ "source": "compact" }).to_string();
        assert!(session_start_from_compaction(&event));

        let mut context = build_codex_session_context(true, None);
        append_context_recovery_hint(&mut context);
        assert!(context.contains("Context was just compacted"));
        assert!(context.contains("tracedecay_lcm_expand_query"));
        assert!(context.contains("tracedecay_lcm_describe"));
    }

    #[test]
    fn non_compact_session_start_events_do_not_get_recovery_hint() {
        let event = serde_json::json!({ "source": "resume" }).to_string();
        assert!(!session_start_from_compaction(&event));
    }

    #[test]
    fn claude_edit_tools_are_recognized_case_insensitively() {
        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit", "write"] {
            assert!(is_claude_edit_tool(tool), "{tool} should count as an edit");
        }
        assert!(!is_claude_edit_tool("Bash"));
        assert!(!is_claude_edit_tool("Read"));
        assert!(is_claude_bash_tool("Bash"));
        assert!(!is_claude_bash_tool("Edit"));
    }

    #[test]
    fn claude_edit_rel_paths_resolves_file_path_against_project_root() {
        let root = Path::new("/repo");
        let cwd = Path::new("/repo/sub");
        let event = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/lib.rs" }
        });
        assert_eq!(
            claude_edit_rel_paths(&event, cwd, root),
            vec!["src/lib.rs".to_string()]
        );

        // Relative paths resolve against the session cwd.
        let event = serde_json::json!({
            "tool_name": "Write",
            "tool_input": { "file_path": "module.rs" }
        });
        assert_eq!(
            claude_edit_rel_paths(&event, cwd, root),
            vec!["sub/module.rs".to_string()]
        );

        // NotebookEdit uses notebook_path.
        let event = serde_json::json!({
            "tool_name": "NotebookEdit",
            "tool_input": { "notebook_path": "/repo/analysis.ipynb" }
        });
        assert_eq!(
            claude_edit_rel_paths(&event, cwd, root),
            vec!["analysis.ipynb".to_string()]
        );

        // Paths outside the project root are skipped.
        let event = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/elsewhere/other.rs" }
        });
        assert!(claude_edit_rel_paths(&event, cwd, root).is_empty());
    }

    #[test]
    fn index_status_line_formats_freshness_and_init_nudge() {
        assert_eq!(
            index_status_line(true, Some("last indexed 5m ago")),
            "tracedecay index status: last indexed 5m ago.\n"
        );
        assert_eq!(
            index_status_line(true, None),
            "tracedecay index status: initialized.\n"
        );
        assert!(index_status_line(false, None).contains("run `tracedecay init`"));
    }
}
