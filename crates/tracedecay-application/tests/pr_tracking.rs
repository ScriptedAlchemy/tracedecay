use tracedecay_application::pr_tracking::{
    ManagedPr, PrAutotrackState, load_state, managed_summary, save_state,
};
use tracedecay_domain::errors::TraceDecayError;

#[test]
fn missing_managed_pr_state_is_empty() {
    let store = tempfile::tempdir().expect("store root");

    assert!(
        load_state(store.path())
            .expect("load missing state")
            .managed
            .is_empty()
    );
    assert!(
        managed_summary(store.path())
            .expect("summarize missing state")
            .is_empty()
    );
}

#[test]
fn managed_pr_state_round_trips_through_application_owner() {
    let store = tempfile::tempdir().expect("store root");
    let mut state = PrAutotrackState::default();
    state.managed.insert(
        "tracedecay/autotrack/pr/7".to_owned(),
        ManagedPr {
            pr: 7,
            head_branch: "feature-7".to_owned(),
            head_sha: "sha-7".to_owned(),
            worktree: store.path().join("pr-worktrees/pr-7"),
            tracking_ref: "refs/tracedecay/pr/7".to_owned(),
        },
    );

    save_state(store.path(), &state).expect("persist managed PR state");

    assert_eq!(
        load_state(store.path())
            .expect("load managed PR state")
            .managed,
        state.managed
    );
    assert_eq!(
        managed_summary(store.path()).expect("summarize managed PR state")[0].pr,
        7
    );
}

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

#[test]
fn manual_branch_lifecycle_lease_is_exclusive_per_branch() {
    let store = tempfile::tempdir().expect("store root");
    let first = tracedecay_application::pr_tracking::try_acquire_manual_branch_lifecycle(
        store.path(),
        "feature/exclusive",
    )
    .expect("first lease");
    let contended = tracedecay_application::pr_tracking::try_acquire_manual_branch_lifecycle(
        store.path(),
        "feature/exclusive",
    );

    assert!(first.matches_branch("feature/exclusive"));
    assert!(matches!(
        contended,
        Err(
            tracedecay_application::pr_tracking::ManualBranchActivationError::LifecycleContended { .. }
        )
    ));
    drop(first);
    tracedecay_application::pr_tracking::try_acquire_manual_branch_lifecycle(
        store.path(),
        "feature/exclusive",
    )
    .expect("lease after drop");
}

#[test]
fn malformed_managed_pr_state_is_a_typed_json_error() {
    let store = tempfile::tempdir().expect("store root");
    std::fs::write(store.path().join("pr-autotrack.json"), "{not json")
        .expect("write malformed state");

    assert!(matches!(
        load_state(store.path()),
        Err(TraceDecayError::Json(_))
    ));
    assert!(matches!(
        managed_summary(store.path()),
        Err(TraceDecayError::Json(_))
    ));
}

#[test]
fn unreadable_managed_pr_state_is_a_typed_io_error() {
    let store = tempfile::tempdir().expect("store root");
    std::fs::create_dir(store.path().join("pr-autotrack.json"))
        .expect("create unreadable state path");

    assert!(matches!(
        load_state(store.path()),
        Err(TraceDecayError::Io(_))
    ));
    assert!(matches!(
        managed_summary(store.path()),
        Err(TraceDecayError::Io(_))
    ));
}
