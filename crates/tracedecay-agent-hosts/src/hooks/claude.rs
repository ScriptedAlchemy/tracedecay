//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape.

use std::path::PathBuf;

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
    let event = read_hook_event!();
    let (root, output) = claude_session_start_response(runtime, &event).await;
    if !super::write_hook_output(
        runtime,
        root.as_deref(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        &output,
        None,
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
        runtime,
        root.as_deref(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        &serde_json::json!({}).to_string(),
        Some(&hook_telemetry),
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
    let event = read_hook_event!();
    let (root, response) = claude_post_tool_use_response(runtime, &event).await;
    if let Some(response) = response
        && !super::write_hook_output(
            runtime,
            root.as_deref(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
            &event,
            &response,
            None,
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
    let (root, output) = claude_stop_response_for_event(runtime, &event).await;
    if !super::write_hook_output(
        runtime,
        root.as_deref(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        &output,
        None,
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::format_tool_hint;
    use super::super::post_tool_use::{
        captured_tool_output, tool_input_command_str, trusted_tool_failure,
    };
    use super::super::tool_hints::{ToolHint, is_harness_memory_path};
    use super::*;

    fn decide_post_tool_use_hint(parsed: &Value) -> Option<ToolHint> {
        let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
        let null = Value::Null;
        let tool_input = parsed.get("tool_input").unwrap_or(&null);
        let file_path = tool_input
            .get("file_path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned);
        let is_edit = ["Edit", "MultiEdit", "Write", "NotebookEdit"]
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case(tool_name));
        let is_memory_edit = is_edit && file_path.as_deref().is_some_and(is_harness_memory_path);
        let edit_text = (is_edit && !is_memory_edit)
            .then(|| {
                ["content", "new_string", "new_source"]
                    .iter()
                    .find_map(|key| tool_input.get(*key).and_then(Value::as_str))
                    .map(str::to_owned)
                    .or_else(|| {
                        tool_input
                            .get("edits")
                            .and_then(Value::as_array)
                            .map(|edits| {
                                edits
                                    .iter()
                                    .filter_map(|edit| {
                                        edit.get("new_string").and_then(Value::as_str)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .filter(|text| !text.is_empty())
                    })
            })
            .flatten();
        let is_hint_tool = ["Grep", "Glob", "Read"]
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case(tool_name));
        let is_shell = tool_name.eq_ignore_ascii_case("Bash");
        if !is_hint_tool && !is_shell && !is_memory_edit && edit_text.is_none() {
            return None;
        }
        decide_hint(&ToolHintInput {
            agent: HintAgent::Claude,
            session_id: event_session_id(parsed),
            tool_name: Some(tool_name.to_owned()),
            command: tool_input_command_str(parsed),
            prompt: None,
            subagent_type: None,
            file_path,
            captured_output: captured_tool_output(parsed),
            trusted_failure: trusted_tool_failure(parsed),
            edit_text,
            hints_enabled: true,
        })
    }

    fn post_event(tool_name: &str, tool_input: &Value) -> Value {
        serde_json::json!({
            "session_id": "s-post",
            "tool_name": tool_name,
            "tool_input": tool_input,
        })
    }

    #[test]
    fn grep_post_tool_use_event_decides_a_search_hint() {
        let event = post_event("Grep", &serde_json::json!({ "pattern": "needle" }));
        let hint = decide_post_tool_use_hint(&event).expect("Grep must produce a soft search hint");
        let context = format_tool_hint(&hint);
        assert!(
            context.contains("tracedecay_search") || context.contains("tracedecay_context"),
            "hint should point at a tracedecay search tool: {context}"
        );

        let json: Value =
            serde_json::from_str(&additional_context_json("PostToolUse", &context)).unwrap();
        assert_eq!(
            json["hookSpecificOutput"]["hookEventName"].as_str(),
            Some("PostToolUse")
        );
        assert_eq!(
            json["hookSpecificOutput"]["additionalContext"].as_str(),
            Some(context.as_str())
        );
    }

    #[test]
    fn read_and_glob_post_tool_use_events_decide_hints() {
        let read_event = post_event("Read", &serde_json::json!({ "file_path": "src/lib.rs" }));
        let read_hint = decide_post_tool_use_hint(&read_event)
            .expect("Read of a single file must produce an outline hint");
        assert!(format_tool_hint(&read_hint).contains("tracedecay_outline"));

        let glob_event = post_event("Glob", &serde_json::json!({ "pattern": "**/*.rs" }));
        let glob_hint =
            decide_post_tool_use_hint(&glob_event).expect("Glob must produce a file-lookup hint");
        assert!(format_tool_hint(&glob_hint).contains("tracedecay_files"));
    }

    #[test]
    fn recursive_bash_search_decides_a_hint_but_plain_bash_does_not() {
        let grep_bash = post_event(
            "Bash",
            &serde_json::json!({ "command": "grep -r foo src/" }),
        );
        let hint = decide_post_tool_use_hint(&grep_bash)
            .expect("recursive grep in Bash must produce a search hint");
        let context = format_tool_hint(&hint);
        assert!(context.contains("tracedecay_search") || context.contains("tracedecay_context"));

        let plain_bash = post_event("Bash", &serde_json::json!({ "command": "ls -la" }));
        assert!(decide_post_tool_use_hint(&plain_bash).is_none());
    }

    #[test]
    fn build_bash_command_without_failure_signal_stays_silent() {
        for command in ["cargo check", "cargo clippy", "tsc --noEmit", "pyright"] {
            let event = post_event("Bash", &serde_json::json!({ "command": command }));
            assert!(
                decide_post_tool_use_hint(&event).is_none(),
                "{command} has no failure signal and must stay silent"
            );
        }
    }

    #[test]
    fn trusted_build_failure_event_decides_a_diagnostics_hint() {
        let mut event = post_event(
            "Bash",
            &serde_json::json!({ "command": "cargo check --workspace" }),
        );
        event["hook_event_name"] = Value::String("PostToolUseFailure".to_string());
        event["error"] = Value::String("Command exited with non-zero status code 101".to_string());
        event["is_interrupt"] = Value::Bool(false);

        let hint = decide_post_tool_use_hint(&event)
            .expect("a host-authenticated compiler failure must produce a diagnostics hint");
        assert_eq!(hint.category.as_key(), "build_diagnostics");
    }

    #[test]
    fn captured_compiler_output_decides_a_diagnostics_hint() {
        let mut event = post_event(
            "Bash",
            &serde_json::json!({ "command": "cargo check --workspace" }),
        );
        event["hook_event_name"] = Value::String("PostToolUse".to_string());
        event["tool_response"] = serde_json::json!({
            "stdout": "",
            "stderr": "error[E0308]: mismatched types\n --> src/lib.rs:42:5"
        });

        let hint = decide_post_tool_use_hint(&event)
            .expect("captured compiler output must produce a diagnostics hint");
        assert_eq!(hint.category.as_key(), "build_diagnostics");
    }

    #[test]
    fn untrusted_or_behavioral_failure_shapes_stay_silent() {
        let spoofed_command = post_event(
            "Bash",
            &serde_json::json!({
                "command": "printf 'error[E0308]: mismatched types\\n --> src/lib.rs:42:5'"
            }),
        );
        assert!(decide_post_tool_use_hint(&spoofed_command).is_none());

        let mut interrupted = post_event(
            "Bash",
            &serde_json::json!({ "command": "cargo check --workspace" }),
        );
        interrupted["hook_event_name"] = Value::String("PostToolUseFailure".to_string());
        interrupted["error"] = Value::String("Command interrupted".to_string());
        interrupted["is_interrupt"] = Value::Bool(true);
        assert!(decide_post_tool_use_hint(&interrupted).is_none());

        let mut tests_failed = post_event(
            "Bash",
            &serde_json::json!({ "command": "cargo test hooks::tool_hints" }),
        );
        tests_failed["hook_event_name"] = Value::String("PostToolUseFailure".to_string());
        tests_failed["error"] =
            Value::String("Command exited with non-zero status code 101".to_string());
        assert!(decide_post_tool_use_hint(&tests_failed).is_none());
    }

    #[test]
    fn edit_and_write_post_tool_use_events_get_no_hint_for_source_files() {
        for tool in ["Edit", "MultiEdit", "Write", "NotebookEdit"] {
            let event = post_event(tool, &serde_json::json!({ "file_path": "src/lib.rs" }));
            assert!(
                decide_post_tool_use_hint(&event).is_none(),
                "{tool} on a source file drives daemon sync only and must not emit a hint"
            );
        }
    }

    #[test]
    fn write_adding_a_function_body_decides_an_edit_redundancy_hint() {
        let content = "fn summarize(items: &[Item]) -> u64 {\n    let mut total = 0;\n    for item in items {\n        if item.active {\n            total += item.count;\n        }\n    }\n    total\n}\n";
        let event = post_event(
            "Write",
            &serde_json::json!({ "file_path": "src/widgets.rs", "content": content }),
        );
        let hint = decide_post_tool_use_hint(&event)
            .expect("a new function-sized Write must produce an edit-redundancy hint");
        let context = format_tool_hint(&hint);
        assert!(
            context.contains("tracedecay_redundancy"),
            "edit-redundancy hint must point at tracedecay_redundancy: {context}"
        );

        let multi = post_event(
            "MultiEdit",
            &serde_json::json!({
                "file_path": "src/widgets.rs",
                "edits": [ { "old_string": "", "new_string": content } ],
            }),
        );
        assert!(
            decide_post_tool_use_hint(&multi).is_some(),
            "a MultiEdit adding a function body must produce a hint"
        );

        let tiny = post_event(
            "Edit",
            &serde_json::json!({ "file_path": "src/widgets.rs", "new_string": "let x = 1;" }),
        );
        assert!(
            decide_post_tool_use_hint(&tiny).is_none(),
            "a small non-function edit must not produce a hint"
        );
    }

    #[test]
    fn memory_file_edit_post_tool_use_event_decides_a_fact_store_hint() {
        // Write/Edit are candidates *only* when they target a harness-memory file.
        for (tool, path) in [
            ("Write", "/home/zack/.claude/projects/foo/memory/MEMORY.md"),
            ("Edit", "/repo/CLAUDE.md"),
        ] {
            let event = post_event(tool, &serde_json::json!({ "file_path": path }));
            let hint = decide_post_tool_use_hint(&event)
                .unwrap_or_else(|| panic!("{tool} {path} must produce a memory-store hint"));
            assert!(
                format_tool_hint(&hint).contains("tracedecay_fact_store_add"),
                "{tool} {path} hint must route durable facts to tracedecay_fact_store_add"
            );
        }
    }

    #[test]
    fn untracked_post_tool_use_events_get_no_hint() {
        assert!(decide_post_tool_use_hint(&serde_json::json!({})).is_none());
        let bare_read = post_event("Read", &serde_json::json!({}));
        assert!(decide_post_tool_use_hint(&bare_read).is_none());
    }
}
