use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracedecay_daemon_protocol::BrokerStream;
use tracedecay_mcp::BrokerStreamTransport;

#[tokio::test]
async fn rmcp_selected_target_retirement_between_handler_and_send_suppresses_response() {
    let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
    let active_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
    let target_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
    let selected_responses = crate::mcp::server::RmcpSelectedProjectResponseAuthority::default();
    let mut transport = BrokerStreamTransport::new(
        BrokerStream::from_tokio_unix(server).expect("daemon end of the test socket"),
    )
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
        BrokerStreamTransport::receive(&mut transport)
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
    let error = BrokerStreamTransport::send(&transport, response)
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
    let mut transport = BrokerStreamTransport::new(
        BrokerStream::from_tokio_unix(server).expect("daemon end of the test socket"),
    )
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
        BrokerStreamTransport::receive(&mut transport)
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
    BrokerStreamTransport::send(&transport, response)
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
