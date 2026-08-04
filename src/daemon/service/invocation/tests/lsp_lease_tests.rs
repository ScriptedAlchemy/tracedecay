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
async fn shutdown_joins_pending_lease_reclamation() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let session = open_session(&service, &registry, "request.shutdown").await;
    let retained_runtime_state = Arc::downgrade(&service.lsp_sessions);

    service.disconnect_lsp_session(&registry, session).await;
    service.expire_all().await;
    drop(service);

    assert!(
        retained_runtime_state.upgrade().is_none(),
        "shutdown must join every pending LSP lease task"
    );
}
