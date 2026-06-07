//! Git branch resolution utilities for multi-branch indexing.

use std::path::Path;

use crate::branch_meta::BranchMeta;

/// Resolves the current branch name using `gix`. Falls back to
/// `git symbolic-ref HEAD` for worktrees when gix cannot resolve HEAD
/// (e.g. with minimal feature flags that exclude worktree support).
///
/// Returns `None` for detached HEAD or if the repository cannot be opened.
pub fn current_branch(project_root: &Path) -> Option<String> {
    if let Some(branch) = current_branch_gix(project_root) {
        return Some(branch);
    }
    current_branch_git(project_root)
}

/// Returns true if `branch` exists as a local `refs/heads/*` branch.
pub fn local_branch_exists(project_root: &Path, branch: &str) -> bool {
    if branch.is_empty() {
        return false;
    }
    if let Ok(repo) = gix::open(project_root) {
        let refname = format!("refs/heads/{branch}");
        if repo.find_reference(&refname).is_ok() {
            return true;
        }
    }
    std::process::Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(project_root)
        .status()
        .is_ok_and(|status| status.success())
}

fn current_branch_gix(project_root: &Path) -> Option<String> {
    let repo = gix::open(project_root).ok()?;
    let head = repo.head().ok()?;
    let name = head.name().as_bstr();
    let name_str = std::str::from_utf8(name).ok()?;
    name_str
        .strip_prefix("refs/heads/")
        .map(std::string::ToString::to_string)
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

/// Encodes a branch name for use as a filename.
///
/// Leaves ASCII letters, digits, `_`, and `-` unchanged and percent-encodes all
/// other bytes. This keeps generated DB filenames deterministic, path-safe, and
/// collision-free for branch names such as `feature/foo` and `feature_foo`.
pub fn sanitize_branch_name(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::with_capacity(name.len());
    for &byte in name.as_bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => {
                result.push(char::from(byte));
            }
            _ => {
                result.push('%');
                result.push(char::from(HEX[(byte >> 4) as usize]));
                result.push(char::from(HEX[(byte & 0x0F) as usize]));
            }
        }
    }
    result
}

/// Resolves the DB path for a given branch.
///
/// If the branch is tracked in metadata, returns its `db_file` path.
/// Returns `None` if untracked or if the path would escape `tokensave_dir`.
pub fn resolve_branch_db_path(
    tokensave_dir: &Path,
    branch: &str,
    meta: &BranchMeta,
) -> Option<std::path::PathBuf> {
    let entry = meta.branches.get(branch)?;
    let resolved = tokensave_dir.join(&entry.db_file);
    // Prevent path traversal: resolved path must stay within tokensave_dir
    if let (Ok(canonical_dir), Ok(canonical_path)) =
        (tokensave_dir.canonicalize(), resolved.canonicalize())
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

    let mut best: Option<(String, gix::date::Time)> = None;

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

        // Find merge-base between branch and tracked branch
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
        if best
            .as_ref()
            .is_none_or(|(_, best_time)| time.seconds > best_time.seconds)
        {
            best = Some((tracked_name.clone(), time));
        }
    }

    best.map(|(name, _)| name)
}

/// Outcome of [`add_branch_tracking`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAddOutcome {
    /// The project has no `.tokensave/` index; nothing was done.
    NotIndexed,
    /// The branch was already tracked; no copy/sync was performed.
    AlreadyTracked,
    /// A new branch DB was created from the nearest ancestor and synced.
    Added,
}

/// Silently bootstraps/maintains tokensave branch tracking for `branch_name`.
///
/// This is the library-level branch-add path used by hooks without shelling out
/// to a second process. The CLI keeps its own path so it can report detailed
/// sync statistics. This helper:
/// loads or bootstraps [`BranchMeta`] (via [`detect_default_branch`]), no-ops
/// when the branch is already tracked, otherwise copies the nearest tracked
/// ancestor's DB and runs an incremental sync against the new branch DB.
///
/// No-ops (returns [`BranchAddOutcome::NotIndexed`]) when the project has no
/// `.tokensave/` index, so it never bootstraps indexing in an unindexed repo.
/// Idempotent: a re-add of a tracked branch returns
/// [`BranchAddOutcome::AlreadyTracked`] without re-copying.
pub async fn add_branch_tracking(
    project_root: &Path,
    branch_name: &str,
) -> crate::errors::Result<BranchAddOutcome> {
    use crate::branch_meta;
    use crate::config::get_tokensave_dir;

    if !crate::tokensave::TokenSave::is_initialized(project_root) {
        return Ok(BranchAddOutcome::NotIndexed);
    }
    let tokensave_dir = get_tokensave_dir(project_root);

    let _branch_lock = match try_acquire_branch_add_lock(&tokensave_dir) {
        Ok(lock) => lock,
        Err(crate::errors::TokenSaveError::SyncLock { .. }) => {
            return Ok(BranchAddOutcome::AlreadyTracked);
        }
        Err(e) => return Err(e),
    };

    let mut meta = branch_meta::load_branch_meta(&tokensave_dir).unwrap_or_else(|| {
        let default = detect_default_branch(project_root).unwrap_or_else(|| "main".to_string());
        branch_meta::BranchMeta::new(&default)
    });
    prune_missing_branch_dbs(&tokensave_dir, &mut meta);

    if meta.is_tracked(branch_name) {
        return Ok(BranchAddOutcome::AlreadyTracked);
    }

    let parent = find_nearest_tracked_ancestor(project_root, branch_name, &meta)
        .unwrap_or_else(|| meta.default_branch.clone());
    let parent_db = resolve_branch_db_path(&tokensave_dir, &parent, &meta).ok_or_else(|| {
        crate::errors::TokenSaveError::Config {
            message: format!("parent branch '{parent}' has no DB"),
        }
    })?;
    if !parent_db.exists() {
        return Err(crate::errors::TokenSaveError::Config {
            message: format!("parent DB not found at '{}'", parent_db.display()),
        });
    }

    let sanitized = sanitize_branch_name(branch_name);
    let branches_dir = branch_meta::ensure_branches_dir(&tokensave_dir)?;
    let new_db_path = branches_dir.join(format!("{sanitized}.db"));
    if let Err(e) = std::fs::copy(&parent_db, &new_db_path) {
        remove_branch_db_files(&new_db_path);
        return Err(e.into());
    }

    // Save metadata BEFORE open() so it resolves the new branch to its DB.
    let db_file = format!("branches/{sanitized}.db");
    meta.add_branch(branch_name, &db_file, &parent);
    if let Err(e) = branch_meta::save_branch_meta(&tokensave_dir, &meta) {
        remove_branch_db_files(&new_db_path);
        return Err(e.into());
    }

    let sync_result = async {
        let cg = crate::tokensave::TokenSave::open(project_root).await?;
        let _ = cg.sync().await?;
        Ok::<(), crate::errors::TokenSaveError>(())
    }
    .await;
    if let Err(e) = sync_result {
        rollback_branch_tracking(&tokensave_dir, branch_name, &db_file, &new_db_path);
        return Err(e);
    }

    if let Some(mut meta) = branch_meta::load_branch_meta(&tokensave_dir) {
        meta.touch_synced(branch_name);
        let _ = branch_meta::save_branch_meta(&tokensave_dir, &meta);
    }

    Ok(BranchAddOutcome::Added)
}

fn rollback_branch_tracking(
    tokensave_dir: &Path,
    branch_name: &str,
    db_file: &str,
    new_db_path: &Path,
) {
    if let Some(mut meta) = crate::branch_meta::load_branch_meta(tokensave_dir) {
        let should_remove = meta
            .branches
            .get(branch_name)
            .is_some_and(|entry| entry.db_file == db_file);
        if should_remove {
            meta.remove_branch(branch_name);
            let _ = crate::branch_meta::save_branch_meta(tokensave_dir, &meta);
        }
    }
    let still_ours = crate::branch_meta::load_branch_meta(tokensave_dir)
        .and_then(|meta| meta.branches.get(branch_name).cloned())
        .is_none_or(|entry| entry.db_file == db_file);
    if still_ours {
        remove_branch_db_files(new_db_path);
    }
}

fn prune_missing_branch_dbs(tokensave_dir: &Path, meta: &mut crate::branch_meta::BranchMeta) {
    let missing: Vec<String> = meta
        .branches
        .iter()
        .filter_map(|(name, entry)| {
            if name == &meta.default_branch {
                return None;
            }
            let path = tokensave_dir.join(&entry.db_file);
            (!path.exists()).then(|| name.clone())
        })
        .collect();
    for name in missing {
        meta.remove_branch(&name);
    }
}

fn try_acquire_branch_add_lock(tokensave_dir: &Path) -> crate::errors::Result<std::fs::File> {
    use fs2::FileExt;

    std::fs::create_dir_all(tokensave_dir)?;
    let lock_path = tokensave_dir.join(".branch-add.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.try_lock_exclusive()
        .map_err(|e| crate::errors::TokenSaveError::SyncLock {
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

    fn git(project_root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
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

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_branch_name("main"), "main");
    }

    #[test]
    fn sanitize_distinguishes_slashes_from_underscores() {
        assert_ne!(
            sanitize_branch_name("feature/foo"),
            sanitize_branch_name("feature_foo"),
            "valid branch names must not collide on the same DB filename"
        );
    }

    #[test]
    fn sanitize_slashes() {
        assert_eq!(
            sanitize_branch_name("feature/foo/bar"),
            "feature%2Ffoo%2Fbar"
        );
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(
            sanitize_branch_name("fix: bug <1>"),
            "fix%3A%20bug%20%3C1%3E"
        );
    }

    #[test]
    fn sanitize_dots_prevented() {
        assert_eq!(sanitize_branch_name(".."), "%2E%2E");
        assert_eq!(sanitize_branch_name("foo/../bar"), "foo%2F%2E%2E%2Fbar");
    }

    #[tokio::test]
    async fn add_branch_tracking_rolls_back_when_sync_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        git(project_root, &["init"]);
        git(project_root, &["config", "user.email", "test@example.com"]);
        git(project_root, &["config", "user.name", "Test User"]);
        std::fs::write(project_root.join("lib.rs"), "pub fn main_branch() {}\n").unwrap();
        git(project_root, &["add", "."]);
        git(project_root, &["commit", "-m", "initial"]);
        git(project_root, &["branch", "-M", "main"]);

        let _cg = crate::tokensave::TokenSave::init(project_root)
            .await
            .unwrap();
        git(project_root, &["checkout", "-b", "feature/failsync"]);
        std::fs::write(
            project_root.join("lib.rs"),
            "pub fn main_branch() {}\npub fn feature_branch() {}\n",
        )
        .unwrap();

        let _lock = crate::tokensave::try_acquire_sync_lock(project_root).unwrap();
        let result = add_branch_tracking(project_root, "feature/failsync").await;

        assert!(
            result.is_err(),
            "held sync lock should make branch sync fail"
        );
        let tokensave_dir = crate::config::get_tokensave_dir(project_root);
        let meta = crate::branch_meta::load_branch_meta(&tokensave_dir).unwrap();
        assert!(
            !meta.is_tracked("feature/failsync"),
            "failed branch add must not leave stale tracked metadata"
        );
        assert!(
            !tokensave_dir
                .join("branches")
                .join(format!("{}.db", sanitize_branch_name("feature/failsync")))
                .exists(),
            "failed branch add should remove the copied branch DB"
        );
    }

    #[tokio::test]
    async fn add_branch_tracking_repairs_stale_missing_db_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        git(project_root, &["init"]);
        git(project_root, &["config", "user.email", "test@example.com"]);
        git(project_root, &["config", "user.name", "Test User"]);
        std::fs::write(project_root.join("lib.rs"), "pub fn main_branch() {}\n").unwrap();
        git(project_root, &["add", "."]);
        git(project_root, &["commit", "-m", "initial"]);
        git(project_root, &["branch", "-M", "main"]);

        let _cg = crate::tokensave::TokenSave::init(project_root)
            .await
            .unwrap();
        git(project_root, &["checkout", "-b", "feature/stale"]);
        std::fs::write(
            project_root.join("lib.rs"),
            "pub fn main_branch() {}\npub fn feature_branch() {}\n",
        )
        .unwrap();

        let tokensave_dir = crate::config::get_tokensave_dir(project_root);
        let mut meta = crate::branch_meta::load_branch_meta(&tokensave_dir).unwrap();
        let db_file = format!("branches/{}.db", sanitize_branch_name("feature/stale"));
        meta.add_branch("feature/stale", &db_file, "main");
        crate::branch_meta::save_branch_meta(&tokensave_dir, &meta).unwrap();
        assert!(
            !tokensave_dir.join(&db_file).exists(),
            "test setup should leave metadata pointing at a missing DB"
        );

        let outcome = add_branch_tracking(project_root, "feature/stale")
            .await
            .unwrap();

        assert_eq!(outcome, BranchAddOutcome::Added);
        let repaired = crate::branch_meta::load_branch_meta(&tokensave_dir).unwrap();
        assert!(repaired.is_tracked("feature/stale"));
        assert!(
            tokensave_dir.join(&db_file).exists(),
            "stale metadata should be self-repaired by recreating the branch DB"
        );
    }
}
