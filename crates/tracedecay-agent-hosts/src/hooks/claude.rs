//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use crate::ports::hook_runtime::HookRuntimeV1;

use super::post_tool_use::is_post_tool_use_failure_event;
use super::tool_hints::{HintAgent, ToolHintInput, decide_hint};
use super::{
    additional_context_json, compact_daemon_args, event_project_root_with_identity,
    event_session_id, prompt_like_text, read_hook_event, record_hook_invoked_parsed,
    research_block_reason,
};

/// Pure decision logic for the `PreToolUse` hook.
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
        captured_output: None,
        trusted_failure: false,
        edit_text: None,
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

    if let Some(prompt) = parsed.get("prompt").and_then(|v| v.as_str())
        && is_code_research_prompt(prompt)
    {
        return block_msg().to_string();
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

/// Claude Code `SessionStart` hook handler.
#[hotpath::measure(future = true, label = "hosts.hooks.claude.session_start")]
pub async fn hook_claude_session_start(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!();
    let (root, output) = claude_session_start_response(runtime, &event, started).await;
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        &output,
    )
    .await
    {
        return 1;
    }
    0
}

/// Returns the identity-resolved root alongside the response so the handler
/// does not repeat the registry-probing resolution for output delivery.
async fn claude_session_start_response(
    runtime: &HookRuntimeV1,
    event: &str,
    started: Instant,
) -> (Option<PathBuf>, String) {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    // Resolve the project root the same identity-aware way the printed context
    // does, including global-only stores and fresh harness-created worktrees.
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HintAgent::Claude,
        "SessionStart",
        event,
        &parsed,
    );
    let output = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_hooks::HookHostV1::ClaudeCode,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| additional_context_json("SessionStart", &guidance),
    );
    (root, output)
}

/// Claude Code `PostCompact` hook handler.
///
/// Claude does not currently expose machine-verifiable provenance for the
/// compacted source frontier. The daemon therefore treats this event as a
/// read-only capability probe and returns typed unavailable without publishing
/// transcript or summary state.
#[hotpath::measure(future = true, label = "hosts.hooks.claude.post_compact")]
pub async fn hook_claude_post_compact(runtime: &HookRuntimeV1) -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HintAgent::Claude,
        "PostCompact",
        &event,
        &parsed,
    );
    let args = compact_daemon_args("claude_compact", "claude", root.is_none(), &event, None);
    if let Err(error) =
        super::daemon_hook_action(runtime, root.as_deref(), args, Some(&hook_telemetry)).await
    {
        tracing::warn!(%error, "Claude PostCompact daemon call failed");
    }
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        &serde_json::json!({}).to_string(),
    )
    .await
    {
        return 1;
    }
    0
}

/// Claude Code `PostToolUse` / `PostToolUseFailure` hook handler.
#[hotpath::measure(future = true, label = "hosts.hooks.claude.post_tool_use")]
pub async fn hook_claude_post_tool_use(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!();
    let (root, response) = claude_post_tool_use_response(runtime, &event, started).await;
    if let Some(response) = response
        && !super::write_hook_output(
            root.as_deref(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
            &event,
            &response,
        )
        .await
    {
        return 1;
    }
    0
}

/// Returns the identity-resolved root alongside the response so the handler
/// does not repeat the registry-probing resolution for output delivery.
async fn claude_post_tool_use_response(
    runtime: &HookRuntimeV1,
    event: &str,
    started: Instant,
) -> (Option<PathBuf>, Option<String>) {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let hook_event_name = if is_post_tool_use_failure_event(&parsed) {
        "PostToolUseFailure"
    } else {
        "PostToolUse"
    };
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HintAgent::Claude,
        hook_event_name,
        event,
        &parsed,
    );
    let response = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_hooks::HookHostV1::ClaudeCode,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map(|guidance| additional_context_json(hook_event_name, &guidance));
    (root, response)
}

/// `Stop` hook handler: submits the native turn boundary to the daemon.
#[hotpath::measure(future = true, label = "hosts.hooks.claude.stop")]
pub async fn hook_stop(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = match super::read_stdin_bounded() {
        Ok(super::HookStdinRead::Event(event)) => event,
        Ok(super::HookStdinRead::Oversized) => {
            eprintln!(
                "tracedecay hook: stdin exceeds wire message bound ({})",
                tracedecay_framing::WIRE_RECORD_TOO_LARGE
            );
            return 1;
        }
        Err(_) => String::new(),
    };
    let (root, output) = claude_stop_response_for_event(runtime, &event, started).await;
    if !super::write_hook_output(
        root.as_deref(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        &output,
    )
    .await
    {
        return 1;
    }
    0
}

/// Returns the identity-resolved root alongside the response so the handler
/// does not repeat the registry-probing resolution for output delivery.
async fn claude_stop_response_for_event(
    runtime: &HookRuntimeV1,
    event: &str,
    started: Instant,
) -> (Option<PathBuf>, String) {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HintAgent::Claude,
        "Stop",
        event,
        &parsed,
    );
    let output = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_hooks::HookHostV1::ClaudeCode,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| additional_context_json("Stop", &guidance),
    );
    (root, output)
}
