//! The `git` subprocess calls: diffs, PR comparisons, commit logs, and the file-role classification applied to their output.

use super::*;

const PR_CONTEXT_MAX_ANCESTRY_COMMITS: usize = 100_000;
const PR_CONTEXT_MAX_CHANGED_FILES: usize = 20_000;

/// Diff two git refs and return changed file paths with coarse status.
pub(super) fn git_diff_file_changes(
    project_root: &std::path::Path,
    from_ref: &str,
    to_ref: &str,
) -> std::result::Result<Vec<GitFileChange>, String> {
    git_diff_file_changes_controlled(project_root, from_ref, to_ref, &|| false)
}

/// Resolve PR refs to their common ancestor and compare only changes reachable
/// from the head. This matches `git diff base...head`; comparing the two tip
/// trees directly would incorrectly report unrelated files added to an
/// advanced default branch as deletions in the PR.
#[cfg(test)]
pub(super) fn git_pr_comparison(
    project_root: &std::path::Path,
    base_ref: &str,
    head_ref: &str,
) -> std::result::Result<GitPrComparison, String> {
    git_pr_comparison_controlled(project_root, base_ref, head_ref, &|| false)
}

pub(super) fn git_pr_comparison_controlled(
    project_root: &std::path::Path,
    base_ref: &str,
    head_ref: &str,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<GitPrComparison, String> {
    check_git_pr_cancelled(cancelled)?;
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;
    check_git_pr_cancelled(cancelled)?;
    let base_commit = repo
        .rev_parse_single(base_ref)
        .map_err(|e| format!("cannot resolve '{base_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{base_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{base_ref}' to commit: {e}"))?;
    check_git_pr_cancelled(cancelled)?;
    let head_commit = repo
        .rev_parse_single(head_ref)
        .map_err(|e| format!("cannot resolve '{head_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{head_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{head_ref}' to commit: {e}"))?;
    let base_oid = base_commit.id.to_string();
    let head_oid = head_commit.id.to_string();
    check_git_pr_cancelled(cancelled)?;
    ensure_pr_ancestry_bounded(&repo, base_commit.id, head_commit.id, cancelled)?;
    let merge_base = repo
        .merge_base(base_commit.id, head_commit.id)
        .map_err(|e| format!("cannot find merge base for '{base_ref}' and '{head_ref}': {e}"))?;
    let merge_base = merge_base.to_string();
    check_git_pr_cancelled(cancelled)?;

    Ok(GitPrComparison {
        changes: git_diff_file_changes_controlled(project_root, &merge_base, &head_oid, cancelled)?,
        commits: git_commit_log_controlled(project_root, &merge_base, &head_oid, cancelled)?,
        base_oid,
        head_oid,
        merge_base,
    })
}

fn ensure_pr_ancestry_bounded(
    repo: &gix::Repository,
    base: gix::ObjectId,
    head: gix::ObjectId,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<(), String> {
    for (label, tip) in [("base", base), ("head", head)] {
        let walk = repo
            .rev_walk([tip])
            .all()
            .map_err(|error| format!("cannot walk {label} ancestry: {error}"))?;
        for (index, info) in walk.enumerate() {
            check_git_pr_cancelled(cancelled)?;
            info.map_err(|error| format!("cannot walk {label} ancestry: {error}"))?;
            if index >= PR_CONTEXT_MAX_ANCESTRY_COMMITS {
                return Err(format!(
                    "git PR comparison {label} ancestry exceeds the {PR_CONTEXT_MAX_ANCESTRY_COMMITS}-commit limit"
                ));
            }
        }
    }
    Ok(())
}

fn check_git_pr_cancelled(
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<(), String> {
    if cancelled() {
        Err("git PR comparison cancelled".to_owned())
    } else {
        Ok(())
    }
}

fn git_diff_file_changes_controlled(
    project_root: &std::path::Path,
    from_ref: &str,
    to_ref: &str,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<Vec<GitFileChange>, String> {
    check_git_pr_cancelled(cancelled)?;
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;
    let from_tree = repo
        .rev_parse_single(from_ref)
        .map_err(|e| format!("cannot resolve '{from_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{from_ref}': {e}"))?
        .peel_to_tree()
        .map_err(|e| format!("cannot peel '{from_ref}' to tree: {e}"))?;
    check_git_pr_cancelled(cancelled)?;
    let to_tree = repo
        .rev_parse_single(to_ref)
        .map_err(|e| format!("cannot resolve '{to_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{to_ref}': {e}"))?
        .peel_to_tree()
        .map_err(|e| format!("cannot peel '{to_ref}' to tree: {e}"))?;

    let mut changed = Vec::new();
    let mut reached_limit = false;
    from_tree
        .changes()
        .map_err(|e| format!("diff init failed: {e}"))?
        .for_each_to_obtain_tree(&to_tree, |change| {
            use gix::object::tree::diff::Change;
            if cancelled() {
                return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Break(()));
            }
            if changed.len() >= PR_CONTEXT_MAX_CHANGED_FILES {
                reached_limit = true;
                return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Break(()));
            }
            match &change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "added",
                        });
                    }
                }
                Change::Modification {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "modified",
                        });
                    }
                }
                Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "deleted",
                        });
                    }
                }
                Change::Rewrite {
                    source_location,
                    location,
                    source_entry_mode,
                    entry_mode,
                    ..
                } => {
                    if !source_entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: source_location.to_string(),
                            status: "deleted",
                        });
                    }
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "added",
                        });
                    }
                }
            }
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|e| format!("tree diff failed: {e}"))?;
    check_git_pr_cancelled(cancelled)?;
    if reached_limit {
        return Err(format!(
            "git PR comparison exceeds the {PR_CONTEXT_MAX_CHANGED_FILES}-file diff limit"
        ));
    }
    Ok(changed)
}

pub(super) fn default_pr_base_ref(project_root: &std::path::Path) -> String {
    crate::branch::detect_default_branch(project_root).unwrap_or_else(|| "main".to_string())
}

/// Returns file paths changed in the working tree (unstaged + staged, or staged-only).
pub(super) fn git_changed_files(
    project_root: &std::path::Path,
    staged_only: bool,
) -> std::result::Result<Vec<String>, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let head_tree = repo
        .head()
        .map_err(|e| format!("cannot read HEAD: {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel HEAD to commit: {e}"))?
        .tree()
        .map_err(|e| format!("cannot read HEAD tree: {e}"))?;

    // Compare HEAD tree against the index (staged changes)
    let index = repo
        .index()
        .map_err(|e| format!("cannot read index: {e}"))?;

    let mut changed = HashSet::new();

    // Walk the index to find files that differ from HEAD
    for entry in index.entries() {
        let path = entry.path(&index);
        let path_str = String::from_utf8_lossy(path.as_ref()).to_string();
        if path_str.is_empty() {
            continue;
        }

        let head_entry = head_tree
            .lookup_entry_by_path(std::path::Path::new(&path_str))
            .ok()
            .flatten();

        match head_entry {
            Some(he) => {
                // File exists in both - check if content differs
                if he.object_id() != entry.id {
                    changed.insert(path_str);
                }
            }
            None => {
                // New file (in index but not in HEAD)
                changed.insert(path_str);
            }
        }
    }

    // If not staged_only, also check working-tree modifications via mtime
    if !staged_only {
        for entry in index.entries() {
            let path = entry.path(&index);
            let path_str = String::from_utf8_lossy(path.as_ref()).to_string();
            if path_str.is_empty() {
                continue;
            }
            let full_path = project_root.join(&path_str);
            if let Ok(meta) = std::fs::metadata(&full_path) {
                use std::time::UNIX_EPOCH;
                let mtime = meta
                    .modified()
                    .unwrap_or(UNIX_EPOCH)
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u32;
                // gix index entry stores mtime; if disk mtime is newer, file is modified
                if mtime > entry.stat.mtime.secs {
                    changed.insert(path_str);
                }
            }
        }
    }

    let mut result: Vec<String> = changed.into_iter().collect();
    result.sort();
    Ok(result)
}

/// Returns the last N commit subjects from HEAD.
pub(super) fn git_recent_commits(
    project_root: &std::path::Path,
    count: usize,
) -> std::result::Result<Vec<String>, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let mut commits = Vec::new();
    let head = repo
        .head()
        .map_err(|e| format!("cannot read HEAD: {e}"))?
        .into_peeled_id()
        .map_err(|e| format!("cannot peel HEAD: {e}"))?;

    let mut current_id = head.detach();

    for _ in 0..count {
        let commit = repo
            .find_object(current_id)
            .map_err(|e| format!("cannot find object: {e}"))?
            .try_into_commit()
            .map_err(|e| format!("not a commit: {e}"))?;

        let message = commit
            .message_raw()
            .map_err(|e| format!("cannot read commit message: {e}"))?;
        let subject = String::from_utf8_lossy(message.as_ref())
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        commits.push(subject);

        let parent_id = commit.parent_ids().next().map(gix::Id::detach);
        match parent_id {
            Some(pid) => current_id = pid,
            None => break,
        }
    }

    Ok(commits)
}

/// Returns commit subjects between two refs.
fn git_commit_log_controlled(
    project_root: &std::path::Path,
    base_ref: &str,
    head_ref: &str,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> std::result::Result<Vec<Value>, String> {
    check_git_pr_cancelled(cancelled)?;
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let base_id = repo
        .rev_parse_single(base_ref)
        .map_err(|e| format!("cannot resolve '{base_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{base_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{base_ref}' to commit: {e}"))?
        .id;

    let head_id = repo
        .rev_parse_single(head_ref)
        .map_err(|e| format!("cannot resolve '{head_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{head_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{head_ref}' to commit: {e}"))?
        .id;

    let mut commits = Vec::new();
    let walk = repo
        .rev_walk([head_id])
        .with_hidden([base_id])
        .all()
        .map_err(|e| format!("cannot walk commits from '{base_ref}' to '{head_ref}': {e}"))?;

    // Include commits reachable from head but not base, including merge-shaped
    // histories where the merge base is not on the first-parent chain.
    for info in walk.take(100) {
        check_git_pr_cancelled(cancelled)?;
        let info = info.map_err(|e| format!("cannot walk commit: {e}"))?;
        let commit = repo
            .find_object(info.id)
            .map_err(|e| format!("cannot find object: {e}"))?
            .try_into_commit()
            .map_err(|e| format!("not a commit: {e}"))?;

        let message = commit
            .message_raw()
            .map_err(|e| format!("cannot read message: {e}"))?;
        let subject = String::from_utf8_lossy(message.as_ref())
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let short_id = format!("{:.7}", commit.id);
        commits.push(json!({"hash": short_id, "subject": subject}));
    }

    check_git_pr_cancelled(cancelled)?;
    Ok(commits)
}

/// Classify a file path into a semantic role.
///
/// Inline tests inside source files don't make the file's role "test" —
/// that bucket is reserved for files that exist purely to host tests
/// (the path-based check). A `src/foo.rs` with a `#[cfg(test)] mod tests`
/// at the bottom still has role "source".
#[allow(clippy::ptr_arg)]
pub(super) fn classify_file_role(
    path: &str,
    _files_with_inline_tests: &HashSet<String>,
) -> &'static str {
    if crate::tracedecay::is_test_file(path) {
        return "test";
    }
    let lower = path.to_lowercase();
    let ext = std::path::Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str());
    // Config files
    if matches!(
        ext,
        Some("toml" | "yaml" | "yml" | "json" | "lock" | "ini" | "cfg")
    ) || lower.contains("config")
    {
        return "config";
    }
    // Documentation
    if matches!(ext, Some("md" | "rst" | "txt"))
        || lower.starts_with("docs/")
        || lower.starts_with("doc/")
    {
        return "docs";
    }
    "source"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn pr_comparison_anchors_at_merge_base_when_base_advanced() {
        let temp = tempfile::tempdir().expect("temp repo");
        let root = temp.path();
        test_git(root, &["init", "-b", "main"]);
        std::fs::write(root.join("common.txt"), "common\n").expect("write common");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "common"]);
        test_git(root, &["switch", "-c", "feature"]);
        std::fs::write(root.join("feature.txt"), "feature\n").expect("write feature");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "feature"]);
        test_git(root, &["switch", "main"]);
        std::fs::write(root.join("main-only.txt"), "main\n").expect("write main");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "main advanced"]);

        let comparison = git_pr_comparison(root, "main", "feature").expect("PR comparison");
        let paths: Vec<_> = comparison
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect();

        assert_eq!(paths, ["feature.txt"]);
        assert_eq!(comparison.commits.len(), 1);
        assert_eq!(comparison.commits[0]["subject"], "feature");
    }

    #[test]
    fn pr_comparison_stops_from_inside_the_tree_diff_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let temp = tempfile::tempdir().expect("temp repo");
        let root = temp.path();
        test_git(root, &["init", "-b", "main"]);
        std::fs::write(root.join("base.txt"), "base\n").expect("write base");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "base"]);
        test_git(root, &["switch", "-c", "feature"]);
        std::fs::write(root.join("feature.txt"), "feature\n").expect("write feature");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "feature"]);

        let checkpoints = AtomicUsize::new(0);
        let result = git_pr_comparison_controlled(root, "main", "feature", &|| {
            checkpoints.fetch_add(1, Ordering::Relaxed) >= 7
        });
        let Err(error) = result else {
            panic!("tree diff cancellation must stop the comparison");
        };
        assert_eq!(error, "git PR comparison cancelled");
        assert!(
            checkpoints.load(Ordering::Relaxed) >= 8,
            "cancellation must be observed after entering the diff callback",
        );
    }

    #[test]
    fn pr_context_default_base_detects_master() {
        let temp = tempfile::tempdir().expect("temp repo");
        test_git(temp.path(), &["init", "-b", "master"]);
        std::fs::write(temp.path().join("README.md"), "test\n").expect("write fixture");
        test_git(temp.path(), &["add", "."]);
        test_git(temp.path(), &["commit", "-m", "initial"]);
        assert_eq!(default_pr_base_ref(temp.path()), "master");
    }

    #[test]
    fn config_files_classified_as_config_not_source() {
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(classify_file_role("Cargo.toml", &empty), "config");
        assert_eq!(classify_file_role("package.json", &empty), "config");
        assert_eq!(classify_file_role("foo.yaml", &empty), "config");
        assert_eq!(classify_file_role("config.ini", &empty), "config");
    }

    /// Regression for bug #3 follow-up: a source file with `#[cfg(test)] mod
    /// tests` at the bottom is still a source file — its role must not flip
    /// to "test" just because it contains inline tests. Only the path-based
    /// `is_test_file` check governs role classification.
    #[test]
    fn source_file_with_inline_tests_keeps_source_role() {
        let mut with_inline: HashSet<String> = HashSet::new();
        with_inline.insert("src/lib.rs".to_string());
        assert_eq!(classify_file_role("src/lib.rs", &with_inline), "source");
    }

    #[test]
    fn path_based_test_files_classify_as_test() {
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(classify_file_role("tests/integration.rs", &empty), "test");
        assert_eq!(classify_file_role("src/foo_test.rs", &empty), "test");
    }
}
