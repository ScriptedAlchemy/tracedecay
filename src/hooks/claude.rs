//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape.

use serde_json::Value;
use tracedecay_hooks::{DaemonHookEvent, HookAgent};

use super::codex::codex_additional_context_json;
use super::memory_inject;
use super::post_tool_use::{
    CLAUDE_POST_TOOL_USE_SHELL_TOOLS, CLAUDE_POST_TOOL_USE_SPEC, captured_tool_output,
    is_claude_edit_tool, is_claude_hint_tool, is_post_tool_use_failure_event,
    notify_post_tool_use_with_telemetry, tool_input_command_str, tool_input_edit_text,
    tool_input_file_path_str, trusted_tool_failure,
};
use super::steering::{
    append_context_block, append_context_recovery_hint, append_tracedecay_bootstrap_context,
    cursor_index_signals_for_root, index_status_line, session_start_from_compaction,
};
use super::tool_hints::{HintAgent, ToolHint, ToolHintInput, decide_hint, is_harness_memory_path};
use super::{
    deduped_project_hint, event_cwd_from_parsed, event_project_root,
    event_project_root_with_identity, event_project_root_with_identity_from_json, event_session_id,
    format_tool_hint, is_project_like_workspace, process_cwd_project_root, prompt_like_text,
    read_hook_event, record_hook_analytics, record_hook_invoked, research_block_reason,
};

/// `PreToolUse` hook handler for Claude Code's Agent tool matcher.
pub fn hook_pre_tool_use() {
    let tool_input = std::env::var("TOOL_INPUT").unwrap_or_default();
    let parsed: Value = serde_json::from_str(&tool_input).unwrap_or(Value::Null);
    // TOOL_INPUT has no `cwd`; Claude Code runs hooks with the project as the
    // process working directory, so fall back to it for attribution.
    let root = event_project_root(&parsed).or_else(process_cwd_project_root);
    let _hook_telemetry = record_hook_invoked(
        root.as_deref(),
        HintAgent::Claude,
        "preToolUse",
        &tool_input,
    );
    let decision = evaluate_hook_decision(&tool_input);
    // Explore-block telemetry: record every invocation with its deny/allow
    // outcome and session attribution so deny frequency is measurable. The
    // deny behavior itself (printing `decision`) is unchanged.
    record_explore_block_outcome(root.as_deref(), &parsed, !decision.is_empty());
    if !decision.is_empty() {
        println!("{decision}");
    }
}

/// Records the outcome of a `PreToolUse` explore-block evaluation. `denied`
/// is true when the hook blocked the call (a non-empty decision was printed),
/// false when the call was allowed through. Session id and tool attribution
/// are pulled from the already-parsed `TOOL_INPUT`.
fn record_explore_block_outcome(root: Option<&std::path::Path>, parsed: &Value, denied: bool) {
    record_hook_analytics(
        root,
        "explore_block",
        explore_block_analytics_fields(parsed, denied),
    );
}

/// Builds the `explore_block` analytics payload for an evaluated `PreToolUse`
/// event. Kept pure (no I/O) so the deny/allow attribution is unit-testable
/// without touching the profile store.
fn explore_block_analytics_fields(parsed: &Value, denied: bool) -> Value {
    serde_json::json!({
        "agent": HintAgent::Claude.as_key(),
        "session_id": event_session_id(parsed),
        "tool_name": parsed.get("tool_name").and_then(Value::as_str),
        "subagent_type": parsed.get("subagent_type").and_then(Value::as_str),
        "outcome": if denied { "deny" } else { "allow" },
    })
}

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
pub async fn hook_claude_session_start() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    // Resolve the project root the same identity-aware way the printed context
    // does, including global-only stores and fresh harness-created worktrees.
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Claude, "SessionStart", &event);
    let mut context = claude_session_context_for_event(&event).await;
    let session_id = event_session_id(&parsed);
    if root.is_none() && ingest_user_claude_session(session_id.clone()).await {
        super::schedule_user_session_review("claude", session_id.as_deref());
    }
    let digest = match root.as_deref() {
        Some(root) => {
            memory_inject::combined_session_memory_digest(root, session_id.as_deref()).await
        }
        None => memory_inject::user_session_memory_digest(session_id.as_deref()).await,
    };
    if let Some(digest) = digest {
        append_context_block(&mut context, &digest);
    }
    // Fire-and-forget: nudge the daemon to refresh the index (and, when this
    // session runs in a harness-created linked worktree, auto-track its branch
    // store) before we print the staleness hint. `notify_hook_event` is
    // timeout-guarded and a no-op when the daemon socket is missing, so it is
    // safe on every session start and never blocks it. Gated on a resolved
    // project root and the real session cwd so the linked-worktree detection in
    // `plan_hook_event` sees the session tree rather than the daemon's cwd.
    if let Some(root) = root.as_ref()
        && let Some(event) = claude_session_start_hook_event(&parsed)
    {
        super::notify_hook_event_with_telemetry(root, event, &hook_telemetry).await;
    }
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

/// Compact routing guidance emitted to a Claude subagent at start. Kept short
/// on purpose: a subagent's context budget is precious, so this is a single
/// line steering it to the graph before native grep, noting that the tracedecay
/// tools may be deferred behind `ToolSearch`, plus the literal/symbol/concept
/// routing rule that mirrors the search hint.
const CLAUDE_SUBAGENT_START_CONTEXT: &str = "graph before grep; tools may be deferred — \
ToolSearch select:tracedecay_context,tracedecay_grep,tracedecay_callers; route literal->grep, \
symbol->search, concept->context";

/// Claude Code `SubagentStart` hook handler.
///
/// Mirrors [`hook_codex_subagent_start`](super::codex::hook_codex_subagent_start)
/// but emits a compact context (index status line + routing guidance) so a
/// fresh subagent reaches for tracedecay before a broad native scan. Emission is
/// skipped when the project root cannot be resolved (a non-project workspace has
/// nothing to steer toward). Analytics are fire-and-forget like `SessionStart`.
pub async fn hook_claude_subagent_start() -> i32 {
    let event = read_hook_event!();
    let root = event_project_root_with_identity_from_json(&event).await;
    let _hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Claude, "SubagentStart", &event);
    if let Some(context) = claude_subagent_start_context(&event).await {
        println!(
            "{}",
            codex_additional_context_json("SubagentStart", &context)
        );
    } else {
        println!("{}", serde_json::json!({}));
    }
    0
}

/// Builds the compact `SubagentStart` `additionalContext` for a Claude event, or
/// `None` when root detection fails (no project to steer toward). The status
/// line is resolved the same registry-aware way as `SessionStart` so a
/// global-store-only project still steers correctly.
async fn claude_subagent_start_context(event_json: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await?;
    let (staleness, _) = cursor_index_signals_for_root(&root).await;
    let mut context = index_status_line(true, staleness.as_deref());
    context.push_str(CLAUDE_SUBAGENT_START_CONTEXT);
    Some(context)
}

fn claude_session_start_hook_event(parsed: &Value) -> Option<DaemonHookEvent> {
    event_cwd_from_parsed(parsed).map(|cwd| DaemonHookEvent::session_start(HookAgent::Claude, cwd))
}

/// Builds the Claude `SessionStart` context for code workspaces.
pub async fn claude_session_context_for_event(event_json: &str) -> String {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    match event_project_root_with_identity(&parsed).await {
        Some(root) => {
            let (staleness, _) = cursor_index_signals_for_root(&root).await;
            let mut context = index_status_line(true, staleness.as_deref());
            append_tracedecay_bootstrap_context(&mut context);
            context
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

/// Claude Code `PostToolUse` / `PostToolUseFailure` hook handler used to keep
/// the graph fresh and surface outcome-aware `TraceDecay` hints.
///
/// Two independent outputs: the daemon notification (targeted sync / branch
/// tracking, via stderr/IPC only) and, for the native `Grep`/`Glob`/`Read`
/// tools plus recursive shell searches or compiler failures, an event-matched
/// `additionalContext` hint printed to stdout. The daemon path never writes
/// stdout, so the two do not interfere. Fail-open: no surviving hint leaves
/// prior behavior unchanged.
pub async fn hook_claude_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let is_failure = serde_json::from_str::<Value>(&event)
        .ok()
        .as_ref()
        .is_some_and(is_post_tool_use_failure_event);
    let hook_event_name = if is_failure {
        "PostToolUseFailure"
    } else {
        "PostToolUse"
    };
    let root = event_project_root_with_identity_from_json(&event).await;
    let hook_telemetry =
        record_hook_invoked(root.as_deref(), HintAgent::Claude, hook_event_name, &event);
    if let Some(root) = root.as_deref()
        && let Some(guidance) = super::v2::dispatch(
            tracedecay_hooks::HookHostV1::ClaudeCode,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        if let Some(guidance) = guidance {
            println!(
                "{}",
                codex_additional_context_json(hook_event_name, &guidance)
            );
        }
        return 0;
    }
    if let Some(context) = claude_post_tool_use_hint_context(&event) {
        println!(
            "{}",
            codex_additional_context_json(hook_event_name, &context)
        );
    }
    notify_post_tool_use_with_telemetry(&CLAUDE_POST_TOOL_USE_SPEC, &event, &hook_telemetry).await;
    0
}

/// Builds the `PostToolUse` `additionalContext` string for a native
/// search/read tool event (or a recursive shell search), or `None` when no
/// hint survives dedupe. Decides the raw hint with [`decide_post_tool_use_hint`],
/// then dedupes per (session, category) via [`deduped_project_hint`] exactly
/// like the pre-tool-use surface.
fn claude_post_tool_use_hint_context(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_post_tool_use_hint(&parsed)?;
    // `deduped_project_hint` mints its own candidate id and records the
    // terminal analytics row; the Claude post-tool-use surface does not emit a
    // separate `hint_candidate`, per its documented contract.
    let root = event_project_root(&parsed);
    let session_id = event_session_id(&parsed);
    let hint = deduped_project_hint(root.as_deref(), HintAgent::Claude, session_id, hint)?;
    Some(format_tool_hint(&hint))
}

/// Pure hint decision for a Claude post-tool event: shapes a [`ToolHintInput`]
/// from the event's tool name, input, and host-owned outcome fields, then runs
/// [`decide_hint`]. Returns `None` for non-candidate
/// tools (the edit/write tools that drive daemon sync only) and when no hint
/// applies. No I/O, so it is unit-testable without a profile store.
fn decide_post_tool_use_hint(parsed: &Value) -> Option<ToolHint> {
    let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
    let file_path = tool_input_file_path_str(parsed);
    // Candidates: the native hint tools (Grep/Glob/Read), Bash, edit/write tools
    // targeting a harness-memory file (so the memory_store hint can route
    // durable facts to tracedecay_fact_store), and edit/write tools that add a
    // new function-sized body (so the edit_redundancy hint can nudge a
    // duplicate-logic probe). `edit_text` is read only for edit tools and only
    // to feed those two edit branches; every other edit/write drives daemon sync
    // only and gets no hint.
    let is_shell = CLAUDE_POST_TOOL_USE_SHELL_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(tool_name));
    let is_edit = is_claude_edit_tool(tool_name);
    let is_memory_edit = is_edit && file_path.as_deref().is_some_and(is_harness_memory_path);
    // Only pay the string scan for non-memory edits (memory edits route to their
    // own branch and never carry a code body worth probing).
    let edit_text = (is_edit && !is_memory_edit)
        .then(|| tool_input_edit_text(parsed))
        .flatten();
    if !is_claude_hint_tool(tool_name) && !is_shell && !is_memory_edit && edit_text.is_none() {
        return None;
    }
    decide_hint(&ToolHintInput {
        agent: HintAgent::Claude,
        session_id: event_session_id(parsed),
        tool_name: Some(tool_name.to_string()),
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

/// `UserPromptSubmit` hook handler: resets the project counter and injects
/// scope-correct memory recall.
pub async fn hook_prompt_submit() {
    let event = match super::read_stdin_bounded() {
        Ok(super::HookStdinRead::Event(event)) => event,
        Ok(super::HookStdinRead::Oversized) => {
            eprintln!(
                "tracedecay hook: stdin exceeds wire message bound ({})",
                crate::application::host_admission::WIRE_RECORD_TOO_LARGE
            );
            return;
        }
        Err(error) => {
            eprintln!("tracedecay hook: failed to read stdin: {error}");
            return;
        }
    };
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked(
        root.as_deref(),
        HintAgent::Claude,
        "UserPromptSubmit",
        &event,
    );
    let session_id = event_session_id(&parsed);
    if root.is_none()
        && ingest_user_claude_session_with_telemetry(session_id.clone(), Some(&hook_telemetry))
            .await
    {
        super::schedule_user_session_review("claude", session_id.as_deref());
    }
    if let Some(root) = root.as_deref()
        && let Err(error) = super::daemon_hook_action(
            Some(root),
            serde_json::json!({ "action": "reset_counter" }),
            Some(&hook_telemetry),
        )
        .await
    {
        eprintln!("[tracedecay] local counter reset daemon call failed: {error}");
    }
    let recall = prompt_like_text(&parsed);
    let recall = match (root.as_deref(), recall.as_deref()) {
        (Some(root), Some(prompt)) => {
            Box::pin(memory_inject::combined_prompt_memory_recall(
                root,
                session_id.as_deref(),
                prompt,
            ))
            .await
        }
        (None, Some(prompt)) => {
            memory_inject::user_prompt_memory_recall(session_id.as_deref(), prompt).await
        }
        (_, None) => None,
    };
    if let Some(recall) = recall {
        println!(
            "{}",
            codex_additional_context_json("UserPromptSubmit", &recall)
        );
    } else {
        println!("{}", serde_json::json!({}));
    }
}

/// `Stop` hook handler: ingests new session data and prints a cost receipt.
pub async fn hook_stop() {
    let event = match super::read_stdin_bounded() {
        Ok(super::HookStdinRead::Event(event)) => event,
        Ok(super::HookStdinRead::Oversized) => {
            eprintln!(
                "tracedecay hook: stdin exceeds wire message bound ({})",
                crate::application::host_admission::WIRE_RECORD_TOO_LARGE
            );
            return;
        }
        Err(_) => String::new(),
    };
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked(root.as_deref(), HintAgent::Claude, "Stop", &event);
    if let Some(root) = root.as_deref()
        && let Some(guidance) = super::v2::dispatch(
            tracedecay_hooks::HookHostV1::ClaudeCode,
            &event,
            root,
            Some(&hook_telemetry),
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    {
        if let Some(guidance) = guidance {
            println!("{}", codex_additional_context_json("Stop", &guidance));
        }
        return;
    }
    let session_id = event_session_id(&parsed);
    if root.is_none()
        && ingest_user_claude_session_with_telemetry(session_id.clone(), Some(&hook_telemetry))
            .await
    {
        super::schedule_user_session_review("claude", session_id.as_deref());
    }
}

/// Incrementally ingests one live projectless Claude session into the profile
/// session store. `false` means no new transcript evidence was written.
pub async fn ingest_user_claude_session(session_id: Option<String>) -> bool {
    ingest_user_claude_session_with_telemetry(session_id, None).await
}

async fn ingest_user_claude_session_with_telemetry(
    session_id: Option<String>,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) -> bool {
    super::ingest_user_session("Claude", session_id, telemetry).await
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn claude_session_start_event_signals_daemon_with_real_cwd() {
        let event = claude_session_start_hook_event(&serde_json::json!({
            "cwd": "/workspace/claude-session"
        }))
        .unwrap();

        assert_eq!(event.agent, HookAgent::Claude.as_wire());
        assert_eq!(event.event, "sessionStart");
        assert_eq!(
            event.cwd.as_deref(),
            Some(std::path::Path::new("/workspace/claude-session"))
        );
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

        // The PostToolUse JSON envelope carries the hint as additionalContext —
        // the exact shape hook_claude_post_tool_use prints to stdout.
        let json: Value =
            serde_json::from_str(&codex_additional_context_json("PostToolUse", &context)).unwrap();
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

        // A non-search, non-build Bash command yields no hint.
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
        // A Write whose content is a new function-sized Rust body nudges toward
        // the duplicate-logic probe.
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

        // A MultiEdit whose joined new_strings form a function body also nudges.
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

        // A small, non-function edit stays silent.
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
                format_tool_hint(&hint).contains("tracedecay_fact_store"),
                "{tool} {path} hint must route durable facts to tracedecay_fact_store"
            );
        }
    }

    #[test]
    fn untracked_post_tool_use_events_get_no_hint() {
        // No tool_name → no candidate.
        assert!(decide_post_tool_use_hint(&serde_json::json!({})).is_none());
        // A Read with no file_path is not a single-file read.
        let bare_read = post_event("Read", &serde_json::json!({}));
        assert!(decide_post_tool_use_hint(&bare_read).is_none());
    }

    #[test]
    fn explore_block_records_deny_for_explore_subagent() {
        let parsed: Value = serde_json::from_str(
            r#"{"session_id":"s1","tool_name":"Agent","subagent_type":"Explore"}"#,
        )
        .unwrap();
        // A blocked Explore subagent: denied = true.
        let fields = explore_block_analytics_fields(&parsed, true);
        assert_eq!(fields["outcome"].as_str(), Some("deny"));
        assert_eq!(fields["session_id"].as_str(), Some("s1"));
        assert_eq!(fields["subagent_type"].as_str(), Some("Explore"));
        assert_eq!(fields["agent"].as_str(), Some("claude"));
    }

    #[test]
    fn explore_block_records_allow_for_permitted_agent() {
        let parsed: Value = serde_json::from_str(
            r#"{"session_id":"s2","tool_name":"Agent","subagent_type":"general-purpose"}"#,
        )
        .unwrap();
        // An allowed agent call: denied = false.
        let fields = explore_block_analytics_fields(&parsed, false);
        assert_eq!(fields["outcome"].as_str(), Some("allow"));
        assert_eq!(fields["session_id"].as_str(), Some("s2"));
        assert_eq!(fields["subagent_type"].as_str(), Some("general-purpose"));
    }

    #[test]
    fn explore_block_outcome_tracks_evaluate_hook_decision() {
        // The recorded outcome must mirror whether evaluate_hook_decision blocks.
        let deny_input = r#"{"session_id":"s3","subagent_type":"Explore","prompt":"find files"}"#;
        let deny_parsed: Value = serde_json::from_str(deny_input).unwrap();
        let denied = !evaluate_hook_decision(deny_input).is_empty();
        assert!(denied, "Explore subagent must be denied");
        assert_eq!(
            explore_block_analytics_fields(&deny_parsed, denied)["outcome"].as_str(),
            Some("deny")
        );

        let allow_input =
            r#"{"session_id":"s3","subagent_type":"general-purpose","prompt":"run the build"}"#;
        let allow_parsed: Value = serde_json::from_str(allow_input).unwrap();
        let allowed_denied = !evaluate_hook_decision(allow_input).is_empty();
        assert!(!allowed_denied, "non-explore agent must be allowed");
        assert_eq!(
            explore_block_analytics_fields(&allow_parsed, allowed_denied)["outcome"].as_str(),
            Some("allow")
        );
    }

    #[test]
    fn subagent_start_context_constant_carries_compact_routing() {
        // The compact context must steer toward the graph and name the deferred
        // ToolSearch entry point plus the literal/symbol/concept routing rule.
        assert!(CLAUDE_SUBAGENT_START_CONTEXT.contains("graph before grep"));
        assert!(CLAUDE_SUBAGENT_START_CONTEXT.contains("ToolSearch"));
        assert!(CLAUDE_SUBAGENT_START_CONTEXT.contains("tracedecay_context"));
        assert!(CLAUDE_SUBAGENT_START_CONTEXT.contains("literal->grep"));
        assert!(CLAUDE_SUBAGENT_START_CONTEXT.contains("symbol->search"));
        assert!(CLAUDE_SUBAGENT_START_CONTEXT.contains("concept->context"));
    }

    fn touch_marker_graph_db(project_root: &std::path::Path) -> std::path::PathBuf {
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_global_only".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let layout = crate::storage::resolve_layout_for_current_profile(project_root).unwrap();
        std::fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
        std::fs::write(&layout.graph_db_path, b"").unwrap();
        layout.graph_db_path
    }

    #[tokio::test]
    async fn session_root_uses_shared_identity_resolver_for_global_only_project() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = crate::storage::default_profile_root().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let project_root = project_dir.path().canonicalize().unwrap();
        let status = std::process::Command::new("git")
            .arg("init")
            .arg(&project_root)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");

        let project_id = "proj_claude_identity";
        let gdb = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            &profile_root,
            &project_root,
            tracedecay_domain::ProjectId::new(project_id).unwrap(),
        )
        .await
        .unwrap();
        let graph = gdb
            .initialize_project_graph_for_test(
                &project_root,
                crate::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .unwrap();
        let graph_db_path = graph.store_layout().graph_db_path.clone();
        drop(graph);
        crate::storage::remove_enrollment_marker(&project_root, project_id).unwrap();

        let nested = project_root.join("crates/inner");
        std::fs::create_dir_all(&nested).unwrap();
        let event = serde_json::json!({ "cwd": nested.to_string_lossy() });
        let resolved = event_project_root_with_identity(&event).await;
        assert_eq!(
            resolved
                .as_deref()
                .map(|p| std::fs::canonicalize(p).unwrap()),
            Some(project_root.clone()),
            "a registered, initialized project must resolve for a cwd inside it"
        );

        let outside = tempfile::tempdir().unwrap();
        let outside_root = outside.path().canonicalize().unwrap();
        assert!(
            event_project_root_with_identity(
                &serde_json::json!({ "cwd": outside_root.to_string_lossy() })
            )
            .await
            .is_none(),
            "a cwd outside every registered project must not resolve"
        );

        std::fs::remove_file(graph_db_path).unwrap();
        assert!(
            event_project_root_with_identity(&event).await.is_none(),
            "a registered project without a real graph db must not resolve"
        );
    }

    #[tokio::test]
    async fn session_context_reports_initialized_and_preserves_nudge() {
        let _profile = crate::config::PinnedUserDataDir::new();

        let profile_root = crate::storage::default_profile_root().unwrap();
        let gdb =
            crate::application::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
                .await
                .unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let project_root = project_dir.path().canonicalize().unwrap();
        gdb.upsert(&project_root, 0).await;
        touch_marker_graph_db(&project_root);

        let event = serde_json::json!({
            "hook_event_name": "SessionStart",
            "cwd": project_root.to_string_lossy(),
            "source": "startup",
        })
        .to_string();
        let context = claude_session_context_for_event(&event).await;
        assert!(
            !context.contains("no project index found"),
            "a registered, graph-db-backed project must not emit the init nudge: {context}"
        );
        assert!(
            context.contains("tracedecay index status:"),
            "context must report the index status line: {context}"
        );

        let unindexed = tempfile::tempdir().unwrap();
        let unindexed_root = unindexed.path().canonicalize().unwrap();
        std::fs::write(unindexed_root.join("Cargo.toml"), b"[package]\n").unwrap();
        let bogus_event = serde_json::json!({
            "hook_event_name": "SessionStart",
            "cwd": unindexed_root.to_string_lossy(),
            "source": "startup",
        })
        .to_string();
        let bogus_context = claude_session_context_for_event(&bogus_event).await;
        assert!(
            bogus_context.contains("no project index found"),
            "an unindexed project-like cwd must still emit the real nudge: {bogus_context}"
        );
    }
}
