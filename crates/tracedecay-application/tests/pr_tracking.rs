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
fn undecodable_managed_pr_entries_are_scoped_refusals_that_save_state_resets() {
    let store = tempfile::tempdir().expect("store root");
    let current = ManagedPr {
        pr: 7,
        head_branch: "feature-7".to_owned(),
        head_sha: "sha-7".to_owned(),
        worktree: store.path().join("pr-worktrees/pr-7"),
        tracking_ref: "refs/tracedecay/pr/7".to_owned(),
    };
    let entry_without = |field: &str| {
        let mut entry = serde_json::to_value(&current).expect("encode entry");
        entry.as_object_mut().expect("entry object").remove(field);
        entry
    };
    std::fs::write(
        store.path().join("pr-autotrack.json"),
        serde_json::json!({
            "managed": {
                "tracedecay/autotrack/pr/7": current,
                "tracedecay/autotrack/pr/8": entry_without("head_sha"),
                "tracedecay/autotrack/pr/9": entry_without("tracking_ref"),
            }
        })
        .to_string(),
    )
    .expect("write mixed state");

    let state = load_state(store.path()).expect("stale entries do not fail the file");
    assert_eq!(
        state.managed.into_iter().collect::<Vec<_>>(),
        vec![("tracedecay/autotrack/pr/7".to_owned(), current.clone())]
    );
    assert_eq!(
        state
            .stale
            .iter()
            .map(|stale| (stale.label.as_str(), stale.detail.split(" at ").next()))
            .collect::<Vec<_>>(),
        vec![
            (
                "tracedecay/autotrack/pr/8",
                Some("missing field `head_sha`")
            ),
            (
                "tracedecay/autotrack/pr/9",
                Some("missing field `tracking_ref`")
            ),
        ]
    );
    assert_eq!(managed_summary(store.path()).expect("summary")[0].pr, 7);

    save_state(store.path(), &load_state(store.path()).expect("reload")).expect("reset");
    let reset = load_state(store.path()).expect("load reset state");
    assert!(reset.stale.is_empty());
    assert_eq!(reset.managed["tracedecay/autotrack/pr/7"], current);
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
