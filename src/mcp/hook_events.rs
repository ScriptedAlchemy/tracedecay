//! Normalizes daemon hook notifications into typed sync plans.
//!
//! This module owns wire-level hook semantics. The MCP server owns graph side
//! effects such as branch tracking, sync execution, and token-map refreshes.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// Shared with hook emitters so the receiver accepts the same agent keys.
pub(crate) use crate::daemon::HookAgent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookEventKind {
    FileEdit,
    Shell,
    WorkspaceOpen,
    SessionStart,
    IncrementalSync,
    TerminalReceipt,
    TurnCompleted,
    TurnIngested,
}

impl HookEventKind {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "afterFileEdit" | "postToolUseEdit" => Some(Self::FileEdit),
            "afterShellExecution" | "postToolUseShell" => Some(Self::Shell),
            "workspaceOpen" => Some(Self::WorkspaceOpen),
            "sessionStart" => Some(Self::SessionStart),
            "postToolUse" => Some(Self::IncrementalSync),
            "terminalReceipt" => Some(Self::TerminalReceipt),
            "turnCompleted" => Some(Self::TurnCompleted),
            "turnIngested" => Some(Self::TurnIngested),
            _ => None,
        }
    }

    pub(crate) fn as_key(self) -> &'static str {
        match self {
            Self::FileEdit => "file_edit",
            Self::Shell => "shell",
            Self::WorkspaceOpen => "workspace_open",
            Self::SessionStart => "session_start",
            Self::IncrementalSync => "incremental_sync",
            Self::TerminalReceipt => "terminal_receipt",
            Self::TurnCompleted => "turn_completed",
            Self::TurnIngested => "turn_ingested",
        }
    }
}

pub(crate) struct HookEvent {
    pub(crate) agent: HookAgent,
    pub(crate) kind: HookEventKind,
    pub(crate) rel_paths: Vec<String>,
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) route: Option<crate::daemon::HookRouteMetadata>,
    pub(crate) receipt: Option<crate::daemon::HookTerminalReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookEventPlan {
    SyncFiles(Vec<String>),
    AddBranch(String),
    AddBranchAt {
        root: PathBuf,
        branch: String,
        agent: HookAgent,
    },
    SyncCurrentBranch {
        branch: String,
        agent: HookAgent,
    },
    DebouncedIncrementalSync(HookAgent),
    RecordTerminalReceipt {
        route: Option<crate::daemon::HookRouteMetadata>,
        receipt: crate::daemon::HookTerminalReceipt,
    },
    MarkTurnIngested {
        route: Option<crate::daemon::HookRouteMetadata>,
        transcript_watermark: String,
    },
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
        route: event.route,
        receipt: event.receipt,
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
        HookEventKind::SessionStart => {
            plan_session_start_hook_event(event, project_root, current_branch)
        }
        HookEventKind::IncrementalSync if !event.rel_paths.is_empty() => {
            HookEventPlan::SyncFiles(event.rel_paths.clone())
        }
        HookEventKind::IncrementalSync => HookEventPlan::DebouncedIncrementalSync(event.agent),
        HookEventKind::TerminalReceipt | HookEventKind::TurnCompleted => event
            .receipt
            .clone()
            .map(|receipt| HookEventPlan::RecordTerminalReceipt {
                route: event.route.clone(),
                receipt,
            })
            .unwrap_or(HookEventPlan::Noop),
        HookEventKind::TurnIngested => event
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.transcript_watermark.clone())
            .map(|transcript_watermark| HookEventPlan::MarkTurnIngested {
                route: event.route.clone(),
                transcript_watermark,
            })
            .unwrap_or(HookEventPlan::Noop),
    }
}

pub(crate) fn sync_marker_path(data_root: &Path, agent: HookAgent) -> PathBuf {
    data_root.join(agent.sync_marker_file())
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
    let Some(hook_project_root) = hook_project_root(cwd, project_root) else {
        return HookEventPlan::Noop;
    };
    if !crate::hooks::cursor_shell_command_targets_project(command, cwd, &hook_project_root) {
        return HookEventPlan::Noop;
    }
    let same_project = paths_same(&hook_project_root, project_root);
    let hook_current_branch;
    let current_branch = if same_project {
        current_branch
    } else {
        hook_current_branch = crate::branch::current_branch(&hook_project_root);
        hook_current_branch.as_deref()
    };
    match crate::hooks::cursor_shell_sync_plan_with_current_branch(command, current_branch) {
        crate::hooks::CursorShellSyncPlan::BranchAdd(branch) => {
            branch_plan_for_root(project_root, hook_project_root, branch, event.agent)
        }
        crate::hooks::CursorShellSyncPlan::WorktreeBranchAdd {
            branch,
            worktree_path,
        } => HookEventPlan::AddBranchAt {
            root: crate::hooks::resolve_worktree_add_root(command, cwd, &worktree_path),
            branch,
            agent: event.agent,
        },
        crate::hooks::CursorShellSyncPlan::IncrementalSync => {
            HookEventPlan::DebouncedIncrementalSync(event.agent)
        }
        crate::hooks::CursorShellSyncPlan::CurrentBranchSync(branch) => {
            if same_project {
                HookEventPlan::SyncCurrentBranch {
                    branch,
                    agent: event.agent,
                }
            } else {
                HookEventPlan::AddBranchAt {
                    root: hook_project_root,
                    branch,
                    agent: event.agent,
                }
            }
        }
        crate::hooks::CursorShellSyncPlan::Noop => HookEventPlan::Noop,
    }
}

/// Plans the sync for a `sessionStart` hook.
///
/// In the main checkout this mirrors `WorkspaceOpen`: sync the current branch,
/// or fall back to a debounced incremental sync when the branch is unknown.
///
/// When the event `cwd` is a *linked* git worktree (a harness-created
/// `.claude/worktrees/*` session tree whose `.git` is a gitdir pointer rather
/// than a real directory), we additionally plan `AddBranchAt` against the
/// resolved worktree root so the session gets its own writable branch store
/// instead of the read-only fallback-ancestor DB. The downstream
/// `add_hook_branch_tracking` returns `AlreadyTracked` cheaply and
/// idempotently, so re-planning `AddBranchAt` for an already-tracked worktree
/// branch is a no-op — we do not need branch-meta visibility here.
fn plan_session_start_hook_event(
    event: &HookEvent,
    project_root: &Path,
    current_branch: Option<&str>,
) -> HookEventPlan {
    let cwd = event.cwd.as_deref().unwrap_or(project_root);
    if let Some(plan) = plan_linked_worktree_branch_add(event, cwd, project_root) {
        return plan;
    }
    current_branch
        .filter(|branch| !branch.is_empty())
        .map(|branch| HookEventPlan::SyncCurrentBranch {
            branch: branch.to_string(),
            agent: event.agent,
        })
        .unwrap_or(HookEventPlan::DebouncedIncrementalSync(event.agent))
}

/// When `cwd` resolves to a linked git worktree that belongs to `project_root`,
/// returns an `AddBranchAt` plan for the worktree root and its current branch.
/// Returns `None` for the main checkout, a non-git cwd, or an unrelated repo.
fn plan_linked_worktree_branch_add(
    event: &HookEvent,
    cwd: &Path,
    project_root: &Path,
) -> Option<HookEventPlan> {
    let worktree_root = crate::worktree::git_worktree_root(cwd)?;
    // A linked worktree's git common dir lives outside its own working tree
    // (it points back at the main checkout's `.git`). In the main checkout the
    // common dir is `<root>/.git`, so the two paths match and we bail out.
    let common_dir = crate::worktree::git_common_dir(&worktree_root)?;
    if path_is_inside(&common_dir, &worktree_root) {
        return None;
    }
    if !git_roots_share_common_dir(&worktree_root, project_root) {
        return None;
    }
    let branch = crate::branch::current_branch(&worktree_root)?;
    if branch.is_empty() {
        return None;
    }
    Some(HookEventPlan::AddBranchAt {
        root: worktree_root,
        branch,
        agent: event.agent,
    })
}

fn hook_project_root(cwd: &Path, project_root: &Path) -> Option<PathBuf> {
    if let Some(root) = crate::config::discover_project_root(cwd) {
        if root_belongs_to_project(&root, project_root) {
            return Some(root);
        }
        return None;
    }
    let Some(worktree_root) = crate::worktree::git_worktree_root(cwd) else {
        return path_is_inside(cwd, project_root).then(|| project_root.to_path_buf());
    };
    if git_roots_share_common_dir(&worktree_root, project_root) {
        Some(worktree_root)
    } else {
        None
    }
}

fn branch_plan_for_root(
    project_root: &Path,
    hook_project_root: PathBuf,
    branch: String,
    agent: HookAgent,
) -> HookEventPlan {
    if paths_same(&hook_project_root, project_root) {
        HookEventPlan::AddBranch(branch)
    } else {
        HookEventPlan::AddBranchAt {
            root: hook_project_root,
            branch,
            agent,
        }
    }
}

fn root_belongs_to_project(root: &Path, project_root: &Path) -> bool {
    paths_same(root, project_root) || git_roots_share_common_dir(root, project_root)
}

fn path_is_inside(path: &Path, root: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.starts_with(root)
}

fn git_roots_share_common_dir(a: &Path, b: &Path) -> bool {
    let a_common = crate::worktree::git_common_dir(a);
    let b_common = crate::worktree::git_common_dir(b);
    a_common
        .as_ref()
        .zip(b_common.as_ref())
        .is_some_and(|(a_common, b_common)| paths_same(a_common, b_common))
}

fn paths_same(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a == b
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
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::json;

    use super::{
        HookAgent, HookEvent, HookEventKind, HookEventPlan, parse_hook_event, plan_hook_event,
    };

    fn parse_or_panic(params: &serde_json::Value) -> HookEvent {
        match parse_hook_event(Some(params)) {
            Some(event) => event,
            None => panic!("hook event should parse"),
        }
    }

    /// Resolves the `git` executable to an absolute path exactly once per
    /// process. Under heavy parallel test load (nextest spawns one process per
    /// test, each spawning several `git` subprocesses), a bare
    /// `Command::new("git")` PATH lookup can transiently fail the spawn with
    /// `ENOENT` ("No such file or directory") even though git is installed.
    /// Resolving to an absolute path up front, plus a `GIT` env override,
    /// removes the per-spawn PATH walk and makes the lookup deterministic.
    fn git_program() -> std::ffi::OsString {
        use std::sync::OnceLock;
        static GIT: OnceLock<std::ffi::OsString> = OnceLock::new();
        GIT.get_or_init(|| {
            if let Some(explicit) = std::env::var_os("GIT") {
                return explicit;
            }
            let exe_name = if cfg!(windows) { "git.exe" } else { "git" };
            if let Some(paths) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&paths) {
                    let candidate = dir.join(exe_name);
                    if candidate.is_file() {
                        return candidate.into_os_string();
                    }
                }
            }
            // Fall back to a bare name and let the OS resolve it.
            std::ffi::OsString::from("git")
        })
        .clone()
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        // A cwd that does not yet exist makes the spawn itself fail with
        // ENOENT, which is indistinguishable from git-not-found; guard it so
        // any real failure is attributable.
        assert!(
            cwd.is_dir(),
            "git cwd {cwd:?} should exist before running git {args:?}"
        );
        let git = git_program();
        // Retry a transient spawn ENOENT a few times: under load the initial
        // fork/exec can spuriously fail even with a valid absolute program.
        let mut last_err: Option<std::io::Error> = None;
        let mut output = None;
        for attempt in 0..5 {
            match Command::new(&git).args(args).current_dir(cwd).output() {
                Ok(out) => {
                    output = Some(out);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt < 4 => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
                }
                Err(e) => {
                    panic!("git {args:?} should run (program {git:?}): {e}");
                }
            }
        }
        let output = output.unwrap_or_else(|| {
            panic!("git {args:?} should run (program {git:?}) after retries: {last_err:?}")
        });
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    fn git_test_root(path: &Path) -> std::path::PathBuf {
        path.to_path_buf()
    }

    #[cfg(not(windows))]
    fn git_test_root(path: &Path) -> std::path::PathBuf {
        path.canonicalize()
            .unwrap_or_else(|e| panic!("tempdir should canonicalize: {e}"))
    }

    fn setup_linked_session_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir should create: {e}"));
        let base_root = git_test_root(base.path());
        let project_root = base_root.join("project");
        let worktree_root = base_root.join("session-worktree");
        std::fs::create_dir_all(project_root.join("src"))
            .unwrap_or_else(|e| panic!("project dirs should create: {e}"));
        std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n")
            .unwrap_or_else(|e| panic!("source should write: {e}"));
        run_git(&project_root, &["init", "-b", "main"]);
        run_git(&project_root, &["config", "user.email", "test@test.com"]);
        run_git(&project_root, &["config", "user.name", "Test"]);
        run_git(&project_root, &["add", "."]);
        run_git(&project_root, &["commit", "-m", "initial"]);
        let worktree_arg = worktree_root.to_string_lossy();
        run_git(
            &project_root,
            &[
                "worktree",
                "add",
                worktree_arg.as_ref(),
                "-b",
                "feature/session",
            ],
        );
        (base, project_root, worktree_root)
    }

    fn assert_add_branch_at(plan: HookEventPlan, expected_root: &Path, expected_branch: &str) {
        let HookEventPlan::AddBranchAt {
            root,
            branch,
            agent,
        } = plan
        else {
            panic!("expected AddBranchAt plan, got {plan:?}");
        };
        assert!(
            super::paths_same(&root, expected_root),
            "planned root {root:?} should match expected root {expected_root:?}"
        );
        assert_eq!(branch, expected_branch);
        assert_eq!(agent, HookAgent::Codex);
    }

    fn write_project_marker(root: &Path) {
        let db_path = crate::config::get_project_db_path(root);
        let Some(parent) = db_path.parent() else {
            panic!("db path should have parent");
        };
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("project marker dir should create: {e}"));
        std::fs::write(db_path, b"").unwrap_or_else(|e| panic!("project marker should write: {e}"));
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
    fn preserves_route_metadata_from_hook_notification() {
        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "cargo test",
            "cwd": "/tmp/project",
            "route": {
                "session_id": "session-123",
                "thread_id": "thread-456",
                "cwd": "/tmp/project",
                "worktree": "/tmp/project-worktree",
                "branch": "feature/hook-route"
            }
        });

        let event = parse_or_panic(&params);

        let Some(route) = event.route.as_ref() else {
            panic!("route metadata should parse");
        };
        assert_eq!(route.session_id.as_deref(), Some("session-123"));
        assert_eq!(route.thread_id.as_deref(), Some("thread-456"));
        assert_eq!(route.cwd.as_deref(), Some(Path::new("/tmp/project")));
        assert_eq!(
            route.worktree.as_deref(),
            Some(Path::new("/tmp/project-worktree"))
        );
        assert_eq!(route.branch.as_deref(), Some("feature/hook-route"));
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

    /// Regression: the receiver used to keep its own agent string match, so
    /// the claude-keyed events added for Claude `PostToolUse` were silently
    /// dropped. Every agent the send side can construct must parse here.
    #[test]
    fn accepts_every_constructible_hook_agent() {
        for agent in [
            HookAgent::Claude,
            HookAgent::Codex,
            HookAgent::Cursor,
            HookAgent::Kiro,
        ] {
            let params = json!({
                "agent": agent.as_wire(),
                "event": "postToolUseEdit",
                "rel_paths": ["src/lib.rs"],
                "cwd": "/tmp/project"
            });
            let event = parse_or_panic(&params);
            assert_eq!(event.agent, agent);
            assert_eq!(event.kind, HookEventKind::FileEdit);
        }
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
    fn ignores_shell_branch_add_from_unrelated_project_root() {
        let base = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir should create: {e}"));
        let project_root = base.path().join("project");
        let unrelated_root = base.path().join("unrelated");
        std::fs::create_dir_all(&project_root)
            .unwrap_or_else(|e| panic!("project root should create: {e}"));
        std::fs::create_dir_all(&unrelated_root)
            .unwrap_or_else(|e| panic!("unrelated root should create: {e}"));
        write_project_marker(&project_root);
        write_project_marker(&unrelated_root);

        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git switch feature/unrelated",
            "cwd": unrelated_root
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, &project_root, Some("feature/unrelated")),
            HookEventPlan::Noop
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
                agent: HookAgent::Codex,
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
                agent: HookAgent::Codex,
            }
        );
    }

    #[test]
    fn plans_branch_switch_from_session_worktree_against_worktree_root() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();

        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git switch feature/session",
            "cwd": worktree_root
        });
        let event = parse_or_panic(&params);

        assert_add_branch_at(
            plan_hook_event(&event, &project_root, Some("main")),
            &worktree_root,
            "feature/session",
        );
    }

    #[test]
    fn plans_ambiguous_git_change_from_session_worktree_with_worktree_branch() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();

        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git pull --rebase",
            "cwd": worktree_root
        });
        let event = parse_or_panic(&params);

        assert_add_branch_at(
            plan_hook_event(&event, &project_root, Some("main")),
            &worktree_root,
            "feature/session",
        );
    }

    #[test]
    fn round_trips_session_start_wire_name_and_key() {
        assert_eq!(
            HookEventKind::from_wire("sessionStart"),
            Some(HookEventKind::SessionStart)
        );
        assert_eq!(HookEventKind::SessionStart.as_key(), "session_start");
    }

    #[test]
    fn parses_hermes_terminal_receipt_without_terminal_content() {
        let event = parse_or_panic(&json!({
            "agent": "hermes",
            "event": "terminalReceipt",
            "cwd": "/tmp/project",
            "route": {"session_id": "session-1", "cwd": "/tmp/project"},
            "receipt": {
                "tool_call_id": "call-1",
                "turn_id": "turn-1",
                "status": "success",
                "duration_ms": 12,
                "transcript_watermark": "turn-1"
            }
        }));
        assert_eq!(event.agent, HookAgent::Hermes);
        assert_eq!(event.kind, HookEventKind::TerminalReceipt);
        assert!(matches!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::RecordTerminalReceipt { receipt, .. }
                if receipt.tool_call_id.as_deref() == Some("call-1")
        ));
    }

    #[test]
    fn plans_projectless_hermes_turn_completion_as_a_review_receipt() {
        let event = parse_or_panic(&json!({
            "agent": "hermes",
            "event": "turnCompleted",
            "route": {"session_id": "session-1"},
            "receipt": {
                "status": "success",
                "transcript_watermark": "message-1"
            }
        }));
        assert_eq!(event.kind, HookEventKind::TurnCompleted);
        assert!(matches!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::RecordTerminalReceipt { receipt, .. }
                if receipt.transcript_watermark.as_deref() == Some("message-1")
        ));
    }

    #[test]
    fn plans_session_start_from_main_checkout_as_current_branch_sync() {
        let (_base, project_root, _worktree_root) = setup_linked_session_worktree();

        let params = json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": project_root,
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, &project_root, Some("main")),
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Claude,
            }
        );
    }

    #[test]
    fn plans_session_start_from_linked_worktree_as_branch_add() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();

        let params = json!({
            "agent": "codex",
            "event": "sessionStart",
            "cwd": worktree_root,
        });
        let event = parse_or_panic(&params);

        // The session cwd is the linked worktree, so even though the main
        // checkout reports `main`, the plan tracks the worktree's own branch
        // at the worktree root.
        assert_add_branch_at(
            plan_hook_event(&event, &project_root, Some("main")),
            &worktree_root,
            "feature/session",
        );
    }

    #[test]
    fn plans_session_start_with_empty_branch_as_debounced_incremental_sync() {
        let params = json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": "/tmp/project",
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("")),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Claude)
        );
    }

    #[test]
    fn plans_cursor_session_start_as_current_branch_sync() {
        let params = serde_json::to_value(crate::daemon::DaemonHookEvent::session_start(
            HookAgent::Cursor,
            PathBuf::from("/tmp/project"),
        ))
        .unwrap();
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Cursor,
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
