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
//! [`detect_worktree_index_mismatch`] is best-effort: [`git_worktree_root`]
//! still returns `None` when discovery fails, so a borrowed-index warning is
//! withheld rather than invented. Repository membership for ingest and
//! layout matching uses [`crate::git_discovery`] so uncertainty stays typed.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{ManifestDigest, canonical_sha256};

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
/// belongs to, or `None` when `dir` isn't inside a readable repository.
///
/// `git rev-parse --show-toplevel` returns the per-worktree root: the main
/// checkout and each linked worktree report their own distinct directory,
/// which is exactly the distinction this module relies on.
pub fn git_worktree_root(dir: &Path) -> Option<PathBuf> {
    crate::git_repository::GitRepositoryAuthority::discover(dir)
        .ok()?
        .worktree_root()
        .map(Path::to_path_buf)
}

/// Discovers a worktree root without invoking Git.
///
/// Bounded and cancellable callers use this authority so discovery cannot
/// escape their subprocess deadline through [`git_worktree_root`]'s
/// command-line fallback.
pub fn discover_git_worktree_root(dir: &Path) -> Option<PathBuf> {
    let repo = gix::discover(dir).ok()?;
    realpath(repo.workdir()?)
}

/// Absolute, symlink-resolved path to the repository's git common directory.
///
/// For a linked worktree this is the main checkout's `.git` directory, which is
/// the stable local identity all linked worktrees share.
pub fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    crate::git_repository::GitRepositoryAuthority::discover(dir)
        .ok()
        .map(|repository| repository.common_dir().to_path_buf())
}

/// Stable repository locator digest for a registered project root.
///
/// Linked worktrees share one retained project/configuration authority, so
/// this binds that authority to their canonical Git common directory.
/// Independent clones retain distinct locators. Non-Git projects fall back
/// to their canonical root.
pub fn locator_digest_for_project(project_root: &Path) -> Result<ManifestDigest, TraceDecayError> {
    let repository_locator = git_common_dir(project_root).unwrap_or_else(|| {
        project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf())
    });
    canonical_sha256(&(
        "tracedecay.project-open.repository-locator.v2",
        repository_locator.to_string_lossy().as_ref(),
    ))
    .map_err(|_| TraceDecayError::Config {
        message: "project locator digest is inconsistent".to_owned(),
    })
}

/// Derives the primary checkout root for a linked worktree from its git
/// common directory, or `None` when this checkout already is the primary one
/// or the repository has a shape whose primary checkout cannot be derived
/// safely (bare repos, submodule gitlinks).
///
/// Canonicalize of `project_root` is best-effort. This helper's contract is
/// "redirect to the derived primary when that checkout still exists", not a
/// typed filesystem probe. A failed canonicalize must not return `None`
/// ("already primary") — that would mint a second store for a linked
/// worktree whose path could not be resolved. The unresolved path is
/// compared instead, and `Some(primary)` is still returned when that
/// directory exists.
pub fn primary_checkout_root(
    project_root: &Path,
    git_common_dir: Option<&Path>,
) -> Option<PathBuf> {
    let common_dir = git_common_dir?;
    // Only a plain, non-bare `<repo>/.git` common dir has a parent that is
    // reliably the checkout root. Bare repos and submodule gitlinks (whose
    // common dir lives under `.git/modules/...`) are left alone rather than
    // risk deriving a bogus "primary" and redirecting registration there.
    if common_dir.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return None;
    }
    let primary_root = common_dir.parent()?;
    let canonical_project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    if primary_root == canonical_project_root {
        return None;
    }
    primary_root.is_dir().then(|| primary_root.to_path_buf())
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
    primary_checkout_root(&worktree_root, Some(&common_dir))
}

/// Returns whether `dir` resolves to a linked worktree root.
pub fn is_linked_worktree(dir: &Path) -> bool {
    let Ok(repository) = crate::git_repository::GitRepositoryAuthority::discover(dir) else {
        return false;
    };
    repository.worktree_root().is_some_and(|root| {
        realpath(dir).is_some_and(|canonical_dir| canonical_dir.starts_with(root))
            && repository.git_dir() != repository.common_dir()
    })
}

pub(crate) fn is_detached_linked_worktree(dir: &Path) -> bool {
    is_linked_worktree(dir) && crate::branch::current_branch(dir).is_none()
}

/// Stable internal graph scope for a detached linked worktree.
///
/// Detached worktrees share repository identity and the mutable project store
/// with the primary checkout, but retain an exact graph provenance scope so
/// indexing branchless files cannot replace another checkout's generation.
pub fn detached_worktree_graph_scope(dir: &Path) -> Option<String> {
    if !is_detached_linked_worktree(dir) {
        return None;
    }
    let repository = crate::git_repository::GitRepositoryAuthority::discover(dir).ok()?;
    let git_dir = repository.git_dir();
    let common_dir = repository.common_dir();
    let identity = git_dir.strip_prefix(common_dir).unwrap_or(git_dir);
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
pub fn git_may_resolve_repo(dir: &Path) -> bool {
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
pub(crate) fn detect_worktree_index_mismatch(
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
pub fn detect_scoped_worktree_index_mismatch(
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
/// equality checks. `None` when canonicalize fails (e.g. directory was
/// deleted between rev-parse and the fs call); callers choose the fallback.
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
        assert!(
            detached_worktree_graph_scope(&worktree)
                .is_some_and(|scope| scope.starts_with("detached-worktree/"))
        );
    }

    #[test]
    fn gix_resolves_the_linked_worktree_specific_branch() {
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
        let worktree = tmp.path().join("feature");
        run_git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );

        assert_eq!(
            crate::branch::current_branch(&worktree).as_deref(),
            Some("feature")
        );
    }

    #[test]
    fn linked_worktree_classification_uses_git_identity() {
        let tmp = tempdir().unwrap();
        let main = tmp.path().join("main");
        let linked = tmp.path().join("linked");
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
        run_git(
            &main,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );
        let primary_subdirectory = main.join("packages/service");
        let linked_subdirectory = linked.join("packages/service");
        fs::create_dir_all(&primary_subdirectory).unwrap();
        fs::create_dir_all(&linked_subdirectory).unwrap();

        assert!(!is_linked_worktree(&main));
        assert!(!is_linked_worktree(&primary_subdirectory));
        assert!(is_linked_worktree(&linked));
        assert!(
            is_linked_worktree(&linked_subdirectory),
            "a project rooted below a linked worktree must retain linked-worktree admission"
        );
    }

    #[test]
    fn separate_git_dir_primary_is_not_a_linked_worktree() {
        let tmp = tempdir().unwrap();
        let worktree = tmp.path().join("checkout");
        let git_dir = tmp.path().join("repository.git");
        fs::create_dir_all(&worktree).unwrap();
        run_git(
            &worktree,
            &[
                "init",
                "--quiet",
                "--separate-git-dir",
                git_dir.to_str().unwrap(),
            ],
        );

        assert!(!is_linked_worktree(&worktree));
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
    fn primary_checkout_root_does_not_treat_uncanonicalizable_worktree_as_primary() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("main");
        fs::create_dir_all(&primary).unwrap();
        let primary = fs::canonicalize(&primary).unwrap();
        let common_dir = primary.join(".git");
        fs::create_dir_all(&common_dir).unwrap();
        let missing_worktree = tmp.path().join("deleted-wt");

        assert_eq!(
            primary_checkout_root(&missing_worktree, Some(&common_dir)),
            Some(primary),
            "a worktree path that cannot be canonicalized must still redirect to a live primary"
        );
    }

    #[test]
    fn primary_checkout_root_redirects_linked_worktree_to_existing_primary() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("main");
        let worktree = tmp.path().join("main-wt");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        // `git_common_dir` always returns a canonicalized path — mirror that
        // guarantee here rather than a raw join.
        let primary = fs::canonicalize(&primary).unwrap();
        let common_dir = primary.join(".git");
        fs::create_dir_all(&common_dir).unwrap();

        let redirected = primary_checkout_root(&worktree, Some(&common_dir));

        assert_eq!(
            redirected,
            Some(primary),
            "a linked worktree with a live primary checkout must redirect to it"
        );
    }

    #[test]
    fn primary_checkout_root_is_none_when_project_root_is_already_primary() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("main");
        fs::create_dir_all(&primary).unwrap();
        let primary = fs::canonicalize(&primary).unwrap();
        let common_dir = primary.join(".git");
        fs::create_dir_all(&common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&primary, Some(&common_dir)),
            None,
            "the primary checkout must never be redirected to itself"
        );
    }

    #[test]
    fn primary_checkout_root_is_none_without_git_common_dir() {
        let tmp = tempdir().unwrap();
        let project_root = tmp.path().join("not-a-worktree");
        fs::create_dir_all(&project_root).unwrap();

        assert_eq!(
            primary_checkout_root(&project_root, None),
            None,
            "non-git projects must register themselves unchanged"
        );
    }

    #[test]
    fn primary_checkout_root_keeps_worktree_when_primary_checkout_is_missing() {
        // The primary checkout no longer exists on disk (deleted, moved off
        // this machine, ...). A worktree-only project is legitimate and
        // must keep registering its own root rather than redirecting to a
        // path that doesn't exist.
        let tmp = tempdir().unwrap();
        let missing_primary = tmp.path().join("deleted-main");
        let worktree = tmp.path().join("main-wt");
        fs::create_dir_all(&worktree).unwrap();
        let common_dir = missing_primary.join(".git");

        assert_eq!(
            primary_checkout_root(&worktree, Some(&common_dir)),
            None,
            "a missing primary checkout must not be adopted as canonical_root"
        );
    }

    #[test]
    fn primary_checkout_root_ignores_non_dot_git_common_dirs() {
        // Bare repos and submodule gitlinks resolve `git_common_dir` to a
        // path that isn't a plain `<repo>/.git`, so the parent directory
        // isn't reliably a checkout root — leave registration alone rather
        // than risk deriving a bogus "primary".
        let tmp = tempdir().unwrap();
        let worktree = tmp.path().join("checkout");
        fs::create_dir_all(&worktree).unwrap();
        let submodule_common_dir = tmp.path().join("main/.git/modules/sub");
        fs::create_dir_all(&submodule_common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&worktree, Some(&submodule_common_dir)),
            None,
            "non-`.git` common dirs must not redirect registration"
        );
    }

    #[test]
    fn linked_worktrees_share_repository_locator_but_independent_repositories_do_not() {
        let temporary = tempdir().unwrap();
        let primary = temporary.path().join("primary");
        let linked = temporary.path().join("linked");
        let independent = temporary.path().join("independent");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&independent).unwrap();

        run_git(&primary, &["init", "-b", "main", "--quiet"]);
        fs::write(primary.join("README.md"), "primary\n").unwrap();
        run_git(&primary, &["add", "README.md"]);
        run_git(
            &primary,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        run_git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "feature/linked",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        run_git(&independent, &["init", "-b", "main", "--quiet"]);

        let primary_digest = locator_digest_for_project(&primary).expect("primary locator digest");
        let linked_digest = locator_digest_for_project(&linked).expect("linked locator digest");
        let independent_digest =
            locator_digest_for_project(&independent).expect("independent locator digest");

        assert_eq!(linked_digest, primary_digest);
        assert_ne!(independent_digest, primary_digest);
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
