//! Git branch resolution, tracking, snapshots, and admin mutations for
//! multi-branch indexing, plus the shared branch-add lock and current-branch
//! read used by `branch_meta` and `worktree`.

use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

#[cfg(any(test, feature = "test-helpers"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "test-helpers"))]
use std::path::PathBuf;
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::{LazyLock, Mutex};

/// Counts live [`current_branch`] probes in test builds.
///
/// [`BranchMemo::resolved`] never increments this: it answers from a
/// pre-seeded value and must not open git. A request that skips live
/// resolution therefore reports zero here.
#[cfg(any(test, feature = "test-helpers"))]
static LIVE_BRANCH_RESOLUTIONS: AtomicU64 = AtomicU64::new(0);

/// Per-root live probes so parallel suites do not share one process counter.
#[cfg(any(test, feature = "test-helpers"))]
static LIVE_BRANCH_RESOLUTIONS_BY_ROOT: LazyLock<Mutex<HashMap<PathBuf, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(any(test, feature = "test-helpers"))]
fn live_branch_resolution_counts() -> std::sync::MutexGuard<'static, HashMap<PathBuf, u64>> {
    LIVE_BRANCH_RESOLUTIONS_BY_ROOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(any(test, feature = "test-helpers"))]
fn record_live_branch_resolution(project_root: &Path) {
    LIVE_BRANCH_RESOLUTIONS.fetch_add(1, Ordering::Relaxed);
    let mut counts = live_branch_resolution_counts();
    *counts.entry(project_root.to_path_buf()).or_insert(0) += 1;
    if let Ok(canonical) = project_root.canonicalize()
        && canonical != project_root
    {
        *counts.entry(canonical).or_insert(0) += 1;
    }
}

/// Live [`current_branch`] probes observed since the last reset.
#[cfg(any(test, feature = "test-helpers"))]
#[must_use]
pub fn live_branch_resolution_count_for_test() -> u64 {
    LIVE_BRANCH_RESOLUTIONS.load(Ordering::Relaxed)
}

/// Live [`current_branch`] probes for one project root (and its canonical path).
///
/// Parallel tests share the process, so the global counter is not an isolation
/// boundary. A fixture asserts against its own root.
#[cfg(any(test, feature = "test-helpers"))]
#[must_use]
pub fn live_branch_resolution_count_for_root_for_test(root: &Path) -> u64 {
    let counts = live_branch_resolution_counts();
    if let Some(count) = counts.get(root) {
        return *count;
    }
    root.canonicalize()
        .ok()
        .and_then(|canonical| counts.get(&canonical).copied())
        .unwrap_or(0)
}

/// Clears the live-resolution probe counter.
#[cfg(any(test, feature = "test-helpers"))]
pub fn reset_live_branch_resolution_count_for_test() {
    LIVE_BRANCH_RESOLUTIONS.store(0, Ordering::Relaxed);
    live_branch_resolution_counts().clear();
}

/// Clears live-resolution probes recorded for one project root.
#[cfg(any(test, feature = "test-helpers"))]
pub fn reset_live_branch_resolution_count_for_root_for_test(root: &Path) {
    let mut counts = live_branch_resolution_counts();
    counts.remove(root);
    if let Ok(canonical) = root.canonicalize() {
        counts.remove(&canonical);
    }
}

mod admin;
mod tracking;

pub use admin::{
    BranchAdminAction, BranchAdminOutcome, BranchAdminReport, PreparedBranchAdminMutation,
    SingleStoreBranchRetirementV1, prepare_branch_admin_mutation,
    remove_tracked_branch_store_checked,
};
pub use tracking::{
    BranchAddOutcome, BranchTrackingPreparation, PreparedBranchRollbackOutcome,
    PreparedBranchTracking, finalize_prepared_branch_tracking, find_nearest_tracked_ancestor,
    is_branch_ref_present, local_branch_exists, prepare_branch_tracking_in_layout,
    rollback_prepared_branch_tracking,
};
pub(crate) use tracking::{now_unix_secs, parse_unix_secs};

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
    #[cfg(any(test, feature = "test-helpers"))]
    {
        record_live_branch_resolution(project_root);
    }
    crate::git_repository::GitRepositoryAuthority::discover(project_root)
        .ok()?
        .head()
        .ok()?
        .branch()
        .map(str::to_owned)
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

/// Acquires the shared branch-add lock.
pub fn try_acquire_branch_add_lock(tracedecay_dir: &Path) -> Result<std::fs::File> {
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

/// Blocking-with-timeout variant of [`try_acquire_branch_add_lock`] for
/// synchronous callers. Retries a briefly-contended lock (a concurrent branch
/// add is only holding it for the duration of a DB clone) before giving up.
pub fn acquire_branch_lock_blocking(tracedecay_dir: &Path) -> Result<std::fs::File> {
    acquire_branch_add_lock_blocking_with(tracedecay_dir, try_acquire_branch_add_lock)
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
    let authority = crate::git_repository::GitRepositoryAuthority::discover(project_root).ok()?;
    let references = authority.references().ok()?;

    if let Some(branch) = references
        .iter()
        .find(|reference| reference.name == "refs/remotes/origin/HEAD")
        .and_then(|reference| reference.symbolic_target.as_deref())
        .and_then(|name| name.strip_prefix("refs/remotes/origin/"))
    {
        return Some(branch.to_owned());
    }
    for candidate in &["main", "master"] {
        let refname = format!("refs/heads/{candidate}");
        if references.iter().any(|reference| reference.name == refname) {
            return Some((*candidate).to_string());
        }
    }

    authority.head().ok()?.branch().map(str::to_owned)
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

    /// A pre-seeded memo must not open git; only [`super::current_branch`]
    /// (via an unresolved [`BranchMemo::get`]) is a live probe.
    #[test]
    fn resolved_memo_does_not_increment_the_live_probe_counter() {
        let temp = tempfile::tempdir().expect("temporary directory");
        super::reset_live_branch_resolution_count_for_root_for_test(temp.path());
        let seeded = BranchMemo::resolved(temp.path(), Some("feature/pinned".to_owned()));
        assert_eq!(seeded.get(), Some("feature/pinned"));
        assert_eq!(
            super::live_branch_resolution_count_for_root_for_test(temp.path()),
            0
        );

        let unresolved = BranchMemo::new(temp.path());
        assert_eq!(unresolved.get(), None);
        assert_eq!(
            super::live_branch_resolution_count_for_root_for_test(temp.path()),
            1
        );
        assert_eq!(unresolved.get(), None);
        assert_eq!(
            super::live_branch_resolution_count_for_root_for_test(temp.path()),
            1,
            "a memo must resolve the live branch at most once"
        );
    }

    #[test]
    fn measure_live_probe_against_resolved_memo() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path();
        let init = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(root)
            .status()
            .expect("git init");
        assert!(init.success(), "git init must succeed for a live probe");
        let mut live = Vec::with_capacity(25);
        for _ in 0..25 {
            let started = std::time::Instant::now();
            let _ = super::current_branch(root);
            live.push(started.elapsed());
        }
        let memo = BranchMemo::resolved(root, Some("main".to_owned()));
        let mut seeded = Vec::with_capacity(25);
        for _ in 0..25 {
            let started = std::time::Instant::now();
            let _ = memo.get();
            seeded.push(started.elapsed());
        }
        live.sort();
        seeded.sort();
        let report = format!(
            "MEASURE #818 branch n=25 live_p50={:?} live_p95={:?} resolved_p50={:?} resolved_p95={:?}",
            live[12], live[23], seeded[12], seeded[23]
        );
        println!("{report}");
        std::fs::write(
            std::env::temp_dir().join("td-mcp-818-measure.txt"),
            report.as_bytes(),
        )
        .expect("write #818 measurement");
    }
}
