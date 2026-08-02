#![cfg(unix)]

use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracedecay::daemon::{DaemonHandshake, proxy_transport_to_daemon};
use tracedecay::mcp::transport::ChannelTransport;

fn test_handshake(profile_root: &std::path::Path) -> DaemonHandshake {
    serde_json::from_value(json!({
        "project_path": null,
        "scope_prefix": null,
        "timings": false,
        "allow_init": false,
        "allow_initialize_root_routing": false,
        "client_identity": {
            "global_db_path": profile_root.join("global.db"),
            "profile_root": profile_root,
        },
        "client_version": env!("CARGO_PKG_VERSION"),
        "client_instance_id": "serve-proxy-lifecycle-test",
        "tool_list_changed_capable": false,
        "catalog_version": "",
    }))
    .expect("test handshake")
}

#[tokio::test]
async fn proxy_delivers_in_flight_response_after_host_closes() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let (request_received_tx, request_received_rx) = tokio::sync::oneshot::channel();
    let (write_response_tx, write_response_rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        let (stream, _addr) = listener.accept().await.expect("accept proxied client");
        let (reader, mut writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake line");
        let request = lines
            .next_line()
            .await
            .expect("read request")
            .expect("request line");
        request_received_tx
            .send(())
            .expect("notify request received");
        write_response_rx.await.expect("release daemon response");
        let request: serde_json::Value =
            serde_json::from_str(&request).expect("request must be JSON");
        let response = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": { "tools": [] }
        }))
        .expect("response json");
        writer
            .write_all(response.as_bytes())
            .await
            .expect("write response");
        writer.write_all(b"\n").await.expect("write newline");
        writer.shutdown().await.expect("shutdown fake daemon");
    });

    let (mut transport, sender, mut receiver) = ChannelTransport::new();
    let proxy_socket = socket.clone();
    let handshake = test_handshake(dir.path());
    let proxy = tokio::spawn(async move {
        proxy_transport_to_daemon(&proxy_socket, &handshake, None, &mut transport).await
    });

    sender
        .send(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list"
            }))
            .expect("request json"),
        )
        .expect("send request");
    request_received_rx.await.expect("daemon received request");
    drop(sender);
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    write_response_tx
        .send(())
        .expect("release daemon response after host EOF");

    let response = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await
        .expect("proxy response timed out")
        .expect("proxy must deliver the in-flight response after host EOF");
    let response: serde_json::Value =
        serde_json::from_str(response.trim()).expect("response must be JSON");
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["result"], json!({ "tools": [] }));

    tokio::time::timeout(std::time::Duration::from_secs(2), proxy)
        .await
        .expect("proxy must exit after delivering the in-flight response")
        .expect("proxy task")
        .expect("proxy transport");

    daemon.await.expect("daemon task");
}
