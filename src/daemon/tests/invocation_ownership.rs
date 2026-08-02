#![cfg(unix)]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tracedecay_application::{
    ApplicationProblemKind, CancellationContext, Deadline, WorkProjectionSnapshotRequestV1,
};
use tracedecay_domain::UtcMicros;

use super::{
    enter_test_daemon_database_scope, initialize_test_project, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};
use crate::application::primitives::{Pr12PrimitiveRequest, StorageStatusPrimitiveRequest};
use crate::application_surface::{
    ApplicationSurfaceOperation, ConfigurationListSurfaceRequest, ConfigurationSurfaceRequest,
};
use crate::daemon::service::invocation::DaemonInvocationProblem;
use crate::daemon::{
    DaemonEngine, DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationRequest,
    execute_daemon_invocation,
};
use crate::daemon_contract::WorkApplicationInvocationV1;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("run Git fixture command");
    assert!(status.success(), "git {args:?}");
}

async fn committed_fixture(
    label: &str,
) -> (
    TempDir,
    crate::db::DaemonDatabaseScope,
    DaemonEngine,
    DaemonHandshake,
) {
    let temp = TempDir::new().expect("committed invocation fixture");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("committed invocation project");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n")
        .expect("committed invocation source");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    git(&project, &["init", "--quiet"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    git(&project, &["add", "."]);
    git(&project, &["commit", "--quiet", "-m", "base"]);
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        ..test_handshake_defaults()
    };
    let database_scope = enter_test_daemon_database_scope(&profile_root, label);
    let engine = test_daemon_engine_for_profile(&profile_root);
    engine
        .project_server(&handshake)
        .await
        .expect("committed invocation project open");
    (temp, database_scope, engine, handshake)
}

async fn assert_mounted_invocations(engine: &DaemonEngine, handshake: &DaemonHandshake) {
    let observed_at = tracedecay_application::clock::now_micros();
    let deadline = Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
        .expect("daemon invocation deadline");
    let cancellation = CancellationContext::active("cancel.project-open-invocations")
        .expect("daemon invocation cancellation");
    let configuration = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::configuration(
            "request.project-open.configuration",
            ApplicationSurfaceOperation::ConfigurationList,
            ConfigurationSurfaceRequest::List(ConfigurationListSurfaceRequest::default()),
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        ),
    )
    .await;
    assert!(
        matches!(
            configuration.outcome,
            DaemonInvocationOutcome::Configuration { .. }
        ),
        "project-open must route configuration through the mounted daemon owner: {configuration:?}"
    );

    let primitive = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::primitive(
            "request.project-open.primitive",
            ApplicationSurfaceOperation::StorageStatus,
            Pr12PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                include_details: false,
            }),
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        ),
    )
    .await;
    assert!(
        matches!(primitive.outcome, DaemonInvocationOutcome::Primitive { .. }),
        "project-open must route primitives through the mounted daemon owner: {primitive:?}"
    );

    let work = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::work_application(
            "request.project-open.work",
            WorkApplicationInvocationV1::Snapshot(WorkProjectionSnapshotRequestV1 {
                page_size: 100,
            }),
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        ),
    )
    .await;
    assert!(
        matches!(
            work.outcome,
            DaemonInvocationOutcome::WorkApplication { .. }
        ),
        "project-open must route Work through the mounted daemon owner: {work:?}"
    );

    let feedback = execute_daemon_invocation(
        engine,
        handshake,
        DaemonInvocationRequest::feedback(
            "request.project-open.feedback",
            ApplicationSurfaceOperation::FeedbackList,
            "feedback-handle.unknown".to_owned(),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(
        matches!(
            feedback.outcome,
            DaemonInvocationOutcome::ApplicationProblem { ref problem }
                if problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized
        ),
        "mounted feedback must conceal an unknown handle instead of reporting owner unavailable: {feedback:?}"
    );
}

#[tokio::test]
async fn committed_project_invocation_routes_mounted_operations() {
    let (_temp, _database_scope, engine, handshake) =
        committed_fixture("committed-project-invocation-owners").await;
    assert_mounted_invocations(&engine, &handshake).await;
    engine.shutdown_all().await;
}

#[tokio::test]
async fn unregistered_project_invocation_reports_truthful_unavailable() {
    let temp = TempDir::new().expect("unregistered project fixture");
    let profile_root = temp.path().join("profile");
    let client_identity = test_client_identity_for(profile_root.clone());
    let handshake = DaemonHandshake {
        project_path: Some(temp.path().join("missing-project")),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "unregistered-project-invocation");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let observed_at = tracedecay_application::clock::now_micros();
    let deadline = Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
        .expect("daemon invocation deadline");
    let cancellation = CancellationContext::active("cancel.unregistered-project-invocation")
        .expect("daemon invocation cancellation");
    let response = execute_daemon_invocation(
        &engine,
        &handshake,
        DaemonInvocationRequest::configuration(
            "request.unregistered-project.configuration",
            ApplicationSurfaceOperation::ConfigurationList,
            ConfigurationSurfaceRequest::List(ConfigurationListSurfaceRequest::default()),
            observed_at,
            deadline,
            cancellation,
        ),
    )
    .await;
    assert!(
        matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ),
        "an unregistered project must remain truthfully unavailable: {response:?}"
    );
    engine.shutdown_all().await;
}
