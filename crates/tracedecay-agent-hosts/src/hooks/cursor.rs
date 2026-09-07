//! Cursor hook handlers: subagent/tool-use steering, transcript ingest,
//! post-edit / post-shell daemon notifications, and session lifecycle
//! context.
//!
//! Cursor expects Cursor-shaped stdout, separate from Claude, Codex, and Kiro.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ports::hook_runtime::HookRuntimeV1;

use super::post_tool_use::{captured_tool_output, trusted_tool_failure};
use super::tool_hints::{HintAgent, ToolHint, ToolHintInput, decide_hint};
use super::{
    deduped_project_hint_with_id, event_session_id, format_tool_hint, mint_hint_id,
    nearest_project_like_root, read_hook_event, record_hint_analytics, record_hook_invoked_parsed,
    text_field,
};

/// Largest transcript tail a low-priority Cursor catch-up hook will read.
/// Oversized backlogs stay queued instead of blocking hook execution.
pub const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 =
    tracedecay_sessions::runtime::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

const CURSOR_FILE_PATH_FIELDS: &[&str] = &[
    "file_path",
    "filePath",
    "path",
    "target_file",
    "targetFile",
    "relative_workspace_path",
    "relativeWorkspacePath",
];

/// Cursor `postToolUse` hook handler.
///
/// Emits soft `additional_context` hints steering exploration tools (Grep,
/// Glob, Read, semantic search, shell `rg`) toward tracedecay MCP tools.
/// Registered on `postToolUse` rather than `preToolUse` because Cursor's
/// documented `preToolUse` output schema has no context-injection field —
/// `additional_context` is only honored on `postToolUse`. The hook runs
/// unmatched (the docs enumerate no matcher value for Cursor's semantic
/// search tool) and irrelevant tools fail open with no output. Each hint
/// category is emitted at most once per session via
/// [`super::tool_hints::ToolHintDedupe`] persisted under `.tracedecay/`.
#[hotpath::measure(future = true, label = "hosts.hooks.cursor.post_tool_use")]
pub async fn hook_cursor_post_tool_use(runtime: &HookRuntimeV1) -> i32 {
    let event = read_hook_event!();
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = cursor_project_root_from_parsed_event_with_identity(runtime, &parsed).await;
    let _hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HintAgent::Cursor,
        "postToolUse",
        &event,
        &parsed,
    );
    if let Some(decision) = cursor_post_tool_use_decision(runtime, &event)
        && !super::write_hook_output(
            runtime,
            root.as_deref(),
            tracedecay_hooks::HookHostV1::CursorDesktop,
            &event,
            &decision,
            Some(&_hook_telemetry),
        )
        .await
    {
        return 1;
    }
    0
}

#[hotpath::measure(future = true, label = "hosts.hooks.cursor.session_start")]
pub async fn hook_cursor_session_start(runtime: &HookRuntimeV1) -> i32 {
    let event = read_hook_event!();
    let (root, output) = cursor_session_start_response(runtime, &event).await;
    if !super::write_hook_output(
        runtime,
        root.as_deref(),
        tracedecay_hooks::HookHostV1::CursorDesktop,
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
async fn cursor_session_start_response(
    runtime: &HookRuntimeV1,
    event: &str,
) -> (Option<PathBuf>, String) {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let root = cursor_project_root_from_parsed_event_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HintAgent::Cursor,
        "sessionStart",
        event,
        &parsed,
    );
    let guidance = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_hooks::HookHostV1::CursorDesktop,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    let output = cursor_session_start_json(root.as_deref(), guidance.as_deref().unwrap_or(""));
    (root, output)
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

fn format_cursor_post_tool_use_decision(hint: &ToolHint) -> String {
    serde_json::json!({
        "additional_context": format_tool_hint(hint),
    })
    .to_string()
}

fn prepare_cursor_post_tool_use_hint(event_json: &str) -> Option<(String, ToolHint)> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    let root = cursor_project_root_candidate_from_parsed_event(&parsed);
    let hint_id = mint_hint_id();
    record_hint_analytics(
        root.as_deref(),
        "hint_candidate",
        HintAgent::Cursor,
        event_session_id(&parsed).as_deref(),
        &hint_id,
        &hint,
    );
    Some((hint_id, hint))
}

/// Cursor `postToolUse` hint decision with per-session dedupe persisted under
/// the project's `.tracedecay/` dir.
pub fn cursor_post_tool_use_decision(runtime: &HookRuntimeV1, event_json: &str) -> Option<String> {
    let (hint_id, hint) = prepare_cursor_post_tool_use_hint(event_json)?;
    let hint = deduped_cursor_hint(runtime, event_json, &hint_id, hint)?;
    Some(format_cursor_post_tool_use_decision(&hint))
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
fn cursor_hint_root(
    event_json: &str,
    hint_id: &str,
    hint: &ToolHint,
) -> Option<(PathBuf, Option<String>)> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        record_hint_analytics(
            None,
            "dropped_no_root",
            HintAgent::Cursor,
            None,
            hint_id,
            hint,
        );
        return None;
    };
    let session_id = event_session_id(&parsed);
    let Some(root) = cursor_project_root_candidate_from_parsed_event(&parsed) else {
        record_hint_analytics(
            None,
            "dropped_no_root",
            HintAgent::Cursor,
            session_id.as_deref(),
            hint_id,
            hint,
        );
        return None;
    };
    Some((root, session_id))
}

fn deduped_cursor_hint(
    runtime: &HookRuntimeV1,
    event_json: &str,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    let (root, session_id) = cursor_hint_root(event_json, hint_id, &hint)?;
    if !runtime.is_project_initialized(&root) {
        record_hint_analytics(
            Some(&root),
            "suppressed_uninitialized",
            HintAgent::Cursor,
            session_id.as_deref(),
            hint_id,
            &hint,
        );
        return None;
    }
    deduped_project_hint_with_id(Some(&root), HintAgent::Cursor, session_id, hint_id, hint)
}

pub fn cursor_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_project_root_from_parsed_event(&parsed)
}

fn cursor_project_root_candidate_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    cursor_project_root_from_parsed_event(parsed).or_else(|| {
        cursor_hook_root_candidates(parsed)
            .into_iter()
            .find_map(|candidate| nearest_project_like_root(&candidate))
    })
}

pub(super) fn cursor_project_root_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    let resolved = cursor_hook_root_candidates(parsed)
        .into_iter()
        .find_map(|candidate| crate::config::discover_project_root(&candidate));
    let cwd_root = cursor_hook_cwd(parsed)
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

#[hotpath::measure(future = true, label = "hosts.hooks.cursor.resolve_root")]
async fn cursor_project_root_from_parsed_event_with_identity(
    runtime: &HookRuntimeV1,
    parsed: &Value,
) -> Option<PathBuf> {
    let mut resolved = None;
    for candidate in cursor_hook_root_candidates(parsed) {
        if let Some(root) = runtime.resolve_project_root_with_identity(&candidate).await {
            resolved = Some(root);
            break;
        }
    }
    let cwd_root = match cursor_hook_cwd(parsed) {
        Some(cwd) => runtime.resolve_project_root_with_identity(&cwd).await,
        None => None,
    };
    match (cwd_root, resolved) {
        (Some(cwd_root), Some(resolved)) if !paths_same(&cwd_root, &resolved) => Some(cwd_root),
        (Some(cwd_root), None) => Some(cwd_root),
        (_, other) => other,
    }
}

fn cursor_hook_root_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|seen| seen == &candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(cwd) = cursor_hook_cwd(event) {
        push_unique(cwd);
    }
    if let Ok(project_root) = std::env::var("TRACEDECAY_PROJECT_ROOT") {
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

fn cursor_hook_cwd(event: &Value) -> Option<PathBuf> {
    event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Exposes a known project root as `TRACEDECAY_PROJECT_ROOT` so later session
/// hooks can reuse it.
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
        file_path: text_field(tool_input, CURSOR_FILE_PATH_FIELDS)
            .or_else(|| text_field(parsed, CURSOR_FILE_PATH_FIELDS)),
        captured_output: captured_tool_output(parsed),
        trusted_failure: trusted_tool_failure(parsed),
        // Cursor's `postToolUse` edit payload carries only the target
        // `file_path`, not the applied edit body (confirmed by the recorded
        // fixtures in `tests/hooks_lsp_suite/hooks_test.rs`), so the
        // edit-redundancy nudge cannot run from this surface. The applied edit
        // text only reaches TraceDecay on `afterFileEdit`, where
        // [`cursor_after_file_edit_hint_input`] populates `edit_text` from
        // `edits[].new_string`.
        edit_text: None,
        hints_enabled: true,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn transcript_ingest_forwards_its_budget_to_the_daemon() {
        let _lock = crate::hooks::lock_test_env();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([
            serde_json::json!({ "user_scope": true, "messages_upserted": 2 }),
        ]);
        let event = serde_json::json!({ "session_id": "cursor-budget" }).to_string();

        let runtime = crate::ports::hook_runtime::crate_test_runtime();
        let outcome = crate::hooks::ingest_transcript_for_event(
            &runtime,
            "cursor",
            &event,
            None,
            Some(4_096),
            Duration::from_millis(250),
            None,
        )
        .await;

        assert!(outcome.user_scope);
        assert_eq!(outcome.messages_upserted, 2);
        assert!(outcome.should_schedule_user_review());
        assert!(!outcome.failed);
        assert!(!outcome.timed_out);
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, None);
        assert_eq!(calls[0].1["action"], "ingest_transcript");
        assert_eq!(calls[0].1["provider"], "cursor");
        assert_eq!(calls[0].1["max_new_bytes"], 4_096);
        assert_eq!(calls[0].1["timeout_budget_ms"], 250);
        assert_eq!(calls[0].1["format"], "json");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn transcript_ingest_types_daemon_failure() {
        let _lock = crate::hooks::lock_test_env();
        let _daemon = crate::hooks::TestDaemonHookActionGuard::install([]);
        let event = serde_json::json!({ "session_id": "cursor-fail" }).to_string();

        let runtime = crate::ports::hook_runtime::crate_test_runtime();
        let outcome = crate::hooks::ingest_transcript_for_event(
            &runtime,
            "cursor",
            &event,
            None,
            Some(4_096),
            Duration::from_millis(250),
            None,
        )
        .await;

        assert!(outcome.failed);
        assert!(!outcome.timed_out);
        assert!(!outcome.should_schedule_user_review());
        assert_eq!(outcome.messages_upserted, 0);
    }
}
