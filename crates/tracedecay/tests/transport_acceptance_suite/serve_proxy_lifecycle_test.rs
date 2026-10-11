use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracedecay::daemon::proxy_transport_to_daemon;
use tracedecay_daemon_identity::authority::DaemonAuthority;
use tracedecay_daemon_protocol::{DaemonAuthPreface, DaemonEndpoint, DaemonHandshake};
use tracedecay_mcp::get_tool_definitions;
use tracedecay_mcp::transport::ChannelTransport;

fn test_handshake(profile_root: &std::path::Path) -> DaemonHandshake {
    serde_json::from_value(json!({
        "project_path": null,
        "scope_prefix": null,
        "timings": false,
        "allow_init": false,
        "allow_initialize_root_routing": false,
        "client_identity": {
            "global_db_path": profile_root.join(tracedecay_store::StoreFile::Profile.file_name()),
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
    let authority = DaemonAuthority::acquire(
        dir.path(),
        &DaemonEndpoint::Unix(socket.clone()),
        env!("CARGO_PKG_VERSION"),
    )
    .expect("seed daemon authority");
    let token = authority.auth_token().to_owned();
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let (request_received_tx, request_received_rx) = tokio::sync::oneshot::channel();
    let (write_response_tx, write_response_rx) = tokio::sync::oneshot::channel();
    let definition = get_tool_definitions()
        .expect("canonical tool definitions")
        .into_iter()
        .find(|tool| tool.name == "tracedecay_grep")
        .expect("core grep tool");
    let daemon_tool = serde_json::to_value(definition).expect("daemon tool");
    assert!(daemon_tool["inputSchema"].is_object());
    let served_tool = daemon_tool.clone();
    let daemon = tokio::spawn(async move {
        let (stream, _addr) = listener.accept().await.expect("accept proxied client");
        let (reader, mut writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let preface = lines
            .next_line()
            .await
            .expect("read auth preface")
            .expect("auth preface line");
        assert!(
            DaemonAuthPreface::from_line(preface.trim())
                .expect("auth preface")
                .authenticate(&token)
        );
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
            "result": { "tools": [served_tool] }
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
    let tools = response["result"]["tools"].as_array().expect("tool roster");
    let grep = tools
        .iter()
        .find(|tool| tool["name"] == daemon_tool["name"])
        .expect("in-flight daemon tool delivered after host EOF");
    assert_eq!(grep["description"], daemon_tool["description"]);
    assert_eq!(grep["inputSchema"], daemon_tool["inputSchema"]);

    tokio::time::timeout(std::time::Duration::from_secs(2), proxy)
        .await
        .expect("proxy must exit after delivering the in-flight response")
        .expect("proxy task")
        .expect("proxy transport");

    daemon.await.expect("daemon task");
}
