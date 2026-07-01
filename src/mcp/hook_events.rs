//! Normalizes daemon hook notifications into typed sync plans.
//!
//! This module owns wire-level hook semantics. The MCP server owns graph side
//! effects such as branch tracking, sync execution, and token-map refreshes.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookAgent {
    Codex,
    Cursor,
    Kiro,
}

impl HookAgent {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "kiro" => Some(Self::Kiro),
            _ => None,
        }
    }

    fn marker_file(self) -> &'static str {
        match self {
            Self::Codex => ".codex_shell_sync_at",
            Self::Cursor => ".cursor_shell_sync_at",
            Self::Kiro => ".kiro_post_tool_sync_at",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookEventKind {
    FileEdit,
    Shell,
    WorkspaceOpen,
    IncrementalSync,
}

impl HookEventKind {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "afterFileEdit" | "postToolUseEdit" => Some(Self::FileEdit),
            "afterShellExecution" | "postToolUseShell" => Some(Self::Shell),
            "workspaceOpen" => Some(Self::WorkspaceOpen),
            "postToolUse" => Some(Self::IncrementalSync),
            _ => None,
        }
    }
}

pub(crate) struct HookEvent {
    pub(crate) agent: HookAgent,
    pub(crate) kind: HookEventKind,
    pub(crate) rel_paths: Vec<String>,
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookEventPlan {
    SyncFiles(Vec<String>),
    AddBranch(String),
    AddBranchAt { root: PathBuf, branch: String },
    SyncCurrentBranch { branch: String, agent: HookAgent },
    DebouncedIncrementalSync(HookAgent),
    Noop,
}

pub(crate) fn parse_hook_event(params: Option<&Value>) -> Option<HookEvent> {
    let event = serde_json::from_value::<crate::daemon::DaemonHookEvent>(params?.clone()).ok()?;
    Some(HookEvent {
        agent: HookAgent::from_wire(&event.agent)?,
        kind: HookEventKind::from_wire(&event.event)?,
        rel_paths: safe_hook_rel_paths(&event.rel_paths),
        command: event.command.filter(|command| !command.is_empty()),
        cwd: event.cwd,
    })
}

pub(crate) fn plan_hook_event(
    event: &HookEvent,
    project_root: &Path,
    current_branch: Option<&str>,
) -> HookEventPlan {
    match event.kind {
        HookEventKind::FileEdit => {
            if event.rel_paths.is_empty() {
                HookEventPlan::Noop
            } else {
                HookEventPlan::SyncFiles(event.rel_paths.clone())
            }
        }
        HookEventKind::Shell => plan_shell_hook_event(event, project_root, current_branch),
        HookEventKind::WorkspaceOpen => current_branch
            .filter(|branch| !branch.is_empty())
            .map(|branch| HookEventPlan::SyncCurrentBranch {
                branch: branch.to_string(),
                agent: event.agent,
            })
            .unwrap_or(HookEventPlan::DebouncedIncrementalSync(event.agent)),
        HookEventKind::IncrementalSync if !event.rel_paths.is_empty() => {
            HookEventPlan::SyncFiles(event.rel_paths.clone())
        }
        HookEventKind::IncrementalSync => HookEventPlan::DebouncedIncrementalSync(event.agent),
    }
}

pub(crate) fn sync_marker_path(data_root: &Path, agent: HookAgent) -> PathBuf {
    data_root.join(agent.marker_file())
}

pub(crate) fn should_run_sync(marker: &Path, now_secs: i64, debounce_secs: i64) -> bool {
    crate::hooks::cursor_should_run_sync(now_secs, read_marker_secs(marker), debounce_secs)
}

pub(crate) fn write_sync_marker(marker: &Path, now_secs: i64) {
    let _ = std::fs::write(marker, now_secs.to_string());
}

fn safe_hook_rel_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| {
            let path_ref = Path::new(path.as_str());
            !path.is_empty()
                && !path_ref.is_absolute()
                && path_ref.components().all(|component| {
                    !matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
        })
        .cloned()
        .collect()
}

fn plan_shell_hook_event(
    event: &HookEvent,
    project_root: &Path,
    current_branch: Option<&str>,
) -> HookEventPlan {
    let Some(command) = event.command.as_deref() else {
        return HookEventPlan::Noop;
    };
    let cwd = event.cwd.as_deref().unwrap_or(project_root);
    if !crate::hooks::cursor_shell_command_targets_project(command, cwd, project_root) {
        return HookEventPlan::Noop;
    }
    match crate::hooks::cursor_shell_sync_plan_with_current_branch(command, current_branch) {
        crate::hooks::CursorShellSyncPlan::BranchAdd(branch) => HookEventPlan::AddBranch(branch),
        crate::hooks::CursorShellSyncPlan::WorktreeBranchAdd {
            branch,
            worktree_path,
        } => HookEventPlan::AddBranchAt {
            root: crate::hooks::resolve_worktree_add_root(command, cwd, &worktree_path),
            branch,
        },
        crate::hooks::CursorShellSyncPlan::IncrementalSync => {
            HookEventPlan::DebouncedIncrementalSync(event.agent)
        }
        crate::hooks::CursorShellSyncPlan::CurrentBranchSync(branch) => {
            HookEventPlan::SyncCurrentBranch {
                branch,
                agent: event.agent,
            }
        }
        crate::hooks::CursorShellSyncPlan::Noop => HookEventPlan::Noop,
    }
}

fn read_marker_secs(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{
        parse_hook_event, plan_hook_event, HookAgent, HookEvent, HookEventKind, HookEventPlan,
    };

    fn parse_or_panic(params: &serde_json::Value) -> HookEvent {
        match parse_hook_event(Some(params)) {
            Some(event) => event,
            None => panic!("hook event should parse"),
        }
    }

    #[test]
    fn parses_agent_and_event_kind_from_hook_notification() {
        let params = json!({
            "agent": "cursor",
            "event": "afterFileEdit",
            "rel_paths": ["src/lib.rs", "../outside.rs", "/tmp/outside.rs", ""]
        });

        let event = parse_or_panic(&params);

        assert_eq!(event.agent, HookAgent::Cursor);
        assert_eq!(event.kind, HookEventKind::FileEdit);
        assert_eq!(event.rel_paths, ["src/lib.rs"]);
    }

    #[test]
    fn maps_shell_and_workspace_events_to_typed_kinds() {
        let shell = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git pull --rebase",
            "cwd": "/tmp/project"
        });
        let workspace = json!({
            "agent": "kiro",
            "event": "workspaceOpen"
        });

        let shell = parse_or_panic(&shell);
        let workspace = parse_or_panic(&workspace);

        assert_eq!(shell.agent, HookAgent::Codex);
        assert_eq!(shell.kind, HookEventKind::Shell);
        assert_eq!(shell.command.as_deref(), Some("git pull --rebase"));
        assert_eq!(workspace.agent, HookAgent::Kiro);
        assert_eq!(workspace.kind, HookEventKind::WorkspaceOpen);
    }

    #[test]
    fn ignores_unknown_hook_event_names() {
        let params = json!({
            "agent": "cursor",
            "event": "futureEvent"
        });

        assert!(parse_hook_event(Some(&params)).is_none());
    }

    #[test]
    fn ignores_unknown_hook_agents() {
        let params = json!({
            "agent": "future-agent",
            "event": "postToolUse"
        });

        assert!(parse_hook_event(Some(&params)).is_none());
    }

    #[test]
    fn plans_file_edit_sync_with_sanitized_paths() {
        let params = json!({
            "agent": "cursor",
            "event": "afterFileEdit",
            "rel_paths": ["src/lib.rs", "../outside.rs"]
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::SyncFiles(vec!["src/lib.rs".to_string()])
        );
    }

    #[test]
    fn plans_incremental_sync_with_paths_as_targeted_sync() {
        let params = json!({
            "agent": "kiro",
            "event": "postToolUse",
            "rel_paths": ["src/lib.rs", "../outside.rs"]
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::SyncFiles(vec!["src/lib.rs".to_string()])
        );
    }

    #[test]
    fn plans_shell_branch_add() {
        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git switch feature/daemon-hooks",
            "cwd": "/tmp/project"
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(
                &event,
                Path::new("/tmp/project"),
                Some("feature/daemon-hooks")
            ),
            HookEventPlan::AddBranch("feature/daemon-hooks".to_string())
        );
    }

    #[test]
    fn plans_worktree_add_against_new_worktree_root() {
        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git worktree add ../wt feature/daemon-hooks",
            "cwd": "/tmp/project"
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::AddBranchAt {
                root: Path::new("/tmp/wt").to_path_buf(),
                branch: "feature/daemon-hooks".to_string(),
            }
        );
    }

    #[test]
    fn plans_worktree_add_resolving_path_against_git_dash_c_dir() {
        // `git -C <dir>` makes git resolve the worktree path against <dir>,
        // not the shell cwd: from <base>/project/src, `-C ..` targets the
        // project root, so `../wt` lands beside the project at <base>/wt.
        let base = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir should create: {e}"));
        let base_root = base
            .path()
            .canonicalize()
            .unwrap_or_else(|e| panic!("tempdir should canonicalize: {e}"));
        let project_root = base_root.join("project");
        std::fs::create_dir_all(project_root.join("src"))
            .unwrap_or_else(|e| panic!("project dirs should create: {e}"));
        std::fs::create_dir_all(base_root.join("wt"))
            .unwrap_or_else(|e| panic!("worktree dir should create: {e}"));

        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git -C .. worktree add ../wt feature/daemon-hooks",
            "cwd": project_root.join("src")
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, &project_root, Some("main")),
            HookEventPlan::AddBranchAt {
                root: base_root.join("wt"),
                branch: "feature/daemon-hooks".to_string(),
            }
        );
    }

    #[test]
    fn plans_workspace_open_as_current_branch_sync() {
        let params = json!({
            "agent": "kiro",
            "event": "workspaceOpen"
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Kiro,
            }
        );
    }
}
