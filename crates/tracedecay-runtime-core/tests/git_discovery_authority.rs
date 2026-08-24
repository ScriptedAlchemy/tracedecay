#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity,
};

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git must be available for discovery integration tests");
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

#[tokio::test]
async fn repository_identity_does_not_depend_on_the_cli_helper() {
    let fixture = TempDir::new().expect("fixture");
    let repository = fixture.path().join("repository");
    std::fs::create_dir_all(&repository).expect("repository directory");
    run_git(&repository, &["init", "--quiet"]);

    let stalled_git = fixture.path().join("stalled-git");
    std::fs::write(&stalled_git, "#!/bin/sh\nsleep 5\nexit 1\n").expect("stalled git helper");
    let mut permissions = std::fs::metadata(&stalled_git)
        .expect("stalled git metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&stalled_git, permissions).expect("executable stalled git helper");

    // This integration test is the only test in its process, so the
    // process-wide Git-program cache cannot race another environment writer.
    unsafe { std::env::set_var("GIT", &stalled_git) };

    let outcome = discover_repository_identity(
        &repository,
        MonotonicDeadline::at(Instant::now() + Duration::from_millis(500)),
        &CancellationToken::new(),
    )
    .await;

    let GitRepositoryIdentityOutcome::Resolved(identity) = outcome else {
        panic!("repository identity should not require the CLI helper, got {outcome:?}");
    };
    assert_eq!(
        identity.worktree_root,
        repository.canonicalize().expect("canonical repository")
    );
}
