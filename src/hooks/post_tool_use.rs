//! Shared post-tool-use pipeline for the Claude Code and Codex hooks.
//!
//! Both agents send the same shaped event (Codex adopted Claude's hook
//! schema): parse the event JSON, read `tool_name`, resolve the session `cwd`
//! and project root, check for an initialized store, then notify the daemon
//! about edits (targeted sync) or shell commands (branch tracking /
//! coalesced sync). Each agent supplies a [`PostToolUseSpec`] with its own
//! tool-name predicates and edit-path extractor.

use std::path::Path;

use serde_json::Value;

use super::{codex, event_cwd_from_parsed, rel_under_root};

/// Claude Code tools whose `PostToolUse` events the hook consumes. The
/// installer's `PostToolUse` matcher is derived from this list so the matcher
/// and the handler predicates can never drift.
pub const CLAUDE_POST_TOOL_USE_EDIT_TOOLS: &[&str] =
    &["Edit", "MultiEdit", "Write", "NotebookEdit"];
pub const CLAUDE_POST_TOOL_USE_SHELL_TOOLS: &[&str] = &["Bash"];

/// `Edit|MultiEdit|Write|NotebookEdit|Bash` — the Claude settings.json matcher.
pub fn claude_post_tool_use_matcher() -> String {
    CLAUDE_POST_TOOL_USE_EDIT_TOOLS
        .iter()
        .chain(CLAUDE_POST_TOOL_USE_SHELL_TOOLS)
        .copied()
        .collect::<Vec<_>>()
        .join("|")
}

/// Per-agent parameterization of the shared post-tool-use pipeline.
pub(crate) struct PostToolUseSpec {
    pub agent: crate::daemon::HookAgent,
    pub is_edit_tool: fn(&str) -> bool,
    pub is_shell_tool: fn(&str) -> bool,
    /// (parsed event, session cwd, project root) -> project-relative paths
    pub edit_rel_paths: fn(&Value, &Path, &Path) -> Vec<String>,
}

pub(crate) const CLAUDE_POST_TOOL_USE_SPEC: PostToolUseSpec = PostToolUseSpec {
    agent: crate::daemon::HookAgent::Claude,
    is_edit_tool: is_claude_edit_tool,
    is_shell_tool: is_claude_bash_tool,
    edit_rel_paths: claude_edit_rel_paths,
};

pub(crate) const CODEX_POST_TOOL_USE_SPEC: PostToolUseSpec = PostToolUseSpec {
    agent: crate::daemon::HookAgent::Codex,
    is_edit_tool: is_codex_edit_tool,
    is_shell_tool: is_codex_bash_tool,
    edit_rel_paths: codex_edit_rel_paths,
};

/// Shared post-tool-use daemon notification. Fail-open and silent.
///
/// Behavior note: empty shell commands are skipped for both agents. The
/// Claude path always did this; the Codex path previously forwarded empty
/// commands (which produced no-op daemon events), so the two were unified on
/// the safer skip-empty behavior.
pub(crate) async fn notify_post_tool_use(spec: &PostToolUseSpec, event_json: &str) {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return;
    };
    let tool_name = parsed
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(cwd) = event_cwd_from_parsed(&parsed) else {
        return;
    };
    let Some(root) = crate::config::discover_project_root(&cwd)
        .or_else(|| crate::worktree::git_worktree_root(&cwd))
    else {
        return;
    };
    if !crate::tracedecay::TraceDecay::has_initialized_store(&root).await {
        return;
    }

    if (spec.is_edit_tool)(tool_name) {
        let rels = (spec.edit_rel_paths)(&parsed, &cwd, &root);
        if rels.is_empty() {
            return;
        }
        crate::daemon::notify_hook_event(
            &root,
            crate::daemon::DaemonHookEvent::post_tool_use_edit(spec.agent, rels, cwd),
        )
        .await;
    } else if (spec.is_shell_tool)(tool_name) {
        let command = tool_input_command(&parsed);
        if command.is_empty() {
            return;
        }
        crate::daemon::notify_hook_event(
            &root,
            crate::daemon::DaemonHookEvent::post_tool_use_shell(
                spec.agent,
                command.to_string(),
                cwd,
            ),
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

fn is_claude_edit_tool(tool_name: &str) -> bool {
    CLAUDE_POST_TOOL_USE_EDIT_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(tool_name))
}

fn is_claude_bash_tool(tool_name: &str) -> bool {
    CLAUDE_POST_TOOL_USE_SHELL_TOOLS
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
    fn claude_post_tool_use_matcher_derives_from_tool_lists() {
        assert_eq!(
            claude_post_tool_use_matcher(),
            "Edit|MultiEdit|Write|NotebookEdit|Bash"
        );
        for tool in CLAUDE_POST_TOOL_USE_EDIT_TOOLS {
            assert!(is_claude_edit_tool(tool), "{tool} should count as an edit");
            assert!(!is_claude_bash_tool(tool));
        }
        for tool in CLAUDE_POST_TOOL_USE_SHELL_TOOLS {
            assert!(is_claude_bash_tool(tool), "{tool} should count as shell");
            assert!(!is_claude_edit_tool(tool));
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
