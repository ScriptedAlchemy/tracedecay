//! Plan 36 portability: snapshot capture must canonicalize repository roots so
//! symlink aliases (macOS `/tmp` → `/private/tmp` and Linux fixtures) agree
//! with the daemon owner's mounted root. Exact CAS is preserved.

#![cfg(all(unix, feature = "test-transport"))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tracedecay::daemon::capture_exact_git_snapshot_for_test;
use tracedecay_domain::{ProjectId, RepositoryId, UtcMicros, WorktreeId};

fn git(repository: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repository)
        .args(args)
        .status()
        .expect("Git command starts");
    assert!(status.success(), "git {args:?}");
}

fn repository_fixture() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "--quiet"]);
    git(
        directory.path(),
        &["config", "user.name", "TraceDecay Test"],
    );
    git(
        directory.path(),
        &["config", "user.email", "tracedecay@example.com"],
    );
    fs::write(directory.path().join("packet.txt"), "before\n").expect("write base file");
    git(directory.path(), &["add", "packet.txt"]);
    git(directory.path(), &["commit", "--quiet", "-m", "base"]);
    directory
}

#[test]
fn capture_agrees_across_symlink_repository_root_aliases() {
    let directory = repository_fixture();
    let alias_parent = tempfile::tempdir().expect("alias parent");
    let alias = alias_parent.path().join("repo-alias");
    symlink(directory.path(), &alias).expect("repository root symlink alias");

    let project_id = ProjectId::new("project.fixture").expect("project id");
    let repository_id = RepositoryId::new("repository.fixture").expect("repository id");
    let worktree_id = WorktreeId::new("worktree.fixture").expect("worktree id");
    let captured_at = UtcMicros(1);

    let via_real = capture_exact_git_snapshot_for_test(
        directory.path(),
        project_id.clone(),
        repository_id.clone(),
        worktree_id.clone(),
        captured_at,
    )
    .expect("real-path snapshot");
    let via_alias = capture_exact_git_snapshot_for_test(
        &alias,
        project_id,
        repository_id,
        worktree_id,
        captured_at,
    )
    .expect("alias-path snapshot");

    assert_ne!(
        alias.as_os_str(),
        directory
            .path()
            .canonicalize()
            .expect("canonical root")
            .as_os_str(),
        "fixture must exercise a non-canonical alias path"
    );
    assert_eq!(
        via_real, via_alias,
        "alias and real roots must produce identical snapshot identity at capture"
    );
    assert_eq!(
        via_real.repository_id, via_alias.repository_id,
        "repository id must stay exact across alias capture"
    );
    assert_eq!(
        via_real.worktree_id, via_alias.worktree_id,
        "worktree id must stay exact across alias capture"
    );

    fs::write(directory.path().join("packet.txt"), "drifted\n").expect("content drift");
    let drifted = capture_exact_git_snapshot_for_test(
        &alias,
        ProjectId::new("project.fixture").expect("project id"),
        RepositoryId::new("repository.fixture").expect("repository id"),
        WorktreeId::new("worktree.fixture").expect("worktree id"),
        captured_at,
    )
    .expect("drifted snapshot");
    assert_ne!(
        drifted, via_alias,
        "exact CAS identity must still diverge on genuine content drift"
    );
}
