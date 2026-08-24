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

#[test]
fn repository_discovery_does_not_wait_for_the_blocking_pool() {
    let fixture = TempDir::new().expect("fixture");
    let repository = fixture.path().join("repository");
    std::fs::create_dir_all(&repository).expect("repository directory");
    run_git(&repository, &["init", "--quiet"]);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("announce blocking task");
            release_rx.recv().expect("release blocking task");
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocking task started");

        let discovery = tokio::time::timeout(
            Duration::from_secs(1),
            discover_repository_identity(
                &repository,
                MonotonicDeadline::at(Instant::now() + Duration::from_secs(2)),
                &CancellationToken::new(),
            ),
        )
        .await;

        release_tx.send(()).expect("release blocking task");
        blocker.await.expect("blocking task joined");
        let outcome = discovery.expect("repository discovery must not use the blocking pool");
        assert!(
            matches!(outcome, GitRepositoryIdentityOutcome::Resolved(_)),
            "repository identity should resolve, got {outcome:?}"
        );
    });
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
    assert_eq!(
        identity.git_dir,
        repository
            .join(".git")
            .canonicalize()
            .expect("canonical git dir")
    );
    assert!(matches!(
        non_repository,
        GitRepositoryIdentityOutcome::NotRepository
    ));
}

#[tokio::test]
async fn linked_worktree_identity_preserves_worktree_git_dir_and_common_dir() {
    let fixture = TempDir::new().expect("fixture");
    let repository = fixture.path().join("repository");
    let linked = fixture.path().join("linked");
    std::fs::create_dir_all(&repository).expect("repository directory");
    run_git(&repository, &["init", "--quiet"]);
    run_git(
        &repository,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=test@tracedecay.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    run_git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().expect("UTF-8 fixture"),
        ],
    );

    let outcome = discover_repository_identity(
        &linked,
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(2)),
        &CancellationToken::new(),
    )
    .await;
    let GitRepositoryIdentityOutcome::Resolved(identity) = outcome else {
        panic!("linked worktree should resolve, got {outcome:?}");
    };
    assert_eq!(
        identity.worktree_root,
        linked.canonicalize().expect("canonical linked worktree")
    );
    assert_eq!(
        identity.common_dir,
        repository.join(".git").canonicalize().expect("common dir")
    );
    assert_ne!(identity.git_dir, identity.common_dir);
    assert!(identity.git_dir.ends_with("worktrees/linked"));
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
