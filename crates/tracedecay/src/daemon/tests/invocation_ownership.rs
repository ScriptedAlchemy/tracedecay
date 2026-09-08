#![cfg(unix)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;

use tempfile::TempDir;
use tracedecay_contracts::retained_surfaces::{MemoryStatusRequestV1, RetainedSurfaceRequestV1};
use tracedecay_contracts::{
    ApplicationProblemKind, CancellationContext, Deadline, ProblemTerminality, RetryDirective,
    WorkGraphReadRequestV1, WorkProductSelectionScopeV1,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::{
    enter_test_daemon_database_scope, initialize_test_project, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};
use crate::daemon::{
    DaemonEngine, DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationRequest,
    execute_daemon_invocation,
};
use tracedecay_application::primitives::StorageStatusPrimitiveRequest;
use tracedecay_contracts::retrieval::PrimitiveRequest;
use tracedecay_contracts::{ConfigurationListRequestV1, ConfigurationWireRequestV1};
use tracedecay_daemon_protocol::WorkApplicationInvocationV1;
use tracedecay_daemon_service::{
    DaemonInvocationProblem, ProjectRuntimePublicationStateV1, RegisteredRetainedRuntime,
};

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
    tracedecay_runtime_core::db::DaemonDatabaseScope,
    DaemonEngine,
    DaemonHandshake,
) {
    let (temp, database_scope, engine, handshake) = unopened_committed_fixture(label).await;
    engine
        .project_server(&handshake)
        .await
        .expect("committed invocation project open");
    (temp, database_scope, engine, handshake)
}

/// [`committed_fixture`] before its project open, for journeys that must
/// observe the open in flight.
async fn unopened_committed_fixture(
    label: &str,
) -> (
    TempDir,
    tracedecay_runtime_core::db::DaemonDatabaseScope,
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
    let project_alias = temp.path().join("project-alias");
    std::os::unix::fs::symlink(&project, &project_alias).expect("committed project alias");
    let handshake = DaemonHandshake {
        project_path: Some(project_alias),
        client_identity,
        ..test_handshake_defaults()
    };
    let database_scope = enter_test_daemon_database_scope(&profile_root, label);
    let engine = test_daemon_engine_for_profile(&profile_root);
    (temp, database_scope, engine, handshake)
}

// Each surface check constructs its future inside its own function and returns
// it boxed: the daemon invocation state machines are large in debug builds, and
// materializing all four inline in one async fn produced a ~663 KB resident
// poll frame that, stacked on the production dispatch frames, overflowed the
// default 2 MiB test-thread stack. Boxing inside the constructor pops the
// construction temporaries before the deep poll chain runs.
fn assert_configuration_routes_mounted<'a>(
    engine: &'a DaemonEngine,
    handshake: &'a DaemonHandshake,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let configuration = execute_daemon_invocation(
            engine,
            handshake,
            DaemonInvocationRequest::configuration(
                "request.project-open.configuration",
                ApplicationSurfaceOperation::ConfigurationList,
                ConfigurationWireRequestV1::List(ConfigurationListRequestV1::default()),
                observed_at,
                deadline,
                cancellation,
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
    })
}

fn assert_primitive_routes_mounted<'a>(
    engine: &'a DaemonEngine,
    handshake: &'a DaemonHandshake,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let primitive = execute_daemon_invocation(
            engine,
            handshake,
            DaemonInvocationRequest::primitive(
                "request.project-open.primitive",
                ApplicationSurfaceOperation::StorageStatus,
                PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                    include_details: false,
                }),
                observed_at,
                deadline,
                cancellation,
            ),
        )
        .await;
        assert!(
            matches!(primitive.outcome, DaemonInvocationOutcome::Primitive { .. }),
            "project-open must route primitives through the mounted daemon owner: {primitive:?}"
        );
    })
}

fn assert_work_routes_mounted<'a>(
    engine: &'a DaemonEngine,
    handshake: &'a DaemonHandshake,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let work = execute_daemon_invocation(
            engine,
            handshake,
            DaemonInvocationRequest::work_application(
                "request.project-open.work",
                WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
                    WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                    observed_at,
                )),
                observed_at,
                deadline,
                cancellation,
            ),
        )
        .await;
        // A freshly opened project has published no Work graph version, and a
        // verified version identity requires a real event sequence, so there
        // is no representable empty current graph to answer with: the mounted
        // owner answers the authorized absence as a concealed
        // not-found-or-not-authorized. That still proves routing — an
        // unmounted owner never reaches the Work application and answers
        // `Problem::Unavailable` instead, exactly as the unregistered-project
        // test below asserts.
        let routed_to_mounted_owner = match &work.outcome {
            DaemonInvocationOutcome::WorkApplication { .. } => true,
            DaemonInvocationOutcome::ApplicationProblem { problem } => {
                problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized
            }
            _ => false,
        };
        assert!(
            routed_to_mounted_owner,
            "project-open must route Work through the mounted daemon owner: {work:?}"
        );
    })
}

fn assert_feedback_conceals_unknown_handle<'a>(
    engine: &'a DaemonEngine,
    handshake: &'a DaemonHandshake,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
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
    })
}

async fn assert_mounted_invocations(engine: &DaemonEngine, handshake: &DaemonHandshake) {
    let observed_at = tracedecay_contracts::clock::now_micros();
    let deadline = Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
        .expect("daemon invocation deadline");
    let cancellation = CancellationContext::active("cancel.project-open-invocations")
        .expect("daemon invocation cancellation");
    assert_configuration_routes_mounted(
        engine,
        handshake,
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    )
    .await;
    assert_primitive_routes_mounted(
        engine,
        handshake,
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    )
    .await;
    assert_work_routes_mounted(
        engine,
        handshake,
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    )
    .await;
    assert_feedback_conceals_unknown_handle(engine, handshake, observed_at, deadline, cancellation)
        .await;
}

#[tokio::test]
async fn committed_project_alias_invokes_mounted_primitive_and_shuts_down_cleanly() {
    let (_temp, _database_scope, engine, handshake) =
        committed_fixture("committed-project-invocation-owners").await;
    assert_mounted_invocations(&engine, &handshake).await;
    let project_alias = handshake.project_path.as_deref().expect("project alias");
    assert_eq!(
        engine
            .invocation
            .service
            .project_runtimes
            .publication_state(project_alias),
        Some(ProjectRuntimePublicationStateV1::Ready),
        "a callable owner reached through the alias must be fully published"
    );
    let shutdown = engine.shutdown_all().await;
    assert!(
        shutdown.project_servers.is_clean(),
        "the alias-mounted callable owner must shut down cleanly: {shutdown:?}"
    );
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
    let observed_at = tracedecay_contracts::clock::now_micros();
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
            ConfigurationWireRequestV1::List(ConfigurationListRequestV1::default()),
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

fn memory_status_request(request_id: &str) -> DaemonInvocationRequest {
    let observed_at = tracedecay_contracts::clock::now_micros();
    DaemonInvocationRequest::retained_application(
        request_id,
        RetainedSurfaceRequestV1::MemoryStatus(MemoryStatusRequestV1 {
            memory_scope: None,
            project_selector: None,
        }),
        observed_at,
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
            .expect("daemon invocation deadline"),
        CancellationContext::active(format!("cancel.{request_id}"))
            .expect("daemon invocation cancellation"),
    )
}

/// The core route is admitted before the full server's owner phase registers
/// the retained runtime. Hold that phase open — the configuration registration
/// pause sits in the same owner phase, just ahead of the retained registration
/// — and prove that a retained request landing in the window reads as the
/// owner still mounting, retryable, and never as a scope that has no retained
/// runtime. Once the phase completes the same request is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retained_invocation_while_owners_mount_is_retryable_not_unmounted() {
    let (_temp, _database_scope, engine, handshake) =
        unopened_committed_fixture("retained-invocation-while-owners-mount").await;
    let canonical_project = handshake
        .project_path
        .as_deref()
        .expect("project alias")
        .canonicalize()
        .expect("canonical project root");
    let registration = engine
        .invocation
        .service
        .pause_configuration_runtime_registration(canonical_project.clone())
        .await;
    let opening_engine = engine.clone();
    let opening_handshake = handshake.clone();
    let opening =
        tokio::spawn(async move { opening_engine.project_server(&opening_handshake).await });
    tokio::time::timeout(
        std::time::Duration::from_mins(1),
        registration.before_registration,
    )
    .await
    .expect("project open must reach the owner registration gate")
    .expect("owner registration gate sender");
    assert_eq!(
        engine
            .invocation
            .service
            .project_runtimes
            .publication_state(&canonical_project),
        Some(ProjectRuntimePublicationStateV1::Warming),
        "the admitted route must still be publishing its owners"
    );
    assert!(
        !engine
            .invocation
            .service
            .project_runtimes
            .holds::<RegisteredRetainedRuntime>(&canonical_project)
            .await,
        "the gate must hold the open ahead of the retained registration"
    );

    let mounting = execute_daemon_invocation(
        &engine,
        &handshake,
        memory_status_request("request.retained.while-mounting"),
    )
    .await;
    let DaemonInvocationOutcome::ApplicationProblem { problem } = &mounting.outcome else {
        panic!(
            "a retained request during owner mounting must answer a typed problem: {mounting:?}"
        );
    };
    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
    assert_eq!(problem.terminality(), ProblemTerminality::PreAdmission);
    assert_eq!(problem.retry(), RetryDirective::AfterDelay);
    let diagnostic = problem
        .diagnostic()
        .expect("a mounting retained owner carries a diagnostic");
    assert_eq!(diagnostic.code, "application.surface.unavailable");
    assert!(
        !diagnostic
            .message
            .contains("no retained runtime is registered"),
        "a mounting route must not claim its scope has no retained runtime: {}",
        diagnostic.message
    );

    registration
        .allow_registration
        .send(())
        .expect("release owner registration");
    tokio::time::timeout(
        std::time::Duration::from_mins(1),
        registration.after_registration,
    )
    .await
    .expect("owner registration must publish")
    .expect("owner registration publication sender");
    registration
        .allow_return
        .send(())
        .expect("release project-open owner setup");
    opening
        .await
        .expect("project-open task")
        .expect("project opens once its owners are registered");

    let served = execute_daemon_invocation(
        &engine,
        &handshake,
        memory_status_request("request.retained.after-mounting"),
    )
    .await;
    assert!(
        matches!(
            served.outcome,
            DaemonInvocationOutcome::RetainedApplication { .. }
        ),
        "the same retained request must be answered once the owner phase completes: {served:?}"
    );
    let shutdown = engine.shutdown_all().await;
    assert!(
        shutdown.project_servers.is_clean(),
        "the retained owner must shut down cleanly: {shutdown:?}"
    );
}
