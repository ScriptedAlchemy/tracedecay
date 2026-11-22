#[test]
fn manual_branch_artifacts_hash_the_branch_name_not_the_raw_path() {
    let store = tempfile::tempdir().expect("store root");
    let slashed = tracedecay_application::pr_tracking::ManualBranchArtifactsV1::for_branch(
        store.path(),
        "feature/a",
    );
    let underscored = tracedecay_application::pr_tracking::ManualBranchArtifactsV1::for_branch(
        store.path(),
        "feature_a",
    );

    assert_ne!(slashed.worktree, underscored.worktree);
    assert_ne!(slashed.tracking_ref, underscored.tracking_ref);
    assert!(
        slashed
            .worktree
            .starts_with(store.path().join("branch-worktrees"))
    );
}

/// The daemon owns each exact branch lifecycle: a second caller for the same
/// branch queues until the first releases it instead of being refused, while
/// another branch is admitted at once.
#[tokio::test]
async fn manual_branch_lifecycle_queues_same_branch_callers() {
    use tracedecay_application::pr_tracking::acquire_manual_branch_lifecycle;

    let store = tempfile::tempdir().expect("store root");
    let first = acquire_manual_branch_lifecycle(store.path(), "feature/exclusive")
        .await
        .expect("first lease");
    let other = acquire_manual_branch_lifecycle(store.path(), "feature/other")
        .await
        .expect("independent branch lease");

    let second = acquire_manual_branch_lifecycle(store.path(), "feature/exclusive");
    tokio::pin!(second);
    let queued = tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
        .await
        .is_err();
    drop(first);
    let second = second
        .await
        .map(|lease| lease.matches_branch("feature/exclusive"));

    assert!(queued, "a same-branch caller must wait for the owner");
    assert_eq!(second, Ok(true));
    assert!(other.matches_branch("feature/other"));
}
