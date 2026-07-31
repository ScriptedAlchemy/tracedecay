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
pub const BRANCH_LOCK_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(50);

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
    Err(last_contention.unwrap_or_else(|| TraceDecayError::SyncLock {
        message: format!(
            "timed out waiting for branch metadata lock at {}",
            tracedecay_dir.join(".branch-add.lock").display()
        ),
    }))
}
