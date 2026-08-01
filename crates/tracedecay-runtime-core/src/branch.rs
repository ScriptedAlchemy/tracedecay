//! Kernel-owned slice of the root `branch` module.
//!
//! `branch_meta` and `worktree` moved into this crate and they need three
//! branch items: the shared branch-add lock (both the try and blocking
//! variants) and the current-branch read. Those definitions live here; the
//! root `branch` module re-exports them so `crate::branch::<item>` keeps
//! resolving on both sides of the split.
//!
//! Everything else about branch tracking (admin mutations, snapshots, GC)
//! stays in the root module. The one piece that could not follow — the
//! pending branch-admin recovery gate, which reaches into
//! `branch::admin::transaction` — is injected through
//! [`crate::ports::branch_admin_recovery`].

use std::path::Path;

use crate::errors::{Result, TraceDecayError};

/// Bounded-retry policy for a briefly-contended branch-add lock: a concurrent
/// branch add only holds the lock for the duration of a DB clone, so a short
/// spin lets a contender through instead of failing immediately.
pub const BRANCH_LOCK_RETRY_ATTEMPTS: usize = 20;
/// Interval between branch-lock acquisition retries.
pub const BRANCH_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Resolves the current branch name using `gix`. Linked worktrees use
/// `git symbolic-ref HEAD` because gix can resolve their shared repository's
/// primary HEAD instead of the worktree-specific HEAD.
///
/// Returns `None` for detached HEAD or if the repository cannot be opened.
pub fn current_branch(project_root: &Path) -> Option<String> {
    if crate::worktree::is_linked_worktree(project_root) {
        return current_branch_git(project_root);
    }
    match current_branch_gix(project_root) {
        GixHead::Branch(branch) => Some(branch),
        // A readable repo answered with a detached HEAD; `git symbolic-ref`
        // would fail the same way, so don't spawn it.
        GixHead::Detached => None,
        GixHead::Unavailable => {
            if !crate::worktree::git_may_resolve_repo(project_root) {
                return None;
            }
            current_branch_git(project_root)
        }
    }
}

/// One live-branch resolution, scoped to a single request or write gate.
///
/// [`current_branch`] opens a `gix` repository and, for linked worktrees,
/// spawns `git symbolic-ref`. A single request can cross several drift checks
/// and write gates, each of which used to pay that cost again. A `BranchMemo`
/// is created at the request or gate entry, threaded down, and dropped with
/// the request.
///
/// This is deliberately **not** a TTL cache and must never be stored in a
/// long-lived value: drift detection has to notice a checkout on the very next
/// request, so every request starts from an unresolved memo.
#[derive(Debug, Clone)]
pub struct BranchMemo {
    root: std::path::PathBuf,
    resolved: std::sync::OnceLock<Option<String>>,
}

impl BranchMemo {
    /// A memo that will resolve the live branch of `root` at most once.
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            resolved: std::sync::OnceLock::new(),
        }
    }

    /// A memo pre-seeded with an already-known resolution.
    #[must_use]
    pub fn resolved(root: impl Into<std::path::PathBuf>, branch: Option<String>) -> Self {
        let memo = Self::new(root);
        let _ = memo.resolved.set(branch);
        memo
    }

    /// The root this memo resolves against.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The live branch of this memo's root, resolving it on first use.
    #[must_use]
    pub fn get(&self) -> Option<&str> {
        self.resolved
            .get_or_init(|| current_branch(&self.root))
            .as_deref()
    }

    /// The live branch of `root`.
    ///
    /// Reuses this memo's single resolution when `root` is the root it was
    /// created for; a different root is a different HEAD, so it is resolved
    /// directly rather than answered from the memo.
    #[must_use]
    pub fn resolve_for(&self, root: &Path) -> Option<String> {
        if root == self.root {
            self.get().map(str::to_owned)
        } else {
            current_branch(root)
        }
    }
}

/// What gix could learn about HEAD without spawning `git`.
enum GixHead {
    /// HEAD points at a local branch.
    Branch(String),
    /// A readable repo whose HEAD is detached (or on a non-branch ref).
    Detached,
    /// No repo could be opened at this path or its HEAD was unreadable;
    /// the `git` subprocess fallback should decide.
    Unavailable,
}

fn current_branch_gix(project_root: &Path) -> GixHead {
    let Ok(repo) = gix::open(project_root) else {
        return GixHead::Unavailable;
    };
    let Ok(head) = repo.head() else {
        return GixHead::Unavailable;
    };
    // `Head::name()` is always the literal "HEAD"; the branch HEAD points
    // to (if any) is the referent.
    let Some(name) = head.referent_name() else {
        return GixHead::Detached;
    };
    let Ok(name_str) = std::str::from_utf8(name.as_bstr()) else {
        return GixHead::Unavailable;
    };
    match name_str.strip_prefix("refs/heads/") {
        Some(branch) => GixHead::Branch(branch.to_string()),
        None => GixHead::Detached,
    }
}

fn current_branch_git(project_root: &Path) -> Option<String> {
    let output = std::process::Command::new(crate::git::git_program())
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = std::str::from_utf8(&output.stdout).ok()?;
    name.strip_prefix("refs/heads/")
        .and_then(|s| s.strip_suffix('\n'))
        .map(std::string::ToString::to_string)
}

/// Acquires the shared branch-add lock without consulting the pending
/// branch-admin recovery journal.
pub fn try_acquire_branch_add_lock_raw(tracedecay_dir: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;

    std::fs::create_dir_all(tracedecay_dir)?;
    let lock_path = tracedecay_dir.join(".branch-add.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.try_lock_exclusive()
        .map_err(|e| TraceDecayError::SyncLock {
            message: format!("branch add already running at {}: {e}", lock_path.display()),
        })?;
    Ok(file)
}

/// Acquires the shared branch-add lock and refuses to hand it out while a
/// branch-admin mutation is still pending recovery.
pub fn try_acquire_branch_add_lock(tracedecay_dir: &Path) -> Result<std::fs::File> {
    let file = try_acquire_branch_add_lock_raw(tracedecay_dir)?;
    crate::ports::branch_admin_recovery::ensure_no_pending_recovery(tracedecay_dir)?;
    Ok(file)
}

/// Blocking-with-timeout variant of [`try_acquire_branch_add_lock`] for
/// synchronous callers. Retries a briefly-contended lock (a concurrent branch
/// add is only holding it for the duration of a DB clone) before giving up.
pub fn acquire_branch_lock_blocking(tracedecay_dir: &Path) -> Result<std::fs::File> {
    acquire_branch_add_lock_blocking_with(tracedecay_dir, try_acquire_branch_add_lock)
}

/// Blocking acquisition that skips the pending-recovery gate; the recovery
/// path itself needs the lock before it can clear the journal.
pub fn acquire_branch_add_lock_blocking_raw(tracedecay_dir: &Path) -> Result<std::fs::File> {
    acquire_branch_add_lock_blocking_with(tracedecay_dir, try_acquire_branch_add_lock_raw)
}

fn acquire_branch_add_lock_blocking_with(
    tracedecay_dir: &Path,
    acquire: fn(&Path) -> Result<std::fs::File>,
) -> Result<std::fs::File> {
    let mut last_contention = None;
    for _ in 0..BRANCH_LOCK_RETRY_ATTEMPTS {
        match acquire(tracedecay_dir) {
            Ok(lock) => return Ok(lock),
            Err(error @ TraceDecayError::SyncLock { .. }) => {
                last_contention = Some(error);
                std::thread::sleep(BRANCH_LOCK_RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
    Err(
        last_contention.unwrap_or_else(|| TraceDecayError::SyncLock {
            message: format!(
                "timed out waiting for branch metadata lock at {}",
                tracedecay_dir.join(".branch-add.lock").display()
            ),
        }),
    )
}

/// Auto-detects the repository's default branch.
///
/// Strategy:
/// 1. Try `git symbolic-ref refs/remotes/origin/HEAD`
/// 2. Fall back to checking if `main` or `master` exists locally
/// 3. Fall back to the currently checked-out local branch
///
/// The final fallback deliberately returns `None` for detached HEAD rather
/// than inventing a default branch.
#[must_use]
pub fn detect_default_branch(project_root: &Path) -> Option<String> {
    let repo = gix::open(project_root).ok()?;

    // Try symbolic-ref first (refs/remotes/origin/HEAD -> refs/remotes/origin/<branch>)
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Some(Ok(target)) = reference.follow()
        && let Some(name) = target
            .name()
            .as_bstr()
            .to_string()
            .strip_prefix("refs/remotes/origin/")
    {
        return Some(name.to_string());
    }

    // Fall back to heuristics
    for candidate in &["main", "master"] {
        let refname = format!("refs/heads/{candidate}");
        if repo.find_reference(&refname).is_ok() {
            return Some((*candidate).to_string());
        }
    }

    current_branch(project_root)
}

/// Sanitizes a branch name for use as a filename.
///
/// Replaces `/` with `_`, strips characters unsafe for filenames,
/// and collapses `..` sequences to prevent path traversal.
#[must_use]
pub fn sanitize_branch_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | ' ' | '.' => '_',
            c => c,
        })
        .collect();
    // Collapse runs of underscores
    let mut result = String::with_capacity(sanitized.len());
    let mut prev_underscore = false;
    for c in sanitized.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push(c);
            }
            prev_underscore = true;
        } else {
            result.push(c);
            prev_underscore = false;
        }
    }
    // Strip leading/trailing underscores
    result.trim_matches('_').to_string()
}

/// Resolves the DB path for a given branch.
///
/// If the branch is tracked in metadata, returns its `db_file` path.
/// Returns `None` if untracked or if the path would escape `tracedecay_dir`.
#[must_use]
pub fn resolve_branch_db_path(
    tracedecay_dir: &Path,
    branch: &str,
    meta: &crate::branch_meta::BranchMeta,
) -> Option<std::path::PathBuf> {
    let entry = meta.branches.get(branch)?;
    let resolved = tracedecay_dir.join(&entry.db_file);
    // Prevent path traversal: resolved path must stay within tracedecay_dir
    if let (Ok(canonical_dir), Ok(canonical_path)) =
        (tracedecay_dir.canonicalize(), resolved.canonicalize())
        && !canonical_path.starts_with(&canonical_dir)
    {
        return None;
    }
    Some(resolved)
}

#[cfg(test)]
mod branch_memo_tests {
    use super::BranchMemo;

    /// A memo answers repeated reads of its own root from one resolution, and
    /// refuses to answer for a different root — a different repository is a
    /// different HEAD, so it must be resolved directly.
    #[test]
    fn memo_serves_its_own_root_and_bypasses_for_another() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("worktree");
        let other = temp.path().join("elsewhere");
        std::fs::create_dir_all(&root).expect("worktree directory");
        std::fs::create_dir_all(&other).expect("other directory");

        // Seed a resolution that could not have come from these non-repository
        // paths, so any value observed below can only be the memoized one.
        let memo = BranchMemo::resolved(&root, Some("feature/pinned".to_owned()));

        assert_eq!(memo.get(), Some("feature/pinned"));
        assert_eq!(memo.get(), Some("feature/pinned"));
        assert_eq!(memo.root(), root.as_path());
        assert_eq!(
            memo.resolve_for(&root).as_deref(),
            Some("feature/pinned"),
            "the memo's own root must be served from the single resolution"
        );
        assert_eq!(
            memo.resolve_for(&other),
            None,
            "a different root must be resolved directly, not from the memo"
        );
    }

    /// An unseeded memo resolves lazily and caches even a `None` answer, so a
    /// detached HEAD or non-repository root is not re-probed within a request.
    #[test]
    fn unresolved_memo_caches_a_negative_answer() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let memo = BranchMemo::new(temp.path());
        assert_eq!(memo.get(), None);
        assert_eq!(memo.get(), None);
    }
}
