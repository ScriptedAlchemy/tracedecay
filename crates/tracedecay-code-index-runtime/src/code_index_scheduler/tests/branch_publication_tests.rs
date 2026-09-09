#[cfg(unix)]
use std::os::unix::fs::symlink;

use tempfile::TempDir;
use tracedecay_dashboard_api::code_index_freshness_api::{
    CodeGraphServingReadinessV1, CodeIndexWorktreeFreshnessV1,
};

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
