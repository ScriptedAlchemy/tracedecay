//! Shared post-tool-use event helpers.

use std::path::Path;

use serde_json::Value;
use tracedecay_hooks::DaemonHookEvent;

/// Whether a write event that named no in-project path is still worth sending.
///
/// Codex `PostToolUse` carries nothing but the paths, so an empty list is not
/// an event. Kiro's `postToolUse` also reports the session `cwd`, which the
/// daemon uses for worktree and branch tracking, so it is sent either way.
pub(super) enum EmptyPathPolicy {
    Skip,
    Send,
}

/// The tail every host's post-write daemon notification shares: gate on an
/// initialized store, resolve the edited project-relative paths, apply the
/// host's empty-path policy, and send one route-annotated event.
///
/// `rel_paths` is a closure because path extraction is the one genuinely
/// host-specific step (Claude reads `tool_input.file_path`, Codex parses an
/// `apply_patch` envelope, Cursor reads `file_path` plus `edits[]`, Kiro sweeps
/// several path-shaped keys) and because it must not run for a project with no
/// store, exactly as before.
pub(super) async fn notify_edited_paths(
    project_root: &Path,
    parsed: &Value,
    rel_paths: impl FnOnce() -> Vec<String>,
    build_event: impl FnOnce(Vec<String>) -> DaemonHookEvent,
    empty_paths: EmptyPathPolicy,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    if !crate::tracedecay::TraceDecay::is_initialized(project_root) {
        return;
    }
    let rels = rel_paths();
    if rels.is_empty() && matches!(empty_paths, EmptyPathPolicy::Skip) {
        return;
    }
    super::notify_hook_event_with_optional_telemetry(
        project_root,
        build_event(rels).with_route(Some(super::hook_route_metadata_from_parsed(
            parsed,
            project_root,
        ))),
        telemetry,
    )
    .await;
}

#[cfg(test)]
fn tool_input_command(parsed: &Value) -> &str {
    parsed
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The `tool_input.command` string, if any (Bash's shell command). Empty when
/// absent. Shared with the Claude hint path so it reads the command from the
/// same place the daemon-sync path does.
#[cfg(test)]
pub(super) fn tool_input_command_str(parsed: &Value) -> Option<String> {
    let command = tool_input_command(parsed);
    (!command.is_empty()).then(|| command.to_string())
}

/// Host-captured tool output from a post-tool event. Only top-level outcome
/// fields are accepted: command text under `tool_input` is agent-controlled
/// and must never masquerade as execution evidence.
pub(super) fn captured_tool_output(parsed: &Value) -> Option<String> {
    ["tool_response", "toolResponse", "tool_output", "toolOutput"]
        .into_iter()
        .find_map(|key| parsed.get(key))
        .and_then(json_value_text)
}

fn json_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(_) | Value::Array(_) => serde_json::to_string(value).ok(),
        _ => None,
    }
}

/// Whether the host, rather than command text, authenticated this as a failed
/// tool execution. Supports Claude/Codex's named failure event and Cursor's
/// documented failure envelope. Interrupts, timeouts, and denials are not
/// compiler failures.
pub(super) fn trusted_tool_failure(parsed: &Value) -> bool {
    if parsed
        .get("is_interrupt")
        .or_else(|| parsed.get("isInterrupt"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return false;
    }

    let failure_type = parsed
        .get("failure_type")
        .or_else(|| parsed.get("failureType"))
        .and_then(Value::as_str);
    if failure_type.is_some_and(|kind| !kind.eq_ignore_ascii_case("error")) {
        return false;
    }

    let error = parsed
        .get("error")
        .or_else(|| parsed.get("error_message"))
        .or_else(|| parsed.get("errorMessage"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    let Some(error) = error else {
        return false;
    };
    let lower_error = error.to_ascii_lowercase();
    if ["timed out", "timeout", "permission denied", "interrupted"]
        .iter()
        .any(|marker| lower_error.contains(marker))
    {
        return false;
    }

    let named_failure = parsed
        .get("hook_event_name")
        .or_else(|| parsed.get("hookEventName"))
        .and_then(Value::as_str)
        .is_some_and(|name| normalize_event_name(name) == "posttoolusefailure");
    let cursor_failure = failure_type.is_some_and(|kind| kind.eq_ignore_ascii_case("error"));
    named_failure || cursor_failure
}

pub(super) fn is_post_tool_use_failure_event(parsed: &Value) -> bool {
    parsed
        .get("hook_event_name")
        .or_else(|| parsed.get("hookEventName"))
        .and_then(Value::as_str)
        .is_some_and(|name| normalize_event_name(name) == "posttoolusefailure")
}

fn normalize_event_name(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}
