use tracedecay_application::pr_tracking::{
    ManagedPr, PrAutotrackState, load_state, managed_summary, save_state,
};

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

    assert_eq!(load_state(store.path()).managed, state.managed);
    assert_eq!(managed_summary(store.path())[0].pr, 7);
}
