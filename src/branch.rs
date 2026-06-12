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
    /// The project has no `.tracedecay/` index; nothing was done.
    NotIndexed,
    /// The branch was already tracked; no copy/sync was performed.
    AlreadyTracked,
    /// A new branch DB was created from the nearest ancestor and synced.
    Added,
}

/// Silently bootstraps/maintains tracedecay branch tracking for `branch_name`.
///
/// This is the library-level core shared with the `tracedecay branch add` CLI
/// command, callable from hooks without shelling out to a second process. It:
/// loads or bootstraps [`BranchMeta`] (via [`detect_default_branch`]), no-ops
/// when the branch is already tracked, otherwise copies the nearest tracked
/// ancestor's DB and runs an incremental sync against the new branch DB.
///
/// No-ops (returns [`BranchAddOutcome::NotIndexed`]) when the project has no
/// `.tracedecay/` index, so it never bootstraps indexing in an unindexed repo.
/// Idempotent: a re-add of a tracked branch returns
/// [`BranchAddOutcome::AlreadyTracked`] without re-copying.
pub async fn add_branch_tracking(
    project_root: &Path,
    branch_name: &str,
) -> crate::errors::Result<BranchAddOutcome> {
    use crate::branch_meta;
    use crate::config::get_tracedecay_dir;

    if !crate::tracedecay::TraceDecay::is_initialized(project_root) {
        return Ok(BranchAddOutcome::NotIndexed);
    }
    let tracedecay_dir = get_tracedecay_dir(project_root);

    let mut meta = branch_meta::load_branch_meta(&tracedecay_dir).unwrap_or_else(|| {
        let default = detect_default_branch(project_root).unwrap_or_else(|| "main".to_string());
        branch_meta::BranchMeta::new_for_dir(&tracedecay_dir, &default)
    });

    if meta.is_tracked(branch_name) {
        return Ok(BranchAddOutcome::AlreadyTracked);
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
    let parent_db = resolve_branch_db_path(&tracedecay_dir, &parent, &meta).ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: format!("parent branch '{parent}' has no DB"),
        }
    })?;
    if !parent_db.exists() {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!("parent DB not found at '{}'", parent_db.display()),
        });
    }

    let branches_dir = branch_meta::ensure_branches_dir(&tracedecay_dir)?;
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
    std::fs::copy(&parent_db, &new_db_path)?;

    // Save metadata BEFORE open() so it resolves the new branch to its DB.
    let db_file = format!("branches/{stem}.db");
    meta.add_branch(branch_name, &db_file, &parent);
    branch_meta::save_branch_meta(&tracedecay_dir, &meta)?;

    let cg = crate::tracedecay::TraceDecay::open(project_root).await?;
    let _ = cg.sync().await?;

    if let Some(mut meta) = branch_meta::load_branch_meta(&tracedecay_dir) {
        meta.touch_synced(branch_name);
        let _ = branch_meta::save_branch_meta(&tracedecay_dir, &meta);
    }

    Ok(BranchAddOutcome::Added)
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
