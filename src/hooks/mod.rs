//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! These functions are invoked by each agent's hook system to intercept tool
//! calls, redirect exploration work to tracedecay MCP tools, keep the index
//! fresh after edits / git state changes, and track per-session token savings.
//! Each agent sends its own event schema on stdin and expects its own output
//! shape, so the handlers are kept agent-specific rather than shared blindly.
//!
//! This module holds the shared plumbing (stdin reader, hook analytics,
//! event-field helpers, and per-session hint dedupe); the per-agent handlers
//! live in the `claude`, `codex`, `cursor`, and `kiro` submodules, with the
//! shared post-tool-use pipeline in `post_tool_use` and the session/steering
//! context builders in `steering`. Every public item is re-exported here so
//! it stays reachable at `crate::hooks::<name>`.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

mod claude;
mod codex;
mod cursor;
mod cursor_compact;
mod cursor_shell;
mod kiro;
pub(crate) mod memory_inject;
mod post_tool_use;
mod steering;
pub mod tool_hints;

pub use claude::{
    evaluate_hook_decision, hook_claude_post_tool_use, hook_claude_session_start,
    hook_pre_tool_use, hook_prompt_submit, hook_stop,
};
pub use codex::{
    codex_additional_context_json, codex_apply_patch_rel_paths, codex_project_root_from_event,
    codex_subagent_start_log_line, codex_user_prompt_submit_context_for_event,
    codex_workspace_status_from_event, evaluate_codex_subagent_start, hook_codex_post_compact,
    hook_codex_post_tool_use, hook_codex_session_start, hook_codex_subagent_start,
    hook_codex_user_prompt_submit, record_codex_subagent_start,
};
pub use cursor::{
    cursor_after_file_edit_rel_paths, cursor_post_tool_use_decision,
    cursor_project_root_from_event, cursor_session_start_json, cursor_should_run_sync,
    evaluate_cursor_post_tool_use, evaluate_cursor_subagent_start, hook_cursor_after_file_edit,
    hook_cursor_after_shell, hook_cursor_before_submit_prompt, hook_cursor_post_tool_use,
    hook_cursor_pre_compact, hook_cursor_session_end, hook_cursor_session_start, hook_cursor_stop,
    hook_cursor_subagent_start, hook_cursor_workspace_open, CURSOR_CATCH_UP_INGEST_MAX_BYTES,
};
pub use cursor_compact::{cursor_pre_compact_for_event_with_config, CursorPreCompactOutcome};
pub use cursor_shell::{
    cursor_branch_switch_target, cursor_shell_command_targets_project, cursor_shell_sync_plan,
    cursor_shell_sync_plan_with_current_branch, is_git_state_changing_command,
    resolve_worktree_add_root, CursorShellSyncPlan,
};
pub use kiro::{
    evaluate_kiro_pre_tool_use, hook_kiro_post_tool_use, hook_kiro_pre_tool_use,
    hook_kiro_prompt_submit, kiro_post_tool_use_rel_paths,
};
pub use post_tool_use::{
    claude_post_tool_use_matcher, CLAUDE_POST_TOOL_USE_EDIT_TOOLS, CLAUDE_POST_TOOL_USE_SHELL_TOOLS,
};
pub use steering::{
    build_codex_session_context, build_codex_session_context_for_workspace,
    build_cursor_session_context, cursor_staleness_hint, HookWorkspaceStatus, CURSOR_PLUGIN_SKILLS,
};

pub(crate) use cursor_shell::shell_words;

use tool_hints::{HintAgent, HintCategory, ToolHint};

macro_rules! read_hook_event {
    () => {{
        match $crate::hooks::read_stdin_to_string() {
            Ok(event) => event,
            Err(e) => {
                eprintln!("tracedecay hook: failed to read stdin: {e}");
                return 1;
            }
        }
    }};
}
pub(crate) use read_hook_event;

const TRACEDECAY_RESEARCH_BLOCK_REASON: &str = "STOP: Use tracedecay MCP tools \
(tracedecay_context, tracedecay_search, tracedecay_callees, tracedecay_callers, \
tracedecay_impact, tracedecay_files, tracedecay_affected) instead of agents for \
code research. TraceDecay is faster and more precise for symbol relationships, \
call paths, and code structure. Only use agents for code exploration if you \
have already tried tracedecay and it cannot answer the question.";

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

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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
    if claude::is_code_research_prompt(&text) {
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

pub(crate) fn read_stdin_to_string() -> std::io::Result<String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    Ok(input)
}
