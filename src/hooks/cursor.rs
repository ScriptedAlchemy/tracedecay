//! Cursor hook adapters.
//!
//! These handlers only submit a bounded native event to the daemon's durable
//! admission boundary. They never open a store, inspect repository contents,
//! run sync, ingest a transcript, or invoke a model on Cursor's hook deadline.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::post_tool_use::{captured_tool_output, trusted_tool_failure};
use super::tool_hints::{HintAgent, ToolHint, ToolHintInput, decide_hint};
use super::{event_session_id, format_tool_hint, read_hook_event, rel_under_root, text_field};

/// The Cursor configuration permits five seconds, but host work is limited to
/// daemon admission so the ACK stays within the 25ms p95 objective.
const CURSOR_ADMISSION_BUDGET: Duration = Duration::from_millis(20);

/// The daemon owns transcript catch-up limits. This retained public constant
/// is consumed by daemon runtime ports and does not cause host-side ingest.
pub const CURSOR_CATCH_UP_INGEST_MAX_BYTES: u64 =
    crate::sessions::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;

const CURSOR_FILE_PATH_FIELDS: &[&str] = &[
    "file_path",
    "filePath",
    "path",
    "target_file",
    "targetFile",
    "relative_workspace_path",
    "relativeWorkspacePath",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CursorAdmissionReceipt {
    Deferred,
    Unavailable,
}

/// Submit one privacy-filtered event to the daemon and bound the complete
/// client wait. The daemon validates it again before durable admission.
pub(super) async fn submit_cursor_event(
    event_name: &str,
    event_json: &str,
) -> CursorAdmissionReceipt {
    let Some(project_root) = cursor_daemon_project_root(event_json) else {
        return CursorAdmissionReceipt::Unavailable;
    };
    let Some(event) = cursor_daemon_payload(event_name, event_json) else {
        return CursorAdmissionReceipt::Unavailable;
    };
    let args = serde_json::json!({
        "action": "cursor_event",
        "event_name": event_name,
        "event": event,
    });
    let submitted = tokio::time::timeout(
        CURSOR_ADMISSION_BUDGET,
        super::daemon_hook_action(Some(&project_root), args, None),
    )
    .await;
    match submitted {
        Ok(Ok(receipt))
            if matches!(
                receipt.get("status").and_then(Value::as_str),
                Some("accepted" | "deferred")
            ) =>
        {
            CursorAdmissionReceipt::Deferred
        }
        _ => CursorAdmissionReceipt::Unavailable,
    }
}

/// Select the same content-free fields the daemon persists before this event
/// crosses the MCP tool boundary, where generic request accounting may observe
/// arguments. The only extra routing material is an edit path, which the
/// daemon immediately converts to a repo-relative scheduler hint and does not
/// write into the durable event body.
fn cursor_daemon_payload(event_name: &str, event_json: &str) -> Option<Value> {
    let raw = serde_json::from_str::<Value>(event_json).ok()?;
    let mut event = crate::mcp::tools::handlers::hook_runtime::cursor_event::sanitize_cursor_event(
        event_name, &raw,
    )
    .ok()?
    .event;
    if event_name == "afterFileEdit" {
        let paths = cursor_event_file_paths(&raw);
        if !paths.is_empty() {
            event
                .as_object_mut()?
                .insert("file_paths".to_owned(), Value::Array(paths));
        }
    }
    Some(event)
}

fn cursor_event_file_paths(event: &Value) -> Vec<Value> {
    let mut paths = Vec::new();
    let mut retain = |path: Option<&str>| {
        let Some(path) = path else {
            return;
        };
        if path.is_empty()
            || path.len() > 4 * 1024
            || path.as_bytes().contains(&0)
            || path.chars().any(char::is_control)
            || paths.iter().any(|seen| seen.as_str() == Some(path))
            || paths.len() >= 64
        {
            return;
        }
        paths.push(Value::String(path.to_owned()));
    };
    retain(event.get("file_path").and_then(Value::as_str));
    for edit in event
        .get("edits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        retain(edit.get("file_path").and_then(Value::as_str));
    }
    paths
}

/// Locate a routing hint without touching the filesystem. The daemon owns
/// authoritative project resolution once the event reaches its admission port.
fn cursor_daemon_project_root(event_json: &str) -> Option<PathBuf> {
    let parsed = serde_json::from_str::<Value>(event_json).ok()?;
    cursor_routing_root_from_parsed_event(&parsed)
}

fn cursor_routing_root_from_parsed_event(event: &Value) -> Option<PathBuf> {
    let workspace_roots = event
        .get("workspace_roots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let cwd = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(cwd) = cwd {
        if let Some(root) = workspace_roots
            .iter()
            .filter(|root| cwd.starts_with(root))
            .max_by_key(|root| root.as_os_str().as_encoded_bytes().len())
        {
            return Some(root.clone());
        }
        return Some(cwd);
    }
    workspace_roots.into_iter().next().or_else(|| {
        crate::config::brand_env("PROJECT_ROOT")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    })
}

async fn submit_and_discard(event_name: &str, event: &str) {
    let _ = submit_cursor_event(event_name, event).await;
}

/// Cursor `subagentStart` is always fail-open.
pub async fn hook_cursor_subagent_start() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("subagentStart", &event).await;
    0
}

pub async fn hook_cursor_post_tool_use() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("postToolUse", &event).await;
    0
}

pub async fn hook_cursor_before_submit_prompt() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("beforeSubmitPrompt", &event).await;
    println!("{}", cursor_before_submit_prompt_json(None));
    0
}

pub async fn hook_cursor_pre_compact() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("preCompact", &event).await;
    println!("{}", serde_json::json!({}));
    0
}

pub async fn hook_cursor_after_file_edit() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("afterFileEdit", &event).await;
    0
}

pub async fn hook_cursor_session_start() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("sessionStart", &event).await;
    println!("{}", cursor_session_start_json(None, ""));
    0
}

pub async fn hook_cursor_session_end() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("sessionEnd", &event).await;
    println!("{}", serde_json::json!({}));
    0
}

pub async fn hook_cursor_after_shell() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("afterShellExecution", &event).await;
    0
}

pub async fn hook_cursor_workspace_open() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("workspaceOpen", &event).await;
    println!("{}", serde_json::json!({}));
    0
}

pub async fn hook_cursor_stop() -> i32 {
    let event = read_hook_event!();
    submit_and_discard("stop", &event).await;
    println!("{}", serde_json::json!({}));
    0
}

/// Builds the fail-open Cursor prompt response. Guidance is produced only by
/// daemon-owned asynchronous work and is never awaited by this hook.
pub fn cursor_before_submit_prompt_json(additional_context: Option<&str>) -> String {
    match additional_context.filter(|text| !text.trim().is_empty()) {
        Some(context) => {
            serde_json::json!({ "continue": true, "additional_context": context }).to_string()
        }
        None => serde_json::json!({ "continue": true }).to_string(),
    }
}

/// Cursor subagent startup has no denial path.
pub fn evaluate_cursor_subagent_start(_event_json: &str) -> Option<String> {
    None
}

/// Pure hint formatting retained for non-hook callers. The live Cursor hook
/// does not emit it because durable daemon admission is the only fast path.
pub fn evaluate_cursor_post_tool_use(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_tool_hint_input(&parsed))?;
    Some(format_cursor_hint(&hint))
}

/// Compatibility helper for callers that formerly requested a persisted hint.
/// Persistence now belongs to daemon replay; this pure surface performs no
/// local store access.
pub fn cursor_post_tool_use_decision(event_json: &str) -> Option<String> {
    evaluate_cursor_post_tool_use(event_json)
}

fn format_cursor_hint(hint: &ToolHint) -> String {
    serde_json::json!({
        "additional_context": format_tool_hint(hint),
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
        edit_text: None,
        hints_enabled: true,
    }
}

fn cursor_after_file_edit_hint_input(parsed: &Value) -> ToolHintInput {
    ToolHintInput {
        agent: HintAgent::Cursor,
        session_id: event_session_id(parsed),
        tool_name: Some("Edit".to_owned()),
        command: None,
        prompt: None,
        subagent_type: None,
        file_path: text_field(parsed, CURSOR_FILE_PATH_FIELDS),
        captured_output: None,
        trusted_failure: false,
        edit_text: cursor_after_file_edit_new_text(parsed),
        hints_enabled: true,
    }
}

fn cursor_after_file_edit_new_text(parsed: &Value) -> Option<String> {
    let joined = parsed
        .get("edits")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|edit| edit.get("new_string").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

pub fn evaluate_cursor_after_file_edit(event_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    let hint = decide_hint(&cursor_after_file_edit_hint_input(&parsed))?;
    Some(format_cursor_hint(&hint))
}

pub fn cursor_project_root_from_event(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    cursor_routing_root_from_parsed_event(&parsed)
}

pub(super) fn cursor_project_root_from_parsed_event(parsed: &Value) -> Option<PathBuf> {
    cursor_routing_root_from_parsed_event(parsed)
}

pub fn cursor_after_file_edit_rel_paths(event_json: &str, project_root: &Path) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut consider = |path: Option<&str>| {
        if let Some(path) = path
            && let Some(relative) = rel_under_root(project_root, Path::new(path))
            && !paths.contains(&relative)
        {
            paths.push(relative);
        }
    };
    consider(parsed.get("file_path").and_then(Value::as_str));
    for edit in parsed
        .get("edits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        consider(edit.get("file_path").and_then(Value::as_str));
    }
    paths
}

pub fn cursor_session_start_json(project_root: Option<&Path>, additional_context: &str) -> String {
    let mut env = serde_json::Map::new();
    if let Some(root) = project_root {
        env.insert(
            "TRACEDECAY_PROJECT_ROOT".to_owned(),
            Value::String(root.to_string_lossy().into_owned()),
        );
    }
    serde_json::json!({
        "additional_context": additional_context,
        "env": Value::Object(env),
    })
    .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cursor_hooks_submit_one_fast_daemon_receipt() {
        let _lock = crate::hooks::lock_test_env();
        let daemon = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "cursor_event",
            "status": "deferred",
        })]);
        let event = serde_json::json!({
            "cwd": "/workspace/project",
            "workspace_roots": ["/workspace/project"],
            "prompt": "private host text stays out of the durable record",
        })
        .to_string();

        let started = Instant::now();
        assert_eq!(
            submit_cursor_event("beforeSubmitPrompt", &event).await,
            CursorAdmissionReceipt::Deferred
        );
        assert!(
            started.elapsed() < Duration::from_millis(25),
            "Cursor admission must not inspect the project before its receipt"
        );
        let calls = daemon.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["action"], "cursor_event");
        assert_eq!(calls[0].1["event_name"], "beforeSubmitPrompt");
        assert!(calls[0].1["event"].get("prompt").is_none());
    }

    #[test]
    fn cursor_before_submit_prompt_keeps_submission_fail_open() {
        let parsed: Value = serde_json::from_str(&cursor_before_submit_prompt_json(None)).unwrap();
        assert_eq!(parsed["continue"], Value::Bool(true));
        assert!(parsed.get("additional_context").is_none());
    }
}
