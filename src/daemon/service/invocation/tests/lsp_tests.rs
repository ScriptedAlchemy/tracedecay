//! `lsp` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

fn bridge_lsp_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("LSP deadline")
}

fn bridge_lsp_cancellation() -> CancellationContext {
    CancellationContext::active("cancel.lsp.bridge-backpressure").expect("LSP cancellation")
}

#[tokio::test]
async fn production_lsp_bridge_retries_only_an_unconsumed_full_queue_frame() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/bridge-backpressure");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory(project_root.clone(), unavailable_lsp_session_factory())
        .await
        .expect("register LSP owner");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let response = service
        .invoke(
            &registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///bridge-backpressure",
            ))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                "request.bridge.open",
                "client.bridge",
                Some("file:///bridge-backpressure".to_owned()),
                Vec::new(),
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
        panic!("expected an admitted LSP session");
    };

    let mut deferred = None;
    for sequence in 0..=tracedecay_lsp::MAX_QUEUED_OUTBOUND_MESSAGES {
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": sequence,
            "method": "tracedecay/testQueueAdmission",
            "params": {},
        })
        .to_string();
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_frame(
                    format!("request.bridge.fill.{sequence}"),
                    session.clone(),
                    frame.clone(),
                    bridge_lsp_deadline(),
                    bridge_lsp_cancellation(),
                ),
            )
            .await;
        let DaemonInvocationOutcome::LspFrameAccepted {
            backpressured,
            closed,
        } = response.outcome
        else {
            panic!("expected typed LSP frame admission");
        };
        assert!(!closed);
        if backpressured {
            deferred = Some((
                u64::try_from(sequence).expect("bounded queue sequence fits u64"),
                frame,
            ));
            break;
        }
    }
    let (deferred_id, deferred_frame) =
        deferred.expect("bounded outbound queue must eventually apply backpressure");

    let mut delivered_ids = Vec::new();
    for sequence in 0..tracedecay_lsp::MAX_QUEUED_OUTBOUND_MESSAGES {
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_poll(
                    format!("request.bridge.poll.{sequence}"),
                    session.clone(),
                    bridge_lsp_deadline(),
                    bridge_lsp_cancellation(),
                ),
            )
            .await;
        let DaemonInvocationOutcome::LspFrame {
            frame,
            closed: false,
        } = response.outcome
        else {
            panic!("expected typed LSP frame poll");
        };
        let Some(frame) = frame else {
            break;
        };
        let response: serde_json::Value =
            serde_json::from_str(&frame).expect("queued frame must be JSON-RPC");
        delivered_ids.push(
            response["id"]
                .as_u64()
                .expect("queued response must retain its request id"),
        );
        let acknowledged = service
            .invoke(
                &registry,
                None,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_acknowledge(
                    format!("request.bridge.ack.{sequence}"),
                    session.clone(),
                    bridge_lsp_deadline(),
                    bridge_lsp_cancellation(),
                ),
            )
            .await;
        assert!(matches!(
            acknowledged.outcome,
            DaemonInvocationOutcome::LspAcknowledged { acknowledged: true }
        ));
    }
    assert!(
        !delivered_ids.contains(&deferred_id),
        "a backpressured frame must not have been consumed"
    );

    let retried = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_frame(
                "request.bridge.retry",
                session.clone(),
                deferred_frame,
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        retried.outcome,
        DaemonInvocationOutcome::LspFrameAccepted {
            backpressured: false,
            closed: false,
        }
    ));
    let delivered = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_poll(
                "request.bridge.retry.poll",
                session,
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspFrame {
        frame: Some(frame),
        closed: false,
    } = delivered.outcome
    else {
        panic!("retried frame must produce one response");
    };
    let response: serde_json::Value =
        serde_json::from_str(&frame).expect("retried response must be JSON-RPC");
    assert_eq!(response["id"].as_u64(), Some(deferred_id));
}

#[test]
fn lsp_scope_roots_canonicalize_independent_of_folder_order() {
    let scope_a = ResolvedScope::new(
        ProjectId::new("project.a").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.a").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.a").unwrap(),
        None,
    )
    .unwrap();
    let scope_b = ResolvedScope::new(
        ProjectId::new("project.b").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.b").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.b").unwrap(),
        None,
    )
    .unwrap();
    let locator_a = tracedecay_application::RegisteredRootLocatorV1::new(
        ProjectId::new("project.a").unwrap(),
        tracedecay_domain::UserProfileId::new("profile.fixture").unwrap(),
        "store.a",
        "/a",
    )
    .unwrap();
    let locator_b = tracedecay_application::RegisteredRootLocatorV1::new(
        ProjectId::new("project.b").unwrap(),
        tracedecay_domain::UserProfileId::new("profile.fixture").unwrap(),
        "store.b",
        "/b",
    )
    .unwrap();
    let mut forward = vec![
        (
            PathBuf::from("/a"),
            "file:///a".to_owned(),
            scope_a.clone(),
            locator_a.clone(),
        ),
        (
            PathBuf::from("/b"),
            "file:///b".to_owned(),
            scope_b.clone(),
            locator_b.clone(),
        ),
    ];
    let mut reverse = vec![
        (
            PathBuf::from("/b"),
            "file:///b".to_owned(),
            scope_b,
            locator_b,
        ),
        (
            PathBuf::from("/a"),
            "file:///a".to_owned(),
            scope_a,
            locator_a,
        ),
    ];

    assert!(canonicalize_lsp_roots(&mut forward));
    assert!(canonicalize_lsp_roots(&mut reverse));
    assert_eq!(forward, reverse);
}

#[tokio::test]
async fn linked_workspace_owner_requires_its_exact_registered_scope() {
    let root = PathBuf::from("/linked/worktree");
    let expected = ResolvedScope::new(
        ProjectId::new("project.linked").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.linked").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.expected").unwrap(),
        None,
    )
    .unwrap();
    let sibling = ResolvedScope::new(
        ProjectId::new("project.linked").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.linked").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.sibling").unwrap(),
        None,
    )
    .unwrap();
    let capability =
        CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
            .unwrap();
    let use_case =
        UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.lsp.linked").unwrap(),
        1,
        canonical_sha256(&"grant.lsp.linked").unwrap(),
        ActorId::new("actor.lsp.linked").unwrap(),
        UtcMicros(1),
        UtcMicros(10_000),
        expected.clone(),
        std::collections::BTreeSet::from([capability]),
        std::collections::BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    let service = DaemonInvocationService::default();
    service
        .install_lsp_owner(
            root.clone(),
            DaemonLspInvocationOwner {
                factory: unavailable_lsp_session_factory(),
                scope_grant: Some(grant),
                scope_set_storage: None,
            },
        )
        .await
        .unwrap();

    assert!(service.lsp_owner_matches_scope(&root, &expected).await);
    assert!(!service.lsp_owner_matches_scope(&root, &sibling).await);
}
