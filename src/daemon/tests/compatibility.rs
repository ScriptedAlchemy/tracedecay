use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{DaemonClientIdentity, DaemonHandshake};

fn client_identity() -> DaemonClientIdentity {
    DaemonClientIdentity {
        profile_root: PathBuf::from("/profiles/client"),
        global_db_path: PathBuf::from("/profiles/client/global.db"),
    }
}

fn current_handshake() -> DaemonHandshake {
    DaemonHandshake {
        project_path: Some(PathBuf::from("/work/repo")),
        scope_prefix: Some("src".to_string()),
        attested_scope: None,
        timings: true,
        allow_init: false,
        allow_initialize_root_routing: true,
        client_identity: client_identity(),
        client_version: "2.0.0".to_string(),
        client_instance_id: "client-instance".to_string(),
        tool_list_changed_capable: true,
        catalog_version: "2.0.0".to_string(),
    }
}

#[test]
fn old_handshake_missing_new_fields_uses_safe_defaults() {
    let encoded = json!({
        "project_path": "/work/repo",
        "scope_prefix": null,
        "timings": false,
        "allow_init": false,
        "client_identity": {
            "profile_root": "/profiles/client",
            "global_db_path": "/profiles/client/global.db"
        }
    })
    .to_string();

    let decoded = DaemonHandshake::from_line(&encoded).expect("legacy handshake should decode");

    assert!(!decoded.allow_initialize_root_routing);
    assert!(decoded.client_version.is_empty());
    assert!(decoded.client_instance_id.is_empty());
    assert!(!decoded.tool_list_changed_capable);
    assert!(decoded.catalog_version.is_empty());
}

#[test]
fn new_handshake_deserializes_into_legacy_projection() {
    #[derive(Deserialize)]
    struct LegacyHandshake {
        project_path: Option<PathBuf>,
        scope_prefix: Option<String>,
        timings: bool,
        allow_init: bool,
        client_identity: DaemonClientIdentity,
    }

    let current = current_handshake();
    let legacy: LegacyHandshake =
        serde_json::from_str(&current.to_line().expect("current handshake should encode"))
            .expect("legacy projection should ignore new fields");

    assert_eq!(legacy.project_path, current.project_path);
    assert_eq!(legacy.scope_prefix, current.scope_prefix);
    assert_eq!(legacy.timings, current.timings);
    assert_eq!(legacy.allow_init, current.allow_init);
    assert_eq!(legacy.client_identity, current.client_identity);
}

#[test]
fn new_initialize_response_deserializes_into_legacy_projection() {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyServerInfo {
        name: String,
        version: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyInitializeResult {
        protocol_version: String,
        server_info: LegacyServerInfo,
        capabilities: Value,
    }

    #[derive(Deserialize)]
    struct LegacyInitializeResponse {
        jsonrpc: String,
        id: Value,
        result: LegacyInitializeResult,
    }

    let response = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "result": {
            "protocolVersion": "2024-11-05",
            "serverInfo": {"name": "tracedecay", "version": "2.0.0"},
            "capabilities": {"tools": {"listChanged": true}},
            "_meta": {"tracedecayInitializeRoute": {"projectPath": "/work/repo"}}
        }
    });

    let legacy: LegacyInitializeResponse =
        serde_json::from_value(response).expect("legacy client should ignore new response fields");

    assert_eq!(legacy.jsonrpc, "2.0");
    assert_eq!(legacy.id, json!(7));
    assert_eq!(legacy.result.protocol_version, "2024-11-05");
    assert_eq!(legacy.result.server_info.name, "tracedecay");
    assert_eq!(legacy.result.server_info.version, "2.0.0");
    assert!(legacy.result.capabilities.get("tools").is_some());
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn restart_grace_is_bounded_when_daemon_never_rebinds() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let grace = std::time::Duration::from_millis(150);
    let poll = std::time::Duration::from_millis(25);
    let started = tokio::time::Instant::now();

    super::super::connect_with_restart_grace(
        &super::super::connection_for_socket_path(&socket),
        grace,
        poll,
    )
    .await
    .expect_err("missing daemon must stop retrying");

    let elapsed = started.elapsed();
    assert!(elapsed >= grace);
    assert!(elapsed <= grace + poll);
}

#[cfg(unix)]
#[test]
fn version_skew_guidance_covers_both_upgrade_directions() {
    let stale_client = super::super::version_skew_action("2.0.0", "1.0.0");
    assert!(stale_client.contains("MCP host"));

    let stale_daemon = super::super::version_skew_action("1.0.0", "2.0.0");
    assert!(stale_daemon.contains("tracedecay daemon restart"));
}

#[cfg(unix)]
#[tokio::test]
async fn unauthenticated_legacy_handshake_is_rejected_before_routing() {
    use tokio::io::AsyncWriteExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("bind broker");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept legacy client");
        Box::pin(super::super::serve_authenticated_socket_client(
            stream,
            super::super::DaemonEngine::default(),
            TOKEN.to_string(),
        ))
        .await
    });

    let mut client = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect legacy client");
    client
        .write_all(current_handshake().to_line().expect("handshake").as_bytes())
        .await
        .expect("write unauthenticated handshake");
    client.write_all(b"\n").await.expect("write newline");
    client.shutdown().await.expect("shutdown legacy client");

    let error = server
        .await
        .expect("server task")
        .expect_err("unauthenticated legacy client must fail closed");
    assert!(error.to_string().contains("authentication failed"));
}
