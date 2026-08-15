use std::sync::Arc;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_activates_discovered_pr_head_when_scheduler_is_injected() {
    use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    let origin = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        repo.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "initial"]);
    git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(
        repo.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    git(repo.path(), &["push", "-q", "origin", "main"]);
    git(repo.path(), &["checkout", "-q", "-b", "feature-11", "main"]);
    std::fs::write(repo.path().join("src/pr_11.rs"), "pub fn pr_eleven() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "PR 11 content"]);
    git(repo.path(), &["push", "-q", "origin", "feature-11"]);
    git(
        origin.path(),
        &["update-ref", "refs/pull/11/head", "refs/heads/feature-11"],
    );
    git(repo.path(), &["checkout", "-q", "main"]);
    git(repo.path(), &["branch", "-q", "-D", "feature-11"]);

    let graph = Arc::new(
        crate::tracedecay::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let discovery = discover_open_prs(repo.path()).expect("discover PR head");
    assert_eq!(discovery.open.len(), 1);
    assert_eq!(discovery.open[0].number, 11);

    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let command_control = PrCommandControl::default();
    let report = reconcile_project_with_administration(
        repo.path(),
        &data_root,
        &discovery,
        10,
        PrStoreAdministration::with_control(&schedulers, &graph, &command_control),
    )
    .await;

    assert_eq!(report.failures, Vec::<(String, String)>::new());
    assert_eq!(report.tracked, vec![pr_label(11)]);
    let worktree = data_root.join("pr-worktrees/pr-11");
    assert!(worktree.is_dir(), "PR head must be checked out");
    assert!(
        schedulers.is_worktree_mounted(&worktree).await,
        "scheduler must mount the registered PR worktree"
    );
    assert!(load_state(&data_root).managed.contains_key(&pr_label(11)));
    schedulers.shutdown().await;
}

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
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

fn init_manual_branch_repo(repo: &Path, branch: &str) {
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.name", "TraceDecay Test"]);
    git(
        repo,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "initial"]);
    git(repo, &["checkout", "-q", "-b", branch, "main"]);
    std::fs::write(repo.join("src/feature.rs"), "pub fn on_feature() {}\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "feature content"]);
    git(repo, &["checkout", "-q", "main"]);
}

fn git_ref_exists(repo: &Path, reference: &str) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options", reference])
        .current_dir(repo)
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_activates_when_scheduler_is_injected() {
    use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-manual");

    let graph = Arc::new(
        crate::tracedecay::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let activation =
        activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature-manual")
            .await
            .expect("manual branch activation");

    assert_eq!(activation.branch, "feature-manual");
    assert_eq!(activation.outcome, crate::branch::BranchAddOutcome::Added);
    assert!(
        activation.worktree.is_dir(),
        "branch head must be checked out"
    );
    assert!(
        schedulers.is_worktree_mounted(&activation.worktree).await,
        "scheduler must mount the registered branch worktree"
    );
    assert!(git_ref_exists(
        repo.path(),
        "refs/tracedecay/branch/feature-manual"
    ));
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_fails_closed_without_scheduler_before_git_or_state_mutation() {
    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-denied");

    let graph = Arc::new(
        crate::tracedecay::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let error = activate_manual_branch_head(repo.path(), &graph, None, "feature-denied")
        .await
        .expect_err("missing scheduler must deny activation");

    assert!(matches!(
        error,
        ManualBranchActivationError::SchedulerUnavailable { .. }
    ));
    assert_eq!(error.reason_code(), "code_index_scheduler_unavailable");
    assert!(!data_root.join("branch-worktrees").exists());
    assert!(!git_ref_exists(
        repo.path(),
        "refs/tracedecay/branch/feature-denied"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_missing_ref_is_typed_failure() {
    use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-present");

    let graph = Arc::new(
        crate::tracedecay::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let error = activate_manual_branch_head(
        repo.path(),
        &graph,
        Some(&schedulers),
        "definitely-missing-branch",
    )
    .await
    .expect_err("missing branch ref must be a typed failure");

    assert!(matches!(
        error,
        ManualBranchActivationError::InvalidBranchRef { .. }
    ));
    assert_eq!(error.reason_code(), "invalid_branch_ref");
    assert!(!data_root.join("branch-worktrees").exists());
    assert!(!git_ref_exists(
        repo.path(),
        "refs/tracedecay/branch/definitely-missing-branch"
    ));
    schedulers.shutdown().await;
}
