//! Git branch resolution utilities for multi-branch indexing.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;

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
        // (~100-300ms on Windows) to re-ask git.
        return repo.find_reference(&refname).is_ok();
    }
    if !crate::worktree::git_may_resolve_repo(project_root) {
        return false;
    }
    std::process::Command::new("git")
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
    let output = std::process::Command::new("git")
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
    let output = std::process::Command::new("git")
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
/// spawn (~100-300ms on Windows) on every branch-add parent ranking.
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

/// Auto-detects the default branch (main or master).
///
/// Strategy:
/// 1. Try `git symbolic-ref refs/remotes/origin/HEAD`
/// 2. Fall back to checking if `main` or `master` exists locally
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

    None
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
    Some(format!("{base}-{}", short_branch_hash(branch_name)))
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
    /// The branch was already tracked; no copy/sync was performed.
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
                Err(crate::errors::TraceDecayError::SyncLock { .. }) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(crate::errors::TraceDecayError::SyncLock { .. }) => {
                    return Ok(BranchTrackingPreparation::Deferred);
                }
                Err(e) => return Err(e),
            }
        }
    };

    let meta_path = tracedecay_dir.join("branch-meta.json");
    let mut meta = match branch_meta::load_branch_meta(tracedecay_dir) {
        Some(meta) => meta,
        None if meta_path.exists() => {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "corrupt branch metadata at '{}'; repair or remove it before adding branch tracking",
                    meta_path.display()
                ),
            });
        }
        None => {
            let default = detect_default_branch(project_root).unwrap_or_else(|| "main".to_string());
            branch_meta::BranchMeta::new_for_dir(tracedecay_dir, &default)
        }
    };
    prune_missing_branch_dbs(tracedecay_dir, &mut meta);

    if meta.is_tracked(branch_name) {
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
    if let Err(e) = std::fs::copy(&parent_db, &new_db_path) {
        remove_branch_db_files(&new_db_path);
        return Err(e.into());
    }

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

fn remove_branch_db_files(db_path: &Path) {
    let _ = std::fs::remove_file(db_path);
    let mut sidecar = db_path.to_path_buf();
    sidecar.set_extension("db-wal");
    let _ = std::fs::remove_file(&sidecar);
    sidecar.set_extension("db-shm");
    let _ = std::fs::remove_file(&sidecar);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_branch_name("main"), "main");
    }

    #[test]
    fn sanitize_slashes() {
        assert_eq!(sanitize_branch_name("feature/foo/bar"), "feature_foo_bar");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_branch_name("fix: bug <1>"), "fix_bug_1");
    }

    #[test]
    fn sanitize_dots_prevented() {
        // ".." becomes all underscores, collapsed and trimmed to empty
        assert_eq!(sanitize_branch_name(".."), "");
        // dots and slashes become underscores, collapsed
        assert_eq!(sanitize_branch_name("foo/../bar"), "foo_bar");
    }

    #[test]
    fn unique_stem_keeps_free_name() {
        let meta = crate::branch_meta::BranchMeta::new("main");
        let dir = Path::new("/nonexistent-branches-dir-for-test");
        assert_eq!(
            unique_branch_db_stem(&meta, dir, "feature/new").unwrap(),
            "feature_new"
        );
    }

    #[test]
    fn unique_stem_disambiguates_sanitization_collision() {
        // "feature/foo" sanitizes to the same stem as the literal "feature_foo".
        let mut meta = crate::branch_meta::BranchMeta::new("main");
        meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
        let dir = Path::new("/nonexistent-branches-dir-for-test");
        let stem = unique_branch_db_stem(&meta, dir, "feature_foo").unwrap();
        assert_ne!(
            stem, "feature_foo",
            "second branch must not reuse the first branch's DB file"
        );
        assert!(stem.starts_with("feature_foo-"), "got: {stem}");
    }

    #[test]
    fn unique_stem_is_idempotent_for_same_branch() {
        // Recomputing for a branch already in meta must not treat its own entry
        // as a conflict.
        let mut meta = crate::branch_meta::BranchMeta::new("main");
        meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
        let dir = Path::new("/nonexistent-branches-dir-for-test");
        assert_eq!(
            unique_branch_db_stem(&meta, dir, "feature/foo").unwrap(),
            "feature_foo"
        );
    }

    #[test]
    fn unique_stem_rejects_empty_sanitization() {
        let meta = crate::branch_meta::BranchMeta::new("main");
        let dir = Path::new("/nonexistent-branches-dir-for-test");
        assert!(unique_branch_db_stem(&meta, dir, "..").is_none());
        assert!(unique_branch_db_stem(&meta, dir, "///").is_none());
    }
}
