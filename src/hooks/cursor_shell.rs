//! Cursor shell-plan logic: classifies `afterShellExecution` commands into
//! sync actions (branch add, worktree add, incremental sync) and owns the
//! shared shell-word / git-command parsing helpers.

use std::path::{Component, Path, PathBuf};

/// Returns `true` when `command` is a git invocation that changes the working
/// tree / HEAD enough that a broad re-sync is warranted (checkout, switch,
/// pull, merge, rebase, reset, cherry-pick, `stash pop`/`stash apply`).
///
/// Read-only git commands (`status`, `log`, `diff`), `commit`/`add`, and
/// non-git commands return `false`. Only commands whose first token is `git`
/// match, so `echo git checkout` is ignored.
pub fn is_git_state_changing_command(command: &str) -> bool {
    let tokens = shell_words(command);
    let Some(sub_pos) = git_subcommand_pos(&tokens) else {
        return false;
    };
    let sub = tokens[sub_pos].to_ascii_lowercase();
    match sub.as_str() {
        "checkout" | "switch" | "pull" | "merge" | "rebase" | "reset" | "cherry-pick" => true,
        "stash" => {
            let after = tokens
                .iter()
                .skip(sub_pos + 1)
                .map(|t| t.to_ascii_lowercase())
                .find(|t| !t.starts_with('-'));
            matches!(after.as_deref(), Some("pop" | "apply"))
        }
        _ => false,
    }
}

/// The action a Cursor `afterShellExecution` hook should take for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorShellSyncPlan {
    /// Bootstrap/maintain branch tracking for the given branch (supersedes a
    /// plain sync; the branch-add path copies the parent DB and syncs).
    BranchAdd(String),
    /// Bootstrap/maintain branch tracking in a newly-created linked worktree.
    WorktreeBranchAdd {
        branch: String,
        worktree_path: String,
    },
    /// Run a full incremental sync (same-branch change set).
    IncrementalSync,
    /// Ensure the current branch is tracked, then sync it if it already was.
    CurrentBranchSync(String),
    /// Do nothing.
    Noop,
}

/// Classifies a shell command into the sync action a Cursor
/// `afterShellExecution` hook should take. Branch switches take precedence
/// over plain incremental syncs.
pub fn cursor_shell_sync_plan(command: &str) -> CursorShellSyncPlan {
    cursor_shell_sync_plan_with_current_branch(command, None)
}

/// Like [`cursor_shell_sync_plan`], but supplies the post-command current branch
/// for state-changing commands whose branch target is ambiguous or implicit.
pub fn cursor_shell_sync_plan_with_current_branch(
    command: &str,
    current_branch: Option<&str>,
) -> CursorShellSyncPlan {
    let raw = shell_words(command);
    if let Some(parts) = cursor_worktree_add_parts_from_tokens(&raw) {
        return CursorShellSyncPlan::WorktreeBranchAdd {
            branch: parts.branch,
            worktree_path: parts.worktree_path,
        };
    }
    if let Some(branch) = cursor_branch_switch_target_from_tokens(&raw) {
        return CursorShellSyncPlan::BranchAdd(branch);
    }
    if is_git_state_changing_command(command) {
        if let Some(branch) = current_branch.filter(|branch| !branch.is_empty()) {
            return CursorShellSyncPlan::CurrentBranchSync(branch.to_string());
        }
        return CursorShellSyncPlan::IncrementalSync;
    }
    CursorShellSyncPlan::Noop
}

/// Returns the target branch for a branch-changing git command:
/// `git checkout <branch>`, `git switch <branch>`, `git checkout -b <branch>`,
/// and `git switch -c <branch>`. Worktree creation is classified separately by
/// [`cursor_shell_sync_plan`], which owns `git worktree add` parsing.
///
/// Path checkouts (`git checkout -- <file>` or obvious file pathspecs), remote
/// tracking shortcuts such as `git switch --track origin/feature`, and
/// non-switch commands return `None`. Only commands whose first shell word is
/// `git` are considered.
pub fn cursor_branch_switch_target(command: &str) -> Option<String> {
    let raw = shell_words(command);
    cursor_branch_switch_target_from_tokens(&raw)
}

fn cursor_branch_switch_target_from_tokens(raw: &[String]) -> Option<String> {
    let sub_pos = git_subcommand_pos(raw)?;
    let sub = raw[sub_pos].to_ascii_lowercase();

    match sub.as_str() {
        "checkout" | "switch" => {
            let after = &raw[sub_pos + 1..];
            let mut i = 0;
            let mut uses_tracking_shortcut = false;
            while i < after.len() {
                let tok = &after[i];
                if tok == "--" {
                    return None;
                }
                if matches!(tok.as_str(), "-b" | "-B" | "-c" | "-C" | "--orphan") {
                    return after.get(i + 1).cloned();
                }
                if tok == "-t" || tok == "--track" || tok.starts_with("--track=") {
                    uses_tracking_shortcut = true;
                    i += 1;
                    continue;
                }
                if tok.starts_with('-') {
                    i += 1;
                    continue;
                }
                if uses_tracking_shortcut {
                    return None;
                }
                if is_obvious_checkout_pathspec(tok) {
                    return None;
                }
                return Some(tok.clone());
            }
            None
        }
        _ => None,
    }
}

fn cursor_worktree_add_parts_from_tokens(raw: &[String]) -> Option<WorktreeAddParts> {
    let sub_pos = git_subcommand_pos(raw)?;
    if raw.get(sub_pos)?.eq_ignore_ascii_case("worktree")
        && raw.get(sub_pos + 1)?.eq_ignore_ascii_case("add")
    {
        return cursor_worktree_add_parts(&raw[sub_pos + 2..]);
    }
    None
}

struct WorktreeAddParts {
    branch: String,
    worktree_path: String,
}

fn cursor_worktree_add_parts(after: &[String]) -> Option<WorktreeAddParts> {
    let mut i = 0;
    let mut positional = Vec::new();
    let mut detached = false;
    let mut new_branch = None;
    while i < after.len() {
        let tok = &after[i];
        if tok == "--" {
            positional.extend(after[i + 1..].iter().cloned());
            break;
        }
        if matches!(tok.as_str(), "-b" | "-B") {
            new_branch = after.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if tok == "-d" || tok == "--detach" {
            detached = true;
            i += 1;
            continue;
        }
        if tok == "--reason" {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        positional.push(tok.clone());
        i += 1;
    }
    if detached {
        return None;
    }
    let worktree_path = positional.first()?.clone();
    let branch = new_branch.or_else(|| positional.get(1).cloned())?;
    Some(WorktreeAddParts {
        branch,
        worktree_path,
    })
}

fn is_obvious_checkout_pathspec(token: &str) -> bool {
    token == "."
        || token == ":/"
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with(":/")
        || token
            .rsplit_once('.')
            .is_some_and(|(_, ext)| !ext.is_empty())
}

/// Splits a shell command line into words, honoring single/double quotes and
/// backslash escapes. Shared with `tool_hints` so search-command
/// classification sees the same tokens as the checkout/sync parsing here.
pub(crate) fn shell_words(command: &str) -> Vec<String> {
    shell_words_for_platform(command, cfg!(windows))
}

fn shell_words_for_platform(command: &str, windows: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for c in command.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => current.push(c),
            },
            _ => match c {
                '\'' | '"' => quote = Some(c),
                '\\' if windows => current.push(c),
                '\\' => escaped = true,
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            },
        }
    }

    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn git_subcommand_pos(tokens: &[String]) -> Option<usize> {
    if !tokens.first()?.eq_ignore_ascii_case("git") {
        return None;
    }

    let mut i = 1;
    while i < tokens.len() {
        let token = tokens[i].to_ascii_lowercase();
        match token.as_str() {
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                i += 2;
            }
            "--" => {
                i += 1;
            }
            _ if token.starts_with("--git-dir=")
                || token.starts_with("--work-tree=")
                || token.starts_with("--namespace=")
                || token.starts_with("--config-env=") =>
            {
                i += 1;
            }
            _ if token.starts_with('-') => {
                i += 1;
            }
            _ => return Some(i),
        }
    }
    None
}

pub fn cursor_shell_command_targets_project(
    command: &str,
    cwd: &Path,
    project_root: &Path,
) -> bool {
    let tokens = shell_words(command);
    if !tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("git"))
    {
        return true;
    }
    let Some(work_dir) = git_explicit_work_dir(&tokens, cwd) else {
        return true;
    };
    let target_root = crate::config::discover_project_root(&work_dir).unwrap_or(work_dir);
    paths_same(&target_root, project_root)
}

fn git_explicit_work_dir(tokens: &[String], cwd: &Path) -> Option<PathBuf> {
    let mut i = 1;
    let mut explicit_work_dir = None;
    while i < tokens.len() {
        let token = &tokens[i];
        match token.as_str() {
            "-C" | "--work-tree" => {
                let value = tokens.get(i + 1)?;
                explicit_work_dir = Some(resolve_shell_path(cwd, value));
                i += 2;
            }
            "-c" | "--git-dir" | "--namespace" | "--config-env" => i += 2,
            _ if token.starts_with("--work-tree=") => {
                let value = token.trim_start_matches("--work-tree=");
                explicit_work_dir = Some(resolve_shell_path(cwd, value));
                i += 1;
            }
            _ if token.starts_with("--git-dir=")
                || token.starts_with("--namespace=")
                || token.starts_with("--config-env=") =>
            {
                i += 1;
            }
            _ if token.starts_with('-') => i += 1,
            _ => break,
        }
    }
    explicit_work_dir
}

fn resolve_shell_path(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Resolves the filesystem root of the worktree created by a
/// `git worktree add` command. git resolves the worktree path against
/// `-C <dir>`/`--work-tree` overrides rather than the shell cwd, so those are
/// honored first. The result is canonicalized when the worktree exists (it
/// does by the time a post-shell hook fires) so symlinked components resolve
/// the way git resolved them, falling back to lexical `..` normalization.
pub fn resolve_worktree_add_root(command: &str, cwd: &Path, worktree_path: &str) -> PathBuf {
    let tokens = shell_words(command);
    let base = git_explicit_work_dir(&tokens, cwd).unwrap_or_else(|| cwd.to_path_buf());
    let joined = resolve_shell_path(&base, worktree_path);
    joined
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(&joined))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub(super) fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_preserves_unquoted_windows_paths() {
        assert_eq!(
            shell_words_for_platform(r"git --work-tree=C:\Users\me\repo pull", true),
            vec!["git", r"--work-tree=C:\Users\me\repo", "pull"]
        );
        assert_eq!(
            shell_words_for_platform(r"git --work-tree=C:\Users\me\repo pull", false),
            vec!["git", r"--work-tree=C:Usersmerepo", "pull"]
        );
    }

    #[test]
    fn git_work_tree_overrides_prior_c_directory() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("repo");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let command = format!(
            "git -C {} --git-dir={}/.git --work-tree={} pull",
            outside.display(),
            project.display(),
            project.display()
        );

        assert!(cursor_shell_command_targets_project(
            &command, &outside, &project
        ));
    }
}
