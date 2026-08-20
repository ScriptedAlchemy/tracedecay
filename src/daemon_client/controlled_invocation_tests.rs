use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{DaemonInvocationClient, InvocationCancellationPolicy};
use crate::client_identity::DaemonClientIdentity;
use crate::daemon::{DaemonConnection, DaemonHandshake};
use crate::daemon_contract::{
    CanonicalQualificationBlob, DaemonInvocationOutcome, DaemonInvocationPayload,
    DaemonInvocationProblem, DaemonInvocationRequest, DaemonInvocationResponse,
    parse_daemon_invocation_cancellation_request,
};
use tracedecay_application::{CancellationContext, CancellationSignal, Deadline};
use tracedecay_domain::UtcMicros;

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn deadline_after(duration: Duration) -> Deadline {
    let now = now_micros();
    let delta = i64::try_from(duration.as_micros()).unwrap_or(i64::MAX);
    Deadline::new(UtcMicros(now.0.saturating_add(delta))).expect("deadline")
}

fn semantic_qualification_candidate()
-> tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1 {
    let material = crate::search_eval::load_default_evaluated_profile_material("query-fallback")
        .expect("checked-in query fallback profile");
    tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1 {
        evaluated_profile_id: "query-fallback".to_owned(),
        profile: tracedecay_usecases::semantic_runtime::SemanticEvaluationFusionCandidateV1 {
            profile_id: material.profile.profile_id.clone(),
            calibrations: material.profile.calibrations.clone(),
            score_domain_calibrations: material.profile.score_domain_calibrations.clone(),
            weights_micros: material.profile.weights_micros.clone(),
            diversity_policy_id: material.profile.diversity_policy_id.clone(),
            rerank_policy_id: material.profile.rerank_policy_id.clone(),
            retrieval_budget: material.profile.retrieval_budget,
        },
        diversity: tracedecay_usecases::semantic_runtime::SemanticEvaluationDiversityCandidateV1 {
            policy_id: material.diversity.policy_id.clone(),
            per_source_namespace: material.diversity.per_source_namespace,
            per_source_instance: material.diversity.per_source_instance,
            per_repository: material.diversity.per_repository,
            per_file: material.diversity.per_file,
            per_session_or_thread: material.diversity.per_session_or_thread,
            per_copy_cluster: material.diversity.per_copy_cluster,
            per_evidence_role: material.diversity.per_evidence_role,
        },
        rerank: None,
        compatibility:
            tracedecay_usecases::config::retrieval::RetrievalCompatibilityPinsV1::default(),
    }
}

fn invocation_request(request_id: &str, deadline: Deadline) -> DaemonInvocationRequest {
    let observed_at = now_micros();
    DaemonInvocationRequest::feedback(
        request_id,
        crate::application_surface::ApplicationSurfaceOperation::FeedbackList,
        "feedback.remote-controlled-settlement".to_owned(),
        observed_at,
        deadline,
        CancellationContext::active(format!("cancel.{request_id}")).expect("request cancellation"),
    )
}

async fn controlled_client(
    request_id: &'static str,
) -> (
    DaemonInvocationClient,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (listener, endpoint) = crate::daemon::transport::BrokerListener::bind(
        &crate::daemon::transport::default_loopback_endpoint(),
    )
    .await
    .expect("bind invocation listener");
    let (request_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, mut invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        assert_eq!(request.request_id, request_id);
        let _ = request_admitted.send(());

        let control_stream = listener.accept().await.expect("accept cancellation");
        let (control_reader, _control_writer) = control_stream.into_split();
        let mut control_lines = BufReader::new(control_reader).lines();
        control_lines
            .next_line()
            .await
            .expect("read cancellation handshake")
            .expect("cancellation handshake");
        let cancellation_line = control_lines
            .next_line()
            .await
            .expect("read cancellation request")
            .expect("cancellation request");
        let cancellation = parse_daemon_invocation_cancellation_request(&cancellation_line)
            .expect("typed invocation cancellation");
        assert_eq!(cancellation.target_request_id(), request_id);

        let response =
            DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::ResetRequired);
        invocation_writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write authoritative settlement");
        invocation_writer
            .write_all(b"\n")
            .await
            .expect("write response newline");
        invocation_writer
            .flush()
            .await
            .expect("flush authoritative settlement");
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let handshake = DaemonHandshake {
        project_path: Some(profile_root.clone()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: format!("client.{request_id}"),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
    };
    (
        DaemonInvocationClient::for_connection_for_test(
            DaemonConnection::unauthenticated_for_test(endpoint),
            handshake,
        ),
        admitted,
        server,
    )
}

async fn reset_then_reconnect_client(
    first_request_id: &'static str,
    second_request_id: &'static str,
) -> (
    DaemonInvocationClient,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (listener, endpoint) = crate::daemon::transport::BrokerListener::bind(
        &crate::daemon::transport::default_loopback_endpoint(),
    )
    .await
    .expect("bind invocation listener");
    let (first_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let first_stream = listener.accept().await.expect("accept first invocation");
        let (first_reader, _first_writer) = first_stream.into_split();
        let mut first_lines = BufReader::new(first_reader).lines();
        first_lines
            .next_line()
            .await
            .expect("read first handshake")
            .expect("first handshake");
        let first_line = first_lines
            .next_line()
            .await
            .expect("read first invocation")
            .expect("first invocation");
        let first: DaemonInvocationRequest =
            serde_json::from_str(&first_line).expect("typed first invocation");
        assert_eq!(first.request_id, first_request_id);
        let _ = first_admitted.send(());

        let control_stream = listener.accept().await.expect("accept cancellation");
        let (control_reader, _control_writer) = control_stream.into_split();
        let mut control_lines = BufReader::new(control_reader).lines();
        control_lines
            .next_line()
            .await
            .expect("read cancellation handshake")
            .expect("cancellation handshake");
        control_lines
            .next_line()
            .await
            .expect("read cancellation request")
            .expect("cancellation request");

        // The response-grace read polls liveness with handshake-less probe
        // connections; skip them like the real daemon's accept loop does.
        let (mut second_lines, mut second_writer) = loop {
            let second_stream = listener.accept().await.expect("accept second invocation");
            let (second_reader, second_writer) = second_stream.into_split();
            let mut second_lines = BufReader::new(second_reader).lines();
            if let Ok(Some(_handshake)) = second_lines.next_line().await {
                break (second_lines, second_writer);
            }
        };
        let second_line = second_lines
            .next_line()
            .await
            .expect("read second invocation")
            .expect("second invocation");
        let second: DaemonInvocationRequest =
            serde_json::from_str(&second_line).expect("typed second invocation");
        assert_eq!(second.request_id, second_request_id);
        let response = DaemonInvocationResponse::problem(
            second_request_id,
            DaemonInvocationProblem::Unavailable,
        );
        second_writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write second response");
        second_writer
            .write_all(b"\n")
            .await
            .expect("write second response newline");
        second_writer.flush().await.expect("flush second response");
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let handshake = DaemonHandshake {
        project_path: Some(profile_root.clone()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: "client.remote-effect-reconnect".to_owned(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
    };
    (
        DaemonInvocationClient::for_connection_for_test(
            DaemonConnection::unauthenticated_for_test(endpoint),
            handshake,
        ),
        admitted,
        server,
    )
}

#[derive(Clone, Copy)]
enum UnsettledControl {
    CancellationDelivered,
    CancellationConnectionRejected,
}

async fn unsettled_client(
    request_id: &'static str,
    control: UnsettledControl,
) -> (
    DaemonInvocationClient,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (listener, endpoint) = crate::daemon::transport::BrokerListener::bind(
        &crate::daemon::transport::default_loopback_endpoint(),
    )
    .await
    .expect("bind invocation listener");
    let (request_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, _invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        assert_eq!(request.request_id, request_id);

        match control {
            UnsettledControl::CancellationDelivered => {
                let _ = request_admitted.send(());
                let control_stream = listener.accept().await.expect("accept cancellation");
                let (control_reader, _control_writer) = control_stream.into_split();
                let mut control_lines = BufReader::new(control_reader).lines();
                control_lines
                    .next_line()
                    .await
                    .expect("read cancellation handshake")
                    .expect("cancellation handshake");
                let cancellation_line = control_lines
                    .next_line()
                    .await
                    .expect("read cancellation request")
                    .expect("cancellation request");
                let cancellation = parse_daemon_invocation_cancellation_request(&cancellation_line)
                    .expect("typed invocation cancellation");
                assert_eq!(cancellation.target_request_id(), request_id);
            }
            UnsettledControl::CancellationConnectionRejected => {
                drop(listener);
                let _ = request_admitted.send(());
            }
        }
        std::future::pending::<()>().await;
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let handshake = DaemonHandshake {
        project_path: Some(profile_root.clone()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: format!("client.{request_id}"),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
    };
    (
        DaemonInvocationClient::for_connection_for_test(
            DaemonConnection::unauthenticated_for_test(endpoint),
            handshake,
        ),
        admitted,
        server,
    )
}

fn assert_authoritative_settlement(response: DaemonInvocationResponse) {
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::ResetRequired
        }
    ));
}

#[tokio::test]
async fn remote_effect_cancellation_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.remote-effect-cancel";
    let (client, admitted, server) = controlled_client(REQUEST_ID).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-cancel").expect("cancellation signal");
    let cancel_after_admission = cancellation.clone();
    let cancel = tokio::spawn(async move {
        admitted.await.expect("request admission");
        assert!(cancel_after_admission.cancel(now_micros()));
    });
    let deadline = deadline_after(Duration::from_secs(1));

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client.invoke_controlled(
            invocation_request(REQUEST_ID, deadline.clone()),
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative settlement is joined")
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    cancel.await.expect("cancellation task");
    server.await.expect("server task");
}

#[tokio::test]
async fn remote_effect_deadline_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.remote-effect-deadline";
    let (client, admitted, server) = controlled_client(REQUEST_ID).await;
    drop(admitted);
    let deadline = deadline_after(Duration::from_millis(250));

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client.invoke_controlled(
            invocation_request(REQUEST_ID, deadline.clone()),
            deadline,
            CancellationSignal::active("cancel.remote-effect-deadline")
                .expect("cancellation signal"),
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative settlement is joined")
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    server.await.expect("server task");
}

#[tokio::test(start_paused = true)]
async fn remote_effect_without_authoritative_settlement_returns_reset_required() {
    const REQUEST_ID: &str = "request.remote-effect-no-settlement";
    let (client, admitted, server) =
        unsettled_client(REQUEST_ID, UnsettledControl::CancellationDelivered).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-no-settlement").expect("cancellation");
    let cancel_after_admission = cancellation.clone();
    let cancel = tokio::spawn(async move {
        admitted.await.expect("request admission");
        assert!(cancel_after_admission.cancel(now_micros()));
    });
    let deadline = deadline_after(Duration::from_secs(10));

    // The unsettled server never answers, so the join bound must outlive the
    // full authoritative response grace. The paused clock auto-advances that
    // grace only while every task is idle on loopback I/O that will never
    // arrive, so the join is virtual instead of a real 30s wait.
    let response = tokio::time::timeout(
        crate::daemon::DAEMON_TOOL_RESPONSE_GRACE + Duration::from_secs(1),
        client.invoke_controlled(
            invocation_request(REQUEST_ID, deadline.clone()),
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative join is bounded")
    .expect("indeterminate settlement is typed");

    assert_authoritative_settlement(response);
    cancel.await.expect("cancellation task");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn remote_effect_cancel_delivery_failure_returns_reset_required() {
    const REQUEST_ID: &str = "request.remote-effect-cancel-delivery-failure";
    let (client, admitted, server) =
        unsettled_client(REQUEST_ID, UnsettledControl::CancellationConnectionRejected).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-delivery-failure").expect("cancellation");
    let cancel_after_admission = cancellation.clone();
    let cancel = tokio::spawn(async move {
        admitted.await.expect("request admission");
        assert!(cancel_after_admission.cancel(now_micros()));
    });
    let deadline = deadline_after(Duration::from_secs(10));

    // The paused clock virtualizes the full response grace; see the
    // no-settlement test above.
    let response = tokio::time::timeout(
        crate::daemon::DAEMON_TOOL_RESPONSE_GRACE + Duration::from_secs(1),
        client.invoke_controlled(
            invocation_request(REQUEST_ID, deadline.clone()),
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative join is bounded after cancel delivery failure")
    .expect("indeterminate settlement is typed");

    assert_authoritative_settlement(response);
    cancel.await.expect("cancellation task");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn indeterminate_effect_discards_connection_before_next_invocation() {
    const FIRST_ID: &str = "request.remote-effect-reset-state";
    const SECOND_ID: &str = "request.remote-after-effect-reset";
    // The paused clock virtualizes the response grace the first invocation
    // must exhaust before it settles as an indeterminate effect; the
    // choreography itself still runs over real loopback connections.
    let (client, admitted, server) = reset_then_reconnect_client(FIRST_ID, SECOND_ID).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-reset-state").expect("cancellation");
    let cancel_after_admission = cancellation.clone();
    let cancel = tokio::spawn(async move {
        admitted.await.expect("request admission");
        assert!(cancel_after_admission.cancel(now_micros()));
    });
    let deadline = deadline_after(Duration::from_secs(10));
    let first = client
        .invoke_controlled(
            invocation_request(FIRST_ID, deadline.clone()),
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        )
        .await
        .expect("indeterminate effect is typed");
    assert_authoritative_settlement(first);

    let second = client
        .invoke(invocation_request(
            SECOND_ID,
            deadline_after(Duration::from_secs(1)),
        ))
        .await
        .expect("next invocation reconnects");
    assert!(matches!(
        second.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    cancel.await.expect("cancellation task");
    server.await.expect("server task");
}

#[tokio::test]
async fn semantic_qualification_uses_the_submitted_deadline_and_maps_canonical_bytes() {
    const DEADLINE_MICROS: i64 = 5_000_000;
    const CANCELLATION_ID: &str = "cancel.semantic-qualification.success";
    let (listener, endpoint) = crate::daemon::transport::BrokerListener::bind(
        &crate::daemon::transport::default_loopback_endpoint(),
    )
    .await
    .expect("bind invocation listener");
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, mut invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        let request_id = request.request_id.clone();
        match request.payload {
            DaemonInvocationPayload::SemanticQualify {
                candidate,
                observed_at,
                deadline,
                cancellation,
            } => {
                assert_eq!(candidate.evaluated_profile_id, "query-fallback");
                assert_eq!(
                    deadline.expires_at.0 - observed_at.0,
                    DEADLINE_MICROS,
                    "the client must carry the exact caller deadline"
                );
                assert_eq!(cancellation.token_id.as_str(), CANCELLATION_ID);
            }
            payload => panic!("expected semantic qualification payload, got {payload:?}"),
        }
        let response = DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SemanticEvaluatedProfileQualified {
                qualification: CanonicalQualificationBlob::new(vec![0x51, 0x55, 0x41, 0x4c])
                    .expect("bounded canonical qualification"),
            },
        );
        invocation_writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write qualification response");
        invocation_writer
            .write_all(b"\n")
            .await
            .expect("write response newline");
        invocation_writer
            .flush()
            .await
            .expect("flush qualification response");
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let client = DaemonInvocationClient::for_connection_for_test(
        DaemonConnection::unauthenticated_for_test(endpoint),
        DaemonHandshake {
            project_path: Some(profile_root.clone()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity {
                global_db_path: profile_root.join("global.db"),
                profile_root,
            },
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_instance_id: "client.semantic-qualification-success".to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
        },
    );
    let cancellation = CancellationSignal::active(CANCELLATION_ID).expect("cancellation");

    let result = client
        .qualify_semantic_profile_until(
            semantic_qualification_candidate(),
            DEADLINE_MICROS,
            cancellation,
        )
        .await
        .expect("canonical qualification bytes");

    assert_eq!(result.qualification_bytes, vec![0x51, 0x55, 0x41, 0x4c]);
    server.await.expect("server task");
}

#[tokio::test]
async fn semantic_qualification_cancellation_controls_the_same_payload_request() {
    const CANCELLATION_ID: &str = "cancel.semantic-qualification.control";
    let (listener, endpoint) = crate::daemon::transport::BrokerListener::bind(
        &crate::daemon::transport::default_loopback_endpoint(),
    )
    .await
    .expect("bind invocation listener");
    let (request_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, _invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        let request_id = request.request_id.clone();
        match request.payload {
            DaemonInvocationPayload::SemanticQualify { cancellation, .. } => {
                assert_eq!(cancellation.token_id.as_str(), CANCELLATION_ID);
            }
            payload => panic!("expected semantic qualification payload, got {payload:?}"),
        }
        let _ = request_admitted.send(());

        let control_stream = listener.accept().await.expect("accept cancellation");
        let (control_reader, _control_writer) = control_stream.into_split();
        let mut control_lines = BufReader::new(control_reader).lines();
        control_lines
            .next_line()
            .await
            .expect("read cancellation handshake")
            .expect("cancellation handshake");
        let cancellation_line = control_lines
            .next_line()
            .await
            .expect("read cancellation request")
            .expect("cancellation request");
        let control = parse_daemon_invocation_cancellation_request(&cancellation_line)
            .expect("typed invocation cancellation");
        assert_eq!(control.target_request_id(), request_id);
        std::future::pending::<()>().await;
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let client = DaemonInvocationClient::for_connection_for_test(
        DaemonConnection::unauthenticated_for_test(endpoint),
        DaemonHandshake {
            project_path: Some(profile_root.clone()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity {
                global_db_path: profile_root.join("global.db"),
                profile_root,
            },
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_instance_id: "client.semantic-qualification-cancel".to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: crate::tracedecay::MovedStoreAdoption::Never,
        },
    );
    let cancellation = CancellationSignal::active(CANCELLATION_ID).expect("cancellation");
    let call_cancellation = cancellation.clone();
    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .qualify_semantic_profile_until(
                semantic_qualification_candidate(),
                5_000_000,
                call_cancellation,
            )
            .await
    });
    admitted.await.expect("request admission");
    assert!(cancellation.cancel(now_micros()));

    let error = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .expect("read-only cancellation returns promptly")
        .expect("qualification call task")
        .expect_err("cancelled qualification must not return bytes");
    let (reason, retryable, _) = error
        .project_route_context()
        .expect("typed semantic qualification cancellation");
    assert_eq!(reason, "semantic_qualification_cancelled");
    assert!(!retryable);
    server.abort();
}
