use std::sync::Arc;
use std::time::Duration;

use super::*;

#[tokio::test]
async fn spawned_loop_is_cancellable_and_joinable() {
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let task = spawn_with_administration(StoreAdministration::default(), schedulers);

    assert!(
        tokio::time::timeout(Duration::from_secs(1), task.shutdown())
            .await
            .is_ok()
    );
}

// ---- Reconcile: removal + idempotency (no index required) -------------------

#[tokio::test]
async fn reconcile_preserves_closed_pr_when_scheduler_retirement_is_unavailable() {
    use tracedecay_runtime_core::branch_meta::{BranchMeta, load_branch_meta, save_branch_meta};

    let data_root = tempfile::tempdir().unwrap();
    let repo_root = tempfile::tempdir().unwrap(); // not a git repo; git ops no-op

    let mut meta = BranchMeta::new("main");
    meta.add_branch("pr/5", "branches/pr_5.db", "main");
    std::fs::create_dir_all(data_root.path().join("branches")).unwrap();
    drop(
        rusqlite::Connection::open(data_root.path().join("branches/pr_5.db"))
            .expect("empty branch database"),
    );
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

    // Empty discovery means PR 5 closed, but no scheduler retirement authority
    // is injected into this state-only fixture. Reconciliation must fail closed
    // without deleting its durable state or Git-adjacent artifacts.
    // The profile identity root must be a directory `load_or_create` creates
    // (and restricts to 0700) itself; a umask-default tempdir trips the
    // fail-closed private-root validation.
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(
        &data_root.path().join("profile"),
    )
    .unwrap();
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
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
    .await
    .expect("load managed PR state");

    assert!(report.untracked.is_empty());
    assert!(report.tracked.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0]
            .1
            .starts_with("code_index_scheduler_unavailable:")
    );
    assert!(
        load_state(data_root.path())
            .expect("load managed PR state")
            .managed
            .contains_key("pr/5")
    );
    let reloaded = load_branch_meta(data_root.path()).unwrap();
    assert!(reloaded.is_tracked("pr/5"));
    assert!(data_root.path().join("branches/pr_5.db").exists());
}

#[tokio::test]
async fn cancelled_pr_teardown_preserves_artifacts_and_retries_exactly() {
    let repo = tempfile::tempdir().expect("repository root");
    let data_root = tempfile::tempdir().expect("data root");
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        repo.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(repo.path().join("tracked.txt"), "tracked\n").expect("write fixture");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "initial"]);

    let pr = 5;
    let label = pr_label(pr);
    let tracking_ref = pr_tracking_ref(pr);
    let head_sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
    let worktree = data_root.path().join("pr-worktrees/pr-5");
    std::fs::create_dir_all(worktree.parent().expect("worktree parent"))
        .expect("create worktree parent");
    git(repo.path(), &["update-ref", &tracking_ref, &head_sha]);
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &label,
            worktree.to_str().expect("utf-8 worktree"),
            &head_sha,
        ],
    );
    let mut state = PrAutotrackState::default();
    state.managed.insert(
        label.clone(),
        ManagedPr {
            pr,
            head_branch: "feature-5".to_owned(),
            head_sha: head_sha.clone(),
            worktree: worktree.clone(),
            tracking_ref: tracking_ref.clone(),
        },
    );
    save_state(data_root.path(), &state).expect("persist managed state");

    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
    cancellation.cancel();
    let cancelled_control = PrCommandControl::with_cancellation(cancellation);
    let cancelled = PrStoreAdministration {
        schedulers: Some(&schedulers),
        graph: None,
        command_control: &cancelled_control,
    };
    let report = reconcile_project_with_administration(
        repo.path(),
        data_root.path(),
        &PrDiscovery::default(),
        10,
        cancelled,
    )
    .await
    .expect("cancelled reconciliation returns a report");

    assert!(report.untracked.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(
        load_state(data_root.path())
            .expect("reload cancelled state")
            .managed
            .contains_key(&label),
        "cancelled cleanup must preserve durable ownership"
    );
    assert!(worktree.exists(), "cancelled cleanup preserves worktree");
    assert!(git_ref_exists(repo.path(), &format!("refs/heads/{label}")));
    assert!(git_ref_exists(repo.path(), &tracking_ref));

    let retry_control = PrCommandControl::default();
    let retry = PrStoreAdministration {
        schedulers: Some(&schedulers),
        graph: None,
        command_control: &retry_control,
    };
    let report = reconcile_project_with_administration(
        repo.path(),
        data_root.path(),
        &PrDiscovery::default(),
        10,
        retry,
    )
    .await
    .expect("retry reconciliation returns a report");

    assert_eq!(report.untracked, vec![label.clone()]);
    assert!(report.failures.is_empty());
    assert!(
        load_state(data_root.path())
            .expect("reload cleaned state")
            .managed
            .is_empty()
    );
    assert!(!worktree.exists(), "retry removes worktree");
    assert!(!git_ref_exists(repo.path(), &format!("refs/heads/{label}")));
    assert!(!git_ref_exists(repo.path(), &tracking_ref));
}

#[tokio::test]
async fn reconcile_refuses_malformed_state_before_branch_mutation() {
    let data_root = tempfile::tempdir().expect("data root");
    let repo_root = tempfile::tempdir().expect("repository root");
    std::fs::write(data_root.path().join("pr-autotrack.json"), "{not json")
        .expect("write malformed state");
    let discovery = PrDiscovery {
        open: vec![DiscoveredPr {
            number: 9,
            head_branch: "feature-9".to_owned(),
            head_sha: "sha-9".to_owned(),
        }],
        ..PrDiscovery::default()
    };
    let daemon_administration = StoreAdministration::default();

    let error = reconcile_project_with_administration(
        repo_root.path(),
        data_root.path(),
        &discovery,
        10,
        PrStoreAdministration::state_only(&daemon_administration),
    )
    .await
    .expect_err("malformed durable state must fail closed");

    assert!(matches!(
        error,
        tracedecay_domain::errors::TraceDecayError::Json(_)
    ));
    assert!(!data_root.path().join("pr-worktrees").exists());
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
    .await
    .expect("load managed PR state");

    assert!(report.tracked.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0]
            .1
            .starts_with("code_index_scheduler_unavailable:")
    );
    assert!(
        load_state(data_root.path())
            .expect("load managed PR state")
            .managed
            .is_empty()
    );
    assert!(!data_root.path().join("pr-worktrees").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_activates_discovered_pr_head_when_scheduler_is_injected() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

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
        crate::project::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let discovery = discover_open_prs_with_control(repo.path(), default_pr_command_control())
        .expect("discover PR head");
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
    .await
    .expect("load managed PR state");

    assert_eq!(report.failures, Vec::<(String, String)>::new());
    assert_eq!(report.tracked, vec![pr_label(11)]);
    let worktree = data_root.join("pr-worktrees/pr-11");
    assert!(worktree.is_dir(), "PR head must be checked out");
    assert!(
        schedulers.is_worktree_mounted(&worktree).await,
        "scheduler must mount the registered PR worktree"
    );
    assert!(
        load_state(&data_root)
            .expect("load managed PR state")
            .managed
            .contains_key(&pr_label(11))
    );
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

/// Runs one Git command and reports only whether it succeeded. Used for
/// options whose availability depends on the installed Git version.
fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Runs one Git command and returns its trimmed stdout.
fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("git output")
        .trim()
        .to_owned()
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
    .await
    .expect("load managed PR state");

    // Already managed and still open: nothing changes.
    assert!(report.tracked.is_empty());
    assert!(report.untracked.is_empty());
    assert!(
        load_state(data_root.path())
            .expect("load managed PR state")
            .managed
            .contains_key("tracedecay/autotrack/pr/3")
    );
}

#[tokio::test]
async fn partial_discovery_suppresses_removals() {
    use tracedecay_runtime_core::branch_meta::{BranchMeta, load_branch_meta, save_branch_meta};

    let data_root = tempfile::tempdir().unwrap();
    let repo_root = tempfile::tempdir().unwrap();

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
    .await
    .expect("load managed PR state");

    assert!(
        report.removals_suppressed,
        "partial view suppresses removals"
    );
    assert!(report.untracked.is_empty(), "no untrack on a partial view");
    assert!(
        load_state(data_root.path())
            .expect("load managed PR state")
            .managed
            .contains_key("pr/5"),
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
    // Pin the files ref backend. This suite's exact-ref coverage opens the
    // loose ref file directly, which a reftable repository never materializes.
    // Git versions that predate `--ref-format` reject the option and already
    // create files-backed repositories.
    if !git_succeeds(repo, &["init", "-q", "-b", "main", "--ref-format=files"]) {
        git(repo, &["init", "-q", "-b", "main"]);
    }
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
        .output()
        .is_ok_and(|output| output.status.success())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_activates_when_scheduler_is_injected() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-manual");

    let graph = Arc::new(
        crate::project::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let activation =
        activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature-manual")
            .await
            .expect("manual branch activation");

    assert_eq!(activation.branch, "feature-manual");
    assert_eq!(
        activation.outcome,
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
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
        &ManualBranchArtifactsV1::for_head(
            &graph.store_layout().data_root,
            "feature-manual",
            &activation.head_sha
        )
        .tracking_ref
    ));
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_linked_worktree_honors_parent_native_graph_refusal() {
    use tracedecay_code_index_runtime::code_index_scheduler::{
        CodeIndexSchedulerRegistryV1, identity::IndexingIdentityV1,
    };
    use tracedecay_domain::configuration::{
        ConfigurationGrantId, ConfigurationGrantReceiptId, ConfigurationIdempotencyKey,
        ConfigurationLayerIdV1, ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
        ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationValueV1,
        INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY, SettingKey,
    };
    use tracedecay_domain::{AccessPolicyDigest, ActorId, UtcMicros};
    use tracedecay_global_db::configuration::contracts::{
        ConfigurationControlStore, ConfigurationMutationAuthority, DirectConfigurationMutation,
    };

    let repo = tempfile::tempdir().expect("repository root");
    let linked_parent = tempfile::tempdir().expect("linked-worktree parent");
    let linked = linked_parent.path().join("linked");
    init_manual_branch_repo(repo.path(), "feature-retained-refusal");

    let graph = crate::project::TraceDecay::open(repo.path())
        .await
        .expect("open writable parent graph");
    let current = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("read parent configuration");
    let project_id = current.target().project_id.clone();
    let mutation = DirectConfigurationMutation::Set {
        layer: ConfigurationLayerIdV1::Project {
            project_id: project_id.clone(),
        },
        key: SettingKey::new(INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY)
            .expect("native graph activation key"),
        value: Box::new(ConfigurationValueV1::Boolean(false)),
    };
    let authority = ConfigurationMutationAuthority {
        receipt: ConfigurationMutationGrantReceiptV1::issue(
            ConfigurationGrantReceiptId::new("configuration.grant-receipt.linked-graph-refusal")
                .expect("grant receipt id"),
            ConfigurationGrantId::new("configuration.grant.linked-graph-refusal")
                .expect("grant id"),
            ActorId::new("actor.linked-graph-refusal").expect("actor id"),
            ConfigurationMutationOperationV1::DirectMutation,
            mutation
                .target_scope_digest()
                .expect("mutation target scope"),
            current.revision_id().clone(),
            1,
            AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64))).expect("policy digest"),
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
            Some(
                ConfigurationIdempotencyKey::new("configuration.idempotency.linked-graph-refusal")
                    .expect("idempotency key"),
            ),
            UtcMicros(1),
            UtcMicros(100),
        )
        .expect("issue mutation grant"),
    };
    ConfigurationControlStore::commit_direct(
        &graph.configuration_runtime().configuration_store(),
        &authority,
        &mutation,
        current.revision_id(),
    )
    .await
    .expect("persist native graph refusal");
    let data_root = graph.store_layout().data_root.clone();
    graph.close();

    let graph = Arc::new(
        crate::project::TraceDecay::open_read_only(repo.path())
            .await
            .expect("reopen parent graph from persisted configuration"),
    );
    assert!(
        !graph.get_config().native_graph_activation,
        "the parent graph must carry the persisted refusal into linked-worktree activation"
    );

    let head = resolve_branch_head(
        repo.path(),
        "feature-retained-refusal",
        default_pr_command_control(),
    )
    .expect("resolve linked-worktree head");
    let artifacts =
        ManualBranchArtifactsV1::for_head(&data_root, "feature-retained-refusal", &head);
    prepare_manual_branch_worktree(
        repo.path(),
        &linked,
        &artifacts.tracking_ref,
        &artifacts.label,
        &head,
        default_pr_command_control(),
    )
    .expect("prepare linked worktree");

    let code_index_store = data_root.join("code-index-v1");
    let seeder = CodeIndexSchedulerRegistryV1::new(1);
    seeder
        .mount_worktree(project_id.clone(), &linked, code_index_store.clone(), None)
        .await
        .expect("mount retained-generation seeder");
    tokio::time::timeout(Duration::from_secs(5), async {
        while seeder.latest_generation_id(&linked).await.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("seal retained linked-worktree generation");
    seeder.shutdown().await;

    let identity = IndexingIdentityV1::resolve(&linked).expect("linked-worktree identity");
    let scope = tracedecay_contracts::ResolvedScope::new(
        project_id,
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("linked-worktree scope");
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    activate_linked_worktree(&schedulers, &graph, &linked)
        .await
        .expect("mount retained linked-worktree generation");
    let latest = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(latest) = schedulers.latest_text_serving_for_scope(&scope).await {
                break latest;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("configured graph refusal must still seat retained text serving");
    assert!(
        latest.production_query_owners().is_ok(),
        "exact and lexical owners must warm from the retained generation"
    );
    assert!(
        latest.interactive_graph_store().is_err(),
        "configured refusal must not open the persistent Grafeo graph"
    );
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_identity_keeps_slashed_and_underscored_names_disjoint() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/a");
    git(repo.path(), &["checkout", "-q", "-b", "feature_a", "main"]);
    std::fs::write(
        repo.path().join("src/underscored.rs"),
        "pub fn underscored() {}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "underscored feature"]);
    git(repo.path(), &["checkout", "-q", "main"]);

    let graph = Arc::new(crate::project::TraceDecay::open(repo.path()).await.unwrap());
    let data_root = graph.store_layout().data_root.clone();
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let slashed = activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature/a")
        .await
        .expect("slash branch activation");
    let underscored =
        activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature_a")
            .await
            .expect("underscore branch activation");

    assert_eq!(
        slashed.outcome,
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    assert_eq!(
        underscored.outcome,
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    assert_ne!(slashed.worktree, underscored.worktree);
    assert_ne!(
        ManualBranchArtifactsV1::for_head(&data_root, "feature/a", &slashed.head_sha).worktree,
        ManualBranchArtifactsV1::for_head(&data_root, "feature_a", &underscored.head_sha).worktree
    );
    assert!(git_ref_exists(
        repo.path(),
        &ManualBranchArtifactsV1::for_head(&data_root, "feature/a", &slashed.head_sha).tracking_ref
    ));
    assert!(git_ref_exists(
        repo.path(),
        &ManualBranchArtifactsV1::for_head(&data_root, "feature_a", &underscored.head_sha)
            .tracking_ref
    ));
    schedulers.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_stages_new_head_without_replacing_published_worktree() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/advance");
    let graph = Arc::new(crate::project::TraceDecay::open(repo.path()).await.unwrap());
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let initial =
        activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature/advance")
            .await
            .expect("initial activation");

    let publication = crate::daemon::branch_add::branch_publication_context(&graph).unwrap();
    publication
        .track_exact_worktree_branch(
            &schedulers,
            repo.path(),
            &initial.worktree,
            "feature/advance",
            &tracedecay_runtime_core::cancellation::CancellationToken::new(),
        )
        .await
        .expect("publish initial branch generation");
    let original_source =
        tracedecay_runtime_core::branch_meta::load_branch_meta(&graph.store_layout().data_root)
            .unwrap()
            .branches["feature/advance"]
            .graph_source
            .clone()
            .unwrap();

    git(repo.path(), &["checkout", "-q", "feature/advance"]);
    std::fs::write(
        repo.path().join("src/advanced.rs"),
        "pub fn advanced() {}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "advance branch head"]);
    let advanced_head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    git(repo.path(), &["checkout", "-q", "main"]);

    let staged_graph = Arc::clone(&graph);
    let staged_schedulers = schedulers.clone();
    let staged_repo = repo.path().to_path_buf();
    let (staged_sender, staged_receiver) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        let lifecycle = try_acquire_manual_branch_lifecycle(
            &staged_graph.store_layout().data_root,
            "feature/advance",
        )
        .unwrap();
        let staged = activate_manual_branch_head_with_lifecycle(
            &staged_repo,
            &staged_graph,
            Some(&staged_schedulers),
            "feature/advance",
            &lifecycle,
            default_pr_command_control(),
        )
        .await
        .expect("stage advanced head");
        staged_sender.send(staged).unwrap();
        std::future::pending::<()>().await;
        drop(lifecycle);
    });
    let replay = staged_receiver.await.unwrap();
    // A hard owner abort after staging, before metadata publication, must leave
    // the previously published worktree and its exact Git identity usable.
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    assert_ne!(initial.worktree, replay.worktree);
    assert!(schedulers.is_worktree_mounted(&initial.worktree).await);
    assert_eq!(
        git_output(&initial.worktree, &["rev-parse", "HEAD"]).trim(),
        initial.head_sha
    );
    assert_eq!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(&graph.store_layout().data_root)
            .unwrap()
            .branches["feature/advance"]
            .graph_source
            .as_ref(),
        Some(&original_source)
    );
    let data_root = graph.store_layout().data_root.clone();
    let metadata_lock =
        tracedecay_runtime_core::branch::try_acquire_branch_add_lock(&data_root).unwrap();
    let deferred = crate::daemon::branch_add::activate_and_track_manual_branch_owned(
        repo.path().to_path_buf(),
        Arc::clone(&graph),
        schedulers.clone(),
        "feature/advance".to_owned(),
        data_root.clone(),
        try_acquire_manual_branch_lifecycle(&data_root, "feature/advance").unwrap(),
        tracedecay_runtime_core::cancellation::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        deferred,
        tracedecay_runtime_core::branch::BranchAddOutcome::Deferred
    );
    assert!(initial.worktree.exists());
    assert!(schedulers.is_worktree_mounted(&initial.worktree).await);
    assert_eq!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(&data_root)
            .unwrap()
            .branches["feature/advance"]
            .graph_source
            .as_ref(),
        Some(&original_source)
    );
    drop(metadata_lock);
    crate::daemon::branch_add::activate_and_track_manual_branch_owned(
        repo.path().to_path_buf(),
        Arc::clone(&graph),
        schedulers.clone(),
        "feature/advance".to_owned(),
        data_root.clone(),
        try_acquire_manual_branch_lifecycle(&data_root, "feature/advance").unwrap(),
        tracedecay_runtime_core::cancellation::CancellationToken::new(),
    )
    .await
    .expect("publish staged generation after lock releases");
    assert!(
        !initial.worktree.exists(),
        "retire prior worktree only after publication commits"
    );
    assert!(!schedulers.is_worktree_mounted(&initial.worktree).await);
    assert_eq!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(&data_root)
            .unwrap()
            .branches["feature/advance"]
            .graph_source
            .as_ref()
            .unwrap()
            .source_oid,
        replay.head_sha
    );
    let mounted_head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&replay.worktree)
        .output()
        .unwrap();

    assert_eq!(
        replay.outcome,
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    assert_ne!(initial.head_sha, replay.head_sha);
    assert_eq!(
        String::from_utf8_lossy(&advanced_head.stdout).trim(),
        String::from_utf8_lossy(&mounted_head.stdout).trim(),
        "the new candidate must carry the newly resolved branch head"
    );
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_activation_refuses_exact_lifecycle_contention_before_mutating_git() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/contended");
    let graph = Arc::new(crate::project::TraceDecay::open(repo.path()).await.unwrap());
    let lifecycle =
        try_acquire_manual_branch_lifecycle(&graph.store_layout().data_root, "feature/contended")
            .expect("first lifecycle owner");
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);

    let error =
        activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature/contended")
            .await
            .expect_err("concurrent exact branch activation must be rejected");

    assert!(matches!(
        &error,
        ManualBranchActivationError::LifecycleContended { .. }
    ));
    assert!(
        git_output(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tracedecay/branch"
            ]
        )
        .trim()
        .is_empty()
    );
    drop(lifecycle);
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_manual_branch_sealing_retires_the_exact_mount_worktree_and_tracking_ref() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/failure-cleanup");
    let graph = Arc::new(crate::project::TraceDecay::open(repo.path()).await.unwrap());
    let data_root = graph.store_layout().data_root.clone();
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let lifecycle = try_acquire_manual_branch_lifecycle(&data_root, "feature/failure-cleanup")
        .expect("lifecycle owner");
    let activation = activate_manual_branch_head_with_lifecycle(
        repo.path(),
        &graph,
        Some(&schedulers),
        "feature/failure-cleanup",
        &lifecycle,
        default_pr_command_control(),
    )
    .await
    .expect("activation before synthetic sealing failure");

    cleanup_manual_branch_activation(
        repo.path(),
        &data_root,
        &schedulers,
        &activation,
        &lifecycle,
    )
    .await
    .expect("failed sealing must clean activation-owned artifacts");

    assert!(
        !activation.worktree.exists(),
        "the linked worktree must not leak after sealing failure"
    );
    assert!(
        !git_ref_exists(
            repo.path(),
            &ManualBranchArtifactsV1::for_head(
                &data_root,
                "feature/failure-cleanup",
                &activation.head_sha
            )
            .tracking_ref
        ),
        "the exact tracking ref must not leak after sealing failure"
    );
    assert!(
        !schedulers.is_worktree_mounted(&activation.worktree).await,
        "the scheduler generation must retire with the failed worktree"
    );
    drop(lifecycle);
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_fails_closed_without_scheduler_before_git_or_state_mutation() {
    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-denied");

    let graph = Arc::new(
        crate::project::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let error = activate_manual_branch_head(repo.path(), &graph, None, "feature-denied")
        .await
        .expect_err("missing scheduler must deny activation");

    assert!(matches!(
        &error,
        ManualBranchActivationError::SchedulerUnavailable { .. }
    ));
    assert_eq!(error.reason_code(), "code_index_scheduler_unavailable");
    assert!(!data_root.join("branch-worktrees").exists());
    assert!(
        git_output(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tracedecay/branch"
            ]
        )
        .trim()
        .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_missing_ref_is_typed_failure() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-present");

    let graph = Arc::new(
        crate::project::TraceDecay::open(repo.path())
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
        &error,
        ManualBranchActivationError::InvalidBranchRef { .. }
    ));
    assert_eq!(error.reason_code(), "invalid_branch_ref");
    assert!(
        !error.retryable(),
        "a permanently missing branch identity must not become retryable"
    );
    assert!(!data_root.join("branch-worktrees").exists());
    assert!(
        git_output(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tracedecay/branch"
            ]
        )
        .trim()
        .is_empty()
    );
    schedulers.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn cancelled_activation_keeps_its_lifecycle_owner_bounded_during_stalled_exact_read() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    let branch = "feature/stalled-exact-read";
    init_manual_branch_repo(repo.path(), branch);
    let graph = Arc::new(
        crate::project::TraceDecay::open(repo.path())
            .await
            .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let activation = activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), branch)
        .await
        .expect("initial activation creates exact artifacts");
    let artifacts = ManualBranchArtifactsV1::for_head(&data_root, branch, &activation.head_sha);
    // Ask Git for the loose-ref path rather than assuming the ref stayed loose
    // after activation: a loose entry is what Git's exact-ref reader opens
    // first, and it takes precedence over any packed entry, so the FIFO stalls
    // that read whether or not the ref was packed away.
    let ref_path = {
        let reported = std::path::PathBuf::from(git_output(
            repo.path(),
            &["rev-parse", "--git-path", &artifacts.tracking_ref],
        ));
        if reported.is_absolute() {
            reported
        } else {
            repo.path().join(reported)
        }
    };
    if let Some(parent) = ref_path.parent() {
        std::fs::create_dir_all(parent).expect("loose exact-ref directory");
    }
    match std::fs::remove_file(&ref_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("replace exact tracking ref with a FIFO: {error}"),
    }
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&ref_path)
            .status()
            .expect("run mkfifo")
            .success(),
        "the exact Git ref reader must block on the real FIFO"
    );

    let (writer_ready_tx, writer_ready_rx) = std::sync::mpsc::sync_channel(1);
    let writer_path = ref_path.clone();
    let fifo_writer_task = tokio::task::spawn_blocking(move || {
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open FIFO writer once Git begins its exact ref read");
        writer_ready_tx
            .send(writer)
            .expect("deliver open FIFO writer");
    });

    let (owner_done_tx, owner_done_rx) = tokio::sync::oneshot::channel();
    let owner_repo = repo.path().to_path_buf();
    let owner_data_root = data_root.clone();
    let owner_graph = Arc::clone(&graph);
    let owner_schedulers = schedulers.clone();
    let owner_branch = branch.to_owned();
    let requester = tokio::spawn(async move {
        let owner = tokio::spawn(async move {
            let lifecycle = try_acquire_manual_branch_lifecycle(&owner_data_root, &owner_branch)
                .expect("activation owner acquires the exact lifecycle");
            let control = PrCommandControl::with_timeout(Duration::from_millis(300));
            let outcome = activate_manual_branch_with_administration(
                &owner_repo,
                &owner_data_root,
                &owner_branch,
                PrStoreAdministration::with_control(&owner_schedulers, &owner_graph, &control),
                &lifecycle,
            )
            .await;
            let _ = owner_done_tx.send(outcome);
        });
        let _ = owner.await;
    });

    let fifo_writer = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            writer_ready_rx
                .recv()
                .expect("Git exact-ref read opens the FIFO")
        }),
    )
    .await
    .expect("activation reaches the stalled exact-ref read")
    .expect("FIFO-writer task joins");
    fifo_writer_task.await.expect("FIFO writer task joins");
    let (heartbeat_tx, heartbeat_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        let _ = heartbeat_tx.send(());
    });
    tokio::time::timeout(Duration::from_millis(100), heartbeat_rx)
        .await
        .expect("the current-thread runtime keeps scheduling while Git exact-read stalls")
        .expect("heartbeat task runs");

    requester.abort();
    assert!(
        requester.await.is_err(),
        "the requester is cancelled while its lifecycle owner continues"
    );
    let error = tokio::time::timeout(Duration::from_secs(2), owner_done_rx)
        .await
        .expect("lifecycle owner remains bounded by its Git deadline")
        .expect("lifecycle owner reports its terminal activation outcome")
        .expect_err("a timed-out exact read cannot be treated as a missing ref");
    assert!(matches!(
        &error,
        ManualBranchActivationError::GitAuthorityUnavailable { .. }
    ));
    assert!(error.retryable());
    let response = super::super::branch_add::typed_project_route_error(
        serde_json::json!("exact-read-timeout"),
        error.reason_code(),
        error.retryable(),
        error.detail(),
    );
    let response = serde_json::to_value(response).expect("serialize production JSON-RPC error");
    assert_eq!(
        response["error"]["data"]["reason_code"],
        "git_authority_unavailable"
    );
    assert_eq!(response["error"]["data"]["retryable"], true);

    drop(fifo_writer);
    std::fs::remove_file(&ref_path).expect("remove stalled FIFO ref");
    git(
        repo.path(),
        &["update-ref", &artifacts.tracking_ref, &activation.head_sha],
    );
    assert!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(&data_root)
            .is_none_or(|metadata| !metadata.branches.contains_key(branch)),
        "activation alone must not leak sealed branch provenance"
    );
    let lifecycle = try_acquire_manual_branch_lifecycle(&data_root, branch)
        .expect("completed owner releases the exact lifecycle lease");
    cleanup_manual_branch_activation(
        repo.path(),
        &data_root,
        &schedulers,
        &activation,
        &lifecycle,
    )
    .await
    .expect("recovered exact artifacts cleanly retire");
    assert!(!activation.worktree.exists(), "no linked worktree leaks");
    assert!(
        !git_ref_exists(repo.path(), &artifacts.tracking_ref),
        "no synthetic tracking ref leaks"
    );
    assert!(
        !schedulers.is_worktree_mounted(&activation.worktree).await,
        "no scheduler mount leaks"
    );
    assert!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(&data_root)
            .is_none_or(|metadata| !metadata.branches.contains_key(branch)),
        "no branch provenance leaks"
    );
    drop(lifecycle);
    schedulers.shutdown().await;
}

#[test]
fn dashboard_managed_summary_reader_matches_canonical_state() {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    let data_root = tempfile::tempdir().expect("temp data root");
    let state = PrAutotrackState {
        managed: BTreeMap::from([
            (
                pr_label(3),
                ManagedPr {
                    pr: 3,
                    head_branch: "feature-three".into(),
                    head_sha: String::new(),
                    worktree: PathBuf::from("/tmp/pr-3"),
                    tracking_ref: pr_tracking_ref(3),
                },
            ),
            (
                pr_label(1),
                ManagedPr {
                    pr: 1,
                    head_branch: "feature-one".into(),
                    head_sha: String::new(),
                    worktree: PathBuf::from("/tmp/pr-1"),
                    tracking_ref: pr_tracking_ref(1),
                },
            ),
        ]),
    };
    save_state(data_root.path(), &state).expect("write pr-autotrack state");

    let canonical = managed_summary(data_root.path()).expect("read canonical managed summary");
    let reader: tracedecay_dashboard_api::PrAutoTrackManagedSummaryReader =
        Arc::new(|store_root| {
            managed_summary(&store_root).map(|entries| {
                entries
                    .into_iter()
                    .map(
                        |entry| tracedecay_dashboard_api::PrAutoTrackManagedSummaryEntryV1 {
                            branch: entry.branch,
                            pr: entry.pr,
                            head_branch: entry.head_branch,
                        },
                    )
                    .collect()
            })
        });
    let projected = reader(data_root.path().to_path_buf()).expect("read projected managed summary");

    assert_eq!(projected.len(), canonical.len());
    for (entry, summary) in projected.iter().zip(canonical.iter()) {
        assert_eq!(entry.branch, summary.branch);
        assert_eq!(entry.pr, summary.pr);
        assert_eq!(entry.head_branch, summary.head_branch);
    }
    assert_eq!(projected[0].pr, 1);
    assert_eq!(projected[1].pr, 3);
}

#[test]
fn dashboard_managed_summary_reader_is_empty_without_state() {
    use std::sync::Arc;

    let data_root = tempfile::tempdir().expect("temp data root");
    let reader: tracedecay_dashboard_api::PrAutoTrackManagedSummaryReader =
        Arc::new(|store_root| {
            managed_summary(&store_root).map(|entries| {
                entries
                    .into_iter()
                    .map(
                        |entry| tracedecay_dashboard_api::PrAutoTrackManagedSummaryEntryV1 {
                            branch: entry.branch,
                            pr: entry.pr,
                            head_branch: entry.head_branch,
                        },
                    )
                    .collect()
            })
        });
    assert!(
        reader(data_root.path().to_path_buf())
            .expect("read empty managed summary")
            .is_empty()
    );
}
