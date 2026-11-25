#![cfg(unix)]

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tracedecay_daemon_protocol::{
    DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationRequest,
};

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::retained_surfaces::{
    FactStoreCurateRequestV1, MemoryStatusRequestV1, RetainedSurfaceRequestV1,
};
use tracedecay_contracts::retrieval::RuntimeDoctorReportV1;
use tracedecay_contracts::{
    ApplicationProblem, ApplicationProblemKind, CancellationContext, Deadline, ProblemTerminality,
    RetryDirective, WorkGraphReadRequestV1, WorkProductSelectionScopeV1,
};
use tracedecay_domain::{ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::{
    enter_test_daemon_database_scope, initialize_test_project, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};
use crate::daemon::engine::DaemonEngine;
use crate::daemon::invocation_dispatch::execute_daemon_invocation;
use tracedecay_application::primitives::{PrimitiveProjectRuntime, StorageStatusPrimitiveRequest};
use tracedecay_contracts::retrieval::catalog::primitive_read_operation;
use tracedecay_contracts::retrieval::{PrimitiveInvocation, PrimitiveRequest};
use tracedecay_contracts::{
    CapabilityGrantId, CapabilityGrantSnapshot, DisclosureClass, RequestContext, RequestId,
    ResolvedScope,
};
use tracedecay_contracts::{ConfigurationListRequestV1, ConfigurationWireRequestV1};
use tracedecay_daemon_protocol::WorkApplicationInvocationV1;
use tracedecay_daemon_service::{
    DaemonInvocationProblem, ProjectRuntimePublicationStateV1, RegisteredRetainedRuntime,
};
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

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
    super::git(&project, &["init", "--quiet"]);
    super::git(&project, &["config", "user.name", "TraceDecay Test"]);
    super::git(
        &project,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    super::git(&project, &["add", "."]);
    super::git(&project, &["commit", "--quiet", "-m", "base"]);
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

#[tokio::test]
async fn doctor_probe_preserves_a_stalled_project_open_cause_and_retry_delay() {
    use crate::daemon::project_open_admission::{ProjectOpenTaskClaim, ProjectOpenTasks};
    use tracedecay_domain::errors::{ProjectOpenFailureKind, TraceDecayError};

    let (_temp, _database_scope, engine, handshake) =
        unopened_committed_fixture("doctor-stalled-project-open").await;
    super::prewarm_test_profile_runtime(&engine.store_administration).await;
    let (project_path, route) = engine
        .project_route(&handshake)
        .expect("Doctor project route");
    let tasks =
        crate::daemon::project_routing::project_open_tasks(&engine.project_open_gates).await;
    const CAUSE: &str = "session authority rejected mutable committed receipt history";
    let ProjectOpenTaskClaim::InFlight(state) = tasks.start(route.clone(), async {
        Err(TraceDecayError::project_open(
            ProjectOpenFailureKind::AuthorityVerdict {
                migration_pending: false,
            },
            CAUSE,
        ))
    }) else {
        panic!("fixture must own the first project open");
    };
    ProjectOpenTasks::wait_for_completion(state)
        .await
        .expect_err("record the rejected project open");
    let recorded_status = tasks.status(&route).expect("recorded project status");
    assert_eq!(
        recorded_status.state,
        tracedecay_contracts::project_open::ProjectOpenStatusStateV1::Stalled,
    );
    let delay_before_probe = recorded_status
        .retry_after_ms
        .expect("the stalled project records its retry delay");

    let (listener, endpoint) = tracedecay_daemon_protocol::BrokerListener::bind(
        &tracedecay_daemon_protocol::default_loopback_endpoint(),
    )
    .await
    .expect("Doctor daemon endpoint");
    let serving_engine = engine.clone();
    let serving = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept Doctor probe");
        Box::pin(
            crate::daemon::connection_serving::serve_authenticated_socket_client_with_class(
                stream,
                serving_engine,
                super::TEST_AUTH_TOKEN.to_owned(),
                crate::daemon::core_admission::DaemonClientAdmissionClass::General,
            ),
        )
        .await
    });
    let client = tracedecay_daemon_protocol::DaemonInvocationClient::new(
        tracedecay_daemon_protocol::DaemonConnection::new(
            endpoint,
            super::TEST_AUTH_TOKEN.to_owned(),
        ),
        handshake.clone(),
    );
    let reported_delay = crate::doctor::assert_stalled_doctor_probe(
        &client,
        &handshake.client_identity.profile_root,
        &project_path,
        CAUSE,
    )
    .await;
    let delay_after_probe = tasks
        .status(&route)
        .expect("the Doctor probe does not restart the rejected open")
        .retry_after_ms
        .expect("the recorded retry delay remains active");
    assert!(
        (delay_after_probe..=delay_before_probe).contains(&reported_delay),
        "Doctor's wire delay {reported_delay} must match the owner's remaining backoff \
         ({delay_after_probe}..={delay_before_probe})"
    );
    drop(client);
    tokio::time::timeout(Duration::from_secs(30), serving)
        .await
        .expect("Doctor connection must finish")
        .expect("Doctor serving task")
        .expect("Doctor daemon response");
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
}

/// Forward to the actual socket client, refusing a second Doctor attempt so
/// the regression fails at the first unwanted resend instead of its deadline.
struct SingleDoctorProbe<'a> {
    executor: &'a dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
    attempts: AtomicUsize,
}

impl tracedecay_contracts::ApplicationInvocationExecutor for SingleDoctorProbe<'_> {
    fn invoke(
        &self,
        invocation: tracedecay_contracts::ApplicationInvocation,
    ) -> tracedecay_contracts::ApplicationInvocationFuture<
        '_,
        Result<tracedecay_contracts::ApplicationResponse, tracedecay_contracts::InvocationError>,
    > {
        self.executor.invoke(invocation)
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for SingleDoctorProbe<'_> {
    fn invoke_controlled(
        &self,
        request: DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: tracedecay_contracts::CancellationSignal,
        policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        assert_eq!(
            self.attempts.fetch_add(1, Ordering::SeqCst),
            0,
            "Doctor must report this route refusal without a mounting resend"
        );
        self.executor
            .invoke_controlled(request, deadline, cancellation, policy)
    }

    fn observe_feedback(
        &self,
        subject_digest: tracedecay_domain::ManifestDigest,
        observed_at: UtcMicros,
        event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        tracedecay_domain::errors::Result<()>,
    > {
        self.executor
            .observe_feedback(subject_digest, observed_at, event)
    }
}

async fn assert_socket_doctor_project_open_refusal(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    expected_reason: &str,
    expected_message: &str,
) {
    let (project_path, _) = engine
        .project_route(handshake)
        .expect("Doctor project route");
    let (listener, endpoint) = tracedecay_daemon_protocol::BrokerListener::bind(
        &tracedecay_daemon_protocol::default_loopback_endpoint(),
    )
    .await
    .expect("Doctor daemon endpoint");
    let serving_engine = engine.clone();
    let serving = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept Doctor probe");
        Box::pin(
            crate::daemon::connection_serving::serve_authenticated_socket_client_with_class(
                stream,
                serving_engine,
                super::TEST_AUTH_TOKEN.to_owned(),
                crate::daemon::core_admission::DaemonClientAdmissionClass::General,
            ),
        )
        .await
    });
    let client = tracedecay_daemon_protocol::DaemonInvocationClient::new(
        tracedecay_daemon_protocol::DaemonConnection::new(
            endpoint,
            super::TEST_AUTH_TOKEN.to_owned(),
        ),
        handshake.clone(),
    );
    let observed_at = tracedecay_contracts::clock::now_micros();
    let ordinary = client
        .invoke(DaemonInvocationRequest::graph_tool(
            "request.runtime.admission-control",
            ApplicationSurfaceOperation::Runtime,
            serde_json::Map::from_iter([("doctor_report".to_owned(), false.into())]),
            observed_at,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
                .expect("runtime control deadline"),
            CancellationContext::active("cancel.runtime.admission-control")
                .expect("runtime control cancellation"),
        ))
        .await
        .expect("ordinary runtime refusal crosses the actual typed socket");
    assert_eq!(
        ordinary.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::runtime_mounting(),
        },
        "ordinary typed clients must retain their mounting retry contract"
    );
    let probe = SingleDoctorProbe {
        executor: &client,
        attempts: AtomicUsize::new(0),
    };
    crate::doctor::assert_project_route_doctor_probe(
        &probe,
        &handshake.client_identity.profile_root,
        &project_path,
        expected_reason,
        expected_message,
    )
    .await;
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    drop(client);
    tokio::time::timeout(Duration::from_secs(30), serving)
        .await
        .expect("Doctor connection must finish")
        .expect("Doctor serving task")
        .expect("Doctor daemon response");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_probe_preserves_project_server_capacity() {
    let (temp, _database_scope, engine, mut refused_handshake) =
        unopened_committed_fixture("doctor-project-server-capacity").await;
    let mut occupied = Vec::new();
    for index in 0..crate::daemon::MAX_CACHED_PROJECT_SERVERS {
        let project = temp.path().join(format!("occupied-{index}"));
        std::fs::create_dir_all(&project).expect("occupied project directory");
        initialize_test_project(&project, &refused_handshake.client_identity).await;
        occupied.push(DaemonHandshake {
            project_path: Some(project),
            ..refused_handshake.clone()
        });
    }
    super::prewarm_test_profile_runtime(&engine.store_administration).await;
    let mut leased_servers = Vec::new();
    for handshake in occupied {
        match engine.project_server(&handshake).await {
            Ok(server) => leased_servers.push(server),
            Err(error)
                if matches!(
                    error.project_route_context(),
                    Some((
                        tracedecay_daemon_protocol::PROJECT_SERVER_CAPACITY_REASON_CODE,
                        true,
                        _
                    ))
                ) =>
            {
                // Resource admission may reach the same canonical capacity
                // refusal before the route cache's maximum on a small host.
                refused_handshake = handshake;
                break;
            }
            Err(error) => panic!("open an occupied project: {error}"),
        }
    }
    assert!(
        !leased_servers.is_empty(),
        "the fixture must hold a live owner"
    );
    assert_socket_doctor_project_open_refusal(
        &engine,
        &refused_handshake,
        tracedecay_daemon_protocol::PROJECT_SERVER_CAPACITY_REASON_CODE,
        "Project open stalled: daemon project server capacity reached",
    )
    .await;
    drop(leased_servers);
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_probe_preserves_deferred_repository_discovery() {
    use tracedecay_runtime_core::git_repository::{
        block_repository_discovery_for_test, reset_repository_discovery_for_test,
    };

    let (_temp, _database_scope, engine, handshake) =
        unopened_committed_fixture("doctor-deferred-repository-discovery").await;
    super::prewarm_test_profile_runtime(&engine.store_administration).await;
    let (project_path, _) = engine
        .project_route(&handshake)
        .expect("Doctor project route");
    let mut blocked = block_repository_discovery_for_test(&project_path);
    let opening_engine = engine.clone();
    let opening_handshake = handshake.clone();
    let opening =
        tokio::spawn(async move { opening_engine.project_server(&opening_handshake).await });
    blocked.wait_entered().await;
    let Err(error) = opening.await.expect("blocked project open task") else {
        panic!("the blocked discovery must refuse project admission");
    };
    assert!(crate::daemon::error_is_repository_discovery_deferred(
        &error
    ));
    assert_socket_doctor_project_open_refusal(
        &engine,
        &handshake,
        tracedecay_runtime_core::git_discovery::REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE,
        &format!(
            "daemon is still warming: repository discovery blocked on {}",
            project_path.display()
        ),
    )
    .await;
    blocked.release();
    reset_repository_discovery_for_test(&project_path);
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
}

const PREPARATION_RELEASE_DEADLINE: Duration = Duration::from_mins(1);

/// Runtime telemetry can serve during project open, but a requested Doctor
/// report must wait for the report reader's publication after dependent owners
/// mount. The typed route must not settle that window as an unsupported report.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn doctor_report_mounts_before_the_typed_runtime_request_is_admitted() {
    let (_temp, _database_scope, engine, handshake) =
        unopened_committed_fixture("doctor-report-while-owners-mount").await;
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
    tokio::time::timeout(Duration::from_mins(1), registration.before_registration)
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
    );
    let runtime_request = |request_id: &str, doctor_report: bool| {
        let observed_at = tracedecay_contracts::clock::now_micros();
        DaemonInvocationRequest::graph_tool(
            request_id,
            ApplicationSurfaceOperation::Runtime,
            serde_json::Map::from_iter([("doctor_report".to_owned(), doctor_report.into())]),
            observed_at,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
                .expect("daemon invocation deadline"),
            CancellationContext::active(format!("cancel.{request_id}"))
                .expect("daemon invocation cancellation"),
        )
    };

    let mounting = execute_daemon_invocation(
        &engine,
        &handshake,
        runtime_request("request.doctor.while-mounting", true),
    )
    .await;
    let DaemonInvocationOutcome::ApplicationProblem { problem } = &mounting.outcome else {
        panic!("an unpublished Doctor reader must answer a mounting refusal: {mounting:?}");
    };
    assert_eq!(problem, &ApplicationProblem::runtime_mounting());

    let telemetry = execute_daemon_invocation(
        &engine,
        &handshake,
        runtime_request("request.telemetry.while-doctor-mounts", false),
    )
    .await;
    let DaemonInvocationOutcome::GraphTool { completion, .. } = &telemetry.outcome else {
        panic!("ordinary runtime telemetry must remain available: {telemetry:?}");
    };
    let GraphToolResultV1::Runtime(runtime) = &completion.result else {
        panic!("runtime request returned another result: {completion:?}");
    };
    assert!(runtime.doctor_report.is_none());

    registration
        .allow_registration
        .send(())
        .expect("release owner registration");
    tokio::time::timeout(Duration::from_mins(1), registration.after_registration)
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
        runtime_request("request.doctor.after-mounting", true),
    )
    .await;
    let DaemonInvocationOutcome::GraphTool { completion, .. } = &served.outcome else {
        panic!("the published Doctor reader must answer its report: {served:?}");
    };
    let GraphToolResultV1::Runtime(runtime) = &completion.result else {
        panic!("Doctor request returned another result: {completion:?}");
    };
    assert!(
        matches!(
            runtime.doctor_report,
            Some(RuntimeDoctorReportV1::Observed { .. })
        ),
        "Doctor must observe the canonical report after its reader publishes: {runtime:?}"
    );
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
}

#[derive(Default)]
struct CuratorPreparationPause {
    run_id: &'static str,
    release: Mutex<Option<mpsc::Receiver<()>>>,
    entered: tokio::sync::Notify,
}

struct PauseCuratorPreparation(Arc<CuratorPreparationPause>);

struct CuratorPreparationRun(bool);

struct CuratorPreparationRunVisitor<'a> {
    expected: &'a str,
    matches: bool,
}

impl Visit for CuratorPreparationRunVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "run_id" {
            self.matches = value == self.expected;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "run_id" {
            self.matches = format!("{value:?}") == self.expected;
        }
    }
}

impl<S> Layer<S> for PauseCuratorPreparation
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attributes: &Attributes<'_>,
        id: &tracing::span::Id,
        context: Context<'_, S>,
    ) {
        if attributes.metadata().name() != "daemon.automation.effect.reserve" {
            return;
        }
        let mut run = CuratorPreparationRunVisitor {
            expected: self.0.run_id,
            matches: false,
        };
        attributes.record(&mut run);
        context
            .span(id)
            .unwrap()
            .extensions_mut()
            .insert(CuratorPreparationRun(run.matches));
    }

    fn on_enter(&self, id: &tracing::span::Id, context: Context<'_, S>) {
        let span = context.span(id).unwrap();
        if !span
            .extensions()
            .get::<CuratorPreparationRun>()
            .is_some_and(|run| run.0)
        {
            return;
        }
        if let Some(release) = self.0.release.lock().unwrap().take() {
            self.0.entered.notify_one();
            release
                .recv_timeout(PREPARATION_RELEASE_DEADLINE)
                .expect("release the curator reservation writer");
        }
    }
}

/// The forced client phase drops the authenticated direct invocation, but a
/// journal write it already queued must keep the exact project owner alive.
#[tokio::test]
async fn aborted_socket_curator_preparation_keeps_its_writer_owned_through_shutdown() {
    const REQUEST_ID: &str = "request.socket-curator-preparation-shutdown";
    const CHILD: &str = "TRACEDECAY_TEST_CURATOR_PREPARATION_SHUTDOWN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // A global subscriber follows the real reserve onto its native
        // blocking thread. Isolate it from the other tests in this process.
        let test_name = format!(
            "{}::aborted_socket_curator_preparation_keeps_its_writer_owned_through_shutdown",
            module_path!().split_once("::").unwrap().1
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("run isolated curator preparation shutdown journey");
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
            "curator preparation shutdown journey failed: {output:?}"
        );
        return;
    }

    let pause = Arc::new(CuratorPreparationPause {
        run_id: REQUEST_ID,
        ..CuratorPreparationPause::default()
    });
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(PauseCuratorPreparation(Arc::clone(&pause))),
    )
    .expect("isolated curator preparation subscriber");
    let (_temp, _database_scope, engine, handshake) =
        committed_fixture("curator-preparation-shutdown").await;
    super::prewarm_test_profile_runtime(&engine.store_administration).await;
    let server = engine
        .project_server(&handshake)
        .await
        .expect("project server");
    let dashboard_root = server
        .cg_snapshot()
        .await
        .store_layout()
        .dashboard_root
        .clone();
    drop(server);
    let registry = engine.invocation.service.project_runtimes.clone();

    let (mut client, server) = tokio::net::UnixStream::pair().expect("curator socket pair");
    let server = tracedecay_daemon_protocol::BrokerStream::Unix(server);
    let serving = Box::pin(
        crate::daemon::connection_serving::serve_authenticated_socket_client_with_class(
            server,
            engine.clone(),
            super::TEST_AUTH_TOKEN.to_owned(),
            crate::daemon::core_admission::DaemonClientAdmissionClass::General,
        ),
    );
    let mut clients = crate::daemon::bootstrap::ClientTasks::default();
    clients
        .spawn(async move { serving.await.map_err(|error| error.to_string()) })
        .expect("native authenticated client task");
    super::write_test_auth_preface(&mut client).await;
    client
        .write_all(handshake.to_line().expect("curator handshake").as_bytes())
        .await
        .expect("write curator handshake");
    client.write_all(b"\n").await.expect("handshake newline");

    let (release, release_rx) = mpsc::channel();
    *pause.release.lock().unwrap() = Some(release_rx);
    let request_id = REQUEST_ID;
    let observed_at = tracedecay_contracts::clock::now_micros();
    let request = DaemonInvocationRequest::retained_application(
        request_id,
        RetainedSurfaceRequestV1::FactStoreCurate(FactStoreCurateRequestV1::default()),
        observed_at,
        Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000)))
            .expect("curator request deadline"),
        CancellationContext::active("cancel.socket-curator-preparation-shutdown")
            .expect("curator cancellation"),
    );
    client
        .write_all(serde_json::to_string(&request).unwrap().as_bytes())
        .await
        .expect("write retained curator invocation");
    client.write_all(b"\n").await.expect("invocation newline");
    tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, pause.entered.notified())
        .await
        .expect("authenticated curator reaches the real reservation writer");

    let run_id = tracedecay_domain::RunId::new(request_id).expect("curator run id");
    let journal_key = tracedecay_domain::canonical_sha256(&(
        "tracedecay.automation-run.terminal-key.v1",
        &run_id,
    ))
    .expect("curator journal key");
    let journal_path = dashboard_root.join("automation_effects").join(format!(
        "{}.json",
        journal_key.as_str().trim_start_matches("sha256:")
    ));
    assert!(!journal_path.exists(), "the reservation I/O is paused");

    let aborted = tracedecay_daemon_service::shutdown::join_shutdown_owners(
        Instant::now() + PREPARATION_RELEASE_DEADLINE,
        vec![crate::daemon::bootstrap::client_shutdown_owner(clients)],
    )
    .await;
    assert_eq!(aborted.owners.len(), 1);
    assert!(aborted.owners[0].status.is_clean(), "{aborted:?}");
    drop(client);

    let (drain_waiting, drain_is_waiting) = tokio::sync::oneshot::channel();
    registry.arm_shutdown_drain_waiting(drain_waiting);
    let shutdown_engine = engine.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_engine.shutdown_all().await });
    tokio::select! {
        result = &mut shutdown => panic!(
            "shutdown returned while the aborted client's writer was live: {result:?}"
        ),
        waiting = tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, drain_is_waiting) => {
            waiting.expect("shutdown reaches the project runtime drain")
                .expect("the detached reservation still owns the project drain");
        }
    }
    assert!(!shutdown.is_finished());
    assert!(!journal_path.exists(), "the writer is still paused");
    release.send(()).expect("release the reservation writer");
    let shutdown = tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, shutdown)
        .await
        .expect("the completed writer releases project shutdown")
        .expect("shutdown task");
    assert!(
        shutdown
            .background
            .owners
            .iter()
            .any(|owner| owner.name == "invocation" && owner.status.is_clean()),
        "the project runtime owner must finish cleanly: {shutdown:?}"
    );
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(journal_path).expect("reservation I/O completed before shutdown"),
    )
    .expect("durable curator reservation");
    assert_eq!(record["admission"]["request"]["run_id"], request_id);
}

#[tokio::test]
async fn aborted_automation_preparation_keeps_its_writer_owned_through_shutdown() {
    const CHILD: &str = "TRACEDECAY_TEST_AUTOMATION_PREPARATION_SHUTDOWN_CHILD";
    const RUN_ID: &str = "run.curator-preparation-shutdown";
    if std::env::var_os(CHILD).is_none() {
        let test_name = format!(
            "{}::aborted_automation_preparation_keeps_its_writer_owned_through_shutdown",
            module_path!().split_once("::").unwrap().1
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("run isolated automation preparation shutdown journey");
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
            "automation preparation shutdown journey failed: {output:?}"
        );
        return;
    }

    let pause = Arc::new(CuratorPreparationPause {
        run_id: RUN_ID,
        ..CuratorPreparationPause::default()
    });
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(PauseCuratorPreparation(Arc::clone(&pause))),
    )
    .expect("isolated automation preparation subscriber");
    let (_temp, _database_scope, engine, handshake) =
        committed_fixture("automation-preparation-shutdown").await;
    super::prewarm_test_profile_runtime(&engine.store_administration).await;
    let server = engine
        .project_server(&handshake)
        .await
        .expect("project server");
    let cg = server.cg_snapshot().await;
    let project_root = cg.project_root().to_path_buf();
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    drop(server);
    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("configuration");
    let digest = tracedecay_automation_runtime::automation::effect_runtime::pinned_automation_configuration_digest(
        pinned.revision_id(),
        &pinned.snapshot().effective_behavior_digest,
        &pinned.snapshot().resolution_provenance_digest,
    ).expect("pinned automation configuration");
    let request_id =
        tracedecay_daemon_service::automation_effect::scheduler_automation_request_id(Some(RUN_ID))
            .expect("scheduler request identity");
    let expected_request_id = request_id.as_str().to_owned();
    let cancellation =
        tracedecay_contracts::CancellationSignal::active(format!("cancel.{expected_request_id}"))
            .expect("scheduler cancellation");
    let observed_at = tracedecay_contracts::now_micros();
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("scheduler deadline");
    let options =
        tracedecay_automation_runtime::automation::runner::MemoryCuratorAutomationOptions::default(
        );
    let request =
        tracedecay_automation_runtime::automation::effect_runtime::memory_curator_run_request(
            RUN_ID,
            options.fact_review_limit,
            options.min_confidence,
        )
        .expect("curator request");
    let registry = engine.invocation.service.project_runtimes.clone();
    let service = engine.invocation.service.clone();
    let (release, release_rx) = mpsc::channel();
    *pause.release.lock().unwrap() = Some(release_rx);
    let prepare_dashboard_root = dashboard_root.clone();
    let preparation = tracedecay_runtime_core::native_runtime::NativeTask::spawn(async move {
        // Scheduler and dashboard callers use this shared production boundary,
        // without the socket caller's accepted-run lease or task-local scope.
        let admission = Box::pin(tracedecay_daemon_service::automation_effect::prepare(
            &service,
            &cg,
            &project_root,
            &prepare_dashboard_root,
            request_id,
            deadline,
            &cancellation,
            observed_at,
            digest,
            request,
        )).await.expect("automation preparation");
        assert!(matches!(admission, tracedecay_automation_runtime::automation::effect_runtime::AutomationEffectAdmission::Execute(_)));
    }).expect("native automation preparation");
    tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, pause.entered.notified())
        .await
        .expect("the exact automation reservation writer is paused");
    let run_id = tracedecay_domain::RunId::new(RUN_ID).expect("curator run identity");
    let journal_key = tracedecay_domain::canonical_sha256(&(
        "tracedecay.automation-run.terminal-key.v1",
        &run_id,
    ))
    .expect("curator journal key");
    let journal_path = dashboard_root.join("automation_effects").join(format!(
        "{}.json",
        journal_key.as_str().trim_start_matches("sha256:")
    ));
    assert!(!journal_path.exists(), "the reservation I/O is paused");
    preparation.abort();
    let stopped = tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, preparation.wait())
        .await
        .expect("aborted preparation wait joins");
    assert!(matches!(
        stopped,
        tracedecay_runtime_core::native_runtime::NativeTaskOutcome::Cancelled
    ));

    let (drain_waiting, drain_is_waiting) = tokio::sync::oneshot::channel();
    registry.arm_shutdown_drain_waiting(drain_waiting);
    let shutdown_engine = engine.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_engine.shutdown_all().await });
    tokio::select! {
        result = &mut shutdown => panic!(
            "shutdown returned while the automation writer was live: {result:?}"
        ),
        waiting = tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, drain_is_waiting) => {
            waiting.expect("shutdown reaches the project runtime drain")
                .expect("the automation reservation still owns the project drain");
        }
    }
    assert!(!shutdown.is_finished());
    assert!(!journal_path.exists(), "the writer is still paused");
    release
        .send(())
        .expect("release the automation reservation writer");
    let shutdown = tokio::time::timeout(PREPARATION_RELEASE_DEADLINE, shutdown)
        .await
        .expect("the writer releases project shutdown")
        .expect("shutdown task");
    assert!(
        shutdown
            .background
            .owners
            .iter()
            .any(|owner| owner.name == "invocation" && owner.status.is_clean()),
        "{shutdown:?}"
    );
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(journal_path).expect("reservation completed before shutdown"),
    )
    .expect("durable automation reservation");
    assert_eq!(record["admission"]["request"]["run_id"], RUN_ID);
    assert_eq!(record["admission"]["request_id"], expected_request_id);
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
            )
            .expect("valid primitive request"),
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
        // not-found-or-not-authorized. That still proves routing, an
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
async fn primitive_identity_is_refused_before_daemon_and_application_owners() {
    let (_temp, _database_scope, engine, handshake) =
        committed_fixture("primitive-invocation-identity").await;
    let now = tracedecay_contracts::clock::now_micros();
    let deadline = Deadline::new(UtcMicros(now.0 + 30_000_000)).expect("deadline");
    let cancellation =
        CancellationContext::active("cancel.primitive-identity").expect("cancellation");
    assert_primitive_routes_mounted(
        &engine,
        &handshake,
        now,
        deadline.clone(),
        cancellation.clone(),
    )
    .await;
    let frame = DaemonInvocationRequest::primitive(
        "request.primitive-identity",
        ApplicationSurfaceOperation::StorageStatus,
        PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
            include_details: true,
        }),
        now,
        deadline.clone(),
        cancellation.clone(),
    )
    .expect("matching frame");
    let mut wire = serde_json::to_value(frame).expect("serialize frame");
    wire["surface_operation"] = serde_json::json!("source_lines");
    let malformed = serde_json::from_value(wire).expect("deserialize malformed pair");
    let response = execute_daemon_invocation(&engine, &handshake, malformed).await;
    assert!(
        matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::InvalidRequest
            }
        ),
        "{response:?}"
    );

    let root = Path::new(handshake.project_path.as_deref().expect("project root"));
    let dispatch = engine
        .invocation
        .service
        .project_runtimes
        .read(root, PrimitiveProjectRuntime::dispatch)
        .await
        .expect("mounted primitive dispatcher");
    let operation = primitive_read_operation("source_lines")
        .expect("catalog")
        .expect("source-lines operation");
    let foreign_scope = ResolvedScope::new(
        ProjectId::new("project.foreign-primitive").expect("project"),
        RepositoryId::new("repository.foreign-primitive").expect("repository"),
        WorktreeId::new("worktree.foreign-primitive").expect("worktree"),
        None,
    )
    .expect("scope");
    let actor = ActorId::new("actor.primitive-identity").expect("actor");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.primitive-identity").expect("grant"),
        1,
        foreign_scope.scope_digest.clone(),
        actor.clone(),
        now,
        deadline.expires_at,
        foreign_scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("fixture grant");
    let context = RequestContext::new(
        actor,
        foreign_scope,
        grant,
        RequestId::new("request.application-primitive-identity").expect("request"),
        deadline.clone(),
        cancellation.clone(),
    )
    .expect("context");
    let mismatch = dispatch
        .dispatch(
            PrimitiveInvocation {
                operation,
                request: PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                    include_details: true,
                }),
            },
            context.clone(),
            now,
        )
        .await
        .expect("application result")
        .expect_err("operation/body mismatch");
    assert_eq!(
        mismatch.problem.kind(),
        ApplicationProblemKind::InvalidRequest
    );
    let matching = dispatch
        .dispatch(
            PrimitiveInvocation {
                operation: primitive_read_operation("storage_status")
                    .expect("catalog")
                    .expect("storage operation"),
                request: PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                    include_details: true,
                }),
            },
            context,
            now,
        )
        .await
        .expect("application result")
        .expect_err("foreign scope");
    assert_eq!(
        matching.problem.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );

    drop(dispatch);

    for (label, deadline, cancellation, expected) in [
        (
            "cancelled",
            deadline,
            CancellationContext::cancelled("cancel.primitive-identity.cancelled", now)
                .expect("cancelled"),
            ApplicationProblemKind::Cancelled,
        ),
        (
            "expired",
            Deadline::new(UtcMicros(now.0 - 1)).expect("expired deadline"),
            cancellation,
            ApplicationProblemKind::TimedOut,
        ),
    ] {
        let request = DaemonInvocationRequest::primitive(
            format!("request.primitive-identity.{label}"),
            ApplicationSurfaceOperation::StorageStatus,
            PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                include_details: false,
            }),
            now,
            deadline,
            cancellation,
        )
        .expect("matching controls");
        let response = execute_daemon_invocation(&engine, &handshake, request).await;
        assert!(
            matches!(response.outcome, DaemonInvocationOutcome::ApplicationProblem {ref problem} if problem.kind() == expected),
            "{response:?}"
        );
    }
    let shutdown = engine.shutdown_all().await;
    assert!(shutdown.project_servers.is_clean(), "{shutdown:?}");
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

/// Right after a restart the profile store's readers can all be busy. The
/// project source-access read then times out, and a mounted project must
/// answer that as a retryable unavailable state naming the configuration
/// authority, never as a denial of a project the caller is standing in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn primitive_read_with_a_saturated_configuration_store_is_unavailable_not_denied() {
    let (_temp, _database_scope, engine, handshake) =
        committed_fixture("primitive-read-saturated-configuration").await;
    let server = engine
        .project_server(&handshake)
        .await
        .expect("mounted project server");
    let configuration_store = server
        .cg_snapshot()
        .await
        .configuration_runtime()
        .registered_database();
    // Lease readers until the pool, burst workers included, refuses one.
    let mut held_readers = Vec::new();
    while let Ok(reader) = configuration_store.read_snapshot().await {
        held_readers.push(reader);
    }
    assert!(
        !held_readers.is_empty(),
        "the store admitted readers before saturating"
    );

    let observed_at = tracedecay_contracts::clock::now_micros();
    let response = execute_daemon_invocation(
        &engine,
        &handshake,
        DaemonInvocationRequest::primitive(
            "request.saturated-configuration.primitive",
            ApplicationSurfaceOperation::StorageStatus,
            PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                include_details: false,
            }),
            observed_at,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
                .expect("daemon invocation deadline"),
            CancellationContext::active("cancel.saturated-configuration")
                .expect("daemon invocation cancellation"),
        )
        .expect("valid primitive request"),
    )
    .await;
    let DaemonInvocationOutcome::ApplicationProblem { problem } = &response.outcome else {
        panic!("a saturated configuration store must answer a typed problem: {response:?}");
    };
    assert_eq!(
        problem.kind(),
        ApplicationProblemKind::Unavailable,
        "{problem:?}"
    );
    assert_eq!(problem.retry(), RetryDirective::AfterDelay, "{problem:?}");
    assert_eq!(
        problem
            .diagnostic()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("configuration_authority_unavailable"),
        "{problem:?}"
    );

    drop(held_readers);
    let served = execute_daemon_invocation(
        &engine,
        &handshake,
        DaemonInvocationRequest::primitive(
            "request.saturated-configuration.released",
            ApplicationSurfaceOperation::StorageStatus,
            PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
                include_details: false,
            }),
            tracedecay_contracts::clock::now_micros(),
            Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000)))
                .expect("daemon invocation deadline"),
            CancellationContext::active("cancel.saturated-configuration.released")
                .expect("daemon invocation cancellation"),
        )
        .expect("valid primitive request"),
    )
    .await;
    assert!(
        matches!(served.outcome, DaemonInvocationOutcome::Primitive { .. }),
        "the same read is served once readers free up: {served:?}"
    );
    drop((configuration_store, server));
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
/// the retained runtime. Hold that phase open, the configuration registration
/// pause sits in the same owner phase, just ahead of the retained registration
///, and prove that a retained request landing in the window reads as the
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
    assert_eq!(
        diagnostic.code,
        tracedecay_contracts::RUNTIME_MOUNTING_REASON_CODE
    );
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

/// Primitive reads belong to the admitted core route and must not wait for
/// later configuration, retained, native-integration, or Work owners. Hold
/// configuration publication after primitive registration and prove a status
/// read is already served while the overall project publication is warming.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn primitive_invocation_serves_while_later_owners_mount() {
    let (_temp, _database_scope, engine, handshake) =
        unopened_committed_fixture("primitive-invocation-while-later-owners-mount").await;
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
    .expect("project open must reach the later owner registration gate")
    .expect("owner registration gate sender");
    assert_eq!(
        engine
            .invocation
            .service
            .project_runtimes
            .publication_state(&canonical_project),
        Some(ProjectRuntimePublicationStateV1::Warming),
        "later owners must still be publishing"
    );

    let observed_at = tracedecay_contracts::clock::now_micros();
    assert_primitive_routes_mounted(
        &engine,
        &handshake,
        observed_at,
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
            .expect("daemon invocation deadline"),
        CancellationContext::active("cancel.primitive-while-later-owners-mount")
            .expect("daemon invocation cancellation"),
    )
    .await;

    registration
        .allow_registration
        .send(())
        .expect("release configuration registration");
    tokio::time::timeout(
        std::time::Duration::from_mins(1),
        registration.after_registration,
    )
    .await
    .expect("configuration owner must publish")
    .expect("configuration registration publication sender");
    registration
        .allow_return
        .send(())
        .expect("release project-open owner setup");
    opening
        .await
        .expect("project-open task")
        .expect("project opens once its owners are registered");
    engine.shutdown_all().await;
}
