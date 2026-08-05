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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_lease_completion_is_not_retained_during_admission() {
    let registry = Arc::new(LspLeaseTaskRegistry::default());
    let session_id = LspSessionId::new("lsp-immediate-expiry").expect("session id");
    registry
        .start(session_id, std::future::ready(()))
        .await
        .expect("start immediate lease task");
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
async fn cancellation_before_lease_activation_retires_reserved_task_ownership() {
    let registry = Arc::new(LspLeaseTaskRegistry::default());
    let session_id = LspSessionId::new("lsp-pending-disconnect").expect("session id");
    let (activate, activated) = tokio::sync::oneshot::channel::<()>();
    registry
        .start(session_id.clone(), async move {
            if activated.await.is_ok() {
                std::future::pending::<()>().await;
            }
        })
        .await
        .expect("reserve pending lease task");
    assert_eq!(registry.active_tasks(), 1);

    registry
        .cancel(&session_id)
        .await
        .expect("cancel pending lease task");

    assert!(
        activate.send(()).is_err(),
        "a detached endpoint must not activate lease work after explicit cancellation"
    );
    assert_eq!(registry.active_tasks(), 0);
}

#[tokio::test]
async fn disconnect_reclamation_does_not_outlive_daemon_service() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.owner-drop").await;
    let retained_runtime_state = Arc::downgrade(&service.lsp_sessions);

    service.disconnect_lsp_session(&registry, session).await;
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

    service.disconnect_lsp_session(&registry, session).await;
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
async fn explicit_detach_accepts_an_actor_that_already_exited_gracefully() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.detach-exited").await;
    let access = session.clone().into_access().expect("session access");
    {
        let mut sessions = service.lsp_sessions.lock().await;
        let actor = &mut sessions
            .get_mut(access.session_id())
            .expect("runtime session")
            .actor;
        for frame in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///authoritative","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"exit","params":{}}"#,
        ] {
            actor.handle_payload(frame.as_bytes(), now_millis());
        }
        assert_eq!(actor.lifecycle(), SessionLifecycle::Exited);
    }

    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_detach(
                "request.detach-exited",
                session,
                lsp_deadline(),
                CancellationContext::active("cancel.detach-exited").unwrap(),
            ),
        )
        .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspDetached
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

    service.disconnect_lsp_session(&registry, session).await;

    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_detach_racing_disconnect_leaves_no_unowned_lease_task() {
    for attempt in 0..32 {
        let service = Arc::new(DaemonInvocationService::default());
        let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
        let session = open_session(
            &service,
            &registry,
            &format!("request.detach-race.{attempt}"),
        )
        .await;
        let disconnect_service = Arc::clone(&service);
        let disconnect_registry = Arc::clone(&registry);
        let disconnect_session = session.clone();
        let disconnect = tokio::spawn(async move {
            disconnect_service
                .disconnect_lsp_session(&disconnect_registry, disconnect_session)
                .await;
        });
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_detach(
                    format!("request.detach-race.{attempt}"),
                    session,
                    lsp_deadline(),
                    CancellationContext::active(format!("cancel.detach-race.{attempt}")).unwrap(),
                ),
            )
            .await;
        disconnect.await.expect("disconnect race task");

        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspDetached
                | DaemonInvocationOutcome::Problem {
                    problem: DaemonInvocationProblem::Unavailable
                        | DaemonInvocationProblem::NotFoundOrNotAuthorized
                }
        ));
        assert_eq!(registry.lock().await.active_sessions(), 0);
        assert!(service.lsp_sessions.lock().await.is_empty());
        assert_eq!(
            service.lsp_lease_tasks.active_tasks(),
            0,
            "attempt {attempt} retained a lease task after explicit detach"
        );
    }
}

#[tokio::test]
async fn repeated_disconnect_preserves_the_existing_bounded_lease() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.double-disconnect").await;

    service
        .disconnect_lsp_session(&registry, session.clone())
        .await;
    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.active_lsp_runtime_count().await, 1);
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 1);

    service.disconnect_lsp_session(&registry, session).await;

    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.active_lsp_runtime_count().await, 1);
    assert_eq!(
        service.lsp_lease_tasks.active_tasks(),
        1,
        "an idempotent second disconnect must not cancel the first bounded lease"
    );
}

#[tokio::test(start_paused = true)]
async fn reconnect_at_lease_expiry_joins_reclamation_before_rotating_credentials() {
    let service = Arc::new(DaemonInvocationService::default());
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.reconnect-at-expiry").await;
    service
        .disconnect_lsp_session(&registry, session.clone())
        .await;
    tokio::task::yield_now().await;

    let endpoint_guard = registry.lock().await;
    let reconnect_service = Arc::clone(&service);
    let reconnect_registry = Arc::clone(&registry);
    let reconnect = tokio::spawn(async move {
        reconnect_service
            .reconnect_lsp_session(
                &reconnect_registry,
                "request.reconnect-at-expiry".to_owned(),
                session,
                now_millis(),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(LSP_SESSION_TTL_MS)).await;
    tokio::task::yield_now().await;
    drop(endpoint_guard);

    let response = reconnect.await.expect("near-expiry reconnect");
    let DaemonInvocationOutcome::LspReconnected {
        session: reconnected,
    } = response.outcome
    else {
        panic!("near-expiry reconnect must win after authenticating");
    };
    tokio::task::yield_now().await;

    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.active_lsp_runtime_count().await, 1);
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
    let detached = service
        .detach_lsp_session(
            &registry,
            "request.reconnect-at-expiry.detach".to_owned(),
            reconnected,
            now_millis(),
        )
        .await;
    assert!(matches!(
        detached.outcome,
        DaemonInvocationOutcome::LspDetached
    ));
}

#[tokio::test]
async fn abnormal_reconnect_cancels_and_joins_the_registered_lease() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.reconnect-divergent").await;
    let access = session.clone().into_access().expect("session access");

    service
        .disconnect_lsp_session(&registry, session.clone())
        .await;
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 1);
    service
        .lsp_sessions
        .lock()
        .await
        .remove(access.session_id());

    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_reconnect(
                "request.reconnect-divergent",
                session,
                lsp_deadline(),
                CancellationContext::active("cancel.reconnect-divergent").unwrap(),
            ),
        )
        .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
}

#[tokio::test]
async fn shutdown_fences_racing_lsp_open_before_it_can_publish_state() {
    let service = Arc::new(DaemonInvocationService::default());
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let shutdown_service = Arc::clone(&service);
    let shutdown = tokio::spawn(async move {
        shutdown_service.begin_shutdown().await;
    });
    tokio::task::yield_now().await;

    let response = service
        .open_lsp_session(
            &registry,
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///authoritative",
            ))),
            "request.open-during-shutdown".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            None,
            Vec::new(),
            now_millis(),
            Some(DaemonLspInvocationOwner::new(
                unavailable_lsp_session_factory(),
            )),
        )
        .await;
    shutdown.await.expect("shutdown admission fence");

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_shutdown_fences_a_queued_open_before_the_endpoint_expiry_sweep() {
    let state = Arc::new(crate::daemon::invocation_state::DaemonInvocationState::default());
    let endpoint_guard = state.lsp_session_registry.lock().await;
    let shutdown_state = Arc::clone(&state);
    let shutdown = tokio::spawn(async move {
        shutdown_state.shutdown().await;
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    let open_state = Arc::clone(&state);
    let (open_started, started) = tokio::sync::oneshot::channel();
    let open = tokio::spawn(async move {
        open_started.send(()).expect("open-start observer");
        open_state
            .service
            .open_lsp_session(
                &open_state.lsp_session_registry,
                Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                    "file:///authoritative",
                ))),
                "request.state-shutdown-race".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
                None,
                Vec::new(),
                now_millis(),
                Some(DaemonLspInvocationOwner::new(
                    unavailable_lsp_session_factory(),
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

#[tokio::test]
async fn shutdown_fences_reconnect_before_lease_and_endpoint_expiry() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.reconnect-shutdown").await;
    service
        .disconnect_lsp_session(&registry, session.clone())
        .await;
    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.active_lsp_runtime_count().await, 1);
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 1);

    service.begin_shutdown().await;
    let response = service
        .reconnect_lsp_session(
            &registry,
            "request.reconnect-shutdown".to_owned(),
            session,
            now_millis(),
        )
        .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.active_lsp_runtime_count().await, 1);
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 1);

    service.expire_all().await;
    registry.lock().await.expire_at(u64::MAX);
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert_eq!(service.active_lsp_runtime_count().await, 0);
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
}

#[tokio::test]
async fn shutdown_joins_pending_lease_reclamation() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.shutdown").await;
    let retained_runtime_state = Arc::downgrade(&service.lsp_sessions);

    service.disconnect_lsp_session(&registry, session).await;
    service.expire_all().await;
    assert_eq!(
        service
            .lsp_lease_tasks
            .start(
                LspSessionId::new("lsp-after-shutdown").expect("session id"),
                std::future::ready(()),
            )
            .await,
        Err(DaemonInvocationProblem::Unavailable),
        "shutdown must close lease-task admission before draining"
    );
    drop(service);

    assert!(
        retained_runtime_state.upgrade().is_none(),
        "shutdown must join every pending LSP lease task"
    );
}
