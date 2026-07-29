// This file is `include!`d into build.rs as well as compiled as a module, so it
// carries no inner doc comments and no `use` statements: both would collide
// with the build script's own file-level items.

/// Commit identity of a source tree at build time.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildIdentity {
    /// Short `HEAD` commit, or `None` when the tree is not a git worktree.
    pub sha: Option<String>,
    /// Whether tracked modifications or untracked files were present.
    pub dirty: bool,
}

/// Resolves the git identity of the crate rooted at `root`.
///
/// A registry install unpacks the crate into a directory that can itself sit
/// inside an unrelated repository, where `git rev-parse HEAD` would happily
/// report that repository's commit. A commit is therefore only trusted when git
/// reports `root` as the worktree top level; every other case — no git binary,
/// no checkout, a nested unpack — yields an empty identity and leaves the
/// version at bare `CARGO_PKG_VERSION`.
pub fn resolve(root: &std::path::Path) -> BuildIdentity {
    if !is_own_worktree(root) {
        return BuildIdentity::default();
    }
    let Some(sha) = git_stdout(root, &["rev-parse", "--short=12", "HEAD"]) else {
        return BuildIdentity::default();
    };
    BuildIdentity {
        sha: Some(sha),
        dirty: git_stdout(root, &["status", "--porcelain"]).is_some(),
    }
}

/// Paths whose change makes [`resolve`] answer differently: `HEAD` and its
/// reflog move with commits, checkouts, and resets; the index tracks staged and
/// stat-refreshed worktree state. Without these the baked commit silently
/// describes whichever tree happened to trigger the previous build-script run.
///
/// Absent paths are dropped, because Cargo treats a missing `rerun-if-changed`
/// path as perpetually changed and would rerun the script on every build.
pub fn watch_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    if !is_own_worktree(root) {
        return Vec::new();
    }
    ["HEAD", "logs/HEAD", "index"]
        .iter()
        .filter_map(|name| {
            let raw = git_stdout(root, &["rev-parse", "--git-path", name])?;
            let relative = std::path::Path::new(&raw);
            let path = if relative.is_absolute() {
                relative.to_path_buf()
            } else {
                root.join(relative)
            };
            path.exists().then_some(path)
        })
        .collect()
}

/// Whether git reports `root` itself — not some ancestor — as a worktree top
/// level. This is the guard that keeps an unrelated enclosing repository out of
/// both the baked commit and the rebuild triggers.
fn is_own_worktree(root: &std::path::Path) -> bool {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return false;
    };
    let Some(toplevel) = git_stdout(root, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    std::fs::canonicalize(toplevel).ok() == Some(canonical_root)
}

/// Trimmed stdout of a successful `git` run in `root`, or `None` when git is
/// missing, the command failed, or it printed nothing.
///
/// `--no-optional-locks` keeps every probe strictly read-only. Plain
/// `git status` refreshes the index stat cache and rewrites `.git/index` — a
/// path [`watch_paths`] hands Cargo as a rebuild trigger — so probing the tree
/// would arm the very trigger it reads. The flag suppresses only that
/// incidental write; the reported status is unchanged.
fn git_stdout(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::{BuildIdentity, resolve, watch_paths};

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git should run");
        assert!(status.status.success(), "git {args:?} failed");
    }

    fn committed_repo(root: &std::path::Path) {
        git(root, &["init", "--quiet"]);
        std::fs::write(root.join("tracked.txt"), "one").expect("write tracked file");
        git(root, &["add", "tracked.txt"]);
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
    }

    #[test]
    fn a_tree_without_git_has_no_commit_identity() {
        let dir = tempfile::tempdir().expect("temp dir");

        assert_eq!(resolve(dir.path()), BuildIdentity::default());
        assert!(watch_paths(dir.path()).is_empty());
    }

    #[test]
    fn a_committed_worktree_reports_its_head_and_is_clean() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());

        let identity = resolve(dir.path());

        let sha = identity.sha.expect("a committed worktree has a HEAD");
        assert_eq!(
            sha.len(),
            12,
            "expected a 12-character short sha, got {sha}"
        );
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!identity.dirty, "a freshly committed tree is not dirty");
        assert!(!watch_paths(dir.path()).is_empty());
    }

    #[test]
    fn an_uncommitted_change_marks_the_worktree_dirty() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());

        std::fs::write(dir.path().join("tracked.txt"), "two").expect("modify tracked file");
        assert!(
            resolve(dir.path()).dirty,
            "a modified tracked file is dirty"
        );

        git(dir.path(), &["checkout", "--", "tracked.txt"]);
        std::fs::write(dir.path().join("stray.txt"), "stray").expect("write untracked file");
        assert!(resolve(dir.path()).dirty, "an untracked file is dirty");
    }

    /// `watch_paths` hands `.git/index` to Cargo as a rebuild trigger, and any
    /// build-script rerun recompiles the whole root crate. Probing identity
    /// must therefore leave the index alone: a plain `git status` refreshes its
    /// stat cache and rewrites it, which would arm that trigger on every build
    /// that follows an edit.
    #[test]
    fn probing_identity_does_not_rewrite_the_index_it_watches() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());
        let index = dir.path().join(".git/index");
        // A stale stat cache is what tempts git into rewriting the index.
        std::fs::write(dir.path().join("tracked.txt"), "modified").expect("modify tracked file");
        let before = std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .expect("index mtime");

        assert!(resolve(dir.path()).dirty, "the fixture must read as dirty");

        let after = std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .expect("index mtime");
        assert_eq!(
            before, after,
            "resolve() rewrote .git/index, which watch_paths registers as a rebuild trigger"
        );
    }

    /// A crate unpacked below an unrelated repository must not inherit that
    /// repository's commit — the exact shape of a `cargo install` from the
    /// registry inside a developer's checkout.
    #[test]
    fn a_subdirectory_of_another_repository_has_no_commit_identity() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());
        let unpacked = dir.path().join("registry/tracedecay-0.0.0");
        std::fs::create_dir_all(&unpacked).expect("create unpacked crate dir");

        assert_eq!(resolve(&unpacked), BuildIdentity::default());
        assert!(
            watch_paths(&unpacked).is_empty(),
            "an unrelated repository must not drive this crate's rebuilds"
        );
    }
}
