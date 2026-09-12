#[cfg(unix)]
use std::os::unix::fs::symlink;

use tempfile::TempDir;
use tracedecay_contracts::code_index_freshness::{
    CodeGraphServingReadinessV1, CodeIndexWorktreeFreshnessV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

use super::super::branch_publication::{
    BranchPublicationContextV1, branch_generation_work_is_active,
};
use super::{ALPHA_LIB_V1, CodeIndexSchedulerRegistryV1, GitFixture, test_project_id};

async fn mounted_registry(fixture: &GitFixture, store: &TempDir) -> CodeIndexSchedulerRegistryV1 {
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    super::wait_for_initial_generation(&registry, fixture.path()).await;
    super::wait_for_dashboard_ready(&registry, fixture.path()).await;
    registry
}

#[test]
fn branch_publication_requires_authoritative_project_identity() {
    let project = TempDir::new().expect("project root");
    let store = TempDir::new().expect("store root");

    let error = BranchPublicationContextV1::new(None, project.path(), store.path())
        .expect_err("missing project identity must fail closed");

    assert_eq!(
        error.project_route_context(),
        Some((
            "code_index_scheduler_identity_mismatch",
            false,
            "branch graph publication requires an authoritative project identity",
        ))
    );
}

#[test]
fn pending_graph_activation_keeps_exact_branch_wait_live() {
    let pending = CodeIndexWorktreeFreshnessV1 {
        rebuild_in_flight: false,
        code_graph_serving: Some(CodeGraphServingReadinessV1::Pending),
        ..CodeIndexWorktreeFreshnessV1::default()
    };
    assert!(branch_generation_work_is_active(&pending));

    let terminal = CodeIndexWorktreeFreshnessV1 {
        rebuild_in_flight: false,
        code_graph_serving: Some(CodeGraphServingReadinessV1::Refused {
            reason: "fixture refusal".to_owned(),
        }),
        ..CodeIndexWorktreeFreshnessV1::default()
    };
    assert!(!branch_generation_work_is_active(&terminal));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_branch_source_uses_the_mounted_git_identity() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let project_id = test_project_id();
    let registry = mounted_registry(&fixture, &store).await;
    let context =
        BranchPublicationContextV1::new(Some(project_id.as_str()), fixture.path(), store.path())
            .expect("branch publication context");

    let source = context
        .capture_exact_branch_source(&registry, fixture.path(), fixture.path(), "main")
        .await
        .expect("capture exact branch source");

    assert_eq!(source.project_id, project_id.as_str());
    assert_eq!(source.reference, "refs/heads/main");
    assert_eq!(
        source.source_oid,
        super::git_stdout(fixture.path(), &["rev-parse", "HEAD"])
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_generation_wait_rolls_back_prepared_branch_metadata() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let project_id = test_project_id();
    let registry = mounted_registry(&fixture, &store).await;
    std::fs::write(
        fixture.path().join("src/lib.rs"),
        b"pub fn changed_after_mount() {}\n",
    )
    .expect("advance branch source");
    super::git(fixture.path(), &["add", "src/lib.rs"]);
    super::git(fixture.path(), &["commit", "-qm", "advance branch"]);

    let context =
        BranchPublicationContextV1::new(Some(project_id.as_str()), fixture.path(), store.path())
            .expect("branch publication context");
    let cancellation = CancellationToken::new();
    let publication_cancellation = cancellation.clone();
    let publication_registry = registry.clone();
    let project_root = fixture.path().to_path_buf();
    let worktree_root = project_root.clone();
    let publication = tokio::spawn(async move {
        context
            .track_exact_worktree_branch(
                &publication_registry,
                &project_root,
                &worktree_root,
                "cancelled-publication",
                &publication_cancellation,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if tracedecay_runtime_core::branch_meta::load_branch_meta(store.path())
                .is_some_and(|meta| meta.is_tracked("cancelled-publication"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("branch metadata preparation becomes visible");
    cancellation.cancel();

    let error = publication
        .await
        .expect("publication task joins")
        .expect_err("cancelled publication must fail");
    assert_eq!(
        error.project_route_context().map(|context| context.0),
        Some("branch_tracking_failed")
    );
    assert!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(store.path())
            .is_some_and(|meta| !meta.is_tracked("cancelled-publication")),
        "cancelled generation wait must roll back prepared metadata"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_serving_claim_does_not_erase_pending_branch_tracking() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let project_id = test_project_id();
    let registry = mounted_registry(&fixture, &store).await;
    let generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("ready serving generation");
    let super::super::ServingGenerationInstallationOutcomeV1::Installed(held) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("fixture must hold the serving claim")
    };
    let context =
        BranchPublicationContextV1::new(Some(project_id.as_str()), fixture.path(), store.path())
            .expect("branch publication context");
    let cancellation = CancellationToken::new();
    let publication_registry = registry.clone();
    let project_root = fixture.path().to_path_buf();
    let worktree_root = project_root.clone();
    let publication = tokio::spawn(async move {
        context
            .track_exact_worktree_branch(
                &publication_registry,
                &project_root,
                &worktree_root,
                "feature/replay",
                &cancellation,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if tracedecay_runtime_core::branch_meta::load_branch_meta(store.path())
                .is_some_and(|meta| meta.is_tracked("feature/replay"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending branch metadata becomes visible");

    drop(held);
    assert!(registry.notify_hook_overflow(fixture.path()).await);
    assert_eq!(
        publication
            .await
            .expect("publication task joins")
            .expect("released serving claim must publish"),
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    assert!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(store.path())
            .is_some_and(|meta| meta.is_query_eligible("feature/replay")),
        "a transient serving handoff must preserve and finish pending branch tracking"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_branch_publication_completes_from_a_retained_graph_head() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let project_id = test_project_id();
    let seeded = mounted_registry(&fixture, &store).await;
    seeded.shutdown().await;

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            project_id.clone(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("remount retained worktree");
    let context =
        BranchPublicationContextV1::new(Some(project_id.as_str()), fixture.path(), store.path())
            .expect("branch publication context");
    let cancellation = CancellationToken::new();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        context.track_exact_worktree_branch(
            &registry,
            fixture.path(),
            fixture.path(),
            "retained-main",
            &cancellation,
        ),
    )
    .await
    .expect("retained exact branch publication must keep its queued continuation live")
    .expect("retained exact branch publication");

    assert_eq!(
        outcome,
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    assert!(
        tracedecay_runtime_core::branch_meta::load_branch_meta(store.path())
            .is_some_and(|meta| meta.is_query_eligible("retained-main")),
        "the exact retained generation must become query eligible"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_retained_project_root_is_a_typed_path_error() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = mounted_registry(&fixture, &store).await;
    let missing = store.path().join("missing-project");
    let context =
        BranchPublicationContextV1::new(Some(test_project_id().as_str()), &missing, store.path())
            .expect("branch publication context");

    let error = context
        .capture_exact_branch_source(&registry, fixture.path(), fixture.path(), "main")
        .await
        .expect_err("missing retained root must fail as a path error");

    assert!(matches!(
        error,
        tracedecay_domain::errors::TraceDecayError::File { path, .. }
            if path == missing.display().to_string()
    ));
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_project_root_is_denied_before_snapshot_capture() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let foreign = GitFixture::new(&[("src/lib.rs", "pub fn foreign() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = mounted_registry(&fixture, &store).await;
    let context = BranchPublicationContextV1::new(
        Some(test_project_id().as_str()),
        fixture.path(),
        store.path(),
    )
    .expect("branch publication context");

    let error = context
        .capture_exact_branch_source(&registry, foreign.path(), fixture.path(), "main")
        .await
        .expect_err("foreign project root must be denied");

    assert_eq!(
        error.project_route_context().map(|context| context.0),
        Some("code_index_scheduler_identity_mismatch")
    );
    registry.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_retained_project_root_is_a_typed_path_error() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = mounted_registry(&fixture, &store).await;
    let unreadable = store.path().join("project-loop");
    symlink("project-loop", &unreadable).expect("create unreadable project root");
    let context = BranchPublicationContextV1::new(
        Some(test_project_id().as_str()),
        &unreadable,
        store.path(),
    )
    .expect("branch publication context");

    let error = context
        .capture_exact_branch_source(&registry, fixture.path(), fixture.path(), "main")
        .await
        .expect_err("unreadable retained root must fail as a path error");

    assert!(matches!(
        error,
        tracedecay_domain::errors::TraceDecayError::File { path, .. }
            if path == unreadable.display().to_string()
    ));
    registry.shutdown().await;
}
