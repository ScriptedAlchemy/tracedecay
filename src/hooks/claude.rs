//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape, while older Claude
//! handlers keep their legacy input/output contracts.

use serde_json::Value;

use super::codex::{
    codex_additional_context_json, codex_project_root_from_event,
    codex_project_root_from_parsed_event,
};
use super::post_tool_use::{notify_post_tool_use, CLAUDE_POST_TOOL_USE_SPEC};
use super::steering::{
    append_context_recovery_hint, cursor_index_signals_for_root, index_status_line,
    session_start_from_compaction,
};
use super::tool_hints::{decide_hint, HintAgent, ToolHintInput};
use super::{
    event_cwd_from_parsed, event_session_id, is_project_like_workspace, prompt_like_text,
    read_hook_event, record_hook_invoked, research_block_reason,
};

/// `PreToolUse` hook handler for Claude Code's Agent tool matcher.
///
/// Blocks Explore agents and exploration-style prompts, directing Claude to
/// use tracedecay MCP tools instead.
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
/// Returns the JSON decision for Claude to print to stdout.
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

    if parsed.get("subagent_type").and_then(|v| v.as_str()) == Some("Explore") {
        return block_msg().to_string();
    }

    if let Some(prompt) = parsed.get("prompt").and_then(|v| v.as_str()) {
        if is_code_research_prompt(prompt) {
            return block_msg().to_string();
        }
    }

    String::new()
}

pub(super) fn is_code_research_prompt(prompt: &str) -> bool {
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

/// Claude Code `SessionStart` hook handler (fail-open).
///
/// Emits session-specific index freshness and compaction recovery context.
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

/// Builds the lean Claude `SessionStart` context for code workspaces.
async fn claude_session_context_for_event(event_json: &str) -> String {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
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
/// Notifies the daemon, which owns targeted sync, branch tracking, and
/// coalescing. Fail-open and silent.
pub async fn hook_claude_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Claude, "PostToolUse", &event);
    notify_post_tool_use(&CLAUDE_POST_TOOL_USE_SPEC, &event).await;
    0
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

/// `Stop` hook handler: ingests new session data and prints a cost receipt.
///
/// Ingests new Claude Code session lines and prints a one-line cost receipt.
pub async fn hook_stop() {
    let Some(gdb) = crate::global_db::GlobalDb::open().await else {
        return;
    };

    let stats = crate::accounting::parser::ingest(&gdb).await;
    if stats.turns_inserted == 0 {
        return;
    }

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

    if stats.cost_usd >= 0.001 {
        eprintln!(
            "\x1b[36mSession: ${:.2} spent | {saved_str} saved | {efficiency:.0}% efficiency\x1b[0m",
            stats.cost_usd
        );
    }
}
