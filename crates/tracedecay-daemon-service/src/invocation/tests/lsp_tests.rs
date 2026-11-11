use tracedecay_contracts::SharedProfileStoreLocatorV1;
use tracedecay_domain::BrainId;

use std::path::PathBuf;
use std::time::Duration;

use super::{
    AdmittedRoot, Arc, DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    DaemonInvocationService, DaemonLspInvocationOwner, DaemonLspOwnerRegistrar,
    DaemonLspSessionAccess, LspSessionId, ProjectId, ResolvedScope, UtcMicros,
    admitted_root_fixture, canonicalize_lsp_roots, lsp_delivery_attempt, now_millis,
    retain_lsp_delivery_attempt, unavailable_lsp_session_factory,
};
use crate::shutdown::ShutdownStatus;
use tokio::sync::Mutex;
use tracedecay_contracts::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    ObservabilityQueryPort,
};
use tracedecay_domain::{ActorId, UserProfileId, canonical_sha256};
use tracedecay_lsp::{AuthorizedLspWorkspace, LSP_SESSION_TTL_MS, LspSessionRegistry};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracing_subscriber::layer::SubscriberExt;

fn bridge_lsp_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("LSP deadline")
}

fn bridge_lsp_cancellation() -> CancellationContext {
    CancellationContext::active("cancel.lsp.bridge-backpressure").expect("LSP cancellation")
}

#[tokio::test]
async fn production_lsp_bridge_retries_only_an_unconsumed_full_queue_frame() {
    let service = DaemonInvocationService::default();
    let (project_root, root_uri) = admitted_root_fixture("bridge-backpressure");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            tracedecay_domain::UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            tracedecay_domain::ProjectId::new("project.test.lsp").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("register LSP owner");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let response = service
        .invoke(
            &registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                root_uri.clone(),
            ))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                "request.bridge.open",
                "client.bridge",
                Some(root_uri.clone()),
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
    let (path_a, uri_a) = admitted_root_fixture("a");
    let (path_b, uri_b) = admitted_root_fixture("b");
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
    let locator_a = tracedecay_contracts::RegisteredRootLocatorV1::new(
        ProjectId::new("project.a").unwrap(),
        SharedProfileStoreLocatorV1::new(
            BrainId::new("brain.fixture").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.fixture").unwrap(),
            "store.fixture",
        )
        .unwrap(),
        &path_a,
    )
    .unwrap();
    let locator_b = tracedecay_contracts::RegisteredRootLocatorV1::new(
        ProjectId::new("project.b").unwrap(),
        SharedProfileStoreLocatorV1::new(
            BrainId::new("brain.fixture").unwrap(),
            tracedecay_domain::UserProfileId::new("profile.fixture").unwrap(),
            "store.fixture",
        )
        .unwrap(),
        &path_b,
    )
    .unwrap();
    let mut forward = vec![
        (
            path_a.clone(),
            uri_a.clone(),
            scope_a.clone(),
            locator_a.clone(),
        ),
        (
            path_b.clone(),
            uri_b.clone(),
            scope_b.clone(),
            locator_b.clone(),
        ),
    ];
    let mut reverse = vec![
        (path_b, uri_b, scope_b, locator_b),
        (path_a, uri_a, scope_a, locator_a),
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
    let capability = CapabilityId::new(crate::LSP_WORKSPACE_CAPABILITY_ID_V1).unwrap();
    let use_case = UseCaseId::new(crate::LSP_WORKSPACE_USE_CASE_ID_V1).unwrap();
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
            DaemonLspInvocationOwner::for_test_project(
                unavailable_lsp_session_factory(),
                UserProfileId::new("profile.test.lsp-linked").unwrap(),
                expected.project_id.clone(),
                root.clone(),
            )
            .with_scope_grant(grant),
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
    _profile: tempfile::TempDir,
    /// The runtime owns the daemon database scope every durable settlement
    /// write is admitted under. Dropping it leaves the lease without an active
    /// write scope, so the recorder's replay would retain every receipt
    /// instead of settling it.
    _runtime: tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
    _project: tempfile::TempDir,
    project_id: ProjectId,
    recorder: Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
    authority: Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
    producer: Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>,
    db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
}

async fn lsp_delivery_fixture() -> LspDeliveryFixture {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.lsp.delivery").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        profile.path(),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = tracedecay_application::observability::ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:lsp-delivery".to_owned(),
        producer_revision: "lsp-delivery-producer.v1".to_owned(),
        configuration_revision: "lsp-delivery-config.v1".to_owned(),
        policy_revision: "lsp-delivery-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        tracedecay_application::observability::BoundedObservabilityProducerV1::start(
            db.clone(),
            identity.clone(),
            8,
        )
        .expect("producer"),
    );
    let authority = Arc::new(
        tracedecay_application::observability::DeliverySettlementAuthorityV1::new(
            db.clone(),
            Arc::clone(&producer),
            identity,
        )
        .expect("settlement authority"),
    );
    let recorder = Arc::new(
        tracedecay_application::observability::BoundedDeliverySettlementRecorderV1::start(
            Arc::clone(&authority),
            8,
        )
        .expect("settlement recorder"),
    );
    LspDeliveryFixture {
        _profile: profile,
        _runtime: runtime,
        _project: project,
        project_id,
        recorder,
        authority,
        producer,
        db,
    }
}

async fn mounted_lsp_delivery_fixture(service: &DaemonInvocationService) -> LspDeliveryFixture {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.lsp.delivery").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        profile.path(),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let revision = canonical_sha256(&"lsp-delivery-runtime").expect("revision");
    let producer = service
        .mount_observability_producer(
            project.path().to_path_buf(),
            db.clone(),
            project_id.clone(),
            revision.clone(),
            revision,
        )
        .await
        .expect("mounted observability producer");
    let recorder = service
        .delivery_settlement_recorder(Some(project.path()))
        .await
        .expect("mounted settlement recorder");
    let authority = service
        .delivery_settlement_authority(Some(project.path()))
        .await
        .expect("settlement authority lookup")
        .expect("mounted settlement authority");
    LspDeliveryFixture {
        _profile: profile,
        _runtime: runtime,
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
            DaemonLspInvocationOwner::for_test_project(
                unavailable_lsp_session_factory(),
                UserProfileId::new("profile.test.lsp-delivery").expect("profile"),
                ProjectId::new("project.test.lsp-delivery").expect("project"),
                project_root.clone(),
            )
            .with_delivery_settlements(Arc::clone(&fixture.recorder)),
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
    assert_one_lsp_delivery_settlement(fixture, 0, 1).await;
}

async fn assert_one_lsp_delivery_settlement(
    fixture: LspDeliveryFixture,
    delivered: u16,
    dropped: u16,
) {
    let summary = fixture
        .recorder
        .shutdown()
        .await
        .expect("drain LSP delivery recorder");
    assert_eq!(
        (summary.settled, summary.failed, summary.retained),
        (1, 0, 0),
        "one outbound frame must settle durably with nothing refused or retained: {summary:?}"
    );
    drop(fixture.recorder);
    drop(fixture.authority);
    let Ok(producer) = Arc::try_unwrap(fixture.producer) else {
        panic!("LSP delivery components must release the producer")
    };
    producer.shutdown().await.expect("flush LSP observability");
    assert_lsp_delivery_observation(&fixture.db, &fixture.project_id, delivered, dropped).await;
}

async fn assert_lsp_delivery_observation(
    db: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    project_id: &ProjectId,
    delivered: u16,
    dropped: u16,
) {
    let page =
        tracedecay_application::observability::RegisteredObservabilityPortV1::new(db.as_ref())
            .query(tracedecay_contracts::ObservabilityQueryV1 {
                authorized_scope_ref: project_id.as_str().to_owned(),
                event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
                horizon: tracedecay_contracts::ObservabilityHorizonV1 {
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
    assert_eq!(fanout.delivered, delivered);
    assert_eq!(fanout.dropped, dropped);
    assert_eq!(fanout.unknown, 0);
}

/// Records the thread that opens each span of one name.
struct SpanThreads {
    span_name: &'static str,
    threads: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanThreads {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attributes.metadata().name() == self.span_name {
            self.threads
                .lock()
                .expect("span thread log")
                .push(std::thread::current().id());
        }
    }
}

/// An acknowledgement publishes its delivery receipt to the recorder's spool
/// with file and directory fsyncs. Measured on an idle macOS disk that is
/// 8 to 13 ms and all of the acknowledge span's busy time, 50 to 80 times any
/// other LSP operation, and it ran on the serving thread under the daemon-wide
/// session map lock. On a disk whose fsyncs stall, that parked the serving
/// worker and every invocation behind the lock until the client's own
/// deadline lapsed. The receipt must still be durable before the
/// acknowledgement is answered, but it must be admitted off the serving thread.
#[tokio::test]
async fn lsp_acknowledgement_admits_its_delivery_receipt_off_the_serving_thread() {
    let fixture = lsp_delivery_fixture().await;
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "ack").await;

    let spool_publishers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let acknowledged = {
        let _capture = tracedecay_runtime_core::logging::set_tracing_capture(
            tracing_subscriber::registry().with(SpanThreads {
                span_name: "private_fs.framed_log.publish",
                threads: Arc::clone(&spool_publishers),
            }),
        );
        service
            .invoke(
                &registry,
                None,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_acknowledge(
                    "request.lsp-delivery.ack.ack",
                    session,
                    bridge_lsp_deadline(),
                    bridge_lsp_cancellation(),
                ),
            )
            .await
    };
    assert!(matches!(
        acknowledged.outcome,
        DaemonInvocationOutcome::LspAcknowledged { acknowledged: true }
    ));
    {
        let publishers = spool_publishers.lock().expect("span thread log");
        assert!(
            !publishers.is_empty(),
            "observe actual durable receipt publication"
        );
        assert!(
            !publishers.contains(&std::thread::current().id()),
            "the acknowledgement published its receipt on the serving thread"
        );
    }

    drop(service);
    assert_one_lsp_delivery_settlement(fixture, 1, 0).await;
}

/// Pause the real recorder before admission, using its production latency span.
struct PausedDeliveryAdmission {
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for PausedDeliveryAdmission {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attributes.metadata().name() == "daemon.service.lsp.delivery_receipt.admit"
            && let Some(started) = self.started.lock().expect("admission signal").take()
        {
            started.send(()).expect("admission waiter");
            self.release
                .lock()
                .expect("admission release")
                .recv_timeout(Duration::from_secs(5))
                .expect("release receipt admission");
        }
    }
}

#[tokio::test]
async fn cancelled_lsp_acknowledgement_pins_its_recorder_until_durable_admission() {
    let service = DaemonInvocationService::default();
    let fixture = mounted_lsp_delivery_fixture(&service).await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "cancelled-ack").await;
    let (started, admission_started) = tokio::sync::oneshot::channel();
    let (release, admission_release) = std::sync::mpsc::channel();
    let _capture = tracedecay_runtime_core::logging::set_tracing_capture(
        tracing_subscriber::registry().with(PausedDeliveryAdmission {
            started: std::sync::Mutex::new(Some(started)),
            release: std::sync::Mutex::new(admission_release),
        }),
    );
    let mut acknowledgement = Box::pin(service.invoke(
        &registry,
        None,
        None,
        None,
        None,
        DaemonInvocationRequest::lsp_acknowledge(
            "request.lsp-delivery.cancelled-ack",
            session,
            bridge_lsp_deadline(),
            bridge_lsp_cancellation(),
        ),
    ));
    tokio::select! {
        response = &mut acknowledgement => panic!("acknowledgement outran admission: {response:?}"),
        started = tokio::time::timeout(Duration::from_secs(2), admission_started) => {
            started.expect("receipt admission started").expect("admission signal");
        }
    }
    drop(acknowledgement);

    let (drain_waiting, drain_is_waiting) = tokio::sync::oneshot::channel();
    service
        .project_runtimes
        .arm_shutdown_drain_waiting(drain_waiting);
    let mut shutdown = Box::pin(service.expire_all(None));
    tokio::select! {
        joined = &mut shutdown => panic!("shutdown outran the claimed receipt: {joined:?}"),
        waiting = tokio::time::timeout(Duration::from_secs(2), drain_is_waiting) => {
            waiting.expect("shutdown joins admitted delivery").expect("drain signal");
        }
    }
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert!(!service.project_runtimes.is_empty());
    release.send(()).expect("release durable admission");
    let joined = tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("shutdown after durable receipt");
    assert_eq!(joined.status, ShutdownStatus::Clean);
    assert!(
        joined.lost_work.is_empty(),
        "receipt must drain without loss: {joined:?}"
    );
    assert!(service.project_runtimes.is_empty());
    assert_lsp_delivery_observation(&fixture.db, &fixture.project_id, 1, 0).await;
}

#[tokio::test]
async fn cancelled_lsp_detach_reclaims_its_lease_before_waiting_for_the_receipt() {
    let fixture = lsp_delivery_fixture().await;
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "cancelled-detach").await;
    service
        .disconnect_lsp_session(&registry, session.clone())
        .await;
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 1);
    let (started, admission_started) = tokio::sync::oneshot::channel();
    let (release, admission_release) = std::sync::mpsc::channel();
    let _capture = tracedecay_runtime_core::logging::set_tracing_capture(
        tracing_subscriber::registry().with(PausedDeliveryAdmission {
            started: std::sync::Mutex::new(Some(started)),
            release: std::sync::Mutex::new(admission_release),
        }),
    );
    let mut detach = Box::pin(service.invoke(
        &registry,
        None,
        None,
        None,
        None,
        DaemonInvocationRequest::lsp_detach(
            "request.lsp-delivery.cancelled-detach",
            session,
            bridge_lsp_deadline(),
            bridge_lsp_cancellation(),
        ),
    ));
    tokio::select! {
        response = &mut detach => panic!("detach outran receipt admission: {response:?}"),
        started = tokio::time::timeout(Duration::from_secs(2), admission_started) => {
            started.expect("receipt admission started").expect("admission signal");
        }
    }
    drop(detach);
    assert_eq!(service.lsp_lease_tasks.active_tasks(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(registry.lock().await.active_sessions(), 0);
    release.send(()).expect("release durable admission");
    let joined = tokio::time::timeout(Duration::from_secs(5), service.expire_all(None))
        .await
        .expect("join cancelled detach admission");
    assert_eq!(joined.status, ShutdownStatus::Clean);
    assert_eq!(joined.lost_work, Vec::<String>::new());
    drop(service);
    assert_one_lsp_delivery_drop(fixture).await;
}

#[tokio::test]
async fn lsp_retirement_refuses_settlement_before_taking_the_in_flight_attempt() {
    let fixture = lsp_delivery_fixture().await;
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "retirement").await;
    let access = session.clone().into_access().expect("session access");
    let attempt = service
        .lsp_sessions
        .lock()
        .await
        .get(access.session_id())
        .expect("polled session")
        .in_flight_delivery_attempt
        .clone()
        .expect("captured delivery attempt");
    service.begin_shutdown().await;
    for request in [
        DaemonInvocationRequest::lsp_acknowledge(
            "request.lsp-delivery.retirement.ack",
            session.clone(),
            bridge_lsp_deadline(),
            bridge_lsp_cancellation(),
        ),
        DaemonInvocationRequest::lsp_detach(
            "request.lsp-delivery.retirement.detach",
            session,
            bridge_lsp_deadline(),
            bridge_lsp_cancellation(),
        ),
    ] {
        let response = service
            .invoke(&registry, None, None, None, None, request)
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ));
        let sessions = service.lsp_sessions.lock().await;
        let session = sessions
            .get(access.session_id())
            .expect("unclaimed session");
        assert_eq!(session.in_flight_delivery_attempt.as_ref(), Some(&attempt));
        assert_eq!(registry.lock().await.active_sessions(), 1);
    }
    let joined = service.expire_all(None).await;
    assert_eq!(joined.status, ShutdownStatus::Clean);
    assert_eq!(joined.lost_work, Vec::<String>::new());
    drop(service);
    assert_one_lsp_delivery_drop(fixture).await;
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

#[tokio::test]
async fn lsp_disconnect_expiry_settles_unacknowledged_outbound_as_dropped() {
    let fixture = lsp_delivery_fixture().await;
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let session = open_polled_lsp_delivery(&fixture, &service, &registry, "expiry").await;

    service.disconnect_lsp_session(&registry, session).await;
    service
        .lsp_lease_tasks
        .expire_leases_through(now_millis() + LSP_SESSION_TTL_MS);
    service.lsp_lease_tasks.wait_until_idle().await;

    assert!(service.lsp_sessions.lock().await.is_empty());
    assert_eq!(registry.lock().await.active_sessions(), 0);
    drop(service);
    assert_one_lsp_delivery_drop(fixture).await;
}
