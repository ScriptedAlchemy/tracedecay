//! `lsp` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

use tracedecay_application::ObservabilityQueryPort;

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
                project_identity: InvocationProjectRuntimeIdentityV1::new(
                    UserProfileId::new("profile.test.lsp-linked").unwrap(),
                    expected.project_id.clone(),
                    root.clone(),
                ),
                factory: unavailable_lsp_session_factory(),
                scope_grant: Some(grant),
                scope_set_storage: None,
                delivery_settlements: None,
            },
        )
        .await
        .unwrap();

    assert!(service.lsp_owner_matches_scope(&root, &expected).await);
    assert!(!service.lsp_owner_matches_scope(&root, &sibling).await);
}

#[test]
fn lsp_delivery_identity_isolated_by_exact_session_authority() {
    let frame = br#"{"jsonrpc":"2.0","method":"tracedecay/testDelivery","params":{}}"#;
    let first_session = LspSessionId::new("lsp-delivery-first").expect("first session");
    let second_session = LspSessionId::new("lsp-delivery-second").expect("second session");

    let first = lsp_delivery_attempt(frame, &first_session, 1, UtcMicros(100))
        .expect("first delivery attempt");
    let second = lsp_delivery_attempt(frame, &second_session, 1, UtcMicros(100))
        .expect("second delivery attempt");

    assert_ne!(
        first.owner_event_id, second.owner_event_id,
        "identical LSP frames from separate sessions must own separate delivery events"
    );
    assert_ne!(
        first.channel, second.channel,
        "separate LSP sessions must retain separate delivery channels"
    );
}

#[test]
fn lsp_delivery_retry_reuses_the_exact_original_attempt() {
    let frame = br#"{"jsonrpc":"2.0","method":"tracedecay/testDelivery","params":{}}"#;
    let session = LspSessionId::new("lsp-delivery-retry").expect("session");
    let mut retained = None;
    let mut next_sequence = 1;

    let first = retain_lsp_delivery_attempt(
        &mut retained,
        &mut next_sequence,
        frame,
        &session,
        UtcMicros(100),
    )
    .expect("first delivery attempt");
    let retry = retain_lsp_delivery_attempt(
        &mut retained,
        &mut next_sequence,
        frame,
        &session,
        UtcMicros(200),
    )
    .expect("retried delivery attempt");

    assert_eq!(
        first, retry,
        "a retry must reuse its original immutable delivery attempt despite a later clock reading"
    );
}

#[test]
fn lsp_delivery_same_session_identical_frames_after_ack_have_distinct_events() {
    let frame = br#"{"jsonrpc":"2.0","method":"tracedecay/testDelivery","params":{}}"#;
    let session = LspSessionId::new("lsp-delivery-repeat").expect("session");
    let mut retained = None;
    let mut next_sequence = 1;

    let first = retain_lsp_delivery_attempt(
        &mut retained,
        &mut next_sequence,
        frame,
        &session,
        UtcMicros(100),
    )
    .expect("first delivery attempt");
    retained = None;
    let second = retain_lsp_delivery_attempt(
        &mut retained,
        &mut next_sequence,
        frame,
        &session,
        UtcMicros(200),
    )
    .expect("second delivery attempt");

    assert_ne!(
        first.owner_event_id, second.owner_event_id,
        "a later identical outbound frame in the same session must not replay the prior event"
    );
}

struct LspDeliveryFixture {
    _pin: tracedecay_runtime_core::config::PinnedUserDataDir,
    _project: tempfile::TempDir,
    project_id: ProjectId,
    recorder: Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>,
    authority: Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>,
    producer: Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    db: crate::global_db::RegisteredGlobalDbLeaseV1,
}

async fn lsp_delivery_fixture() -> LspDeliveryFixture {
    let pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.lsp.delivery").expect("project id");
    let runtime = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = tracedecay_usecases::observability::ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:lsp-delivery".to_owned(),
        producer_revision: "lsp-delivery-producer.v1".to_owned(),
        configuration_revision: "lsp-delivery-config.v1".to_owned(),
        policy_revision: "lsp-delivery-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        tracedecay_usecases::observability::BoundedObservabilityProducerV1::start(
            db.clone(),
            identity.clone(),
            8,
        )
        .expect("producer"),
    );
    let authority = Arc::new(
        tracedecay_usecases::observability::DeliverySettlementAuthorityV1::new(
            db.clone(),
            Arc::clone(&producer),
            identity,
        )
        .expect("settlement authority"),
    );
    let recorder = Arc::new(
        tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1::start(
            Arc::clone(&authority),
            8,
        )
        .expect("settlement recorder"),
    );
    LspDeliveryFixture {
        _pin: pin,
        _project: project,
        project_id,
        recorder,
        authority,
        producer,
        db,
    }
}

async fn open_polled_lsp_delivery(
    fixture: &LspDeliveryFixture,
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    request_suffix: &str,
) -> DaemonLspSessionAccess {
    let project_root = fixture._project.path().to_path_buf();
    let root_uri = url::Url::from_directory_path(&project_root)
        .expect("project root URI")
        .to_string();
    service
        .install_lsp_owner(
            project_root.clone(),
            DaemonLspInvocationOwner {
                project_identity: InvocationProjectRuntimeIdentityV1::new(
                    UserProfileId::new("profile.test.lsp-delivery").expect("profile"),
                    ProjectId::new("project.test.lsp-delivery").expect("project"),
                    project_root.clone(),
                ),
                factory: unavailable_lsp_session_factory(),
                scope_grant: None,
                scope_set_storage: None,
                delivery_settlements: Some(Arc::clone(&fixture.recorder)),
            },
        )
        .await
        .expect("install LSP owner");
    let opened = service
        .invoke(
            registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(root_uri))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                format!("request.lsp-delivery.{request_suffix}.open"),
                "lsp-delivery-client",
                None,
                Vec::new(),
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspOpened { session, .. } = opened.outcome else {
        panic!("expected LSP session");
    };

    let accepted = service
        .invoke(
            registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_frame(
                format!("request.lsp-delivery.{request_suffix}.frame"),
                session.clone(),
                r#"{"jsonrpc":"2.0","id":1,"method":"tracedecay/testQueueAdmission","params":{}}"#,
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        accepted.outcome,
        DaemonInvocationOutcome::LspFrameAccepted {
            backpressured: false,
            closed: false,
        }
    ));
    let polled = service
        .invoke(
            registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_poll(
                format!("request.lsp-delivery.{request_suffix}.poll"),
                session.clone(),
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        polled.outcome,
        DaemonInvocationOutcome::LspFrame {
            frame: Some(_),
            closed: false,
        }
    ));
    session
}

async fn assert_one_lsp_delivery_drop(fixture: LspDeliveryFixture) {
    let summary = fixture
        .recorder
        .shutdown()
        .await
        .expect("drain LSP delivery recorder");
    assert_eq!(summary.settled, 1, "one outbound frame must settle");
    assert_eq!(summary.failed, 0, "terminal LSP drop must persist");
    drop(fixture.recorder);
    drop(fixture.authority);
    let producer = match Arc::try_unwrap(fixture.producer) {
        Ok(producer) => producer,
        Err(_) => panic!("LSP delivery components must release the producer"),
    };
    producer.shutdown().await.expect("flush LSP observability");
    let page =
        tracedecay_usecases::observability::RegisteredObservabilityPortV1::new(fixture.db.as_ref())
            .query(tracedecay_application::ObservabilityQueryV1 {
                authorized_scope_ref: fixture.project_id.as_str().to_owned(),
                event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
                horizon: tracedecay_application::ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: i64::MAX,
                },
                after_watermark: None,
                limit: 8,
            })
            .await
            .expect("read LSP delivery observation");
    let [event] = page.events.as_slice() else {
        panic!("one terminal LSP delivery observation is required");
    };
    let tracedecay_domain::ObservabilityPayloadV1::WorkDeliveryFanout(fanout) = &event.payload
    else {
        panic!("expected LSP delivery fanout observation");
    };
    assert_eq!(fanout.delivered, 0);
    assert_eq!(fanout.dropped, 1);
    assert_eq!(fanout.unknown, 0);
}

#[tokio::test]
async fn lsp_detach_after_unacknowledged_outbound_records_disconnected_drop() {
    let fixture = lsp_delivery_fixture().await;
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "detach").await;
    let detached = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_detach(
                "request.lsp-delivery.detach",
                session,
                bridge_lsp_deadline(),
                bridge_lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        detached.outcome,
        DaemonInvocationOutcome::LspDetached
    ));

    drop(service);
    assert_one_lsp_delivery_drop(fixture).await;
}

#[tokio::test(start_paused = true)]
async fn lsp_disconnect_expiry_settles_unacknowledged_outbound_as_dropped() {
    let fixture = lsp_delivery_fixture().await;
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "expiry").await;

    service.disconnect_lsp_session(&registry, session).await;
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(LSP_SESSION_TTL_MS)).await;
    tokio::task::yield_now().await;

    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(registry.lock().await.active_sessions(), 0);
    drop(service);
    assert_one_lsp_delivery_drop(fixture).await;
}
