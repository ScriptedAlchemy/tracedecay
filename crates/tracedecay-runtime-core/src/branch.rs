//! Git branch resolution utilities for multi-branch indexing.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;

/// Bounded-retry policy for a briefly-contended branch-add lock: a concurrent
/// branch add only holds the lock for the duration of a DB clone, so a short
/// spin lets a contender through instead of failing immediately. Shared by the
/// async [`prepare_branch_tracking_in_layout`] and the synchronous
/// administrative path; only the sleep primitive differs.
pub const BRANCH_LOCK_RETRY_ATTEMPTS: usize = 20;
pub const BRANCH_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

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

        let Err(error) = prepare_branch_tracking_in_layout(&project_root, "trunk", &data_dir).await
        else {
            panic!("detached legacy store must not invent a default branch")
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
) -> crate::errors::Result<Option<String>> {
    let base = sanitize_branch_name(branch_name);
    if base.is_empty() {
        return Ok(None);
    }
    let conflicts = |stem: &str| -> crate::errors::Result<bool> {
        let db_file = format!("branches/{stem}.db");
        let meta_conflict = meta
            .branches
            .iter()
            .any(|(name, entry)| name != branch_name && entry.db_file == db_file);
        let database_path = branches_dir.join(format!("{stem}.db"));
        let file_conflict = database_path.exists();
        let retired_path = crate::db::database_path_is_tombstoned(&database_path)?;
        Ok(meta_conflict || file_conflict || retired_path)
    };
    if !conflicts(&base)? {
        return Ok(Some(base));
    }
    let hashed = format!("{base}-{}", short_branch_hash(branch_name));
    if !conflicts(&hashed)? {
        return Ok(Some(hashed));
    }
    for suffix in 1..10_000 {
        let candidate = format!("{hashed}-{suffix}");
        if !conflicts(&candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
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
    prepare_branch_tracking_in_layout_with_lock(
        project_root,
        branch_name,
        tracedecay_dir,
        try_acquire_branch_add_lock_raw,
    )
    .await
}

/// Runs branch tracking with an injected lock acquisition policy.
///
/// The root compatibility façade supplies its pending branch-admin recovery
/// gate; standalone kernel callers use the raw lock above.
#[doc(hidden)]
pub async fn prepare_branch_tracking_in_layout_with_lock(
    project_root: &Path,
    branch_name: &str,
    tracedecay_dir: &Path,
    acquire_branch_lock: fn(&Path) -> crate::errors::Result<std::fs::File>,
) -> crate::errors::Result<BranchTrackingPreparation> {
    use crate::branch_meta;

    let branch_lock = {
        let mut attempts = 0;
        loop {
            match acquire_branch_lock(tracedecay_dir) {
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
    let pruned_missing_branches = prune_missing_branch_dbs(tracedecay_dir, &mut meta);

    if meta.is_tracked(branch_name) {
        if metadata_was_missing || pruned_missing_branches {
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
    let stem = unique_branch_db_stem(&meta, &branches_dir, branch_name)?.ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot track branch '{branch_name}': no unretired collision-free database filename is available"
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

#[cfg(test)]
#[tokio::test]
async fn already_tracked_branch_persists_pruned_missing_database_entries() {
    let temp = tempfile::tempdir().unwrap();
    let project_root = temp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let data_dir = temp.path().join("profile-shard");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join(crate::config::DB_FILENAME), b"graph").unwrap();

    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("stale", "branches/missing.db", "main");
    crate::branch_meta::save_branch_meta(&data_dir, &meta).unwrap();

    let outcome = prepare_branch_tracking_in_layout(&project_root, "main", &data_dir)
        .await
        .unwrap();

    assert!(matches!(outcome, BranchTrackingPreparation::AlreadyTracked));
    let persisted = crate::branch_meta::load_branch_meta(&data_dir).unwrap();
    assert!(!persisted.is_tracked("stale"));
}

#[cfg(test)]
#[test]
fn rollback_keeps_database_when_metadata_removal_cannot_be_saved() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    let branches_dir = data_dir.join("branches");
    std::fs::create_dir_all(&branches_dir).unwrap();
    let db_path = branches_dir.join("feature.db");
    std::fs::write(&db_path, b"graph").unwrap();

    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature", "branches/feature.db", "main");
    crate::branch_meta::save_branch_meta(data_dir, &meta).unwrap();
    std::fs::create_dir(data_dir.join("branch-meta.json.tmp")).unwrap();

    rollback_branch_tracking(data_dir, "feature", "branches/feature.db", &db_path);

    assert!(db_path.exists());
    let persisted = crate::branch_meta::load_branch_meta(data_dir).unwrap();
    assert!(persisted.is_tracked("feature"));
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
    let metadata_removed =
        crate::branch_meta::load_branch_meta(tracedecay_dir).is_some_and(|mut meta| {
            let should_remove = meta
                .branches
                .get(branch_name)
                .is_some_and(|entry| entry.db_file == db_file);
            if !should_remove {
                return false;
            }
            meta.remove_branch(branch_name);
            crate::branch_meta::save_branch_meta(tracedecay_dir, &meta).is_ok()
        });
    let removal_persisted = metadata_removed
        && crate::branch_meta::load_branch_meta(tracedecay_dir)
            .is_some_and(|meta| !meta.branches.contains_key(branch_name));
    if removal_persisted {
        remove_branch_db_files(new_db_path);
    }
}

fn prune_missing_branch_dbs(
    tracedecay_dir: &Path,
    meta: &mut crate::branch_meta::BranchMeta,
) -> bool {
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
    let changed = !missing.is_empty();
    for name in missing {
        meta.remove_branch(&name);
    }
    changed
}

pub fn try_acquire_branch_add_lock_raw(
    tracedecay_dir: &Path,
) -> crate::errors::Result<std::fs::File> {
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

pub fn try_acquire_branch_add_lock(tracedecay_dir: &Path) -> crate::errors::Result<std::fs::File> {
    try_acquire_branch_add_lock_raw(tracedecay_dir)
}

pub fn acquire_branch_lock_blocking(tracedecay_dir: &Path) -> crate::errors::Result<std::fs::File> {
    try_acquire_branch_add_lock_raw(tracedecay_dir)
}

fn remove_branch_db_files(db_path: &Path) {
    let _ = remove_branch_db_files_checked(db_path);
}

fn remove_branch_db_files_checked(db_path: &Path) -> crate::errors::Result<()> {
    for path in [
        db_path.to_path_buf(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
        db_path.with_extension("db-journal"),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!(
                        "failed to remove branch database '{}': {error}",
                        path.display()
                    ),
                });
            }
        }
    }
    Ok(())
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
        let authority =
            crate::db::DatabaseAuthority::for_runtime(src, "create branch snapshot")?;
        let (source, _) = crate::db::Database::open_read_only(src, &authority).await?;
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
    let cleanup = remove_branch_db_files_checked(&temp);
    match result {
        Err(error) => Err(error),
        Ok(()) => cleanup,
    }
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
pub fn parse_unix_secs(ts: &str) -> u64 {
    ts.trim().parse::<u64>().unwrap_or(0)
}

pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Compatibility wrapper retained for callers that cannot reach the managed
/// daemon. Physical branch-store GC requires daemon-owned store administration,
/// so this API fails closed without mutating metadata or `SQLite` files.
pub fn gc_dead_branch_stores(
    _project_root: &Path,
    _tracedecay_dir: &Path,
    _branch_gc_days: u64,
    _orphan_db_gc_days: u64,
) -> GcReport {
    // Physical branch-store GC requires daemon-owned writer exclusion, cached
    // owner checks, a deletion fence, and holder proof. This compatibility API
    // cannot establish those invariants, so it deliberately fails closed.
    GcReport::default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
