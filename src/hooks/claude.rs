//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape.

use serde_json::Value;

use super::codex::{
    codex_additional_context_json, codex_project_root_from_event,
    codex_project_root_from_parsed_event,
};
use super::post_tool_use::{
    is_claude_edit_tool, is_claude_hint_tool, notify_post_tool_use, tool_input_command_str,
    tool_input_file_path_str, CLAUDE_POST_TOOL_USE_SHELL_TOOLS, CLAUDE_POST_TOOL_USE_SPEC,
};
use super::steering::{
    append_context_recovery_hint, append_tracedecay_bootstrap_context,
    cursor_index_signals_for_root, index_status_line, session_start_from_compaction,
};
use super::tool_hints::{decide_hint, is_harness_memory_path, HintAgent, ToolHint, ToolHintInput};
use super::{
    deduped_project_hint, event_cwd_from_parsed, event_session_id, format_tool_hint,
    is_project_like_workspace, prompt_like_text, read_hook_event, record_hook_analytics,
    record_hook_invoked, research_block_reason,
};

/// `PreToolUse` hook handler for Claude Code's Agent tool matcher.
pub fn hook_pre_tool_use() {
    let tool_input = std::env::var("TOOL_INPUT").unwrap_or_default();
    let parsed: Value = serde_json::from_str(&tool_input).unwrap_or(Value::Null);
    // TOOL_INPUT has no `cwd`; Claude Code runs hooks with the project as the
    // process working directory, so fall back to it for attribution.
    let root = codex_project_root_from_parsed_event(&parsed).or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::config::discover_project_root(&cwd))
    });
    record_hook_invoked(
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

/// Claude Code `SessionStart` hook handler.
pub async fn hook_claude_session_start() -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    // Resolve the project root the same registry-aware way the printed context
    // does: the walk-up-only `codex_project_root_from_event` returns `None` for
    // exactly this feature's targets — global-only-store projects (e.g. the
    // tracedecay repo, which has no repo-local `.tracedecay/`) and fresh
    // harness-created linked worktrees — so gating the notify on it would skip
    // the AddBranchAt path in its primary scenario.
    let root = claude_session_project_root(&parsed).await;
    record_hook_invoked(root.as_deref(), HintAgent::Claude, "SessionStart", &event);
    let mut context = claude_session_context_for_event(&event).await;
    // Fire-and-forget: nudge the daemon to refresh the index (and, when this
    // session runs in a harness-created linked worktree, auto-track its branch
    // store) before we print the staleness hint. `notify_hook_event` is
    // timeout-guarded and a no-op when the daemon socket is missing, so it is
    // safe on every session start and never blocks it. Gated on a resolved
    // project root and the real session cwd so the linked-worktree detection in
    // `plan_hook_event` sees the session tree rather than the daemon's cwd.
    if let Some(root) = root.as_ref() {
        let cwd = event_cwd_from_parsed(&parsed);
        crate::daemon::notify_hook_event(root, session_start_hook_event(cwd)).await;
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
    let root = codex_project_root_from_event(&event);
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
    let root = claude_session_project_root(&parsed).await?;
    let (staleness, _) = cursor_index_signals_for_root(&root).await;
    let mut context = index_status_line(true, staleness.as_deref());
    context.push_str(CLAUDE_SUBAGENT_START_CONTEXT);
    Some(context)
}

/// Builds the `sessionStart` daemon notification for the Claude agent.
///
/// Mirrors the other `DaemonHookEvent` constructors used in this file, but
/// `DaemonHookEvent::new` is private to `daemon.rs`, so the fire-and-forget
/// session-start event is assembled from its public fields here. `cwd` carries
/// the real session working directory so the daemon's linked-worktree detection
/// can auto-track a harness worktree's branch store.
fn session_start_hook_event(cwd: Option<std::path::PathBuf>) -> crate::daemon::DaemonHookEvent {
    crate::daemon::DaemonHookEvent {
        agent: crate::daemon::HookAgent::Claude.as_wire().to_string(),
        event: "sessionStart".to_string(),
        rel_paths: Vec::new(),
        command: None,
        cwd,
        route: None,
    }
}

/// Builds the Claude `SessionStart` context for code workspaces.
pub async fn claude_session_context_for_event(event_json: &str) -> String {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    match claude_session_project_root(&parsed).await {
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

/// Resolves the tracedecay project root for a Claude session event.
///
/// The cwd walk-up ([`codex_project_root_from_parsed_event`], which calls
/// `discover_project_root`) only finds projects with a repo-local store, an
/// enrollment marker, or a profile-sharded store on disk under the session
/// tree. Projects whose store is global-only — no repo-local `.tracedecay/`,
/// e.g. the tracedecay repo itself — are invisible to that walk even though the
/// MCP server serves them via the global-DB registry (see
/// `serve::resolve_serve_from_global_db`). Without this fallback the
/// `SessionStart` hook wrongly prints the "no project index found" init nudge
/// for a project the server indexes fine. Mirror the server's registry step:
/// prefer a registered, initialized project that contains (or equals) the
/// session cwd, deepest match first.
async fn claude_session_project_root(parsed: &Value) -> Option<std::path::PathBuf> {
    if let Some(root) = codex_project_root_from_parsed_event(parsed) {
        return Some(root);
    }
    let cwd = event_cwd_from_parsed(parsed)?;
    claude_session_root_from_global_registry(&cwd).await
}

/// Global-DB registry fallback for [`claude_session_project_root`]: returns the
/// deepest registered, initialized project whose path is an ancestor of (or
/// equal to) `cwd`. Returns `None` when the global DB is unavailable or no
/// registered project contains the session cwd.
async fn claude_session_root_from_global_registry(
    cwd: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let gdb = crate::global_db::GlobalDb::open().await?;
    let mut best: Option<std::path::PathBuf> = None;
    for path in gdb.list_project_paths().await {
        let project_path = std::path::PathBuf::from(&path);
        let canonical_project =
            std::fs::canonicalize(&project_path).unwrap_or_else(|_| project_path.clone());
        if !canonical_cwd.starts_with(&canonical_project) {
            continue;
        }
        if !crate::tracedecay::TraceDecay::is_initialized(&canonical_project) {
            continue;
        }
        // Deepest ancestor wins (most specific registered project).
        let is_deeper = best.as_ref().is_none_or(|current| {
            canonical_project.components().count() > current.components().count()
        });
        if is_deeper {
            best = Some(canonical_project);
        }
    }
    best
}

/// Claude Code `PostToolUse` hook handler used to keep the graph fresh and to
/// surface a soft tracedecay hint for native search/read tools.
///
/// Two independent outputs: the daemon notification (targeted sync / branch
/// tracking, via stderr/IPC only) and, for the native `Grep`/`Glob`/`Read`
/// tools plus recursive shell searches, a `PostToolUse` `additionalContext`
/// hint printed to stdout. The daemon path never writes stdout, so the two do
/// not interfere. Fail-open: no surviving hint leaves prior behavior unchanged.
pub async fn hook_claude_post_tool_use() -> i32 {
    let event = read_hook_event!();
    let root = codex_project_root_from_event(&event);
    record_hook_invoked(root.as_deref(), HintAgent::Claude, "PostToolUse", &event);
    if let Some(context) = claude_post_tool_use_hint_context(&event) {
        println!("{}", codex_additional_context_json("PostToolUse", &context));
    }
    notify_post_tool_use(&CLAUDE_POST_TOOL_USE_SPEC, &event).await;
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
    let root = codex_project_root_from_parsed_event(&parsed);
    let session_id = event_session_id(&parsed);
    let hint = deduped_project_hint(root, HintAgent::Claude, session_id, hint)?;
    Some(format_tool_hint(&hint))
}

/// Pure hint decision for a Claude `PostToolUse` event: shapes a
/// [`ToolHintInput`] from the event's tool name, `file_path`, and Bash
/// `command`, then runs [`decide_hint`]. Returns `None` for non-candidate
/// tools (the edit/write tools that drive daemon sync only) and when no hint
/// applies. No I/O, so it is unit-testable without a profile store.
fn decide_post_tool_use_hint(parsed: &Value) -> Option<ToolHint> {
    let tool_name = parsed.get("tool_name").and_then(Value::as_str)?;
    let file_path = tool_input_file_path_str(parsed);
    // Candidates: the native hint tools (Grep/Glob/Read), Bash, and the
    // edit/write tools *only* when they target a harness-memory file (so the
    // memory_store hint can route durable facts to tracedecay_fact_store).
    // Every other edit/write drives daemon sync only and gets no hint.
    let is_shell = CLAUDE_POST_TOOL_USE_SHELL_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(tool_name));
    let is_memory_edit =
        is_claude_edit_tool(tool_name) && file_path.as_deref().is_some_and(is_harness_memory_path);
    if !is_claude_hint_tool(tool_name) && !is_shell && !is_memory_edit {
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
        hints_enabled: true,
    })
}

/// `UserPromptSubmit` hook handler: resets the per-session local counter.
pub async fn hook_prompt_submit() {
    let project_path = crate::config::resolve_path(None);
    if let Ok(cg) = crate::tracedecay::TraceDecay::open(&project_path).await {
        let _ = cg.reset_local_counter().await;
    }
}

/// `Stop` hook handler: ingests new session data and prints a cost receipt.
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

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
    fn build_bash_command_decides_a_diagnostics_hint() {
        for command in ["cargo check", "cargo clippy", "tsc --noEmit", "pyright"] {
            let event = post_event("Bash", &serde_json::json!({ "command": command }));
            let hint = decide_post_tool_use_hint(&event)
                .unwrap_or_else(|| panic!("{command} must produce a build-diagnostics hint"));
            let context = format_tool_hint(&hint);
            assert!(
                context.contains("tracedecay_diagnostics"),
                "{command} hint must point at tracedecay_diagnostics: {context}"
            );
        }
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

    /// RAII guard that pins an env var for the duration of a test and restores
    /// it on drop, so global-DB env isolation does not leak across tests.
    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
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

    /// Regression for a global-store-only project (no repo-local `.tracedecay/`,
    /// like the tracedecay repo itself): the cwd walk-up
    /// (`codex_project_root_from_parsed_event`) finds nothing, but the project is
    /// registered and initialized in the global DB, so the registry fallback must
    /// resolve it — otherwise `SessionStart`/`SubagentStart` wrongly print the
    /// init nudge for a project the MCP server serves fine.
    // The env-serialization guard is a `std::sync::Mutex` shared with sync
    // tests; it must stay held across the `.await`s below so no other test
    // mutates `TRACEDECAY_GLOBAL_DB` mid-test. Tests run single-threaded per
    // lock, so there is no real deadlock risk from holding it across an await.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn session_root_falls_back_to_global_registry_for_global_only_project() {
        let _lock = crate::hooks::test_env_lock().lock().unwrap();
        let gdb_dir = tempfile::tempdir().unwrap();
        let gdb_path = gdb_dir.path().join("global.db");
        let _gdb_env = EnvGuard::set("TRACEDECAY_GLOBAL_DB", &gdb_path);

        let project_dir = tempfile::tempdir().unwrap();
        let project_root = project_dir.path().canonicalize().unwrap();
        // Register + initialize the project in the global DB only. An enrollment
        // marker makes `is_initialized` true without a repo-local project db.
        {
            let gdb = crate::global_db::GlobalDb::open().await.unwrap();
            gdb.upsert(&project_root, 0).await;
        }
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: "proj_global_only".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();

        // cwd inside the registered project resolves back to its root.
        let nested = project_root.join("crates/inner");
        std::fs::create_dir_all(&nested).unwrap();
        let resolved = claude_session_root_from_global_registry(&nested).await;
        assert_eq!(
            resolved
                .as_deref()
                .map(|p| std::fs::canonicalize(p).unwrap()),
            Some(project_root.clone()),
            "a registered, initialized project must resolve for a cwd inside it"
        );

        // A cwd outside every registered project resolves to nothing.
        let outside = tempfile::tempdir().unwrap();
        let outside_root = outside.path().canonicalize().unwrap();
        assert!(
            claude_session_root_from_global_registry(&outside_root)
                .await
                .is_none(),
            "a cwd outside every registered project must not resolve"
        );
    }
}
