//! Git branch provenance helpers.
//!
//! A branch, ref, or worktree identifies an exact graph snapshot inside the
//! project store. It never selects or owns a separate database.

use gix::bstr::ByteSlice as _;

pub use tracedecay_runtime_core::branch::{BranchMemo, current_branch};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSnapshot {
    pub name: String,
    pub commit: String,
}

/// Returns true when `branch` exists as a local `refs/heads/*` branch.
pub fn local_branch_exists(project_root: &std::path::Path, branch: &str) -> bool {
    if branch.is_empty() {
        return false;
    }
    let refname = format!("refs/heads/{branch}");
    if let Ok(repo) = gix::open(project_root) {
        return repo.find_reference(&refname).is_ok();
    }
    false
}

/// Resolves one exact local `refs/heads/*` branch tip to its peeled commit.
///
/// Remote refs, arbitrary revisions, and abbreviated object ids are not
/// accepted: branch tools bind only to an explicit local ref snapshot.
pub fn local_branch_commit(
    project_root: &std::path::Path,
    branch: &str,
) -> Result<tracedecay_domain::GitOidV1, String> {
    if branch.is_empty() {
        return Err("branch name cannot be empty".to_owned());
    }
    let repo = gix::open(project_root)
        .map_err(|error| format!("failed to open Git repository: {error}"))?;
    let refname = format!("refs/heads/{branch}");
    let mut reference = repo
        .find_reference(&refname)
        .map_err(|error| format!("local branch '{branch}' is unavailable: {error}"))?;
    let commit = reference
        .peel_to_id_in_place()
        .map_err(|error| format!("failed to resolve branch '{branch}': {error}"))?
        .to_string();
    tracedecay_domain::GitOidV1::new(commit)
        .map_err(|error| format!("branch '{branch}' resolved to an invalid commit: {error}"))
}

/// Lists exact local branch tips without spawning Git.
pub fn local_branch_snapshots(
    project_root: &std::path::Path,
) -> Result<Vec<BranchSnapshot>, String> {
    let repo = gix::open(project_root)
        .map_err(|error| format!("failed to open Git repository: {error}"))?;
    let references = repo
        .references()
        .map_err(|error| format!("failed to open Git references: {error}"))?;
    let branches = references
        .local_branches()
        .map_err(|error| format!("failed to enumerate local branches: {error}"))?;
    let mut snapshots = Vec::new();
    for reference in branches {
        let mut reference =
            reference.map_err(|error| format!("failed to read local branch: {error}"))?;
        let name = reference.name().shorten().to_str_lossy().into_owned();
        let commit = reference
            .peel_to_id_in_place()
            .map_err(|error| format!("failed to resolve branch '{name}': {error}"))?
            .to_string();
        snapshots.push(BranchSnapshot { name, commit });
    }
    snapshots.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(snapshots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_owned()
    }

    #[test]
    fn local_branch_commit_resolves_selected_ref_not_active_head() {
        let root = tempfile::tempdir().expect("tempdir");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["checkout", "-q", "-b", "main"]);
        std::fs::write(root.path().join("fixture.txt"), "base\n").expect("write base");
        git(root.path(), &["add", "fixture.txt"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        git(root.path(), &["branch", "selected"]);
        let selected = git(root.path(), &["rev-parse", "refs/heads/selected"]);
        std::fs::write(root.path().join("fixture.txt"), "active\n").expect("write active");
        git(root.path(), &["add", "fixture.txt"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "active",
            ],
        );
        let active = git(root.path(), &["rev-parse", "HEAD"]);

        let resolved = local_branch_commit(root.path(), "selected").expect("selected ref");
        assert_eq!(resolved.as_str(), selected);
        assert_ne!(resolved.as_str(), active);
        assert!(local_branch_commit(root.path(), "missing").is_err());
    }
}
