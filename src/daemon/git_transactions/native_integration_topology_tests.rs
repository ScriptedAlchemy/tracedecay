use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_domain::GitOidV1;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentity, GitRepositoryIdentityOutcome, discover_repository_identity_bounded,
};

use super::native_integration_topology::{
    NativeIntegrationTopologyFailure, NativeIntegrationTopologyLimits,
    capture_native_integration_topology,
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git fixture output")
        .trim()
        .to_owned()
}

fn repository() -> TempDir {
    let repository = tempfile::tempdir().expect("repository");
    git(repository.path(), &["init", "--quiet"]);
    git(
        repository.path(),
        &["config", "user.name", "TraceDecay Test"],
    );
    git(
        repository.path(),
        &["config", "user.email", "tracedecay@example.com"],
    );
    std::fs::write(repository.path().join("packet.txt"), "base\n").expect("base file");
    git(repository.path(), &["add", "packet.txt"]);
    git(repository.path(), &["commit", "--quiet", "-m", "base"]);
    repository
}

fn repository_identity(root: &Path) -> GitRepositoryIdentity {
    let GitRepositoryIdentityOutcome::Resolved(identity) =
        discover_repository_identity_bounded(root)
    else {
        panic!("fixture repository identity must resolve")
    };
    identity
}

fn commit(root: &Path, message: &str, body: &str) -> GitOidV1 {
    std::fs::write(root.join("packet.txt"), body).expect("fixture file");
    git(root, &["add", "packet.txt"]);
    git(root, &["commit", "--quiet", "-m", message]);
    GitOidV1::new(git(root, &["rev-parse", "HEAD"])).expect("commit id")
}

fn commit_tree(root: &Path, tree: &str, parents: &[&GitOidV1], timestamp: i64) -> GitOidV1 {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .arg("commit-tree")
        .arg(tree)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "tracedecay@example.com")
        .env("GIT_AUTHOR_DATE", format!("@{timestamp} +0000"))
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "tracedecay@example.com")
        .env("GIT_COMMITTER_DATE", format!("@{timestamp} +0000"));
    for parent in parents {
        command.arg("-p").arg(parent.as_str());
    }
    let output = command.output().expect("commit-tree fixture");
    assert!(
        output.status.success(),
        "commit-tree: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    GitOidV1::new(
        String::from_utf8(output.stdout)
            .expect("commit-tree output")
            .trim(),
    )
    .expect("commit-tree id")
}

fn control() -> (
    NativeIntegrationTopologyLimits,
    CancellationToken,
    MonotonicDeadline,
) {
    (
        NativeIntegrationTopologyLimits {
            max_commits: 64,
            max_parent_edges: 128,
            max_decoded_commit_bytes: 1024 * 1024,
        },
        CancellationToken::new(),
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(10)),
    )
}

#[test]
fn source_minus_destination_closure_is_deterministic_and_parent_first() {
    let repository = repository();
    let base = GitOidV1::new(git(repository.path(), &["rev-parse", "HEAD"])).expect("base");
    git(repository.path(), &["branch", "destination", base.as_str()]);
    git(repository.path(), &["switch", "-c", "source"]);
    let first = commit(repository.path(), "first", "first\n");
    git(repository.path(), &["switch", "-c", "side"]);
    std::fs::write(repository.path().join("side.txt"), "side\n").expect("side file");
    git(repository.path(), &["add", "side.txt"]);
    git(repository.path(), &["commit", "--quiet", "-m", "side"]);
    let side =
        GitOidV1::new(git(repository.path(), &["rev-parse", "HEAD"])).expect("side commit id");
    git(repository.path(), &["switch", "source"]);
    let second = commit(repository.path(), "second", "second\n");
    git(
        repository.path(),
        &["merge", "--quiet", "--no-ff", "-m", "merge side", "side"],
    );
    let source = GitOidV1::new(git(repository.path(), &["rev-parse", "HEAD"])).expect("source");
    let identity = repository_identity(repository.path());

    let (limits, cancellation, deadline) = control();
    let first_capture = capture_native_integration_topology(
        &identity,
        &source,
        &base,
        limits,
        &cancellation,
        deadline,
    )
    .expect("complete topology");
    let second_capture = capture_native_integration_topology(
        &identity,
        &source,
        &base,
        limits,
        &cancellation,
        deadline,
    )
    .expect("repeat topology");

    assert_eq!(first_capture, second_capture);
    assert_eq!(first_capture.merge_base, base);
    assert_eq!(
        first_capture
            .ordered_source_only
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first, side, second, source])
    );

    let positions = first_capture
        .ordered_source_only
        .iter()
        .enumerate()
        .map(|(index, commit)| (commit.clone(), index))
        .collect::<BTreeMap<_, _>>();
    for commit in &first_capture.ordered_source_only {
        let parents = git(
            repository.path(),
            &["show", "-s", "--format=%P", commit.as_str()],
        );
        for parent in parents.split_whitespace() {
            let parent = GitOidV1::new(parent).expect("parent id");
            if let Some(parent_position) = positions.get(&parent) {
                assert!(
                    parent_position < positions.get(commit).expect("commit position"),
                    "parent {parent:?} must precede child {commit:?}"
                );
            }
        }
    }
}

#[test]
fn multiple_best_merge_bases_are_rejected_as_ambiguous() {
    let repository = repository();
    let base = GitOidV1::new(git(repository.path(), &["rev-parse", "HEAD"])).expect("base");
    let tree = git(repository.path(), &["rev-parse", "HEAD^{tree}"]);
    let left = commit_tree(repository.path(), &tree, &[&base], 2);
    let right = commit_tree(repository.path(), &tree, &[&base], 3);
    let left_tip = commit_tree(repository.path(), &tree, &[&left, &right], 4);
    let right_tip = commit_tree(repository.path(), &tree, &[&right, &left], 5);
    let identity = repository_identity(repository.path());
    let (limits, cancellation, deadline) = control();

    assert_eq!(
        capture_native_integration_topology(
            &identity,
            &left_tip,
            &right_tip,
            limits,
            &cancellation,
            deadline,
        ),
        Err(NativeIntegrationTopologyFailure::AmbiguousMergeBase)
    );
}

#[test]
fn topology_capture_honors_cancellation_and_object_bounds() {
    let repository = repository();
    let destination =
        GitOidV1::new(git(repository.path(), &["rev-parse", "HEAD"])).expect("destination");
    let source = commit(repository.path(), "source", "source\n");
    let identity = repository_identity(repository.path());
    let (limits, cancellation, deadline) = control();
    cancellation.cancel();
    assert_eq!(
        capture_native_integration_topology(
            &identity,
            &source,
            &destination,
            limits,
            &cancellation,
            deadline,
        ),
        Err(NativeIntegrationTopologyFailure::Cancelled)
    );

    let (_, cancellation, deadline) = control();
    assert_eq!(
        capture_native_integration_topology(
            &identity,
            &source,
            &destination,
            NativeIntegrationTopologyLimits {
                max_commits: 0,
                max_parent_edges: 128,
                max_decoded_commit_bytes: 1024 * 1024,
            },
            &cancellation,
            deadline,
        ),
        Err(NativeIntegrationTopologyFailure::CommitLimit)
    );
}

#[test]
fn topology_capture_rejects_identity_drift_and_rewritten_history() {
    let repository = repository();
    let destination =
        GitOidV1::new(git(repository.path(), &["rev-parse", "HEAD"])).expect("destination");
    let source = commit(repository.path(), "source", "source\n");
    let identity = repository_identity(repository.path());
    let (limits, cancellation, deadline) = control();

    let mut drifted_identity = identity.clone();
    drifted_identity.git_dir = identity.common_dir.join("different-worktree");
    assert_eq!(
        capture_native_integration_topology(
            &drifted_identity,
            &source,
            &destination,
            limits,
            &cancellation,
            deadline,
        ),
        Err(NativeIntegrationTopologyFailure::RepositoryIdentityChanged)
    );

    git(
        repository.path(),
        &["config", "remote.origin.promisor", "true"],
    );
    assert_eq!(
        capture_native_integration_topology(
            &identity,
            &source,
            &destination,
            limits,
            &cancellation,
            deadline,
        ),
        Err(NativeIntegrationTopologyFailure::UnsupportedHistory)
    );
}
