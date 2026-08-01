//! Git branch resolution utilities for multi-branch indexing.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;

mod admin;

pub use admin::{
    BranchAdminAction, BranchAdminOutcome, BranchAdminReport, PreparedBranchAdminMutation,
    prepare_branch_admin_mutation, remove_tracked_branch_store_checked,
};
pub(crate) use admin::{BranchAdminRecoveryDisposition, prepare_pending_branch_admin_recovery};

/// Installs the root-owned pending branch-admin recovery gate into the kernel
/// lock primitives.
///
/// The gate reads `branch::admin::transaction`'s journal, which stayed in this
/// crate, so the kernel calls back through
/// `tracedecay_runtime_core::ports::branch_admin_recovery`. Idempotent; every
/// process entry point that can take a branch lock must call it before doing
/// so.
pub fn register_branch_admin_recovery_gate() {
    tracedecay_runtime_core::ports::branch_admin_recovery::register(
        admin::ensure_no_pending_branch_admin_recovery,
    );
}

/// The shared branch-add lock, its retry policy, and the current-branch read
/// moved into `tracedecay_runtime_core::branch`: `branch_meta` and `worktree`
/// depend on them and now live in that crate. Re-exported so every historical
/// `crate::branch::<item>` path keeps resolving.
pub use tracedecay_runtime_core::branch::{
    BRANCH_LOCK_RETRY_ATTEMPTS, BRANCH_LOCK_RETRY_INTERVAL, BranchMemo,
    acquire_branch_add_lock_blocking_raw, acquire_branch_lock_blocking, current_branch,
    try_acquire_branch_add_lock, try_acquire_branch_add_lock_raw,
};

/// Default-branch detection, branch-name sanitisation, and branch DB-path
/// resolution also moved into `tracedecay_runtime_core::branch`: they are pure
/// (`gix`, `current_branch`, and `branch_meta::BranchMeta`, all kernel) and the
/// extracted migration crate consumes all three. Re-exported so every
/// historical `crate::branch::<item>` path keeps resolving.
pub use tracedecay_runtime_core::branch::{
    detect_default_branch, resolve_branch_db_path, sanitize_branch_name,
};

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

impl PreparedBranchTracking {
    pub(crate) fn database_path(&self) -> &Path {
        &self.new_db_path
    }
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
    prepare_branch_tracking_in_layout_with_source(project_root, branch_name, tracedecay_dir, None)
        .await
}

pub(crate) async fn prepare_branch_tracking_from_database(
    project_root: &Path,
    branch_name: &str,
    tracedecay_dir: &Path,
    source: &crate::db::Database,
) -> crate::errors::Result<BranchTrackingPreparation> {
    prepare_branch_tracking_in_layout_with_source(
        project_root,
        branch_name,
        tracedecay_dir,
        Some(source),
    )
    .await
}

async fn prepare_branch_tracking_in_layout_with_source(
    project_root: &Path,
    branch_name: &str,
    tracedecay_dir: &Path,
    source: Option<&crate::db::Database>,
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
    if let Some(source) = source {
        let parent_db = parent_db
            .canonicalize()
            .unwrap_or_else(|_| parent_db.clone());
        if source.database_path() != parent_db {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "retained graph '{}' does not own selected parent branch database '{}'",
                    source.database_path().display(),
                    parent_db.display()
                ),
            });
        }
    }
    let snapshot_result = create_consistent_branch_snapshot(&parent_db, &new_db_path, source).await;
    snapshot_result?;

    // Save metadata before the caller opens the new branch DB for sync.
    let db_file = format!("branches/{stem}.db");
    meta.add_branch(branch_name, &db_file, &parent);
    if let Err(e) = branch_meta::save_branch_meta(tracedecay_dir, &meta) {
        return match admin::remove_branch_db_files_checked(&new_db_path) {
            Ok(()) => Err(e.into()),
            Err(cleanup_error) => Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "failed to publish branch metadata: {e}; unpublished snapshot cleanup also failed: {cleanup_error}"
                ),
            }),
        };
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

    let error = rollback_branch_tracking(data_dir, "feature", "branches/feature.db", &db_path)
        .expect_err("blocked metadata publication must fail rollback");

    assert!(db_path.exists());
    let persisted = crate::branch_meta::load_branch_meta(data_dir).unwrap();
    assert!(persisted.is_tracked("feature"));
    assert!(
        error.to_string().contains("branch metadata"),
        "unexpected rollback error: {error}"
    );
}

#[cfg(test)]
#[test]
fn rollback_quarantines_complete_database_family_and_retires_path() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    let branches_dir = data_dir.join("branches");
    std::fs::create_dir_all(&branches_dir).unwrap();
    let db_path = branches_dir.join("feature.db");
    for path in [
        db_path.clone(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
    ] {
        std::fs::write(path, b"sqlite").unwrap();
    }

    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature", "branches/feature.db", "main");
    crate::branch_meta::save_branch_meta(data_dir, &meta).unwrap();

    rollback_branch_tracking(data_dir, "feature", "branches/feature.db", &db_path).unwrap();

    assert!(!db_path.exists());
    assert!(!db_path.with_extension("db-wal").exists());
    assert!(!db_path.with_extension("db-shm").exists());
    assert!(
        !crate::branch_meta::load_branch_meta(data_dir)
            .unwrap()
            .is_tracked("feature")
    );
    assert!(crate::db::database_path_is_tombstoned(&db_path).unwrap());
    assert!(!data_dir.join(".branch-delete-transaction.json").exists());
}

#[cfg(test)]
#[test]
fn rollback_refuses_database_with_active_authority() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path();
    let branches_dir = data_dir.join("branches");
    std::fs::create_dir_all(&branches_dir).unwrap();
    let db_path = branches_dir.join("feature.db");
    std::fs::write(&db_path, b"sqlite").unwrap();

    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature", "branches/feature.db", "main");
    crate::branch_meta::save_branch_meta(data_dir, &meta).unwrap();
    let _authority = crate::db::DatabaseAuthority::for_runtime(
        &db_path,
        "test active branch database authority",
    )
    .unwrap();

    let error = rollback_branch_tracking(data_dir, "feature", "branches/feature.db", &db_path)
        .expect_err("active database authority must fence rollback");

    assert!(
        error
            .to_string()
            .contains("incompatible database authority")
    );
    assert!(db_path.exists());
    assert!(
        crate::branch_meta::load_branch_meta(data_dir)
            .unwrap()
            .is_tracked("feature")
    );
}

pub fn finalize_prepared_branch_tracking(tracedecay_dir: &Path, prepared: &PreparedBranchTracking) {
    if let Some(mut meta) = crate::branch_meta::load_branch_meta(tracedecay_dir) {
        meta.touch_synced(&prepared.branch_name);
        let _ = crate::branch_meta::save_branch_meta(tracedecay_dir, &meta);
    }
}

pub fn rollback_prepared_branch_tracking(
    tracedecay_dir: &Path,
    prepared: &PreparedBranchTracking,
) -> crate::errors::Result<()> {
    rollback_branch_tracking(
        tracedecay_dir,
        &prepared.branch_name,
        &prepared.db_file,
        &prepared.new_db_path,
    )
}

fn rollback_branch_tracking(
    tracedecay_dir: &Path,
    branch_name: &str,
    db_file: &str,
    new_db_path: &Path,
) -> crate::errors::Result<()> {
    admin::rollback_published_branch_tracking(tracedecay_dir, branch_name, db_file, new_db_path)
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

async fn create_consistent_branch_snapshot(
    src: &Path,
    dst: &Path,
    retained_source: Option<&crate::db::Database>,
) -> crate::errors::Result<()> {
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
        if let Some(source) = retained_source {
            source.snapshot_to(&temp).await?;
        } else {
            crate::sqlite_read_snapshot::backup_live_sqlite_database(src, &temp)
                .await
                .map_err(|error| crate::errors::TraceDecayError::Database {
                    message: format!("failed to back up live branch database: {error}"),
                    operation: "create branch snapshot".to_owned(),
                })?;
        }
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
    let cleanup = admin::remove_branch_db_files_checked(&temp);
    match result {
        Err(error) => Err(error),
        Ok(()) => cleanup,
    }
}

/// Compatibility wrapper for the PR-autotrack lifecycle. Administrative CLI
/// removal uses [`prepare_branch_admin_mutation`] through the daemon so failures
/// are surfaced instead of collapsed to `false`.
pub fn remove_tracked_branch_store(tracedecay_dir: &Path, branch: &str) -> bool {
    remove_tracked_branch_store_checked(tracedecay_dir, branch)
        .is_ok_and(|report| report.outcome == BranchAdminOutcome::Removed)
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
