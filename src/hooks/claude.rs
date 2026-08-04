//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape.

use serde_json::Value;

use super::codex::codex_additional_context_json;
use super::memory_inject;
use super::post_tool_use::is_post_tool_use_failure_event;
use super::steering::{cursor_index_signals_for_root, index_status_line};
use super::tool_hints::{HintAgent, ToolHintInput, decide_hint};
use super::{
    event_project_root, event_project_root_with_identity, event_session_id,
    process_cwd_project_root, prompt_like_text, read_hook_event, record_hook_analytics,
    record_hook_invoked_parsed, research_block_reason,
};

/// `PreToolUse` hook handler for Claude Code's Agent tool matcher.
pub fn hook_pre_tool_use() {
    let tool_input = std::env::var("TOOL_INPUT").unwrap_or_default();
    let parsed: Value = serde_json::from_str(&tool_input).unwrap_or(Value::Null);
    // TOOL_INPUT has no `cwd`; Claude Code runs hooks with the project as the
    // process working directory, so fall back to it for attribution.
    let root = event_project_root(&parsed).or_else(process_cwd_project_root);
    let _hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Claude,
        "preToolUse",
        &tool_input,
        &parsed,
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
    println!("{}", claude_session_start_response(&event).await);
    0
}

async fn claude_session_start_response(event: &str) -> String {
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    // Resolve the project root the same identity-aware way the printed context
    // does, including global-only stores and fresh harness-created worktrees.
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Claude,
        "SessionStart",
        &event,
        &parsed,
    );
    let guidance = super::v2::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::ClaudeCode,
        &event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    guidance.map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| codex_additional_context_json("SessionStart", &guidance),
    )
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
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let _hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Claude,
        "SubagentStart",
        &event,
        &parsed,
    );
    if let Some(context) = claude_subagent_start_context(&parsed).await {
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
async fn claude_subagent_start_context(parsed: &Value) -> Option<String> {
    let root = event_project_root_with_identity(parsed).await?;
    let (staleness, _) = cursor_index_signals_for_root(&root).await;
    let mut context = index_status_line(true, staleness.as_deref());
    context.push_str(CLAUDE_SUBAGENT_START_CONTEXT);
    Some(context)
}

/// Claude Code `PostToolUse` / `PostToolUseFailure` hook handler.
pub async fn hook_claude_post_tool_use() -> i32 {
    let event = read_hook_event!();
    if let Some(response) = claude_post_tool_use_response(&event).await {
        println!("{response}");
    }
    0
}

async fn claude_post_tool_use_response(event: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let hook_event_name = if is_post_tool_use_failure_event(&parsed) {
        "PostToolUseFailure"
    } else {
        "PostToolUse"
    };
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Claude,
        hook_event_name,
        &event,
        &parsed,
    );
    super::v2::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::ClaudeCode,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map(|guidance| codex_additional_context_json(hook_event_name, &guidance))
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
    let hook_telemetry = record_hook_invoked_parsed(
        root.as_deref(),
        HintAgent::Claude,
        "UserPromptSubmit",
        &event,
        &parsed,
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

/// `Stop` hook handler: submits the native turn boundary to daemon-owned V2.
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
    println!("{}", claude_stop_response_for_event(&event).await);
}

/// Runs Claude's native stop boundary through Hook V2 and returns the exact
/// host response. Keeping this separate from stdin I/O makes the admitted
/// boundary the single production path for both the command adapter and tests.
async fn claude_stop_response_for_event(event: &str) -> String {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(&parsed).await;
    let hook_telemetry =
        record_hook_invoked_parsed(root.as_deref(), HintAgent::Claude, "Stop", event, &parsed);
    super::v2::dispatch_for_scope(
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
        |guidance| codex_additional_context_json("Stop", &guidance),
    )
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

    fn bind_v2_project(project_root: &std::path::Path, project_id: &str) {
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        crate::hooks::v2::publish_test_binding(
            project_root,
            tracedecay_hooks::HookHostV1::ClaudeCode,
        )
        .unwrap();
    }

    fn accepted_admission() -> Value {
        serde_json::json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": tracedecay_hooks::HookTransportDispositionV1::Accepted,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "reason": null,
        })
    }

    fn unavailable_admission() -> Value {
        serde_json::json!({
            "action": "hook_v2_admit",
            "status": "unavailable",
        })
    }

    fn assert_v2_admission(
        call: &(Option<std::path::PathBuf>, Value),
        project_root: &std::path::Path,
        session_id: &str,
    ) {
        assert_eq!(call.0.as_deref(), Some(project_root));
        assert_eq!(call.1["action"], "hook_v2_admit");
        assert_eq!(call.1["native_session_id"], session_id);
        assert_eq!(
            call.1["envelope"]["producer"],
            serde_json::json!(tracedecay_hooks::HookHostV1::ClaudeCode)
        );
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
    async fn session_start_and_post_tool_use_admit_once_without_local_fallbacks() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        bind_v2_project(&project_root, "proj_claude_native_admission");
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([
            accepted_admission(),
            accepted_admission(),
        ]);

        let session_start = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "claude-session-start",
            "cwd": project_root,
            "source": "startup",
        })
        .to_string();
        assert_eq!(
            claude_session_start_response(&session_start).await,
            serde_json::json!({}).to_string()
        );

        let post_tool = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "claude-post-tool",
            "transcript_path": project_root.join("session.jsonl"),
            "cwd": project_root,
            "prompt_id": "claude-post-prompt",
            "permission_mode": "acceptEdits",
            "tool_name": "Write",
            "tool_input": { "file_path": project_root.join("src/lib.rs"), "content": "x" },
            "tool_response": {},
            "tool_use_id": "toolu-local",
            "duration_ms": 1,
        })
        .to_string();
        assert_eq!(claude_post_tool_use_response(&post_tool).await, None);

        let calls = daemon.calls();
        assert_eq!(calls.len(), 2);
        assert_v2_admission(&calls[0], &project_root, "claude-session-start");
        assert_v2_admission(&calls[1], &project_root, "claude-post-tool");
    }

    #[tokio::test]
    async fn session_start_and_post_tool_use_fail_open_after_one_unavailable_admission() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        bind_v2_project(&project_root, "proj_claude_native_unavailable");
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([
            unavailable_admission(),
            unavailable_admission(),
        ]);

        let session_start = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "claude-session-start-unavailable",
            "cwd": project_root,
            "source": "startup",
        })
        .to_string();
        assert_eq!(
            claude_session_start_response(&session_start).await,
            serde_json::json!({}).to_string()
        );

        let post_tool = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "claude-post-tool-unavailable",
            "transcript_path": project_root.join("session.jsonl"),
            "cwd": project_root,
            "prompt_id": "claude-post-prompt",
            "permission_mode": "acceptEdits",
            "tool_name": "Write",
            "tool_input": { "file_path": project_root.join("src/lib.rs"), "content": "x" },
            "tool_response": {},
            "tool_use_id": "toolu-local",
            "duration_ms": 1,
        })
        .to_string();
        assert_eq!(claude_post_tool_use_response(&post_tool).await, None);

        let calls = daemon.calls();
        assert_eq!(calls.len(), 2, "each event gets one daemon admission");
        assert_v2_admission(&calls[0], &project_root, "claude-session-start-unavailable");
        assert_v2_admission(&calls[1], &project_root, "claude-post-tool-unavailable");
    }

    #[tokio::test]
    async fn stop_admits_the_native_boundary_within_hook_budget() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_claude_stop_admission".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        crate::hooks::v2::publish_test_binding(
            &project_root,
            tracedecay_hooks::HookHostV1::ClaudeCode,
        )
        .unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": tracedecay_hooks::HookTransportDispositionV1::Accepted,
            "orchestration": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "reason": null,
        })]);
        let event = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "claude-stop-session",
            "transcript_path": project_root.join("session.jsonl"),
            "cwd": project_root,
            "prompt_id": "claude-stop-prompt",
            "permission_mode": "acceptEdits",
            "stop_hook_active": false,
            "last_assistant_message": "done",
            "background_tasks": [],
            "session_crons": [],
        })
        .to_string();

        let started = std::time::Instant::now();
        let response = claude_stop_response_for_event(&event).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "native stop admission exceeded the bounded hook path"
        );
        assert_eq!(response, serde_json::json!({}).to_string());
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_deref(), Some(project_root.as_path()));
        assert_eq!(calls[0].1["action"], "hook_v2_admit");
        assert_eq!(calls[0].1["native_session_id"], "claude-stop-session");
        assert_eq!(
            calls[0].1["envelope"]["producer"],
            serde_json::json!(tracedecay_hooks::HookHostV1::ClaudeCode)
        );
    }

    #[tokio::test]
    async fn stop_returns_empty_after_one_unavailable_daemon_admission() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_claude_stop_unavailable".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        crate::hooks::v2::publish_test_binding(
            &project_root,
            tracedecay_hooks::HookHostV1::ClaudeCode,
        )
        .unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "hook_v2_admit",
            "status": "unavailable",
        })]);
        let event = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "claude-stop-unavailable",
            "transcript_path": project_root.join("session.jsonl"),
            "cwd": project_root,
            "prompt_id": "claude-stop-prompt",
            "permission_mode": "acceptEdits",
            "stop_hook_active": false,
            "last_assistant_message": "done",
            "background_tasks": [],
            "session_crons": [],
        })
        .to_string();

        assert_eq!(
            claude_stop_response_for_event(&event).await,
            serde_json::json!({}).to_string()
        );
        let calls = daemon.calls();
        assert_eq!(
            calls.len(),
            1,
            "unavailable admission must not start a fallback"
        );
        assert_eq!(calls[0].1["action"], "hook_v2_admit");
    }

    #[tokio::test]
    async fn projectless_stop_uses_one_profile_scoped_daemon_admission() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let workspace = tempfile::tempdir().unwrap();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "hook_v2_profile_admit",
            "status": "unavailable",
        })]);
        let event = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "claude-projectless-stop",
            "transcript_path": workspace.path().join("session.jsonl"),
            "cwd": workspace.path(),
            "prompt_id": "claude-stop-prompt",
            "permission_mode": "acceptEdits",
            "stop_hook_active": false,
            "last_assistant_message": "done",
            "background_tasks": [],
            "session_crons": [],
        })
        .to_string();

        assert_eq!(
            claude_stop_response_for_event(&event).await,
            serde_json::json!({}).to_string()
        );
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[0].1["action"], "hook_v2_profile_admit");
        assert!(calls[0].1.get("event_json").is_none());
    }
}
