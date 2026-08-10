use super::*;

#[tokio::test]
async fn spawned_loop_is_cancellable_and_joinable() {
    let profile = tempfile::tempdir().unwrap();
    let task = spawn(Some(profile.path().join("global.db")));

    assert!(
        tokio::time::timeout(Duration::from_secs(1), task.shutdown())
            .await
            .is_ok()
    );
}

#[test]
fn pr_git_commands_enforce_deadline_cancellation_and_output_limits() {
    let root = tempfile::tempdir().unwrap();
    let expired = PrCommandControl {
        command_timeout: Duration::ZERO,
        ..PrCommandControl::default()
    };
    assert!(matches!(
        run_git_with_control(root.path(), &["--version"], &expired),
        Err(tracedecay_runtime_core::git::GitCommandError::DeadlineExceeded)
    ));

    let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
    cancellation.cancel();
    let cancelled = PrCommandControl {
        cancellation: Some(cancellation),
        ..PrCommandControl::default()
    };
    assert!(matches!(
        run_git_with_control(root.path(), &["--version"], &cancelled),
        Err(tracedecay_runtime_core::git::GitCommandError::Cancelled)
    ));

    let limited = PrCommandControl {
        max_stdout_bytes: 1,
        ..PrCommandControl::default()
    };
    assert!(matches!(
        run_git_with_control(root.path(), &["--version"], &limited),
        Err(
            tracedecay_runtime_core::git::GitCommandError::OutputLimitExceeded {
                stream: "stdout",
                bound: 1
            }
        )
    ));
}

// ---- Pure discovery parsers -------------------------------------------------

#[test]
fn gh_pr_list_splits_open_same_repo_from_forks() {
    let json = r#"[
        {"number": 1, "headRefName": "feature-a", "headRefOid": "sha-a", "state": "OPEN", "isCrossRepository": false},
        {"number": 2, "headRefName": "fork-branch", "headRefOid": "sha-fork", "state": "OPEN", "isCrossRepository": true},
        {"number": 3, "headRefName": "closed-branch", "headRefOid": "sha-closed", "state": "CLOSED", "isCrossRepository": false},
        {"number": 4, "headRefName": "feature-b", "headRefOid": "sha-b", "state": "OPEN", "isCrossRepository": false}
    ]"#;
    let discovery = parse_gh_pr_list(json, 200).unwrap();
    assert!(
        !discovery.partial,
        "four PRs under a 200 limit are complete"
    );
    assert_eq!(
        discovery.open,
        vec![
            DiscoveredPr {
                number: 1,
                head_branch: "feature-a".to_string(),
                head_sha: "sha-a".to_string(),
            },
            DiscoveredPr {
                number: 4,
                head_branch: "feature-b".to_string(),
                head_sha: "sha-b".to_string(),
            },
        ]
    );
    assert_eq!(discovery.skipped_forks, vec![2]);
}

#[test]
fn ls_remote_heads_indexes_branch_shas() {
    let output = "\
deadbeef00000000000000000000000000000001\trefs/heads/main
deadbeef00000000000000000000000000000002\trefs/heads/feature-1
cafebabe00000000000000000000000000000003\trefs/tags/v1
";
    let map = parse_ls_remote_heads(output);
    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get("deadbeef00000000000000000000000000000002").unwrap(),
        "feature-1"
    );
    assert!(!map.contains_key("cafebabe00000000000000000000000000000003"));
}

#[test]
fn ls_remote_pull_heads_parses_numbers_and_ignores_merge_refs() {
    let output = "\
deadbeef00000000000000000000000000000002\trefs/pull/1/head
feed000000000000000000000000000000000009\trefs/pull/1/merge
beadfeed00000000000000000000000000000007\trefs/pull/42/head
";
    let heads = parse_ls_remote_pull_heads(output);
    assert_eq!(
        heads,
        vec![
            (1, "deadbeef00000000000000000000000000000002".to_string()),
            (42, "beadfeed00000000000000000000000000000007".to_string()),
        ]
    );
}

#[test]
fn map_pull_heads_matches_same_repo_and_skips_forks() {
    let pull_heads = vec![
        (1, "sha_feature".to_string()),
        (2, "sha_fork_only".to_string()),
    ];
    let mut head_shas = HashMap::new();
    head_shas.insert("sha_feature".to_string(), "feature-1".to_string());
    head_shas.insert("sha_main".to_string(), "main".to_string());

    let discovery = map_pull_heads_to_branches(&pull_heads, &head_shas);
    assert_eq!(
        discovery.open,
        vec![DiscoveredPr {
            number: 1,
            head_branch: "feature-1".to_string(),
            head_sha: "sha_feature".to_string(),
        }]
    );
    assert_eq!(discovery.skipped_forks, vec![2]);
}

#[test]
fn gh_pr_list_flags_partial_when_result_reaches_limit() {
    let json = r#"[
        {"number": 1, "headRefName": "a", "headRefOid": "s1", "state": "OPEN", "isCrossRepository": false},
        {"number": 2, "headRefName": "b", "headRefOid": "s2", "state": "OPEN", "isCrossRepository": false}
    ]"#;
    // Two results at a limit of two: the listing was truncated → partial.
    let truncated = parse_gh_pr_list(json, 2).unwrap();
    assert!(
        truncated.partial,
        "count == limit must be treated as possibly truncated"
    );
    // Same results under a higher limit are complete.
    let complete = parse_gh_pr_list(json, 5).unwrap();
    assert!(!complete.partial);
}

// ---- State persistence ------------------------------------------------------

#[test]
fn state_round_trips_and_defaults_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_state(dir.path()).managed.is_empty());

    let mut state = PrAutotrackState::default();
    state.managed.insert(
        "tracedecay/autotrack/pr/7".to_string(),
        ManagedPr {
            pr: 7,
            head_branch: "feature-7".to_string(),
            head_sha: "sha-7".to_string(),
            worktree: dir.path().join("pr-worktrees/pr-7"),
            tracking_ref: "refs/tracedecay/pr/7".to_string(),
        },
    );
    save_state(dir.path(), &state).unwrap();

    let reloaded = load_state(dir.path());
    assert_eq!(reloaded.managed.len(), 1);
    assert_eq!(reloaded.managed["tracedecay/autotrack/pr/7"].pr, 7);

    let summary = managed_summary(dir.path());
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].branch, "tracedecay/autotrack/pr/7");
    assert_eq!(summary[0].head_branch, "feature-7");

    std::fs::write(
        state_path(dir.path()),
        r#"{"managed":{"pr/8":{"pr":8,"head_branch":"legacy","worktree":"pr-worktrees/pr-8","tracking_ref":"refs/tracedecay/pr/8"}}}"#,
    )
    .unwrap();
    assert_eq!(
        load_state(dir.path()).managed["pr/8"].head_sha,
        "",
        "legacy state without a head SHA must migrate as needing refresh"
    );
}

// ---- Reconcile: removal + idempotency (no index required) -------------------

#[tokio::test]
async fn reconcile_preserves_closed_pr_when_scheduler_retirement_is_unavailable() {
    use crate::branch_meta::{BranchMeta, load_branch_meta, save_branch_meta};

    let data_root = tempfile::tempdir().unwrap();
    let repo_root = tempfile::tempdir().unwrap(); // not a git repo; git ops no-op

    // Seed a tracked PR branch store entry + its DB file.
    let mut meta = BranchMeta::new("main");
    meta.add_branch("pr/5", "branches/pr_5.db", "main");
    std::fs::create_dir_all(data_root.path().join("branches")).unwrap();
    drop(
        rusqlite::Connection::open(data_root.path().join("branches/pr_5.db"))
            .expect("empty branch database"),
    );
    save_branch_meta(data_root.path(), &meta).unwrap();

    // Seed autotrack state marking pr/5 as managed.
    let mut state = PrAutotrackState::default();
    state.managed.insert(
        "pr/5".to_string(),
        ManagedPr {
            pr: 5,
            head_branch: "feature-5".to_string(),
            head_sha: "sha-5".to_string(),
            worktree: data_root.path().join("pr-worktrees/pr-5"),
            tracking_ref: "refs/tracedecay/pr/5".to_string(),
        },
    );
    save_state(data_root.path(), &state).unwrap();

    // Empty discovery means PR 5 closed, but no scheduler retirement authority
    // is injected into this state-only fixture. Reconciliation must fail closed
    // without deleting its durable state or Git-adjacent artifacts.
    let identity = crate::daemon::profile_identity::load_or_create(data_root.path()).unwrap();
    let _database_scope = crate::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "pr-autotrack-removal-test",
    )
    .unwrap();
    let daemon_administration = StoreAdministration::default().with_profile_identity(identity);
    let administration = PrStoreAdministration::state_only(&daemon_administration);
    let report = reconcile_project_with_administration(
        repo_root.path(),
        data_root.path(),
        &PrDiscovery::default(),
        10,
        administration,
    )
    .await;

    assert!(report.untracked.is_empty());
    assert!(report.tracked.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0]
            .1
            .starts_with("code_index_scheduler_unavailable:")
    );
    assert!(load_state(data_root.path()).managed.contains_key("pr/5"));
    let reloaded = load_branch_meta(data_root.path()).unwrap();
    assert!(reloaded.is_tracked("pr/5"));
    assert!(data_root.path().join("branches/pr_5.db").exists());
}

#[tokio::test]
async fn reconcile_does_not_prepare_new_pr_without_scheduler_activation() {
    let data_root = tempfile::tempdir().unwrap();
    let repo_root = tempfile::tempdir().unwrap();
    let discovery = PrDiscovery {
        open: vec![DiscoveredPr {
            number: 9,
            head_branch: "feature-9".to_owned(),
            head_sha: "sha-9".to_owned(),
        }],
        ..Default::default()
    };
    let daemon_administration = StoreAdministration::default();

    let report = reconcile_project_with_administration(
        repo_root.path(),
        data_root.path(),
        &discovery,
        10,
        PrStoreAdministration::state_only(&daemon_administration),
    )
    .await;

    assert!(report.tracked.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0]
            .1
            .starts_with("code_index_scheduler_unavailable:")
    );
    assert!(load_state(data_root.path()).managed.is_empty());
    assert!(!data_root.path().join("pr-worktrees").exists());
}

#[tokio::test]
async fn reconcile_is_idempotent_for_already_managed_pr() {
    let data_root = tempfile::tempdir().unwrap();
    let repo_root = tempfile::tempdir().unwrap();

    let mut state = PrAutotrackState::default();
    state.managed.insert(
        "tracedecay/autotrack/pr/3".to_string(),
        ManagedPr {
            pr: 3,
            head_branch: "feature-3".to_string(),
            head_sha: "sha-3".to_string(),
            worktree: data_root.path().join("pr-worktrees/pr-3"),
            tracking_ref: "refs/tracedecay/pr/3".to_string(),
        },
    );
    save_state(data_root.path(), &state).unwrap();

    let discovery = PrDiscovery {
        open: vec![DiscoveredPr {
            number: 3,
            head_branch: "feature-3".to_string(),
            head_sha: "sha-3".to_string(),
        }],
        skipped_forks: vec![],
        ..Default::default()
    };
    let daemon_administration = StoreAdministration::default();
    let report = reconcile_project_with_administration(
        repo_root.path(),
        data_root.path(),
        &discovery,
        10,
        PrStoreAdministration::state_only(&daemon_administration),
    )
    .await;

    // Already managed and still open: nothing changes.
    assert!(report.tracked.is_empty());
    assert!(report.untracked.is_empty());
    assert!(
        load_state(data_root.path())
            .managed
            .contains_key("tracedecay/autotrack/pr/3")
    );
}

#[tokio::test]
async fn partial_discovery_suppresses_removals() {
    use crate::branch_meta::{BranchMeta, load_branch_meta, save_branch_meta};

    let data_root = tempfile::tempdir().unwrap();
    let repo_root = tempfile::tempdir().unwrap();

    // Seed a managed PR branch store + entry, exactly as the untrack test does.
    let mut meta = BranchMeta::new("main");
    meta.add_branch("pr/5", "branches/pr_5.db", "main");
    std::fs::create_dir_all(data_root.path().join("branches")).unwrap();
    std::fs::write(data_root.path().join("branches/pr_5.db"), b"db").unwrap();
    save_branch_meta(data_root.path(), &meta).unwrap();

    let mut state = PrAutotrackState::default();
    state.managed.insert(
        "pr/5".to_string(),
        ManagedPr {
            pr: 5,
            head_branch: "feature-5".to_string(),
            head_sha: "sha-5".to_string(),
            worktree: data_root.path().join("pr-worktrees/pr-5"),
            tracking_ref: "refs/tracedecay/pr/5".to_string(),
        },
    );
    save_state(data_root.path(), &state).unwrap();

    // Empty BUT partial discovery: PR 5 is absent only because the listing was
    // truncated, not because it closed — it must NOT be untracked.
    let discovery = PrDiscovery {
        partial: true,
        ..Default::default()
    };
    let daemon_administration = StoreAdministration::default();
    let report = reconcile_project_with_administration(
        repo_root.path(),
        data_root.path(),
        &discovery,
        10,
        PrStoreAdministration::state_only(&daemon_administration),
    )
    .await;

    assert!(
        report.removals_suppressed,
        "partial view suppresses removals"
    );
    assert!(report.untracked.is_empty(), "no untrack on a partial view");
    assert!(
        load_state(data_root.path()).managed.contains_key("pr/5"),
        "managed entry survives a partial discovery"
    );
    assert!(
        load_branch_meta(data_root.path())
            .unwrap()
            .is_tracked("pr/5")
    );
    assert!(data_root.path().join("branches/pr_5.db").exists());
}
