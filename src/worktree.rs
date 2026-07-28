//! Borrowed-index detection for git worktrees.
//!
//! A tracedecay index resolves through the active project root or user profile
//! store (see [`config::discover_project_root`](crate::config::discover_project_root)).
//! That walk is unaware of git worktrees: when a worktree is created *inside*
//! the main checkout (e.g. agent tooling that puts worktrees under
//! `.claude/worktrees/<name>/` or `.worktrees/<name>/`), a command run from
//! the worktree walks up and silently resolves the MAIN checkout's index.
//!
//! Every query then returns results from the main tree's code — usually a
//! different branch — rather than the worktree the user is actually editing.
//! Symbols added or changed only in the worktree are invisible to the agent.
//! This module detects that "borrowed index" situation so callers can warn.
//!
//! Detection is best-effort: when git is unavailable or the path isn't a
//! repo, it reports "no mismatch" and callers carry on unchanged.
//!
//! Ported from `codegraph/src/sync/worktree.ts` (#312).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A mismatch between the caller's git working tree and the resolved
/// tracedecay index root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeIndexMismatch {
    /// The git working tree the command was invoked from.
    pub worktree_root: PathBuf,
    /// The (different) working tree whose data-dir index is being
    /// served.
    pub index_root: PathBuf,
}

/// Absolute, symlink-resolved toplevel of the git working tree that `dir`
/// belongs to, or `None` when `dir` isn't inside a git repo (or `git` is
/// missing on PATH).
///
/// `git rev-parse --show-toplevel` returns the per-worktree root: the main
/// checkout and each linked worktree report their own distinct directory,
/// which is exactly the distinction this module relies on.
pub fn git_worktree_root(dir: &Path) -> Option<PathBuf> {
    // gix discovery walks up the same way `git rev-parse` does but without
    // a subprocess spawn. A discovered bare repo (no workdir) matches
    // `--show-toplevel` failing.
    if let Ok(repo) = gix::discover(dir) {
        return realpath(repo.workdir()?);
    }
    if !git_may_resolve_repo(dir) {
        return None;
    }
    let trimmed = crate::git::git_capture(dir, &["rev-parse", "--show-toplevel"])?;
    realpath(Path::new(&trimmed))
}

/// Absolute, symlink-resolved path to the repository's git common directory.
///
/// For a linked worktree this is the main checkout's `.git` directory, which is
/// the stable local identity all linked worktrees share.
pub fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    if let Ok(repo) = gix::discover(dir) {
        let common_dir = repo.common_dir().to_path_buf();
        let resolved = if common_dir.is_absolute() {
            common_dir
        } else {
            dir.join(common_dir)
        };
        return Some(resolved.canonicalize().unwrap_or(resolved));
    }
    if !git_may_resolve_repo(dir) {
        return None;
    }
    let raw = crate::git::git_capture(dir, &["rev-parse", "--git-common-dir"])?;
    let common_dir = PathBuf::from(raw);
    let resolved = if common_dir.is_absolute() {
        common_dir
    } else {
        dir.join(common_dir)
    };
    Some(resolved.canonicalize().unwrap_or(resolved))
}

/// The checkout that owns `dir`'s **repository** identity, or `None` when
/// `dir` already owns it.
///
/// Every linked worktree of a repository shares one git common directory and
/// is therefore one project. Attachment is irrelevant here: whether a worktree
/// has a branch checked out decides which *graph scope* serves it, not which
/// repository it belongs to. Keying identity off the worktree path instead
/// mints a second store for a repository that already has one.
///
/// Returns `None` — meaning "this path is its own identity" — when `dir` is
/// the primary checkout, is not a worktree root at all (a package directory
/// inside a monorepo is its own project), is outside git, or has a repository
/// shape whose primary checkout cannot be derived safely.
pub fn repository_identity_root(dir: &Path) -> Option<PathBuf> {
    let worktree_root = git_worktree_root(dir)?;
    // Only a worktree ROOT inherits repository identity. Without this check a
    // subdirectory indexed as its own project would be absorbed into the
    // enclosing repository's store.
    if worktree_root != realpath(dir)? {
        return None;
    }
    let common_dir = git_common_dir(dir)?;
    crate::project_registry::primary_checkout_root(&worktree_root, Some(&common_dir))
}

pub(crate) fn is_linked_worktree(dir: &Path) -> bool {
    git_worktree_root(dir).is_some_and(|root| root.join(".git").is_file())
}

pub fn is_detached_linked_worktree(dir: &Path) -> bool {
    is_linked_worktree(dir) && crate::branch::current_branch(dir).is_none()
}

/// Stable internal graph scope for a detached linked worktree.
///
/// Detached worktrees share repository identity and storage with the primary
/// checkout, but need a distinct graph database so indexing branchless files
/// cannot replace the primary checkout's graph.
pub fn detached_worktree_graph_scope(dir: &Path) -> Option<String> {
    if !is_detached_linked_worktree(dir) {
        return None;
    }
    let resolve = |raw: String| {
        let path = PathBuf::from(raw);
        let path = if path.is_absolute() {
            path
        } else {
            dir.join(path)
        };
        path.canonicalize().unwrap_or(path)
    };
    // Use Git here deliberately: gix can collapse a linked worktree's git
    // directory onto its common directory, which loses the per-worktree key.
    let git_dir = resolve(crate::git::git_capture(dir, &["rev-parse", "--git-dir"])?);
    let common_dir = resolve(crate::git::git_capture(
        dir,
        &["rev-parse", "--git-common-dir"],
    )?);
    let identity = git_dir.strip_prefix(&common_dir).unwrap_or(&git_dir);
    let mut hasher = Sha256::new();
    hasher.update(crate::os_str_bytes::native_os_str_bytes(
        identity.as_os_str(),
    ));
    let digest = hex::encode(hasher.finalize());
    Some(format!("detached-worktree/{}", &digest[..16]))
}

/// Cheap pre-flight for the `git` subprocess fallbacks in this crate: `git`
/// can only resolve a repository for `dir` when a `.git` entry exists
/// somewhere in its ancestor chain or the caller overrides discovery via
/// `GIT_DIR`. Spawning `git` costs ~100-300ms on Windows, so callers skip
/// the spawn when it is guaranteed to fail anyway.
pub(crate) fn git_may_resolve_repo(dir: &Path) -> bool {
    if std::env::var_os("GIT_DIR").is_some() {
        return true;
    }
    dir.ancestors().any(|p| p.join(".git").exists())
}

/// Detect when `start_path` lives in one git working tree but the resolved
/// tracedecay index (`index_root`) belongs to a *different* working tree.
///
/// Returns `None` — meaning "nothing to warn about" — when:
///   - `start_path` isn't in a git repo (or git is unavailable),
///   - the index already lives in `start_path`'s own working tree, or
///   - `index_root` isn't itself a working-tree root (an unrelated parent
///     directory that merely happens to contain a data dir), which
///     keeps non-git and monorepo-subdir layouts from producing false
///     warnings.
pub fn detect_worktree_index_mismatch(
    start_path: &Path,
    index_root: &Path,
) -> Option<WorktreeIndexMismatch> {
    let worktree_root = git_worktree_root(start_path)?;
    let resolved_index_root = realpath(index_root).unwrap_or_else(|| index_root.to_path_buf());
    if worktree_root == resolved_index_root {
        return None;
    }
    // Only flag when the index root is itself a real working-tree root.
    // This distinguishes "borrowed another worktree's index" from "index
    // sits in a plain ancestor directory", and avoids warning outside git
    // entirely.
    if git_worktree_root(&resolved_index_root)? != resolved_index_root {
        return None;
    }
    Some(WorktreeIndexMismatch {
        worktree_root,
        index_root: resolved_index_root,
    })
}

/// Detect a borrowed index for the client scope represented by a daemon-held
/// project. The daemon process's own current directory is unrelated to the
/// client and must never participate in this decision.
pub(crate) fn detect_scoped_worktree_index_mismatch(
    index_root: &Path,
    scope_prefix: Option<&str>,
) -> Option<WorktreeIndexMismatch> {
    let start_path = scope_prefix.map_or_else(
        || index_root.to_path_buf(),
        |prefix| index_root.join(prefix),
    );
    detect_worktree_index_mismatch(&start_path, index_root)
}

/// Verbose multi-line warning for `tracedecay status` and similar contexts
/// where the answer can sit alongside a heads-up block.
pub fn worktree_mismatch_warning(m: &WorktreeIndexMismatch) -> String {
    format!(
        "This tracedecay index belongs to a different git working tree.\n  \
         Running in: {}\n  \
         Index from: {}\n\
         Results reflect that tree's code (often a different branch), not this worktree — \
         symbols changed only here are missing. Run `tracedecay init` in this worktree for a \
         worktree-local index.",
        m.worktree_root.display(),
        m.index_root.display()
    )
}

/// Compact, single-line variant for prefixing an MCP tool response. Read
/// tools return their answer inline, so the heads-up has to ride on the
/// same payload the agent is already reading — a multi-line block would
/// bury the result.
pub fn worktree_mismatch_notice(m: &WorktreeIndexMismatch) -> String {
    format!(
        "WARNING: tracedecay results below come from a different git worktree ({}), \
         not where you're working ({}) — they may reflect another branch, and symbols \
         changed only here are missing. Run `tracedecay init` here for a worktree-local index.",
        m.index_root.display(),
        m.worktree_root.display()
    )
}

/// Resolve symlinks where possible so tmp/realpath quirks don't break
/// equality checks. Falls back to a plain `absolutize` when canonicalize
/// fails (e.g. directory was deleted between rev-parse and the fs call).
fn realpath(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}

#[cfg(test)]
fn git_command() -> std::process::Command {
    let mut command = std::process::Command::new("git");
    let paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    #[cfg(not(windows))]
    let paths = {
        let mut paths = paths;
        paths.push(PathBuf::from("/usr/bin"));
        paths.push(PathBuf::from("/bin"));
        paths
    };
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = git_command()
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git not on PATH — required for worktree tests");
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    #[test]
    fn no_mismatch_outside_git() {
        let tmp = tempdir().unwrap();
        let index = tmp.path().join("index");
        let start = tmp.path().join("start");
        fs::create_dir_all(&index).unwrap();
        fs::create_dir_all(&start).unwrap();
        assert!(detect_worktree_index_mismatch(&start, &index).is_none());
    }

    #[test]
    fn no_mismatch_when_index_lives_in_same_worktree() {
        let tmp = tempdir().unwrap();
        let project = tmp.path().join("repo");
        fs::create_dir_all(&project).unwrap();
        run_git(&project, &["init", "--quiet"]);
        // start_path is inside the same working tree as the index
        let sub = project.join("src");
        fs::create_dir_all(&sub).unwrap();
        assert!(detect_worktree_index_mismatch(&sub, &project).is_none());
    }

    #[test]
    fn flags_mismatch_when_started_from_linked_worktree() {
        // Two real git working trees: a main checkout and a linked
        // worktree. start_path = the linked worktree; index_root = the
        // main checkout. Expect a mismatch.
        let tmp = tempdir().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        run_git(&main, &["init", "--quiet"]);
        // git worktree add requires at least one commit
        fs::write(main.join("README.md"), "hi").unwrap();
        run_git(&main, &["add", "."]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        let worktree = tmp.path().join("wt");
        run_git(
            &main,
            &["worktree", "add", "--detach", worktree.to_str().unwrap()],
        );
        let mismatch = detect_worktree_index_mismatch(&worktree, &main)
            .expect("expected mismatch when started from linked worktree but index is main");
        assert_eq!(
            mismatch.worktree_root,
            std::fs::canonicalize(&worktree).unwrap()
        );
        assert_eq!(mismatch.index_root, std::fs::canonicalize(&main).unwrap());
    }

    #[test]
    fn scoped_detection_uses_the_client_scope_instead_of_process_cwd() {
        let tmp = tempdir().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        run_git(&main, &["init", "--quiet"]);
        fs::write(main.join("README.md"), "hi").unwrap();
        run_git(&main, &["add", "."]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        let worktree = main.join(".worktrees").join("feature");
        fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        run_git(
            &main,
            &["worktree", "add", "--detach", worktree.to_str().unwrap()],
        );

        assert!(detect_scoped_worktree_index_mismatch(&main, None).is_none());
        let mismatch = detect_scoped_worktree_index_mismatch(&main, Some(".worktrees/feature"))
            .expect("nested client worktree must be compared with the main index");
        assert_eq!(
            mismatch.worktree_root,
            std::fs::canonicalize(&worktree).unwrap()
        );
    }

    #[test]
    fn no_mismatch_when_index_root_is_plain_ancestor() {
        // index_root is a parent of the worktree but NOT a working-tree
        // root itself (no .git). Should not flag.
        let tmp = tempdir().unwrap();
        let outer = tmp.path().join("outer"); // not a repo
        let inner = outer.join("inner-repo");
        fs::create_dir_all(&inner).unwrap();
        run_git(&inner, &["init", "--quiet"]);
        // start in inner-repo, index_root = outer (plain dir, no .git)
        assert!(detect_worktree_index_mismatch(&inner, &outer).is_none());
    }
}
