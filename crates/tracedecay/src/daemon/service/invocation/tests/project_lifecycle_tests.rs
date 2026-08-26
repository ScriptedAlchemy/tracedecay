use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_application::{
    AuthorizedRootAdmission, AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, RegisteredRootLocatorV1, RequestContext,
    RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, ScopeSetId, ScopeSetRevision, UserProfileId, UtcMicros,
    WorktreeId, canonical_sha256,
};
use tracedecay_lsp::{
    AdmittedRoot, AuthorizedLspWorkspace, GatewayCapabilities, LspSessionRegistry,
    UnavailableSemanticProvider, UpstreamCapabilities,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{
    AuthorizedDaemonLspWorkspace, DaemonInvocationOutcome, DaemonInvocationProblem,
    DaemonInvocationRequest, DaemonInvocationService, DaemonLspInvocationOwner,
    DaemonLspOwnerRegistrar, DaemonLspSessionAccess, DaemonLspSessionFactory,
    RecordingFeedbackCycleObservations, UnavailableCancellationAuthority,
    UnavailableContextAuthority, UnavailableDiagnosticAuthority, unavailable_feedback_cycle,
    unavailable_lsp_session_factory,
};

fn recovery_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("LSP deadline")
}

async fn open_detached_session(
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    project_root: &Path,
    profile_id: &UserProfileId,
    project_id: &ProjectId,
    request_id: &str,
    factory: Arc<DaemonLspSessionFactory>,
    disconnect: bool,
) -> DaemonLspSessionAccess {
    DaemonLspOwnerRegistrar::new(service)
        .register_factory_for_project(
            project_root.to_path_buf(),
            profile_id.clone(),
            project_id.clone(),
            factory,
        )
        .await
        .expect("register LSP factory");
    let uri = url::Url::from_directory_path(project_root)
        .expect("absolute project path")
        .to_string();
    let response = service
        .invoke(
            registry,
            Some(project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(uri))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                request_id,
                env!("CARGO_PKG_VERSION"),
                None,
                Vec::new(),
                recovery_deadline(),
                CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
        panic!("expected LSP session");
    };
    if disconnect {
        service
            .disconnect_lsp_session(registry, session.clone())
            .await;
    }
    session
}

fn workspace_lsp_session_factory() -> Arc<DaemonLspSessionFactory> {
    Arc::new(DaemonLspSessionFactory::new(
        tokio::runtime::Handle::current(),
        Arc::new(unavailable_feedback_cycle(Arc::new(
            RecordingFeedbackCycleObservations::default(),
        ))),
        Arc::new(UnavailableSemanticProvider),
        Arc::new(UnavailableDiagnosticAuthority),
        Arc::new(UnavailableCancellationAuthority),
        Arc::new(UnavailableContextAuthority),
        GatewayCapabilities {
            supports_workspace_folders: true,
            ..GatewayCapabilities::default()
        },
        UpstreamCapabilities::default(),
    ))
}

async fn send_frame(
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    session: &DaemonLspSessionAccess,
    request_id: &str,
    frame: &str,
    now_ms: u64,
) {
    let response = service
        .send_lsp_frame(
            registry,
            request_id.to_owned(),
            session.clone(),
            frame.to_owned(),
            now_ms,
        )
        .await;
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspFrameAccepted {
            backpressured: false,
            closed: false
        }
    ));
}

#[tokio::test]
async fn recovery_quiescence_retires_only_the_selected_projects_lsp_owners() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(2)));
    let project_id = ProjectId::new("project.recovery-shared").expect("shared project");
    let profile_a = UserProfileId::new("profile.recovery-a").expect("profile A");
    let profile_b = UserProfileId::new("profile.recovery-b").expect("profile B");
    let root_a = PathBuf::from("/projects/recovery-a");
    let root_b = PathBuf::from("/projects/recovery-b");
    let session_a = open_detached_session(
        &service,
        &registry,
        &root_a,
        &profile_a,
        &project_id,
        "request.recovery-a",
        unavailable_lsp_session_factory(),
        true,
    )
    .await;
    let session_b = open_detached_session(
        &service,
        &registry,
        &root_b,
        &profile_b,
        &project_id,
        "request.recovery-b",
        workspace_lsp_session_factory(),
        false,
    )
    .await;
    send_frame(
        &service,
        &registry,
        &session_b,
        "request.initialize-b",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///projects/recovery-b","name":"B"}],"capabilities":{"workspace":{"workspaceFolders":true}}}}"#,
        1,
    )
    .await;
    send_frame(
        &service,
        &registry,
        &session_b,
        "request.initialized-b",
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        2,
    )
    .await;
    send_frame(
        &service,
        &registry,
        &session_b,
        "request.add-a-to-b",
        r#"{"jsonrpc":"2.0","method":"workspace/didChangeWorkspaceFolders","params":{"event":{"added":[{"uri":"file:///projects/recovery-a","name":"A"}],"removed":[]}}}"#,
        3,
    )
    .await;
    let pending_mutation = service
        .pending_lsp_workspace_folder_mutation(&session_b)
        .await
        .expect("B actor holds the A+B folder mutation");
    service
        .disconnect_lsp_session(&registry, session_b.clone())
        .await;
    let stale_a_owner = service
        .lsp_owner(Some(&root_a))
        .await
        .expect("pre-quiescence project A owner");
    let current_b_owner = service
        .lsp_owner(Some(&root_b))
        .await
        .expect("project B owner");
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 2);

    let quiescence = service
        .quiesce_project(
            &registry,
            &profile_a,
            &project_id,
            &[root_a.clone()].into_iter().collect(),
        )
        .await
        .expect("project A quiescence");
    assert_eq!(
        registry.lock().await.active_sessions(),
        1,
        "retiring a project must reclaim its protocol credential immediately"
    );
    let retired_credential = service
        .send_lsp_frame(
            &registry,
            "request.recovery-a-retired-credential".to_owned(),
            session_a.clone(),
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_owned(),
            4,
        )
        .await;
    assert!(matches!(
        retired_credential.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    let root_c = PathBuf::from("/projects/recovery-c");
    let session_c = open_detached_session(
        &service,
        &registry,
        &root_c,
        &profile_b,
        &ProjectId::new("project.recovery-c").expect("project C"),
        "request.recovery-c",
        unavailable_lsp_session_factory(),
        false,
    )
    .await;
    assert!(
        service
            .lsp_sessions
            .lock()
            .await
            .keys()
            .any(|id| id.as_str() == session_c.session_id),
        "the reclaimed protocol capacity is reusable without waiting for TTL"
    );

    let escaped = service
        .open_lsp_session(
            &registry,
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///projects/recovery-a",
            ))),
            "request.recovery-a-stale-owner".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            None,
            Vec::new(),
            0,
            Some(stale_a_owner.clone()),
        )
        .await;
    assert!(matches!(
        escaped.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    let capability =
        CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
            .expect("capability");
    let use_case = UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1)
        .expect("use case");
    let scope = |suffix: &str, project: &ProjectId| {
        ResolvedScope::new(
            project.clone(),
            RepositoryId::new(format!("repository.{suffix}")).expect("repository"),
            WorktreeId::new(format!("worktree.{suffix}")).expect("worktree"),
            None,
        )
        .expect("scope")
    };
    let scope_a = scope("recovery-a", &project_id);
    let scope_b = scope("recovery-b", &project_id);
    let admission =
        |suffix: &str, profile_id: &UserProfileId, scope: &ResolvedScope, root: &Path| {
            let grant = CapabilityGrantSnapshot::new(
                CapabilityGrantId::new(format!("grant.{suffix}")).expect("grant id"),
                1,
                canonical_sha256(&("grant.recovery", suffix)).expect("grant digest"),
                ActorId::new("actor.lsp.recovery").expect("actor"),
                UtcMicros(1),
                UtcMicros(10_000),
                scope.clone(),
                std::collections::BTreeSet::from([capability.clone()]),
                std::collections::BTreeSet::from([use_case.clone()]),
                DisclosureClass::Sensitive,
            )
            .expect("grant");
            let context = RequestContext::new(
                grant.issuer.clone(),
                scope.clone(),
                grant,
                RequestId::new(format!("request.{suffix}")).expect("request id"),
                Deadline::new(UtcMicros(9_000)).expect("deadline"),
                CancellationContext::active(format!("cancel.{suffix}")).expect("cancellation"),
            )
            .expect("request context");
            let locator = RegisteredRootLocatorV1::new(
                scope.project_id.clone(),
                profile_id.clone(),
                format!("store.{suffix}"),
                root,
            )
            .expect("locator");
            AuthorizedRootAdmission::new(context, locator).expect("root admission")
        };
    let scope_set = AuthorizedScopeSetAuthority::authorize_registered(
        ScopeSetId::new("scope-set.recovery-stale").expect("scope set id"),
        ScopeSetRevision::new(1).expect("revision"),
        vec![
            admission("recovery-a", &profile_a, &scope_a, &root_a),
            admission("recovery-b", &profile_b, &scope_b, &root_b),
        ],
        &capability,
        &use_case,
        UtcMicros(1),
    )
    .expect("scope set");
    let root_a_admission =
        AdmittedRoot::authorized("file:///projects/recovery-a", scope_a.scope_digest);
    let root_b_admission =
        AdmittedRoot::authorized("file:///projects/recovery-b", scope_b.scope_digest);
    let stale_federated_workspace = AuthorizedLspWorkspace::new(
        Some(scope_set.digest().clone()),
        vec![root_a_admission.clone(), root_b_admission.clone()],
    )
    .expect("federated workspace");
    service.authorized_lsp_workspaces.lock().await.insert(
        scope_set.digest().clone(),
        AuthorizedDaemonLspWorkspace {
            scope_set,
            factories: vec![
                (root_a_admission, stale_a_owner.factory),
                (root_b_admission, current_b_owner.factory.clone()),
            ],
        },
    );
    let escaped_through_b = service
        .open_lsp_session(
            &registry,
            Some(stale_federated_workspace.clone()),
            "request.recovery-a-stale-federation".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            None,
            Vec::new(),
            0,
            Some(current_b_owner),
        )
        .await;
    assert!(matches!(
        escaped_through_b.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    service
        .settle_lsp_workspace_folder_mutation(
            &session_b,
            &pending_mutation,
            Some(stale_federated_workspace),
        )
        .await;
    assert!(
        service
            .pending_lsp_workspace_folder_mutation(&session_b)
            .await
            .is_none(),
        "stale A+B mutation is rejected and settled"
    );
    let session_b_access = session_b.clone().into_access().expect("session B access");
    let sessions_after_settlement = service.lsp_sessions.lock().await;
    let workspace_after_settlement = sessions_after_settlement
        .get(session_b_access.session_id())
        .expect("B runtime session")
        .actor
        .workspace();
    assert_eq!(workspace_after_settlement.roots().len(), 1);
    assert_eq!(
        workspace_after_settlement.roots()[0].uri(),
        "file:///projects/recovery-b"
    );
    drop(sessions_after_settlement);

    let sessions = service.lsp_sessions.lock().await;
    assert!(
        !sessions
            .keys()
            .any(|id| id.as_str() == session_a.session_id)
    );
    assert!(
        sessions
            .keys()
            .any(|id| id.as_str() == session_b.session_id)
    );
    drop(sessions);
    assert_eq!(
        service.lsp_lease_tasks.active_tasks(),
        1,
        "project B's detached-session lease remains owned"
    );
    assert!(
        !service
            .project_runtimes
            .holds::<DaemonLspInvocationOwner>(&root_a)
            .await
    );
    assert!(
        service
            .project_runtimes
            .holds::<DaemonLspInvocationOwner>(&root_b)
            .await
    );

    drop(quiescence);
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            root_a,
            profile_a,
            project_id,
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("project A can republish after recovery");
}
