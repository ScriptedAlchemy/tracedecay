use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentityOutcome, discover_repository_identity,
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
async fn bounded_discovery_distinguishes_repository_and_non_repository() {
    let fixture = TempDir::new().expect("fixture");
    let repository = fixture.path().join("repository");
    let nested = repository.join("src/deep");
    let ordinary = fixture.path().join("ordinary");
    std::fs::create_dir_all(&nested).expect("nested repository directory");
    std::fs::create_dir_all(&ordinary).expect("ordinary directory");
    run_git(&repository, &["init", "--quiet"]);

    let cancellation = CancellationToken::new();
    let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_secs(2));
    let member = discover_repository_identity(&nested, deadline, &cancellation).await;
    let non_repository = discover_repository_identity(&ordinary, deadline, &cancellation).await;

    let GitRepositoryIdentityOutcome::Resolved(identity) = member else {
        panic!("repository member should resolve, got {member:?}");
    };
    assert_eq!(
        identity.worktree_root,
        repository.canonicalize().expect("canonical repository")
    );
    assert!(matches!(
        non_repository,
        GitRepositoryIdentityOutcome::NotRepository
    ));
}

#[tokio::test]
async fn cancellation_is_typed_before_discovery_work_starts() {
    let fixture = TempDir::new().expect("fixture");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let outcome = discover_repository_identity(
        fixture.path(),
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(2)),
        &cancellation,
    )
    .await;

    assert_eq!(
        outcome,
        GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled)
    );
}

#[tokio::test]
async fn elapsed_deadline_is_typed_before_discovery_work_starts() {
    let fixture = TempDir::new().expect("fixture");
    let outcome = discover_repository_identity(
        fixture.path(),
        MonotonicDeadline::at(Instant::now()),
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(
        outcome,
        GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
    );
}
