#![cfg(unix)]

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tracedecay_contracts::retained_surfaces::{
    FactStoreCurateRequestV1, MemoryStatusRequestV1, RetainedSurfaceRequestV1,
};
use tracedecay_contracts::{
    ApplicationProblemKind, CancellationContext, Deadline, ProblemTerminality, RetryDirective,
    WorkGraphReadRequestV1, WorkProductSelectionScopeV1,
};
use tracedecay_domain::{ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::{
    enter_test_daemon_database_scope, initialize_test_project, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};
use crate::daemon::{
    DaemonEngine, DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationRequest,
    execute_daemon_invocation,
};
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

const PREPARATION_RELEASE_DEADLINE: Duration = Duration::from_mins(1);

#[derive(Default)]
struct CuratorPreparationPause {
    release: Mutex<Option<mpsc::Receiver<()>>>,
    entered: tokio::sync::Notify,
}

struct PauseCuratorPreparation(Arc<CuratorPreparationPause>);

impl<S> Layer<S> for PauseCuratorPreparation
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &tracing::span::Id, context: Context<'_, S>) {
        if context.span(id).unwrap().metadata().name() != "daemon.automation.effect.reserve" {
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

    let pause = Arc::new(CuratorPreparationPause::default());
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
    let dashboard_root = server.cg().await.store_layout().dashboard_root.clone();
    drop(server);
    let registry = engine.invocation.service.project_runtimes.clone();

    let (mut client, server) = tokio::net::UnixStream::pair().expect("curator socket pair");
    let server = tracedecay_daemon_protocol::BrokerStream::from_tokio_unix(server)
        .expect("native curator socket");
    let serving = Box::pin(crate::daemon::serve_authenticated_socket_client_with_class(
        server,
        engine.clone(),
        super::TEST_AUTH_TOKEN.to_owned(),
        crate::daemon::DaemonClientAdmissionClass::General,
    ));
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
    let request_id = "request.socket-curator-preparation-shutdown";
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
        .cg()
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
