#[cfg(unix)]
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::test_support::git::git;
#[cfg(unix)]
use tracedecay_contracts::code_index_freshness::{
    CodeIndexFreshnessReadFailureV1, CodeIndexReadinessTargetV1, CodeIndexReadinessWaitReadV1,
};

/// Runs one Git command and reports only whether it succeeded. Used for
/// options whose availability depends on the installed Git version.
fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success())
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
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
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

    let graph = tracedecay_project::project::TraceDecay::open_with_options_for_test(
        repo.path(),
        tracedecay_project::project::TraceDecayOpenOptions::default(),
    )
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
        tracedecay_project::project::TraceDecay::open_read_only_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
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
        .mount_worktree(project_id.clone(), &linked, code_index_store.clone())
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
    activate_linked_worktree(None, &schedulers, &graph, &linked)
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

    let graph = Arc::new(
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
        .await
        .unwrap(),
    );
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
    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/advance");
    let hosted = HostedManualBranchFixture::open(repo.path()).await;
    let graph = Arc::clone(&hosted.graph);
    let schedulers = hosted.engine.invocation.code_index_schedulers.clone();
    let initial = hosted.activate(&schedulers, "feature/advance").await;

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
    let advanced_head = git(repo.path(), &["rev-parse", "HEAD"]);
    git(repo.path(), &["checkout", "-q", "main"]);

    let staged_graph = Arc::clone(&graph);
    let staged_schedulers = schedulers.clone();
    let staged_repo = repo.path().to_path_buf();
    let staged_stores = hosted.engine.store_administration.clone();
    let (staged_sender, staged_receiver) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        let lifecycle = acquire_manual_branch_lifecycle(
            &staged_graph.store_layout().data_root,
            "feature/advance",
        )
        .await
        .unwrap();
        let staged = activate_manual_branch_head_with_lifecycle(
            &staged_stores,
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
        git(&initial.worktree, &["rev-parse", "HEAD"]),
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
        hosted.engine.store_administration.clone(),
        acquire_manual_branch_lifecycle(&data_root, "feature/advance")
            .await
            .unwrap(),
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
        hosted.engine.store_administration.clone(),
        acquire_manual_branch_lifecycle(&data_root, "feature/advance")
            .await
            .unwrap(),
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
    let mounted_head = git(&replay.worktree, &["rev-parse", "HEAD"]);

    assert_eq!(
        replay.outcome,
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    assert_ne!(initial.head_sha, replay.head_sha);
    assert_eq!(
        advanced_head, mounted_head,
        "the new candidate must carry the newly resolved branch head"
    );
    schedulers.shutdown().await;
    let shutdown = hosted.engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_activation_queues_behind_the_exact_lifecycle_before_mutating_git() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/contended");
    let graph = Arc::new(
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
        .await
        .unwrap(),
    );
    let lifecycle =
        acquire_manual_branch_lifecycle(&graph.store_layout().data_root, "feature/contended")
            .await
            .expect("first lifecycle owner");
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let tracking_refs = || {
        git(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tracedecay/branch",
            ],
        )
        .trim()
        .to_owned()
    };

    let activation =
        activate_manual_branch_head(repo.path(), &graph, Some(&schedulers), "feature/contended");
    tokio::pin!(activation);
    let queued = tokio::time::timeout(Duration::from_millis(200), &mut activation)
        .await
        .is_err();
    let refs_while_queued = tracking_refs();
    drop(lifecycle);
    let activated = activation.await.map(|activation| activation.branch);

    assert!(
        queued,
        "same-branch activation must wait for the lifecycle owner"
    );
    assert_eq!(refs_while_queued, "");
    assert_eq!(activated, Ok("feature/contended".to_owned()));
    assert_ne!(tracking_refs(), "");
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_manual_branch_sealing_retires_the_exact_mount_worktree_and_tracking_ref() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/failure-cleanup");
    let graph = Arc::new(
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
        .await
        .unwrap(),
    );
    let data_root = graph.store_layout().data_root.clone();
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let lifecycle = acquire_manual_branch_lifecycle(&data_root, "feature/failure-cleanup")
        .await
        .expect("lifecycle owner");
    let activation = unhosted_manual_activation_with_lifecycle(
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
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
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
    assert_eq!(
        git(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tracedecay/branch"
            ]
        )
        .trim()
        .len(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_branch_missing_ref_is_typed_failure() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature-present");

    let graph = Arc::new(
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
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
    assert_eq!(
        git(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tracedecay/branch"
            ]
        )
        .trim()
        .len(),
        0
    );
    schedulers.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn manual_branch_head_git_failure_is_retryable_not_missing_ref() {
    use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

    let repo = tempfile::tempdir().unwrap();
    let branch = "feature-present";
    init_manual_branch_repo(repo.path(), branch);
    let graph = Arc::new(
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
        .await
        .expect("open project graph"),
    );
    let data_root = graph.store_layout().data_root.clone();
    let schedulers = CodeIndexSchedulerRegistryV1::new(2);
    let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
    cancellation.cancel();
    let failing_controls = [
        ("deadline", PrCommandControl::with_timeout(Duration::ZERO)),
        (
            "cancelled",
            PrCommandControl::with_cancellation(cancellation),
        ),
    ];

    for (label, control) in failing_controls {
        let lifecycle = acquire_manual_branch_lifecycle(&data_root, branch)
            .await
            .expect("lifecycle owner");
        let error = unhosted_manual_activation_with_lifecycle(
            repo.path(),
            &graph,
            Some(&schedulers),
            branch,
            &lifecycle,
            &control,
        )
        .await
        .expect_err("failed Git head resolution must not activate the branch");

        assert!(
            matches!(
                &error,
                ManualBranchActivationError::GitAuthorityUnavailable { .. }
            ),
            "{label}: {error}"
        );
        assert_eq!(error.reason_code(), "git_authority_unavailable", "{label}");
        assert!(
            error.retryable(),
            "{label}: a transient Git failure must stay retryable"
        );
        assert!(
            !data_root.join("branch-worktrees").exists(),
            "{label}: no artifacts are staged without a resolved head"
        );
        drop(lifecycle);
    }

    let lifecycle = acquire_manual_branch_lifecycle(&data_root, branch)
        .await
        .expect("lifecycle owner");
    let activation = unhosted_manual_activation_with_lifecycle(
        repo.path(),
        &graph,
        Some(&schedulers),
        branch,
        &lifecycle,
        default_pr_command_control(),
    )
    .await
    .expect("the same branch activates once Git is available");
    cleanup_manual_branch_activation(
        repo.path(),
        &data_root,
        &schedulers,
        &activation,
        &lifecycle,
    )
    .await
    .expect("retire activated branch");
    drop(lifecycle);
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
        tracedecay_project::project::TraceDecay::open_with_options_for_test(
            repo.path(),
            tracedecay_project::project::TraceDecayOpenOptions::default(),
        )
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
        let reported = std::path::PathBuf::from(git(
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

    let writer_path = ref_path.clone();
    let fifo_writer_task = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open FIFO writer once Git begins its exact ref read")
    });

    let (owner_done_tx, mut owner_done_rx) = tokio::sync::oneshot::channel();
    let owner_cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
    let owner_control = PrCommandControl::with_cancellation(owner_cancellation.clone());
    let owner_repo = repo.path().to_path_buf();
    let owner_data_root = data_root.clone();
    let owner_graph = Arc::clone(&graph);
    let owner_schedulers = schedulers.clone();
    let owner_branch = branch.to_owned();
    let requester = tokio::spawn(async move {
        let owner = tokio::spawn(async move {
            let lifecycle = acquire_manual_branch_lifecycle(&owner_data_root, &owner_branch)
                .await
                .expect("activation owner acquires the exact lifecycle");
            let outcome = activate_manual_branch_with_administration(
                &owner_repo,
                &owner_data_root,
                &owner_branch,
                PrStoreAdministration::unhosted_with_control(
                    &owner_schedulers,
                    &owner_graph,
                    &owner_control,
                ),
                &lifecycle,
            )
            .await;
            let _ = owner_done_tx.send(outcome);
        });
        let _ = owner.await;
    });

    // Git opening the FIFO is the stall itself; the test task observing it
    // also proves the current-thread runtime keeps scheduling during the read.
    let fifo_writer = tokio::select! {
        biased;
        writer = fifo_writer_task => writer.expect("FIFO writer task joins"),
        outcome = &mut owner_done_rx => {
            // The read end releases the blocked writer so the runtime can shut down.
            drop(std::fs::File::open(&ref_path));
            panic!("activation finished before Git reached the stalled exact-ref read: {outcome:?}");
        }
    };

    requester.abort();
    assert!(
        requester.await.is_err(),
        "the requester is cancelled while its lifecycle owner continues"
    );
    assert!(
        matches!(
            owner_done_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "the lifecycle owner stays in its stalled exact read after the requester is cancelled"
    );
    owner_cancellation.cancel();
    let error = owner_done_rx
        .await
        .expect("lifecycle owner reports its terminal activation outcome")
        .expect_err("a cancelled exact read cannot be treated as a missing ref");
    assert!(
        error
            .detail()
            .contains(&tracedecay_runtime_core::git::GitCommandError::Cancelled.to_string()),
        "the owner's cancellation, not its Git deadline, ends the stalled read: {error}"
    );
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
    let lifecycle = acquire_manual_branch_lifecycle(&data_root, branch)
        .await
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


async fn unhosted_manual_activation_with_lifecycle(
    repo_root: &Path,
    graph: &Arc<tracedecay_project::project::TraceDecay>,
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    branch: &str,
    lifecycle: &ManualBranchLifecycleLeaseV1,
    command_control: &PrCommandControl,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    let administration = match schedulers {
        Some(schedulers) => {
            PrStoreAdministration::unhosted_with_control(schedulers, graph, command_control)
        }
        None => PrStoreAdministration {
            schedulers: None,
            graph: Some(graph),
            command_control,
            stores: None,
        },
    };
    activate_manual_branch_with_administration(
        repo_root,
        &graph.store_layout().data_root,
        branch,
        administration,
        lifecycle,
    )
    .await
}

#[cfg(unix)]
struct HostedManualBranchFixture {
    engine: crate::daemon::DaemonEngine,
    graph: Arc<tracedecay_project::project::TraceDecay>,
    _profile: tempfile::TempDir,
    _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pr_first_source_reader_recovers_through_the_canonical_broker_slot() {
    let repo = tempfile::tempdir().unwrap();
    init_manual_branch_repo(repo.path(), "feature/recovery");
    let hosted = HostedManualBranchFixture::open(repo.path()).await;
    let stores = &hosted.engine.store_administration;
    let graph = &hosted.graph;
    let schedulers = &hosted.engine.invocation.code_index_schedulers;
    let database_path = &graph.store_layout().sessions_db_path;
    let spool = database_path.parent().unwrap().join(format!(
        ".{}.host-admission",
        database_path.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::write(&spool, "blocked").unwrap();
    let mounted = hosted.activate(schedulers, "feature/recovery").await;
    assert!(schedulers.is_worktree_mounted(&mounted.worktree).await);
    assert!(matches!(
        schedulers.dashboard_freshness_read(&mounted.worktree).await,
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    ));
    let database = stores
        .registered_project_configuration_database(graph.project_root(), graph.store_layout())
        .await
        .unwrap();
    let slot = stores
        .host_admission_source_broker(&database)
        .await
        .unwrap();
    assert!(slot.get().is_none());
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .as_ref()
        .unwrap()
        .clone()
        .try_into()
        .unwrap();
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&mounted.worktree, &project_id)
            .unwrap();
    // Independent Core/PR factories borrow the same canonical slot. Existing
    // source-fence copies must recover without replacing the mounted reader.
    let first = super::super::project_composition::project_hook_source_pending(
        stores,
        graph,
        &mounted.worktree,
        &scope,
    )
    .await
    .unwrap();
    let second = super::super::project_composition::project_hook_source_pending(
        stores,
        graph,
        &mounted.worktree,
        &scope,
    )
    .await
    .unwrap();
    assert!(Arc::ptr_eq(
        &slot,
        &stores
            .host_admission_source_broker(&database)
            .await
            .unwrap()
    ));
    assert_eq!(
        first.read_pending(),
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    );
    assert_eq!(
        first.read_pending_blocking(),
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    );
    assert_eq!(
        second.read_pending(),
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    );
    let observed = first.revision();
    let changed_first = first.changed(observed);
    let changed_second = second.changed(observed);
    tokio::pin!(changed_first, changed_second);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            changed_first.as_mut().poll(cx).is_pending()
        ))
        .await
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            changed_second.as_mut().poll(cx).is_pending()
        ))
        .await
    );
    std::fs::remove_file(&spool).unwrap();
    let broker = stores.host_admission_broker(&database).await.unwrap();
    assert!(Arc::ptr_eq(slot.get().unwrap(), &broker));
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(changed_first, changed_second);
    })
    .await
    .unwrap();
    assert_eq!(first.read_pending(), Ok(false));
    assert_eq!(second.read_pending(), Ok(false));
    assert_ne!(first.revision(), observed);
    activate_linked_worktree(Some(stores), schedulers, graph, &mounted.worktree)
        .await
        .unwrap();
    let reading = schedulers
        .wait_for_readiness(
            &mounted.worktree,
            CodeIndexReadinessTargetV1::Ready,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(
        matches!(reading, CodeIndexReadinessWaitReadV1::Reached { .. }),
        "{reading:?}"
    );
    schedulers.shutdown().await;
    assert!(
        hosted
            .engine
            .shutdown_all()
            .await
            .project_servers
            .is_clean()
    );
}

#[cfg(unix)]
impl HostedManualBranchFixture {
    async fn open(repo: &Path) -> Self {
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        let temporary = tempfile::tempdir().unwrap();
        let profile =
            tracedecay_runtime_core::config::ProfileRoot::new(temporary.path().join("profile"));
        let identity =
            tracedecay_daemon_identity::profile_identity::load_or_create(profile.data_dir())
                .unwrap();
        let handshake = crate::daemon::handshake_for_current_client(
            &profile,
            Some(repo.to_path_buf()),
            None,
            false,
            true,
        )
        .unwrap();
        crate::daemon::tests::initialize_test_project(repo, &handshake.client_identity).await;
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile.data_dir(),
            1,
            "manual-branch-fixture",
        )
        .unwrap();
        let engine = crate::daemon::DaemonEngine::default()
            .with_owner_profile(profile.clone())
            .with_profile_identity(identity);
        engine
            .invocation
            .install_worker_selection(
                &engine.store_administration,
                tracedecay_domain::configuration::CodeIndexWorkerSelectionV1::default(),
            )
            .unwrap();
        let graph = Arc::new(
            Box::pin(crate::daemon::open_project_for_handshake(
                repo,
                &handshake,
                &engine.store_administration,
                &tracedecay_runtime_core::cancellation::CancellationToken::new(),
            ))
            .await
            .unwrap(),
        );
        Self {
            engine,
            graph,
            _profile: temporary,
            _scope: scope,
        }
    }

    async fn activate(
        &self,
        schedulers: &CodeIndexSchedulerRegistryV1,
        branch: &str,
    ) -> ManualBranchActivation {
        let lifecycle =
            acquire_manual_branch_lifecycle(&self.graph.store_layout().data_root, branch)
                .await
                .unwrap();
        activate_manual_branch_head_with_lifecycle(
            &self.engine.store_administration,
            self.graph.project_root(),
            &self.graph,
            Some(schedulers),
            branch,
            &lifecycle,
            default_pr_command_control(),
        )
        .await
        .unwrap()
    }
}
