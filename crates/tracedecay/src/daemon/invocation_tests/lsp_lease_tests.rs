//! Daemon-owned LSP lease reclamation and shutdown behavior.

use std::path::PathBuf;
use std::sync::Arc;
use tracedecay_daemon_protocol::{DaemonInvocationOutcome, DaemonInvocationRequest};
use tracedecay_lsp::AdmittedRoot;

use super::*;
use tokio::sync::Mutex;
use tracedecay_contracts::{CancellationContext, Deadline};
use tracedecay_daemon_service::{DaemonInvocationService, *};
use tracedecay_lsp::{AuthorizedLspWorkspace, LspSessionRegistry};

fn lsp_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("LSP deadline")
}

async fn open_session(
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    request_id: &str,
) -> DaemonLspSessionAccess {
    let (project_root, root_uri) = admitted_root_fixture("authoritative");
    DaemonLspOwnerRegistrar::new(service)
        .register_factory_for_project(
            project_root.clone(),
            tracedecay_domain::UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            tracedecay_domain::ProjectId::new("project.test.lsp").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .unwrap();
    let response = service
        .invoke(
            registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                root_uri.clone(),
            ))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                request_id.to_owned(),
                env!("CARGO_PKG_VERSION"),
                None,
                Vec::new(),
                lsp_deadline(),
                CancellationContext::active(format!("cancel.{request_id}")).unwrap(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
        panic!("expected LSP session");
    };
    session
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_retirement_fences_disconnect_and_joins_its_new_lease() {
    let service = Arc::new(DaemonInvocationService::default());
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.retirement-disconnect").await;
    let access = session.clone().into_access().expect("session access");
    let admission = service.lsp_admission_open.lock().await;
    service
        .lsp_sessions
        .lock()
        .await
        .remove(access.session_id());

    let disconnect_service = Arc::clone(&service);
    let disconnect_registry = Arc::clone(&registry);
    let disconnect = tokio::spawn(async move {
        disconnect_service
            .disconnect_lsp_session(&disconnect_registry, session)
            .await;
    });
    tokio::task::yield_now().await;

    assert_eq!(
        registry.lock().await.active_sessions(),
        1,
        "disconnect must wait behind project protocol retirement"
    );
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);

    drop(admission);
    disconnect.await.expect("disconnect after retirement");

    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert_eq!(
        service.lsp_lease_tasks.active_tasks(),
        0,
        "a disconnect admitted after retirement must join its unused lease"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_shutdown_fences_a_queued_open_before_the_endpoint_expiry_sweep() {
    let state = Arc::new(crate::daemon::invocation_state::DaemonInvocationState::default());
    let endpoint_guard = state.lsp_session_registry.lock().await;
    let shutdown_state = Arc::clone(&state);
    let shutdown = tokio::spawn(async move {
        shutdown_state.shutdown(None).await;
    });
    // The fence the open below must be queued behind is the closed admission
    // gate, so observe it directly: `begin_shutdown` closes the gate and then
    // parks on the endpoint registry this test holds. Yielding a fixed number
    // of times is not an ordering, on a two-worker runtime under load the
    // shutdown task can still be unscheduled after eight yields, and an open
    // that wins the gate first is a pre-shutdown admission the sweep retires,
    // not a queued one the fence refuses.
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if state
                .service
                .lsp_admission_open
                .try_lock()
                .is_ok_and(|open| !*open)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown must close the LSP admission gate while the endpoint registry is held");

    let open_state = Arc::clone(&state);
    let (open_started, started) = tokio::sync::oneshot::channel();
    let open = tokio::spawn(async move {
        open_started.send(()).expect("open-start observer");
        open_state
            .service
            .open_lsp_session(
                &open_state.lsp_session_registry,
                Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                    admitted_root_fixture("authoritative").1,
                ))),
                "request.state-shutdown-race".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
                None,
                Vec::new(),
                now_millis(),
                Some(DaemonLspInvocationOwner::for_test_project(
                    unavailable_lsp_session_factory(),
                    tracedecay_domain::UserProfileId::new("profile.test.lsp")
                        .expect("test LSP profile"),
                    tracedecay_domain::ProjectId::new("project.test.lsp")
                        .expect("test LSP project"),
                    PathBuf::from("/test/lsp"),
                )),
            )
            .await
    });
    started.await.expect("racing open started");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if open.is_finished() || state.service.lsp_admission_open.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown or open must acquire the LSP admission gate");
    drop(endpoint_guard);

    let response = open.await.expect("racing LSP open");
    shutdown.await.expect("daemon invocation state shutdown");

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    assert_eq!(state.lsp_session_registry.lock().await.active_sessions(), 0);
    assert_eq!(state.service.active_lsp_runtime_count().await, 0);
    assert_eq!(state.service.lsp_lease_tasks.active_tasks(), 0);
}
