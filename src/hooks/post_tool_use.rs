//! Shared post-tool-use pipeline for the Claude Code and Codex hooks.
//!
//! Both agents send the same shaped event (Codex adopted Claude's hook
//! schema): parse the event JSON, read `tool_name`, resolve the session `cwd`
//! and project root, check for an initialized store, then notify the daemon
//! about edits (targeted sync) or content-free shell completion observations.
//! Each agent supplies a [`PostToolUseSpec`] with its own tool-name predicates
//! and edit-path extractor.

use std::path::Path;

use serde_json::Value;
use tracedecay_hooks::{DaemonHookEvent, HookAgent};

use super::{codex, event_cwd_from_parsed, hook_route_metadata_from_parsed, rel_under_root};

/// Claude Code tools whose `PostToolUse` events the hook consumes. The
/// installer's `PostToolUse` matcher is derived from this list so the matcher
/// and the handler predicates can never drift.
pub const CLAUDE_POST_TOOL_USE_EDIT_TOOLS: &[&str] =
    &["Edit", "MultiEdit", "Write", "NotebookEdit"];
pub const CLAUDE_POST_TOOL_USE_SHELL_TOOLS: &[&str] = &["Bash"];

/// Native Claude Code search/read tools whose `PostToolUse` events we observe
/// only to emit a soft tracedecay hint. Unlike the edit/shell lists these do
/// not drive daemon sync — they are pure hint surfaces so native `Grep`,
/// `Glob`, and `Read` usage becomes visible to telemetry and hintable.
/// `Bash` is deliberately absent here: it is already matched as a shell tool
/// and its hint is derived from the command text on the shell path.
pub const CLAUDE_POST_TOOL_USE_HINT_TOOLS: &[&str] = &["Grep", "Glob", "Read"];

/// `Edit|MultiEdit|Write|NotebookEdit|Grep|Glob|Read|Bash` — the Claude
/// settings.json matcher, derived from the edit, hint, and shell tool lists so
/// the matcher and the handler predicates can never drift.
pub fn claude_post_tool_use_matcher() -> String {
    CLAUDE_POST_TOOL_USE_EDIT_TOOLS
        .iter()
        .chain(CLAUDE_POST_TOOL_USE_HINT_TOOLS)
        .chain(CLAUDE_POST_TOOL_USE_SHELL_TOOLS)
        .copied()
        .collect::<Vec<_>>()
        .join("|")
}

/// Per-agent parameterization of the shared post-tool-use pipeline.
pub(crate) struct PostToolUseSpec {
    pub agent: HookAgent,
    pub is_edit_tool: fn(&str) -> bool,
    pub is_shell_tool: fn(&str) -> bool,
    /// (parsed event, session cwd, project root) -> project-relative paths
    pub edit_rel_paths: fn(&Value, &Path, &Path) -> Vec<String>,
}

pub(crate) const CLAUDE_POST_TOOL_USE_SPEC: PostToolUseSpec = PostToolUseSpec {
    agent: HookAgent::Claude,
    is_edit_tool: is_claude_edit_tool,
    is_shell_tool: is_claude_bash_tool,
    edit_rel_paths: claude_edit_rel_paths,
};

pub(crate) const CODEX_POST_TOOL_USE_SPEC: PostToolUseSpec = PostToolUseSpec {
    agent: HookAgent::Codex,
    is_edit_tool: is_codex_edit_tool,
    is_shell_tool: is_codex_bash_tool,
    edit_rel_paths: codex_edit_rel_paths,
};

/// Shared post-tool-use daemon notification. Fail-open and silent.
///
/// Shell completion is forwarded without command text. It remains an
/// observation and cannot authorize branch, worktree, or sync planning.
pub(crate) async fn notify_post_tool_use(spec: &PostToolUseSpec, parsed: &Value) {
    notify_post_tool_use_inner(spec, parsed, None).await;
}

pub(crate) async fn notify_post_tool_use_with_telemetry(
    spec: &PostToolUseSpec,
    parsed: &Value,
    telemetry: &super::analytics::HookTimingSpan,
) {
    notify_post_tool_use_inner(spec, parsed, Some(telemetry)).await;
}

/// Takes the event already parsed: the calling handler parsed it to decide the
/// failure shape, resolve the root, and record analytics, and a `PostToolUse`
/// fires on every tool call.
async fn notify_post_tool_use_inner(
    spec: &PostToolUseSpec,
    parsed: &Value,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let tool_name = parsed
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(cwd) = event_cwd_from_parsed(parsed) else {
        return;
    };
    let Some(root) = crate::config::discover_project_root_with_identity(&cwd).await else {
        return;
    };
    if !crate::tracedecay::TraceDecay::is_initialized(&root) {
        return;
    }

    if (spec.is_edit_tool)(tool_name) {
        let rels = (spec.edit_rel_paths)(parsed, &cwd, &root);
        if rels.is_empty() {
            return;
        }
        super::notify_hook_event_with_optional_telemetry(
            &root,
            DaemonHookEvent::post_tool_use_edit(spec.agent, rels, cwd)
                .with_route(Some(hook_route_metadata_from_parsed(parsed, &root))),
            telemetry,
        )
        .await;
    } else if (spec.is_shell_tool)(tool_name) {
        super::notify_hook_event_with_optional_telemetry(
            &root,
            DaemonHookEvent::post_tool_use_shell(spec.agent, cwd)
                .with_route(Some(hook_route_metadata_from_parsed(parsed, &root))),
            telemetry,
        )
        .await;
    }
}

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

/// The text an edit tool adds, if any: `Write`'s `content`, `Edit`'s
/// `new_string`, the joined `MultiEdit` `edits[].new_string`s, or
/// `NotebookEdit`'s `new_source`. Used only by the Claude hint surface to detect
/// a newly added function body; it never drives daemon sync. O(len): reads or
/// concatenates existing JSON string fields without parsing code.
pub(super) fn tool_input_edit_text(parsed: &Value) -> Option<String> {
    let ti = parsed.get("tool_input")?;
    for key in ["content", "new_string", "new_source"] {
        if let Some(text) = ti
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(text.to_string());
        }
    }
    // MultiEdit: join each replacement's new text so a body split across edits
    // still reaches the line/keyword heuristic.
    let joined = ti
        .get("edits")
        .and_then(Value::as_array)
        .map(|edits| {
            edits
                .iter()
                .filter_map(|edit| edit.get("new_string").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|joined| !joined.is_empty())?;
    Some(joined)
}

/// The `tool_input.file_path` string, if any (Grep/Glob/Read/Edit targets).
/// Empty when absent.
pub(super) fn tool_input_file_path_str(parsed: &Value) -> Option<String> {
    parsed
        .get("tool_input")
        .and_then(|ti| ti.get("file_path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

pub(super) fn is_claude_edit_tool(tool_name: &str) -> bool {
    CLAUDE_POST_TOOL_USE_EDIT_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(tool_name))
}

fn is_claude_bash_tool(tool_name: &str) -> bool {
    CLAUDE_POST_TOOL_USE_SHELL_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(tool_name))
}

/// True when `tool_name` is one of the native search/read tools we observe
/// only to emit a soft tracedecay hint ([`CLAUDE_POST_TOOL_USE_HINT_TOOLS`]).
pub(super) fn is_claude_hint_tool(tool_name: &str) -> bool {
    CLAUDE_POST_TOOL_USE_HINT_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(tool_name))
}

fn is_codex_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "apply_patch" | "edit" | "write"
    )
}

fn is_codex_bash_tool(tool_name: &str) -> bool {
    matches!(tool_name.to_ascii_lowercase().as_str(), "bash" | "shell")
}

/// Extracts the project-relative path edited by a Claude edit tool.
///
/// Claude's `Edit`/`Write`/`MultiEdit` put the target in
/// `tool_input.file_path`; `NotebookEdit` uses `tool_input.notebook_path`.
/// Paths are usually absolute but are resolved against the session `cwd`
/// when relative. Paths outside `project_root` are skipped.
fn claude_edit_rel_paths(parsed: &Value, cwd: &Path, project_root: &Path) -> Vec<String> {
    ["file_path", "notebook_path"]
        .iter()
        .filter_map(|key| {
            parsed
                .get("tool_input")
                .and_then(|ti| ti.get(*key))
                .and_then(Value::as_str)
        })
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| {
            let candidate = Path::new(raw);
            let abs = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                cwd.join(candidate)
            };
            rel_under_root(project_root, &abs)
        })
        .collect()
}

/// Extracts the project-relative paths edited by a Codex edit tool. Codex
/// sends the `apply_patch` envelope as `tool_input.command`; the per-file
/// parsing lives in [`codex::codex_apply_patch_rel_paths`].
fn codex_edit_rel_paths(parsed: &Value, cwd: &Path, project_root: &Path) -> Vec<String> {
    codex::codex_apply_patch_rel_paths(tool_input_command(parsed), cwd, project_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_edit_tools_are_recognized_case_insensitively() {
        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit", "write"] {
            assert!(is_claude_edit_tool(tool), "{tool} should count as an edit");
        }
        assert!(!is_claude_edit_tool("Bash"));
        assert!(!is_claude_edit_tool("Read"));
        assert!(is_claude_bash_tool("Bash"));
        assert!(!is_claude_bash_tool("Edit"));
    }

    #[test]
    fn claude_hint_tools_are_recognized_and_disjoint_from_edit_and_shell() {
        for tool in ["Grep", "Glob", "Read", "read", "GREP"] {
            assert!(is_claude_hint_tool(tool), "{tool} should count as a hint");
            assert!(!is_claude_edit_tool(tool), "{tool} is not an edit tool");
            assert!(!is_claude_bash_tool(tool), "{tool} is not a shell tool");
        }
        assert!(!is_claude_hint_tool("Edit"));
        assert!(!is_claude_hint_tool("Bash"));
    }

    #[test]
    fn claude_post_tool_use_matcher_derives_from_tool_lists() {
        assert_eq!(
            claude_post_tool_use_matcher(),
            "Edit|MultiEdit|Write|NotebookEdit|Grep|Glob|Read|Bash"
        );
        for tool in CLAUDE_POST_TOOL_USE_EDIT_TOOLS {
            assert!(is_claude_edit_tool(tool), "{tool} should count as an edit");
            assert!(!is_claude_bash_tool(tool));
            assert!(!is_claude_hint_tool(tool));
        }
        for tool in CLAUDE_POST_TOOL_USE_HINT_TOOLS {
            assert!(is_claude_hint_tool(tool), "{tool} should count as a hint");
            assert!(!is_claude_edit_tool(tool));
            assert!(!is_claude_bash_tool(tool));
        }
        for tool in CLAUDE_POST_TOOL_USE_SHELL_TOOLS {
            assert!(is_claude_bash_tool(tool), "{tool} should count as shell");
            assert!(!is_claude_edit_tool(tool));
            assert!(!is_claude_hint_tool(tool));
        }
        // Every matcher alternative maps to exactly one predicate.
        for tool in claude_post_tool_use_matcher().split('|') {
            let matches = [
                is_claude_edit_tool(tool),
                is_claude_hint_tool(tool),
                is_claude_bash_tool(tool),
            ]
            .into_iter()
            .filter(|hit| *hit)
            .count();
            assert_eq!(matches, 1, "{tool} must map to exactly one predicate");
        }
    }

    #[test]
    fn claude_edit_rel_paths_resolves_file_path_against_project_root() {
        let root = Path::new("/repo");
        let cwd = Path::new("/repo/sub");
        let event = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/lib.rs" }
        });
        assert_eq!(
            claude_edit_rel_paths(&event, cwd, root),
            vec!["src/lib.rs".to_string()]
        );

        // Relative paths resolve against the session cwd.
        let event = serde_json::json!({
            "tool_name": "Write",
            "tool_input": { "file_path": "module.rs" }
        });
        assert_eq!(
            claude_edit_rel_paths(&event, cwd, root),
            vec!["sub/module.rs".to_string()]
        );

        // NotebookEdit uses notebook_path.
        let event = serde_json::json!({
            "tool_name": "NotebookEdit",
            "tool_input": { "notebook_path": "/repo/analysis.ipynb" }
        });
        assert_eq!(
            claude_edit_rel_paths(&event, cwd, root),
            vec!["analysis.ipynb".to_string()]
        );

        // Paths outside the project root are skipped.
        let event = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/elsewhere/other.rs" }
        });
        assert!(claude_edit_rel_paths(&event, cwd, root).is_empty());
    }
}
