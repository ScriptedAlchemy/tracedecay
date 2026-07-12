//! Git branch resolution utilities for multi-branch indexing.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;

/// Bounded-retry policy for a briefly-contended branch-add lock: a concurrent
/// branch add only holds the lock for the duration of a DB clone, so a short
/// spin lets a contender through instead of failing immediately. Shared by the
/// async [`prepare_branch_tracking_in_layout`] and the synchronous
/// [`acquire_branch_add_lock_blocking`]; only the sleep primitive differs.
const BRANCH_LOCK_RETRY_ATTEMPTS: usize = 20;
const BRANCH_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Resolves the current branch name using `gix`. Falls back to
/// `git symbolic-ref HEAD` for worktrees when gix cannot resolve HEAD
/// (e.g. with minimal feature flags that exclude worktree support).
///
/// Returns `None` for detached HEAD or if the repository cannot be opened.
pub fn current_branch(project_root: &Path) -> Option<String> {
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

/// Returns true if `branch` exists as a local `refs/heads/*` branch.
pub fn local_branch_exists(project_root: &Path, branch: &str) -> bool {
    if branch.is_empty() {
        return false;
    }
    let refname = format!("refs/heads/{branch}");
    if let Ok(repo) = gix::open(project_root) {
        // gix reads loose and packed refs, the same sources `git show-ref`
        // consults; trust its answer instead of paying a subprocess spawn
        // to re-ask git.
        return repo.find_reference(&refname).is_ok();
    }
    if !crate::worktree::git_may_resolve_repo(project_root) {
        return false;
    }
    std::process::Command::new(crate::git::git_program())
        .args(["show-ref", "--verify", "--quiet", &refname])
        .current_dir(project_root)
        .status()
        .is_ok_and(|status| status.success())
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

fn git_rev_list_count(project_root: &Path, from_ref: &str, to_ref: &str) -> Option<usize> {
    let output = std::process::Command::new(crate::git::git_program())
        .args(["rev-list", "--count", &format!("{from_ref}..{to_ref}")])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// In-process equivalent of `git rev-list --count hidden..tip`: commits
/// reachable from `tip` but not from `hidden`. Saves a `git` subprocess
/// spawn on every branch-add parent ranking.
fn gix_rev_distance(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    hidden: gix::ObjectId,
) -> Option<usize> {
    let walk = repo.rev_walk([tip]).with_hidden([hidden]).all().ok()?;
    let mut count = 0_usize;
    for info in walk {
        info.ok()?;
        count += 1;
    }
    Some(count)
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
pub fn detect_default_branch(project_root: &Path) -> Option<String> {
    let repo = gix::open(project_root).ok()?;

    // Try symbolic-ref first (refs/remotes/origin/HEAD -> refs/remotes/origin/<branch>)
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(Ok(target)) = reference.follow() {
            if let Some(name) = target
                .name()
                .as_bstr()
                .to_string()
                .strip_prefix("refs/remotes/origin/")
            {
                return Some(name.to_string());
            }
        }
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

#[cfg(test)]
mod default_branch_tests {
    use super::*;

    fn run_git(project_root: &Path, args: &[&str]) {
        let output = std::process::Command::new(crate::git::git_program())
            .args(args)
            .current_dir(project_root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn custom_default_repo() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().to_path_buf();
        run_git(&project_root, &["init", "-b", "trunk"]);
        run_git(&project_root, &["config", "user.email", "test@example.com"]);
        run_git(&project_root, &["config", "user.name", "TraceDecay Test"]);
        std::fs::write(project_root.join("fixture"), b"fixture").unwrap();
        run_git(&project_root, &["add", "fixture"]);
        run_git(&project_root, &["commit", "-m", "fixture"]);
        (temp, project_root)
    }

    #[test]
    fn detects_checked_out_custom_default_without_origin_head() {
        let (_temp, project_root) = custom_default_repo();

        assert_eq!(
            detect_default_branch(&project_root).as_deref(),
            Some("trunk")
        );
    }

    #[test]
    fn detached_custom_default_does_not_guess() {
        let (_temp, project_root) = custom_default_repo();
        run_git(&project_root, &["checkout", "--detach", "HEAD"]);

        assert_eq!(detect_default_branch(&project_root), None);
    }

    #[tokio::test]
    async fn detached_legacy_store_refuses_to_invent_default_metadata() {
        let (temp, project_root) = custom_default_repo();
        run_git(&project_root, &["checkout", "--detach", "HEAD"]);
        let data_dir = temp.path().join("profile-shard");
        std::fs::create_dir(&data_dir).unwrap();
        std::fs::write(data_dir.join(crate::config::DB_FILENAME), b"graph").unwrap();

        let error = match prepare_branch_tracking_in_layout(&project_root, "trunk", &data_dir).await
        {
            Ok(_) => panic!("detached legacy store must not invent a default branch"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("default branch is unknown"));
        assert!(!data_dir.join(crate::storage::BRANCH_META_FILENAME).exists());
    }
}

/// Sanitizes a branch name for use as a filename.
///
/// Replaces `/` with `_`, strips characters unsafe for filenames,
/// and collapses `..` sequences to prevent path traversal.
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

/// Computes a unique, collision-free DB stem (filename without extension) for
/// `branch_name` under `branches_dir`.
///
/// `sanitize_branch_name` is many-to-one: `feature/foo` and `feature_foo` both
/// map to `feature_foo`. Returning the bare sanitized stem unconditionally let
/// a second `branch add` `fs::copy`-overwrite the first branch's index (data
/// loss). This returns the bare stem only when it is free; otherwise it appends
/// a short deterministic hash of the *unsanitized* branch name so distinct
/// branches get distinct files while a given branch always maps to the same
/// stem. Returns `None` when the name sanitizes to empty (which would yield a
/// hidden `branches/.db`).
fn unique_branch_db_stem(
    meta: &BranchMeta,
    branches_dir: &Path,
    branch_name: &str,
) -> Option<String> {
    let base = sanitize_branch_name(branch_name);
    if base.is_empty() {
        return None;
    }
    let conflicts = |stem: &str| -> bool {
        let db_file = format!("branches/{stem}.db");
        let meta_conflict = meta
            .branches
            .iter()
            .any(|(name, entry)| name != branch_name && entry.db_file == db_file);
        let file_conflict = branches_dir.join(format!("{stem}.db")).exists();
        meta_conflict || file_conflict
    };
    if !conflicts(&base) {
        return Some(base);
    }
    let hashed = format!("{base}-{}", short_branch_hash(branch_name));
    if !conflicts(&hashed) {
        return Some(hashed);
    }
    (1..10_000)
        .map(|suffix| format!("{hashed}-{suffix}"))
        .find(|candidate| !conflicts(candidate))
}

/// Short, stable hex digest of a branch name for DB-stem disambiguation.
fn short_branch_hash(branch_name: &str) -> String {
    crate::sync::content_hash(branch_name)
        .chars()
        .take(10)
        .collect()
}

/// Resolves the DB path for a given branch.
///
/// If the branch is tracked in metadata, returns its `db_file` path.
/// Returns `None` if untracked or if the path would escape `tracedecay_dir`.
pub fn resolve_branch_db_path(
    tracedecay_dir: &Path,
    branch: &str,
    meta: &BranchMeta,
) -> Option<std::path::PathBuf> {
    let entry = meta.branches.get(branch)?;
    let resolved = tracedecay_dir.join(&entry.db_file);
    // Prevent path traversal: resolved path must stay within tracedecay_dir
    if let (Ok(canonical_dir), Ok(canonical_path)) =
        (tracedecay_dir.canonicalize(), resolved.canonicalize())
    {
        if !canonical_path.starts_with(&canonical_dir) {
            return None;
        }
    }
    Some(resolved)
}

/// Finds the nearest tracked ancestor branch using `git merge-base`.
///
/// For each tracked branch in the metadata, computes the merge-base with
/// the given branch and picks the one with the most recent common ancestor.
pub fn find_nearest_tracked_ancestor(
    project_root: &Path,
    branch: &str,
    meta: &BranchMeta,
) -> Option<String> {
    let repo = gix::open(project_root).ok()?;

    let branch_ref = format!("refs/heads/{branch}");
    let branch_commit = repo
        .find_reference(&branch_ref)
        .ok()?
        .peel_to_commit()
        .ok()?;

    let mut best_ancestor: Option<(String, usize, gix::date::Time)> = None;
    let mut best_merge_base: Option<(String, gix::date::Time)> = None;

    for tracked_name in meta.branches.keys() {
        if tracked_name == branch {
            continue;
        }
        let tracked_ref = format!("refs/heads/{tracked_name}");
        let Some(tracked_commit) = repo
            .find_reference(&tracked_ref)
            .ok()
            .and_then(|mut r| r.peel_to_commit().ok())
        else {
            continue;
        };

        // Find merge-base between branch and tracked branch.
        let Ok(base_id) = repo.merge_base(branch_commit.id, tracked_commit.id) else {
            continue;
        };

        let Ok(base_commit) = repo.find_commit(base_id) else {
            continue;
        };
        let time = base_commit
            .time()
            .ok()
            .unwrap_or_else(|| gix::date::Time::new(0, 0));

        // Prefer tracked branches that are actual ancestors of the target
        // branch. Rank them by commit distance so a direct parent wins even
        // when multiple merge-bases land in the same timestamp second.
        if base_id == tracked_commit.id {
            let distance = gix_rev_distance(&repo, branch_commit.id, tracked_commit.id)
                .or_else(|| git_rev_list_count(project_root, &tracked_ref, &branch_ref));
            if let Some(distance) = distance {
                let replace = best_ancestor
                    .as_ref()
                    .is_none_or(|(_, best_distance, best_time)| {
                        distance < *best_distance
                            || (distance == *best_distance && time.seconds > best_time.seconds)
                    });
                if replace {
                    best_ancestor = Some((tracked_name.clone(), distance, time));
                }
            }
            continue;
        }

        // Fallback for siblings / non-ancestor branches: keep the most recent
        // common ancestor so seeding still prefers the closest tracked history.
        if best_merge_base
            .as_ref()
            .is_none_or(|(_, best_time)| time.seconds > best_time.seconds)
        {
            best_merge_base = Some((tracked_name.clone(), time));
        }
    }

    best_ancestor
        .map(|(name, _, _)| name)
        .or_else(|| best_merge_base.map(|(name, _)| name))
}

/// Outcome of `TraceDecay` branch tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAddOutcome {
    /// The project has no `.tracedecay/` index; nothing was done.
    NotIndexed,
    /// The branch was already tracked; no copy/sync was performed. Legacy
    /// single-DB metadata may have been persisted for the default branch.
    AlreadyTracked,
    /// A new branch DB was created from the nearest ancestor and synced.
    Added,
    /// Another process was adding or syncing; metadata/DB may be created, but
    /// catch-up sync was deferred.
    Deferred,
}

pub enum BranchTrackingPreparation {
    AlreadyTracked,
    Deferred,
    Added(PreparedBranchTracking),
}

pub struct PreparedBranchTracking {
    branch_name: String,
    db_file: String,
    new_db_path: PathBuf,
    _branch_lock: std::fs::File,
}

/// Copies the nearest tracked ancestor DB and writes branch metadata.
///
/// The returned [`PreparedBranchTracking`] owns the branch-add lock and must be
/// kept alive until the caller either finalizes or rolls back the new branch.
pub async fn prepare_branch_tracking_in_layout(
    project_root: &Path,
    branch_name: &str,
    tracedecay_dir: &Path,
) -> crate::errors::Result<BranchTrackingPreparation> {
    use crate::branch_meta;

    let branch_lock = {
        let mut attempts = 0;
        loop {
            match try_acquire_branch_add_lock(tracedecay_dir) {
                Ok(lock) => break lock,
                Err(crate::errors::TraceDecayError::SyncLock { .. })
                    if attempts < BRANCH_LOCK_RETRY_ATTEMPTS =>
                {
                    attempts += 1;
                    tokio::time::sleep(BRANCH_LOCK_RETRY_INTERVAL).await;
                }
                Err(crate::errors::TraceDecayError::SyncLock { .. }) => {
                    return Ok(BranchTrackingPreparation::Deferred);
                }
                Err(e) => return Err(e),
            }
        }
    };

    let meta_path = tracedecay_dir.join("branch-meta.json");
    let (mut meta, metadata_was_missing) = match branch_meta::load_branch_meta(tracedecay_dir) {
        Some(meta) => (meta, false),
        None if meta_path.exists() => {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "corrupt branch metadata at '{}'; repair or remove it before adding branch tracking",
                    meta_path.display()
                ),
            });
        }
        None => {
            let default = detect_default_branch(project_root).ok_or_else(|| {
                crate::errors::TraceDecayError::Config {
                    message: format!(
                        "cannot initialize missing branch metadata at '{}': repository default branch is unknown (detached HEAD or no default ref)",
                        meta_path.display()
                    ),
                }
            })?;
            (
                branch_meta::BranchMeta::for_legacy_single_db(tracedecay_dir, &default),
                true,
            )
        }
    };
    prune_missing_branch_dbs(tracedecay_dir, &mut meta);

    if meta.is_tracked(branch_name) {
        if metadata_was_missing {
            branch_meta::save_branch_meta(tracedecay_dir, &meta)?;
        }
        return Ok(BranchTrackingPreparation::AlreadyTracked);
    }

    // Fail fast (before parent resolution) when the name sanitizes to empty —
    // it would otherwise produce a hidden `branches/.db`.
    if sanitize_branch_name(branch_name).is_empty() {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot track branch '{branch_name}': its name sanitizes to an empty filename"
            ),
        });
    }

    let parent = find_nearest_tracked_ancestor(project_root, branch_name, &meta)
        .unwrap_or_else(|| meta.default_branch.clone());
    let parent_db = resolve_branch_db_path(tracedecay_dir, &parent, &meta).ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: format!("parent branch '{parent}' has no DB"),
        }
    })?;
    if !parent_db.exists() {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!("parent DB not found at '{}'", parent_db.display()),
        });
    }

    let branches_dir = branch_meta::ensure_branches_dir(tracedecay_dir)?;
    // Pick a collision-free stem so a branch whose sanitized name matches an
    // already-tracked branch gets its own DB instead of overwriting it (#3).
    let stem = unique_branch_db_stem(&meta, &branches_dir, branch_name).ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot track branch '{branch_name}': its name sanitizes to an empty filename"
            ),
        }
    })?;
    let new_db_path = branches_dir.join(format!("{stem}.db"));
    // Copy through SQLite rather than cloning the live main file. The
    // branch-add lock serializes metadata changes, but it does not stop other
    // processes from writing or checkpointing the parent WAL.
    let snapshot_result = create_consistent_branch_snapshot(&parent_db, &new_db_path).await;
    snapshot_result?;

    // Save metadata before the caller opens the new branch DB for sync.
    let db_file = format!("branches/{stem}.db");
    meta.add_branch(branch_name, &db_file, &parent);
    if let Err(e) = branch_meta::save_branch_meta(tracedecay_dir, &meta) {
        remove_branch_db_files(&new_db_path);
        return Err(e.into());
    }

    Ok(BranchTrackingPreparation::Added(PreparedBranchTracking {
        branch_name: branch_name.to_string(),
        db_file,
        new_db_path,
        _branch_lock: branch_lock,
    }))
}

#[cfg(test)]
#[tokio::test]
async fn default_branch_bootstrap_persists_canonical_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let project_root = temp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let run_git = |args: &[&str]| {
        let output = std::process::Command::new(crate::git::git_program())
            .args(args)
            .current_dir(&project_root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init", "-b", "main"]);
    std::fs::write(project_root.join("fixture"), b"fixture").unwrap();
    run_git(&["add", "fixture"]);
    run_git(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=TraceDecay Test",
        "commit",
        "-m",
        "fixture",
    ]);

    let data_dir = temp.path().join("profile-shard");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join(crate::config::DB_FILENAME), b"graph").unwrap();
    let meta_path = data_dir.join(crate::storage::BRANCH_META_FILENAME);
    assert!(!meta_path.exists());

    let outcome = prepare_branch_tracking_in_layout(&project_root, "main", &data_dir)
        .await
        .unwrap();

    assert!(matches!(outcome, BranchTrackingPreparation::AlreadyTracked));
    let meta = crate::branch_meta::load_branch_meta(&data_dir).unwrap();
    assert_eq!(meta.default_branch, "main");
    assert_eq!(meta.branches.len(), 1);
    let default = meta.branches.get("main").unwrap();
    assert_eq!(default.db_file, crate::config::db_filename(&data_dir));
    assert!(default.parent.is_none());
    assert_eq!(default.created_at, "0");
    assert_eq!(default.last_synced_at, "0");
    assert!(!meta_path.with_extension("json.tmp").exists());
    assert!(!data_dir.join("branches").exists());
}

pub fn finalize_prepared_branch_tracking(tracedecay_dir: &Path, prepared: &PreparedBranchTracking) {
    if let Some(mut meta) = crate::branch_meta::load_branch_meta(tracedecay_dir) {
        meta.touch_synced(&prepared.branch_name);
        let _ = crate::branch_meta::save_branch_meta(tracedecay_dir, &meta);
    }
}

pub fn rollback_prepared_branch_tracking(tracedecay_dir: &Path, prepared: &PreparedBranchTracking) {
    rollback_branch_tracking(
        tracedecay_dir,
        &prepared.branch_name,
        &prepared.db_file,
        &prepared.new_db_path,
    );
}

fn rollback_branch_tracking(
    tracedecay_dir: &Path,
    branch_name: &str,
    db_file: &str,
    new_db_path: &Path,
) {
    if let Some(mut meta) = crate::branch_meta::load_branch_meta(tracedecay_dir) {
        let should_remove = meta
            .branches
            .get(branch_name)
            .is_some_and(|entry| entry.db_file == db_file);
        if should_remove {
            meta.remove_branch(branch_name);
            let _ = crate::branch_meta::save_branch_meta(tracedecay_dir, &meta);
        }
    }
    let still_ours = crate::branch_meta::load_branch_meta(tracedecay_dir)
        .and_then(|meta| meta.branches.get(branch_name).cloned())
        .is_none_or(|entry| entry.db_file == db_file);
    if still_ours {
        remove_branch_db_files(new_db_path);
    }
}

fn prune_missing_branch_dbs(tracedecay_dir: &Path, meta: &mut crate::branch_meta::BranchMeta) {
    let missing: Vec<String> = meta
        .branches
        .iter()
        .filter_map(|(name, entry)| {
            if name == &meta.default_branch {
                return None;
            }
            let path = tracedecay_dir.join(&entry.db_file);
            (!path.exists()).then(|| name.clone())
        })
        .collect();
    for name in missing {
        meta.remove_branch(&name);
    }
}

fn try_acquire_branch_add_lock(tracedecay_dir: &Path) -> crate::errors::Result<std::fs::File> {
    use fs2::FileExt;

    std::fs::create_dir_all(tracedecay_dir)?;
    let lock_path = tracedecay_dir.join(".branch-add.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.try_lock_exclusive()
        .map_err(|e| crate::errors::TraceDecayError::SyncLock {
            message: format!("branch add already running at {}: {e}", lock_path.display()),
        })?;
    Ok(file)
}

/// Blocking-with-timeout variant of [`try_acquire_branch_add_lock`] for
/// synchronous callers. Retries a briefly-contended lock (a concurrent branch
/// add is only holding it for the duration of a DB clone) before giving up.
/// Returns `None` on timeout or a non-contention error, so the caller can skip
/// its mutation this round rather than proceed unsynchronized.
fn acquire_branch_add_lock_blocking(tracedecay_dir: &Path) -> Option<std::fs::File> {
    for _ in 0..BRANCH_LOCK_RETRY_ATTEMPTS {
        match try_acquire_branch_add_lock(tracedecay_dir) {
            Ok(lock) => return Some(lock),
            Err(crate::errors::TraceDecayError::SyncLock { .. }) => {
                std::thread::sleep(BRANCH_LOCK_RETRY_INTERVAL);
            }
            Err(_) => return None,
        }
    }
    None
}

fn remove_branch_db_files(db_path: &Path) {
    let _ = std::fs::remove_file(db_path);
    let mut sidecar = db_path.to_path_buf();
    sidecar.set_extension("db-wal");
    let _ = std::fs::remove_file(&sidecar);
    sidecar.set_extension("db-shm");
    let _ = std::fs::remove_file(&sidecar);
}

async fn create_consistent_branch_snapshot(src: &Path, dst: &Path) -> crate::errors::Result<()> {
    let parent_dir = dst
        .parent()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("branch snapshot path '{}' has no parent", dst.display()),
        })?;
    let stem = dst
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("branch");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent_dir.join(format!(
        ".{stem}.snapshot-{}-{nonce}.db",
        std::process::id()
    ));
    let result = async {
        let (source, _) = crate::db::Database::open_read_only(src).await?;
        source.snapshot_to(&temp).await?;
        std::fs::hard_link(&temp, dst).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!(
                    "failed to publish branch snapshot '{}' without replacing an existing store: {error}",
                    dst.display()
                ),
            }
        })?;
        Ok(())
    }
    .await;
    remove_branch_db_files(&temp);
    result
}

/// Untracks a single non-default branch: removes its metadata entry and deletes
/// its DB file (plus `-wal`/`-shm` sidecars). Returns `true` when an entry was
/// removed. The default branch is never removed. This is the shared removal path
/// used by `tracedecay branch remove` and the PR-autotrack lifecycle so both
/// clean up identically.
pub fn remove_tracked_branch_store(tracedecay_dir: &Path, branch: &str) -> bool {
    // Serialize on the same branch-add lock every other `branch-meta.json`
    // mutator holds (prepare_branch_tracking_in_layout, gc_dead_branch_stores).
    // Without it, this unlocked load→remove→save races a concurrent
    // `branch add`: depending on write order it either silently drops the
    // user's just-added branch or resurrects a removed entry pointing at a
    // deleted DB. On sustained contention, skip removal (returning false); the
    // caller retries next cycle — nothing is lost. Callers never hold this lock
    // when calling in (branch add / GC acquire-then-release), so no deadlock.
    let Some(_lock) = acquire_branch_add_lock_blocking(tracedecay_dir) else {
        return false;
    };
    let Some(mut meta) = crate::branch_meta::load_branch_meta(tracedecay_dir) else {
        return false;
    };
    let Some(entry) = meta.remove_branch(branch) else {
        return false;
    };
    remove_branch_db_files(&tracedecay_dir.join(&entry.db_file));
    let _ = crate::branch_meta::save_branch_meta(tracedecay_dir, &meta);
    true
}

/// Returns true if `branch` currently exists as a local `refs/heads/*` ref.
///
/// Thin alias over [`local_branch_exists`] under the name the branch-store GC
/// design refers to; keeping both avoids churning existing call sites.
pub fn is_branch_ref_present(project_root: &Path, branch: &str) -> bool {
    local_branch_exists(project_root, branch)
}

/// Result of a dead/orphan branch-store GC pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// Names of tracked branches whose DB + metadata entry were removed because
    /// their git ref is gone and their last sync predates the grace window.
    pub removed_tracked: Vec<String>,
    /// Paths of orphan `branches/*.db` files (not referenced by any meta entry)
    /// that were deleted because their mtime predates the grace window.
    pub removed_orphan_dbs: Vec<PathBuf>,
}

/// Parses a `last_synced_at` / `created_at` unix-seconds string defensively.
/// Returns 0 (epoch, i.e. maximally stale) when unparseable so a corrupt
/// timestamp never protects a dead store from collection.
fn parse_unix_secs(ts: &str) -> u64 {
    ts.trim().parse::<u64>().unwrap_or(0)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Garbage-collects dead and orphaned branch stores.
///
/// Two independent sweeps, both age-gated so an in-flight branch that has not
/// yet synced or a just-deleted-then-recreated ref is never collected:
///
/// (a) **Tracked, ref-gone branches** — for each unprotected tracked
///     non-default branch whose git ref no longer exists AND whose
///     `last_synced_at` is older than `branch_gc_days`, remove its DB files and
///     metadata entry. The default branch and GC-protected entries are never
///     removed.
/// (b) **Orphan DBs** — `branches/*.db` files not referenced by any meta entry
///     whose mtime is older than `orphan_db_gc_days` are deleted along with
///     their `-wal`/`-shm` sidecars.
///
/// The whole pass holds the branch-add lock so it never races a concurrent
/// branch-add (which is creating files GC would otherwise see as orphans).
/// If the lock cannot be acquired promptly, GC is skipped this round and an
/// empty report is returned — the daemon retries on its next tick. Logging is
/// the caller's responsibility; this function is silent.
pub fn gc_dead_branch_stores(
    project_root: &Path,
    tracedecay_dir: &Path,
    branch_gc_days: u64,
    orphan_db_gc_days: u64,
) -> GcReport {
    let mut report = GcReport::default();

    // Serialize against branch-add so we don't delete a DB it is mid-creation.
    let Ok(_lock) = try_acquire_branch_add_lock(tracedecay_dir) else {
        return report;
    };

    let now = now_unix_secs();

    // (a) Tracked branches whose ref is gone and whose last sync is stale.
    if let Some(mut meta) = crate::branch_meta::load_branch_meta(tracedecay_dir) {
        let branch_grace = branch_gc_days.saturating_mul(86_400);
        let default_branch = meta.default_branch.clone();
        let candidates: Vec<(String, PathBuf, u64)> = meta
            .branches
            .iter()
            .filter(|(name, entry)| **name != default_branch && !entry.gc_protected)
            .map(|(name, entry)| {
                (
                    name.clone(),
                    tracedecay_dir.join(&entry.db_file),
                    parse_unix_secs(&entry.last_synced_at),
                )
            })
            .collect();

        let mut removed_any = false;
        for (name, db_path, last_synced) in candidates {
            // Never collect a branch whose ref still resolves, and never one
            // synced within the grace window (`<= now` age guards a clock skew
            // where last_synced is in the future).
            if is_branch_ref_present(project_root, &name) {
                continue;
            }
            let age = now.saturating_sub(last_synced);
            if age < branch_grace {
                continue;
            }
            remove_branch_db_files(&db_path);
            meta.remove_branch(&name);
            report.removed_tracked.push(name);
            removed_any = true;
        }
        if removed_any {
            let _ = crate::branch_meta::save_branch_meta(tracedecay_dir, &meta);
        }

        // (b) Orphan DBs: files under branches/ not referenced by any surviving
        // meta entry. Recompute the referenced set AFTER the removals above so
        // a just-removed branch's DB (already deleted) is not double-counted.
        let referenced: std::collections::HashSet<PathBuf> = meta
            .branches
            .values()
            .map(|entry| tracedecay_dir.join(&entry.db_file))
            .collect();
        report.removed_orphan_dbs =
            sweep_orphan_dbs(tracedecay_dir, &referenced, orphan_db_gc_days, now);
    } else {
        // No branch metadata: every branches/*.db is an orphan candidate.
        report.removed_orphan_dbs = sweep_orphan_dbs(
            tracedecay_dir,
            &std::collections::HashSet::new(),
            orphan_db_gc_days,
            now,
        );
    }

    report
}

/// Deletes stale `branches/*.db` files (+ sidecars) not in `referenced`.
fn sweep_orphan_dbs(
    tracedecay_dir: &Path,
    referenced: &std::collections::HashSet<PathBuf>,
    orphan_db_gc_days: u64,
    now: u64,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let branches_dir = tracedecay_dir.join("branches");
    let Ok(entries) = std::fs::read_dir(&branches_dir) else {
        return removed;
    };
    let orphan_grace = orphan_db_gc_days.saturating_mul(86_400);
    for entry in entries.flatten() {
        let path = entry.path();
        // Only main `.db` files are stores; sidecars are removed alongside.
        if path.extension().and_then(|e| e.to_str()) != Some("db") {
            continue;
        }
        if referenced.contains(&path) {
            continue;
        }
        // Age-gate on mtime; a freshly-created orphan (e.g. a branch-add whose
        // meta save is momentarily lagging) is kept until it ages out.
        let mtime_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let age = now.saturating_sub(mtime_secs);
        if age < orphan_grace {
            continue;
        }
        remove_branch_db_files(&path);
        removed.push(path);
    }
    removed
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
