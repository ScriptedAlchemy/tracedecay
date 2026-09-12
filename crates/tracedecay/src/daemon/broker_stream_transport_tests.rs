use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracedecay_application::observability::{
    BoundedDeliverySettlementRecorderV1, BoundedObservabilityProducerV1,
    DeliverySettlementAuthorityV1, ObservabilityProducerIdentityV1, RegisteredObservabilityPortV1,
};
use tracedecay_contracts::{ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1};
use tracedecay_daemon_protocol::BrokerStream;
use tracedecay_domain::{ObservabilityPayloadV1, ProjectId};
use tracedecay_mcp::BrokerStreamTransport;

struct DeliverySettlementFixture {
    _pin: tracedecay_runtime_core::config::PinnedUserDataDir,
    _project: tempfile::TempDir,
    recorder: Arc<BoundedDeliverySettlementRecorderV1>,
    authority: Arc<DeliverySettlementAuthorityV1>,
    producer: Arc<BoundedObservabilityProducerV1>,
    db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    project_id: ProjectId,
    // The lease alone does not own daemon write authority. Keep the test
    // runtime alive until every asynchronous settlement has drained.
    _runtime: tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
}

async fn delivery_settlement_fixture() -> DeliverySettlementFixture {
    let pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let project_id = ProjectId::new("project.rmcp.delivery").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:rmcp-delivery".to_owned(),
        producer_revision: "rmcp-delivery-producer.v1".to_owned(),
        configuration_revision: "rmcp-delivery-config.v1".to_owned(),
        policy_revision: "rmcp-delivery-policy.v1".to_owned(),
    };
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(db.clone(), identity.clone(), 8).expect("producer"),
    );
    let authority = Arc::new(
        DeliverySettlementAuthorityV1::new(db.clone(), Arc::clone(&producer), identity)
            .expect("settlement authority"),
    );
    let recorder = Arc::new(
        BoundedDeliverySettlementRecorderV1::start(Arc::clone(&authority), 8)
            .expect("settlement recorder"),
    );
    DeliverySettlementFixture {
        _pin: pin,
        _project: project,
        recorder,
        authority,
        producer,
        db,
        project_id,
        _runtime: runtime,
    }
}

async fn settled_fanout(
    fixture: DeliverySettlementFixture,
) -> tracedecay_domain::WorkDeliveryFanoutObservedV1 {
    let summary = fixture
        .recorder
        .shutdown()
        .await
        .expect("drain settlement recorder");
    assert_eq!(summary.settled, 1, "one RMCP Work response must settle");
    assert_eq!(summary.failed, 0, "transport settlement must persist");

    drop(fixture.recorder);
    drop(fixture.authority);
    let Ok(producer) = Arc::try_unwrap(fixture.producer) else {
        panic!("settlement authority releases producer")
    };
    producer
        .shutdown()
        .await
        .expect("flush observability producer");

    let page = RegisteredObservabilityPortV1::new(fixture.db.as_ref())
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: fixture.project_id.as_str().to_owned(),
            event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("read settled delivery fanout");
    assert_eq!(
        page.events.len(),
        1,
        "settlement must be durably observable"
    );
    let ObservabilityPayloadV1::WorkDeliveryFanout(fanout) = page.events[0].payload.clone() else {
        panic!("expected Work delivery fanout observation");
    };
    fanout
}

#[tokio::test]
async fn rmcp_receive_waits_for_full_close_after_request_half_close() {
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .shutdown()
        .await
        .expect("half-close client request side");
    let mut receive = Box::pin(<BrokerStreamTransport as rmcp::transport::Transport<
        rmcp::RoleServer,
    >>::receive(&mut transport));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut receive)
            .await
            .is_err(),
        "rmcp receive must not treat a request-half close as full peer loss"
    );

    drop(client_writer);
    drop(client_reader);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut receive)
            .await
            .expect("rmcp receive must finish after full peer close")
            .is_none()
    );
}

/// A client that half-closes after every accepted request settled — the
/// cancelling client pattern: it keeps its read half open awaiting the
/// daemon's EOF — must observe this side close without a full peer close.
#[tokio::test]
async fn rmcp_receive_closes_after_half_close_once_accepted_requests_settle() {
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tracedecay_files","arguments":{}}}
"#,
        )
        .await
        .expect("request");
    client_writer.flush().await.expect("flush request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "the transport must accept the request"
    );
    let response = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "result": {"content": [{"type": "text", "text": "settled"}]}
    }))
    .expect("typed response");
    <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
        &mut transport,
        response,
    )
    .await
    .expect("response write");

    client_writer
        .shutdown()
        .await
        .expect("half-close client request side");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport,
            ),
        )
        .await
        .expect("settled connection must close after request half-close")
        .is_none(),
        "a settled half-closed connection carries no further messages"
    );
    drop(client_reader);
}

#[tokio::test]
async fn rmcp_selected_target_retirement_between_handler_and_send_suppresses_response() {
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let active_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
    let target_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
    let selected_responses = crate::mcp::server::RmcpSelectedProjectResponseAuthority::default();
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
        .with_project_response_lifecycle(active_lifecycle.clone())
        .with_rmcp_selected_project_responses(selected_responses.clone());
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"tracedecay_files","arguments":{}}}
"#,
        )
        .await
        .expect("selected-target request");
    client_writer
        .flush()
        .await
        .expect("flush selected-target request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "RMCP transport must retain the selected-target response slot"
    );

    let response_guard = Arc::clone(target_lifecycle.response_gate())
        .read_owned()
        .await;
    selected_responses
        .retain(
            &serde_json::json!(7),
            crate::mcp::server::SelectedProjectResponseLease::new(
                response_guard,
                target_lifecycle.response_revoked().clone(),
            ),
        )
        .expect("handler-to-transport selected response handoff");

    // The active connection remains live. Only the selected target is
    // retired after handler completion but before rmcp calls `send`.
    target_lifecycle.revoke();
    assert!(!active_lifecycle.response_revoked().is_cancelled());
    let target_drain = tokio::spawn({
        let target_lifecycle = target_lifecycle.clone();
        async move { target_lifecycle.wait_for_request_drain().await }
    });
    tokio::task::yield_now().await;
    assert!(
        !target_drain.is_finished(),
        "the handler lease must keep target retirement from completing before send"
    );

    let response = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "result": {"content": [{"type": "text", "text": "must-not-leak"}]}
    }))
    .expect("typed selected-target response");
    let error = <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
        &mut transport,
        response,
    )
    .await
    .expect_err("retired selected target must suppress its response");
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    tokio::time::timeout(std::time::Duration::from_secs(1), target_drain)
        .await
        .expect("selected response lease must release after suppression")
        .expect("join target drain");

    let mut client_reader = tokio::io::BufReader::new(client_reader);
    let mut line = String::new();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            client_reader.read_line(&mut line),
        )
        .await
        .is_err(),
        "the live active server must not authorize a retired target payload: {line}"
    );
}

#[tokio::test]
async fn rmcp_selected_target_response_does_not_fall_back_to_retired_active_server() {
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let active_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
    let target_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
    let selected_responses = crate::mcp::server::RmcpSelectedProjectResponseAuthority::default();
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
        .with_project_response_lifecycle(active_lifecycle.clone())
        .with_rmcp_selected_project_responses(selected_responses.clone());
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"tracedecay_files","arguments":{}}}
"#,
        )
        .await
        .expect("selected-target request");
    client_writer
        .flush()
        .await
        .expect("flush selected-target request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "RMCP transport must retain the selected-target response slot"
    );

    let response_guard = Arc::clone(target_lifecycle.response_gate())
        .read_owned()
        .await;
    selected_responses
        .retain(
            &serde_json::json!(8),
            crate::mcp::server::SelectedProjectResponseLease::new(
                response_guard,
                target_lifecycle.response_revoked().clone(),
            ),
        )
        .expect("handler-to-transport selected response handoff");

    // The accepted connection's original project retires after target
    // selection. Its lifecycle must neither suppress nor authorize a
    // response owned by the still-live selected target.
    active_lifecycle.revoke();
    assert!(!target_lifecycle.response_revoked().is_cancelled());

    let response = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 8,
        "result": {"content": [{"type": "text", "text": "target-owned"}]}
    }))
    .expect("typed selected-target response");
    <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
        &mut transport,
        response,
    )
    .await
    .expect("live selected target must retain response authority");

    let mut client_reader = tokio::io::BufReader::new(client_reader);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client_reader.read_line(&mut line),
    )
    .await
    .expect("selected-target response timeout")
    .expect("read selected-target response");
    assert!(line.contains("target-owned"), "unexpected response: {line}");
}

#[tokio::test]
async fn rmcp_initialize_then_work_response_settles_after_transport_flush() {
    let fixture = delivery_settlement_fixture().await;
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
        .with_rmcp_work_delivery_settlement(crate::mcp::server::RmcpWorkDeliverySettlement::new(
            Some(Arc::clone(&fixture.recorder)),
            "rmcp-transport-settlement-test".to_owned(),
        ));
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"settlement-test","version":"1"}}}
"#,
        )
        .await
        .expect("initialize request");
    client_writer
        .flush()
        .await
        .expect("flush initialize request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "RMCP transport must accept initialize before Work"
    );

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_work_start_attempt","arguments":{}}}
"#,
        )
        .await
        .expect("Work request");
    client_writer.flush().await.expect("flush Work request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "RMCP transport must retain the pending Work response"
    );

    let response = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {"content": [{"type": "text", "text": "delivered"}]}
    }))
    .expect("typed RMCP Work response");
    <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
        &mut transport,
        response,
    )
    .await
    .expect("transport response write and flush");

    let mut client_reader = tokio::io::BufReader::new(client_reader);
    let mut line = String::new();
    client_reader
        .read_line(&mut line)
        .await
        .expect("flushed Work response");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).expect("response JSON")["id"],
        serde_json::json!(2),
        "the client must observe the response before it is recorded as delivered"
    );

    drop(transport);
    let fanout = settled_fanout(fixture).await;
    assert_eq!(fanout.delivered, 1);
    assert_eq!(fanout.dropped, 0);
    assert_eq!(fanout.unknown, 0);
}

/// The peer vanishes after the daemon accepted a Work request but before
/// the response reaches the wire. The attempt must settle as a typed drop
/// rather than being stranded as unknown or reported as delivered.
#[tokio::test]
async fn rmcp_peer_disconnect_mid_delivery_settles_dropped_rather_than_unknown() {
    let fixture = delivery_settlement_fixture().await;
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
        .with_rmcp_work_delivery_settlement(crate::mcp::server::RmcpWorkDeliverySettlement::new(
            Some(Arc::clone(&fixture.recorder)),
            "rmcp-transport-disconnect-test".to_owned(),
        ));
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_work_start_attempt","arguments":{}}}
"#,
        )
        .await
        .expect("Work request");
    client_writer.flush().await.expect("flush Work request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "RMCP transport must retain the pending Work response"
    );

    // The client is gone before the daemon can write its response.
    drop(client_reader);
    drop(client_writer);

    let response = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {"content": [{"type": "text", "text": "never observed"}]}
    }))
    .expect("typed RMCP Work response");
    let write = <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
        &mut transport,
        response,
    )
    .await;
    assert!(
        write.is_err(),
        "a disconnected peer must fail the response write instead of reporting delivery"
    );

    drop(transport);
    let fanout = settled_fanout(fixture).await;
    assert_eq!(
        fanout.delivered, 0,
        "a response the client never observed is never delivered"
    );
    assert_eq!(
        fanout.dropped, 1,
        "disconnect settles the attempt as dropped"
    );
    assert_eq!(
        fanout.unknown, 0,
        "disconnect settles a typed terminal rather than stranding the attempt"
    );
}

/// A client cancels an in-flight Work request. The transport must settle
/// the pending attempt as a cancelled drop and hand the client a typed
/// cancellation, never leaving the attempt open for a later response.
#[tokio::test]
async fn rmcp_client_cancellation_settles_dropped_without_stranding_the_attempt() {
    let fixture = delivery_settlement_fixture().await;
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
        .with_rmcp_work_delivery_settlement(crate::mcp::server::RmcpWorkDeliverySettlement::new(
            Some(Arc::clone(&fixture.recorder)),
            "rmcp-transport-cancellation-test".to_owned(),
        ));
    let (client_reader, mut client_writer) = client.into_split();

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_work_start_attempt","arguments":{}}}
"#,
        )
        .await
        .expect("Work request");
    client_writer.flush().await.expect("flush Work request");
    assert!(
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport
        )
        .await
        .is_some(),
        "RMCP transport must retain the pending Work response"
    );

    client_writer
        .write_all(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"client cancelled"}}
"#,
        )
        .await
        .expect("cancellation notification");
    client_writer.flush().await.expect("flush cancellation");

    // Settlement happens while the transport observes the notification,
    // before the message itself is handed to `rmcp`.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
            &mut transport,
        ),
    )
    .await;

    let mut client_reader = tokio::io::BufReader::new(client_reader);
    let mut line = String::new();
    client_reader
        .read_line(&mut line)
        .await
        .expect("flushed cancellation response");
    let cancelled =
        serde_json::from_str::<serde_json::Value>(&line).expect("cancellation response JSON");
    assert_eq!(
        cancelled,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32800,
                "message": "MCP request cancelled",
                "data": {"reason_code": "request_cancelled"},
            },
        }),
        "the cancelled request must receive its own exact typed terminal",
    );

    drop(transport);
    let fanout = settled_fanout(fixture).await;
    assert_eq!(
        fanout.delivered, 0,
        "a cancelled request is never reported as delivered"
    );
    assert_eq!(
        fanout.dropped, 1,
        "cancellation settles the pending attempt as dropped"
    );
    assert_eq!(
        fanout.unknown, 0,
        "cancellation settles a typed terminal rather than stranding the attempt"
    );
}
