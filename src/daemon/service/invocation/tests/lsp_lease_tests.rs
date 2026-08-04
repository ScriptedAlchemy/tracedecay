//! Daemon-owned LSP lease reclamation and shutdown behavior.

use super::*;

fn lsp_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("LSP deadline")
}

async fn open_session(
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    request_id: &str,
) -> DaemonLspSessionAccess {
    let project_root = PathBuf::from("/authoritative");
    DaemonLspOwnerRegistrar::new(service)
        .register_factory(project_root.clone(), unavailable_lsp_session_factory())
        .await
        .unwrap();
    let response = service
        .invoke(
            registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///authoritative",
            ))),
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

async fn detach_runtime_actor(service: &DaemonInvocationService, session: &DaemonLspSessionAccess) {
    let access = session.clone().into_access().expect("session access");
    service
        .lsp_sessions
        .lock()
        .await
        .get_mut(access.session_id())
        .expect("runtime session")
        .actor
        .detach()
        .expect("detach runtime actor");
}

#[tokio::test]
async fn shutdown_and_exit_can_complete_the_explicit_transport_detach() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.shutdown-exit").await;
    for (request_id, frame) in [
        (
            "request.initialize",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///authoritative","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
        ),
        (
            "request.initialized",
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        ),
        (
            "request.protocol-shutdown",
            r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#,
        ),
        (
            "request.exit",
            r#"{"jsonrpc":"2.0","method":"exit","params":{}}"#,
        ),
    ] {
        let response = service
            .send_lsp_frame(
                &registry,
                request_id.to_owned(),
                session.clone(),
                frame.to_owned(),
                1,
            )
            .await;
        assert!(
            matches!(
                response.outcome,
                DaemonInvocationOutcome::LspFrameAccepted { .. }
            ),
            "protocol frame must be accepted: {:?}",
            response.outcome
        );
    }
    let access = session.clone().into_access().expect("session access");
    assert_eq!(
        service
            .lsp_sessions
            .lock()
            .await
            .get(access.session_id())
            .expect("runtime session")
            .actor
            .lifecycle(),
        SessionLifecycle::Exited
    );

    let response = service
        .detach_lsp_session(&registry, "request.transport-detach".to_owned(), session, 2)
        .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspDetached
    ));
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_success_is_fenced_from_late_disconnect_cleanup() {
    for attempt in 0..64 {
        let service = Arc::new(DaemonInvocationService::default());
        let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
        let session = open_session(
            &service,
            &registry,
            &format!("request.reconnect-race.{attempt}"),
        )
        .await;
        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let disconnect = {
            let service = Arc::clone(&service);
            let registry = Arc::clone(&registry);
            let session = session.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                service.disconnect_lsp_session(&registry, session).await
            })
        };
        let reconnect = {
            let service = Arc::clone(&service);
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                service
                    .reconnect_lsp_session(
                        &registry,
                        format!("request.reconnect-race-response.{attempt}"),
                        session,
                        now_millis(),
                    )
                    .await
            })
        };
        barrier.wait().await;
        let _ = disconnect.await.expect("disconnect task");
        let response = reconnect.await.expect("reconnect task");

        let DaemonInvocationOutcome::LspReconnected { session } = response.outcome else {
            panic!("concurrent reconnect failed on attempt {attempt}");
        };
        let access = session.into_access().expect("reconnected access");
        registry
            .lock()
            .await
            .authenticate(&access, now_millis())
            .expect("successful reconnect remains authenticated");
        assert_ne!(
            service
                .lsp_sessions
                .lock()
                .await
                .get(access.session_id())
                .expect("successful reconnect retains runtime")
                .actor
                .lifecycle(),
            SessionLifecycle::Detached,
            "late cleanup detached reconnected actor on attempt {attempt}"
        );
        assert_eq!(
            service.lsp_lease_tasks.active_tasks(),
            0,
            "late cleanup retained the prior lease on attempt {attempt}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_lease_completion_is_not_retained_during_admission() {
    let registry = Arc::new(LspLeaseTaskRegistry::default());
    let session_id = LspSessionId::new("lsp-immediate-expiry").expect("session id");
    let previous = registry
        .begin_start(
            session_id.clone(),
            crate::application::context::CancellationToken::new(),
            std::future::ready(()),
        )
        .expect("begin immediate lease task");
    registry
        .finish_start(&session_id, previous)
        .await
        .expect("finish immediate lease task");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while registry.active_tasks() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("immediate task must retire");

    assert_eq!(
        registry.active_tasks(),
        0,
        "a task that completes immediately must not leave a retained handle"
    );
}

#[tokio::test]
async fn disconnect_reclamation_does_not_outlive_daemon_service() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.owner-drop").await;
    let retained_runtime_state = Arc::downgrade(&service.lsp_sessions);

    service
        .disconnect_lsp_session(&registry, session)
        .await
        .expect("disconnect session");
    drop(service);
    tokio::task::yield_now().await;

    assert!(
        retained_runtime_state.upgrade().is_none(),
        "lease reclamation must be cancelled with its daemon owner"
    );
}

#[tokio::test(start_paused = true)]
async fn abrupt_disconnect_reclaims_session_at_its_bounded_lease() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.abrupt-drop").await;

    service
        .disconnect_lsp_session(&registry, session)
        .await
        .expect("disconnect session");
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(LSP_SESSION_TTL_MS)).await;
    tokio::task::yield_now().await;

    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(
        service.lsp_lease_tasks.active_tasks(),
        0,
        "bounded reclamation must retire its owned task"
    );
}

#[tokio::test(start_paused = true)]
async fn reconnect_renews_session_and_survives_the_disconnected_lease_deadline() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.reconnect-renewal").await;
    let access = session.clone().into_access().expect("session access");
    let original_expires_at_ms = service
        .lsp_sessions
        .lock()
        .await
        .get(access.session_id())
        .expect("runtime session")
        .expires_at_ms;
    let reconnect_now_ms = original_expires_at_ms
        .saturating_sub(LSP_SESSION_TTL_MS)
        .saturating_add(1_000);

    service
        .disconnect_lsp_session(&registry, session.clone())
        .await
        .expect("disconnect session");
    let response = service
        .reconnect_lsp_session(
            &registry,
            "request.reconnect-renewal".to_owned(),
            session,
            reconnect_now_ms,
        )
        .await;
    let DaemonInvocationOutcome::LspReconnected { session } = response.outcome else {
        panic!("expected reconnected session");
    };
    let renewed_access = session.clone().into_access().expect("renewed access");
    let renewed_expires_at_ms = reconnect_now_ms.saturating_add(LSP_SESSION_TTL_MS);

    {
        let runtime_sessions = service.lsp_sessions.lock().await;
        let runtime = runtime_sessions
            .get(renewed_access.session_id())
            .expect("renewed runtime session");
        assert_eq!(runtime.expires_at_ms, renewed_expires_at_ms);
        assert_eq!(
            runtime.actor.lifecycle(),
            SessionLifecycle::AwaitingInitialize
        );
    }
    assert_eq!(
        registry
            .lock()
            .await
            .authenticate(&renewed_access, original_expires_at_ms)
            .expect("registry expiry must be renewed")
            .lifecycle(),
        SessionLifecycle::AwaitingInitialize
    );
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);

    tokio::time::advance(std::time::Duration::from_millis(LSP_SESSION_TTL_MS)).await;
    tokio::task::yield_now().await;

    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert!(
        service
            .lsp_sessions
            .lock()
            .await
            .contains_key(renewed_access.session_id())
    );
    assert!(
        service
            .authenticate(&registry, session, original_expires_at_ms)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn explicit_detach_reports_actor_failure_after_closing_session_state() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.detach-failure").await;
    detach_runtime_actor(&service, &session).await;

    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_detach(
                "request.detach-failure",
                session,
                lsp_deadline(),
                CancellationContext::active("cancel.detach-failure").unwrap(),
            ),
        )
        .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
}

#[tokio::test]
async fn disconnect_actor_failure_closes_state_without_scheduling_a_lease() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.disconnect-failure").await;
    detach_runtime_actor(&service, &session).await;

    let problem = service
        .disconnect_lsp_session(&registry, session)
        .await
        .expect_err("actor detach failure must be reported");

    assert_eq!(problem, DaemonInvocationProblem::Unavailable);
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
}

#[tokio::test]
async fn shutdown_joins_pending_lease_reclamation() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.shutdown").await;
    let retained_runtime_state = Arc::downgrade(&service.lsp_sessions);

    service
        .disconnect_lsp_session(&registry, session)
        .await
        .expect("disconnect session");
    service.expire_all().await;
    assert!(
        matches!(
            service.lsp_lease_tasks.begin_start(
                LspSessionId::new("lsp-after-shutdown").expect("session id"),
                crate::application::context::CancellationToken::new(),
                std::future::ready(()),
            ),
            Err(DaemonInvocationProblem::Unavailable)
        ),
        "shutdown must close lease-task admission before draining"
    );
    drop(service);

    assert!(
        retained_runtime_state.upgrade().is_none(),
        "shutdown must join every pending LSP lease task"
    );
}
