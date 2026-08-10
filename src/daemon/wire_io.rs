//! Newline-delimited wire framing for daemon responses and requests.
//!
//! Oversized input is rejected with a typed non-durable error without
//! retaining payload bytes; the bound tests live beside that guarantee.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

pub(super) async fn write_json_rpc_response(
    transport: &mut impl McpTransport,
    response: &crate::mcp::JsonRpcResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

pub(super) async fn write_daemon_invocation_response(
    transport: &mut impl McpTransport,
    response: &DaemonInvocationResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

/// Read one newline-delimited frame. Oversized input gets a typed non-durable
/// rejection and returns `Ok(None)` without retaining payload bytes.
pub(super) async fn read_line_handling_wire_oversized(
    transport: &mut impl McpTransport,
) -> Result<Option<String>> {
    match transport.read_line().await {
        Ok(line) => Ok(line),
        Err(error) if tracedecay_usecases::host_admission::is_wire_oversized_io_error(&error) => {
            let _ = crate::mcp::transport::write_wire_oversized_rejection(transport, &error).await;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod wire_bound_tests {
    use std::sync::Arc;

    use super::{
        BrokerStreamTransport, DaemonLifecycle, read_line_handling_wire_oversized,
        serve_routed_rmcp_connection,
    };
    use crate::mcp::McpTransport;
    use rmcp::transport::Transport;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tracedecay_usecases::host_admission::{WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error};

    use super::transport::{BrokerListener, BrokerStream, default_loopback_endpoint};

    #[tokio::test]
    async fn broker_transport_streams_hostile_frame_and_typed_rejection_has_no_payload() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let mut client = client;
            // Stream hostile bytes without pre-building a MAX+1 String in the
            // product reader path; allocate only a small chunk buffer here.
            let chunk = vec![b'w'; 8192];
            let mut remaining =
                tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 64 * 1024;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
        });

        let err = server_transport.read_line().await.expect_err("oversized");
        assert!(is_wire_oversized_io_error(&err));
        assert_eq!(err.to_string(), WIRE_RECORD_TOO_LARGE);
        // Reason code is `wire_record_too_large` (contains 'w'); assert the
        // hostile fill pattern itself is not echoed.
        assert!(!err.to_string().contains("wwww"));
        writer.await.expect("writer");
    }

    #[tokio::test]
    async fn rmcp_broker_transport_keeps_the_tracedecay_frame_limit() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");
        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            client
                .write_all(&vec![
                    b'x';
                    tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
                        + 1
                ])
                .await
                .expect("write oversized frame");
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
        });

        assert!(
            Transport::<rmcp::RoleServer>::receive(&mut transport)
                .await
                .is_none(),
            "rmcp must receive the same bounded rejection as the daemon transport"
        );
        writer.await.expect("oversized frame writer");
    }

    #[tokio::test]
    async fn rmcp_broker_transport_recovers_after_malformed_json() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");
        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut transport = BrokerStreamTransport::new(server);

        client.write_all(b"{not-json}\n").await.expect("malformed");
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .expect("valid frame");
        client.flush().await.expect("flush");

        let recovered = Transport::<rmcp::RoleServer>::receive(&mut transport)
            .await
            .expect("valid frame after parse error");
        let recovered = serde_json::to_value(recovered).expect("serialize received message");
        assert_eq!(recovered["method"], serde_json::json!("ping"));

        let mut client = tokio::io::BufReader::new(client);
        let mut line = String::new();
        client.read_line(&mut line).await.expect("parse response");
        let response: serde_json::Value =
            serde_json::from_str(&line).expect("parse error JSON response");
        assert_eq!(response["error"]["code"], serde_json::json!(-32700));
    }

    #[tokio::test]
    async fn daemon_routed_rmcp_serves_initialize_tools_unknown_and_cancel() {
        let (cg, _dir, _pin) = crate::mcp::server::writer_test_support::init_indexed_repo().await;
        let mcp = crate::mcp::McpServer::new(cg, None).await;
        let lifecycle = DaemonLifecycle::default();
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");
        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "rmcp-production-route-test", "version": "1"}
            }
        })
        .to_string();
        let pending = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tracedecay/unknown"
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 999, "reason": "test cancellation"}
            })
            .to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/list"
            })
            .to_string(),
        ];
        let task = tokio::spawn({
            let mcp = Arc::clone(&mcp);
            let lifecycle = lifecycle.clone();
            async move {
                serve_routed_rmcp_connection(
                    mcp,
                    BrokerStreamTransport::new(server),
                    initialize,
                    pending,
                    None,
                    false,
                    &lifecycle,
                )
                .await
            }
        });
        let mut client = tokio::io::BufReader::new(client);
        let mut line = String::new();

        client
            .read_line(&mut line)
            .await
            .expect("initialize response");
        let initialized: serde_json::Value =
            serde_json::from_str(&line).expect("initialize JSON response");
        assert_eq!(initialized["id"], serde_json::json!(1));
        assert_eq!(
            initialized["result"]["serverInfo"]["name"],
            serde_json::json!("tracedecay")
        );

        line.clear();
        client.read_line(&mut line).await.expect("unknown response");
        let unknown: serde_json::Value =
            serde_json::from_str(&line).expect("unknown method JSON response");
        assert_eq!(unknown["id"], serde_json::json!(2));
        assert_eq!(unknown["error"]["code"], serde_json::json!(-32601));

        line.clear();
        client.read_line(&mut line).await.expect("tools response");
        let tools: serde_json::Value = serde_json::from_str(&line).expect("tools JSON response");
        assert_eq!(tools["id"], serde_json::json!(3));
        assert!(
            tools["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "the production rmcp route must advertise the mounted tool surface"
        );

        lifecycle.begin_draining();
        task.await
            .expect("rmcp route task")
            .expect("rmcp route completion");
        mcp.shutdown_background_tasks().await;
    }

    #[tokio::test]
    async fn broker_transport_accepts_exact_cap_and_recovers_next_frame_after_oversize() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let mut client = client;
            let chunk = vec![b'a'; 8192];
            let mut remaining = tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write exact");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("exact newline");

            let chunk = vec![b'z'; 8192];
            let mut remaining =
                tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 1;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client
                    .write_all(&chunk[..n])
                    .await
                    .expect("write oversized");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("oversized newline");
            client
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
                .await
                .expect("next frame");
            client.flush().await.expect("flush");
        });

        assert_eq!(
            server_transport
                .read_line()
                .await
                .expect("exact accepted")
                .expect("exact line")
                .len(),
            tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
        );
        let error = server_transport
            .read_line()
            .await
            .expect_err("one over rejected");
        assert!(is_wire_oversized_io_error(&error));
        assert_eq!(
            server_transport
                .read_line()
                .await
                .expect("next read")
                .as_deref(),
            Some(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        );
        writer.await.expect("writer");
    }

    #[tokio::test]
    async fn read_line_handling_writes_typed_rejection_without_payload_bytes() {
        let (listener, bound) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .expect("bind");

        let mut client = BrokerStream::connect(&bound).await.expect("connect");
        let server = listener.accept().await.expect("accept");
        let mut server_transport = BrokerStreamTransport::new(server);

        let writer = tokio::spawn(async move {
            let prefix =
                br#"{"jsonrpc":"2.0","id":"daemon-7","method":"tools/call","params":{"payload":""#;
            client.write_all(prefix).await.expect("prefix");
            let chunk = vec![b'q'; 4096];
            let mut remaining = tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
                + 32 * 1024
                - prefix.len();
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                client.write_all(&chunk[..n]).await.expect("write");
                remaining -= n;
            }
            client.write_all(b"\n").await.expect("newline");
            client.flush().await.expect("flush");
            client
        });

        let outcome = read_line_handling_wire_oversized(&mut server_transport)
            .await
            .expect("typed handling");
        assert!(outcome.is_none());

        let mut client = writer.await.expect("writer");
        let mut response = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut client, &mut buf)
                .await
                .expect("read rejection");
            if n == 0 {
                break;
            }
            response.extend_from_slice(&buf[..n]);
            if response.contains(&b'\n') {
                break;
            }
        }
        let response: serde_json::Value =
            serde_json::from_slice(&response).expect("JSON-RPC rejection");
        assert_eq!(response["id"], serde_json::json!("daemon-7"));
        assert_eq!(response["error"]["code"], serde_json::json!(-32600));
        assert_eq!(
            response["error"]["message"],
            serde_json::json!(WIRE_RECORD_TOO_LARGE)
        );
        assert!(!response.to_string().contains('q'));
    }
}
