#![cfg(unix)]

use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
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
async fn proxy_exits_when_host_closes_during_daemon_request() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let (request_received_tx, request_received_rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        let (stream, _addr) = listener.accept().await.expect("accept proxied client");
        let (reader, _writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake line");
        lines
            .next_line()
            .await
            .expect("read request")
            .expect("request line");
        request_received_tx
            .send(())
            .expect("notify request received");
        std::future::pending::<()>().await;
    });

    let (mut transport, sender, _receiver) = ChannelTransport::new();
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
                "method": "initialize"
            }))
            .expect("request json"),
        )
        .expect("send request");
    request_received_rx.await.expect("daemon received request");
    drop(sender);

    tokio::time::timeout(std::time::Duration::from_millis(250), proxy)
        .await
        .expect("proxy must observe host EOF while the daemon request is pending")
        .expect("proxy task")
        .expect("proxy transport");

    daemon.abort();
    let _ = daemon.await;
}
