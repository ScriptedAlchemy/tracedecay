use super::*;

#[cfg(unix)]
fn test_client_instance_id(value: u128) -> String {
    format!("{value:032x}")
}

#[cfg(unix)]
pub(super) async fn daemon_round_trip(
    engine: super::super::DaemonEngine,
    handshake: &DaemonHandshake,
    request: Value,
) -> Vec<Value> {
    let (server_stream, client_stream) =
        tokio::net::UnixStream::pair().expect("daemon socket pair");
    let server = tokio::spawn(async move {
        Box::pin(super::super::serve_socket_client(server_stream, engine)).await
    });
    let (reader, mut writer) = client_stream.into_split();
    writer
        .write_all(handshake.to_line().expect("handshake json").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("write newline");
    writer
        .write_all(request.to_string().as_bytes())
        .await
        .expect("write request");
    writer.write_all(b"\n").await.expect("write newline");
    writer.shutdown().await.expect("shutdown daemon client");
    drop(writer);

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let read_responses = async {
        let mut responses = Vec::new();
        while let Some(line) = lines.next_line().await.expect("read daemon response") {
            responses.push(serde_json::from_str(&line).expect("daemon response json"));
        }
        responses
    };
    let (server_result, responses) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(server, read_responses)
        })
        .await
        .expect("daemon request and response stream should finish");
    server_result
        .expect("daemon socket client task")
        .expect("serve daemon socket client");
    responses
}

#[test]
fn daemon_handshake_round_trips_project_scope_and_timings() {
    let handshake = DaemonHandshake {
        project_path: Some(PathBuf::from("/work/repo")),
        scope_prefix: Some("src/mcp".to_string()),
        timings: true,
        allow_init: true,
        ..test_handshake_defaults()
    };

    let encoded = handshake.to_line().expect("handshake should encode");
    let decoded = DaemonHandshake::from_line(&encoded).expect("handshake should decode");

    assert_eq!(decoded, handshake);
}

#[test]
fn daemon_handshake_requires_client_identity() {
    let encoded = serde_json::json!({
        "project_path": "/work/repo",
        "scope_prefix": null,
        "timings": false,
        "allow_init": false
    })
    .to_string();

    assert!(DaemonHandshake::from_line(&encoded).is_err());
}

#[tokio::test]
async fn portable_broker_rejects_missing_auth_before_routing() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let owners = std::sync::Arc::new(tokio::sync::Mutex::new(
        super::super::DatabaseOwnerRegistry::default(),
    ));
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners));
    let gates = std::sync::Arc::new(tokio::sync::Mutex::new(
        super::super::ProjectOpenGates::default(),
    ));
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server_administration = store_administration.clone();
    let server_attempts = std::sync::Arc::clone(&attempts);
    let lifecycle = DaemonLifecycle::default();
    let server_lifecycle = lifecycle.clone();
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        Box::pin(super::super::serve_windows_broker_client(
            stream,
            TOKEN,
            &server_lifecycle,
            server_administration,
            gates,
            Some(server_attempts),
        ))
        .await
    });
    let mut handshake = test_handshake_defaults();
    handshake.project_path = Some(PathBuf::from("/must-not-route"));
    let mut client = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect client");
    client
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write unauthenticated handshake");
    client.write_all(b"\n").await.expect("write newline");
    client.shutdown().await.expect("shutdown client");

    let error = server
        .await
        .expect("server task")
        .expect_err("missing auth must fail closed");
    assert!(error.to_string().contains("authentication failed"));
    assert!(lifecycle.accepting());
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(owners.lock().await.values().next().is_none());
}

#[test]
fn daemon_handshake_advertises_binary_version() {
    let handshake = test_handshake_defaults();

    let encoded = handshake.to_line().expect("handshake should encode");
    let value: serde_json::Value = serde_json::from_str(&encoded).expect("handshake json");

    assert_eq!(
        value["client_version"],
        serde_json::json!(crate::version::build_version())
    );
    assert_eq!(
        value["client_instance_id"],
        serde_json::json!(crate::runtime_identity::process_run_id())
    );
}

#[test]
fn missing_index_classifier_covers_every_auto_init_store_miss() {
    let missing_messages = [
        "no TraceDecay index found at '/repo'",
        "no TraceDecay database found at '/repo/store.db'",
        "parent DB not found at '/repo/branches/main.db'",
        "parent branch 'main' has no DB",
    ];
    for message in missing_messages {
        let error = crate::errors::TraceDecayError::Config {
            message: message.to_string(),
        };
        assert!(
            super::super::is_missing_index_error(&error),
            "intentional missing-store state should permit config-gated auto-init: {message}"
        );
    }

    let unrelated = crate::errors::TraceDecayError::Config {
        message: "identity cutover conflict".to_string(),
    };
    assert!(!super::super::is_missing_index_error(&unrelated));
}

#[cfg(unix)]
#[test]
fn client_version_skew_flags_only_real_mismatches() {
    assert_eq!(super::super::client_version_skew("1.2.3", "1.2.3"), None);
    assert_eq!(super::super::client_version_skew("", "1.2.3"), None);
    assert_eq!(
        super::super::client_version_skew("1.3.0", "1.2.3"),
        Some("1.3.0".to_string())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_engine_logs_version_skew_once_per_client_version() {
    let engine = super::super::DaemonEngine::default();
    let mut handshake = test_handshake_defaults();
    handshake.client_version = "0.0.0-skewed".to_string();

    assert_eq!(
        engine.client_version_skew_to_log(&handshake).await,
        Some("0.0.0-skewed".to_string()),
        "first connection from a skewed client should be logged"
    );
    assert_eq!(
        engine.client_version_skew_to_log(&handshake).await,
        None,
        "repeat connections from the same client version must not spam the log"
    );

    let matching = test_handshake_defaults();
    assert_eq!(
        engine.client_version_skew_to_log(&matching).await,
        None,
        "matching client versions are not skew"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn catalog_refresh_claim_is_negotiated_and_once_per_generation() {
    let engine = super::super::DaemonEngine::default();
    let mut handshake = test_handshake_defaults();
    handshake.client_version = "0.0.0-old".to_string();
    handshake.catalog_version = "0.0.0-old".to_string();
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    handshake.client_instance_id.clear();
    handshake.tool_list_changed_capable = true;
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none()
    );

    handshake.client_instance_id = test_client_instance_id(2);
    handshake.tool_list_changed_capable = false;
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none()
    );

    handshake.tool_list_changed_capable = true;
    handshake.catalog_version.clear();
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none(),
        "catalog refresh requires an explicitly negotiated catalog version"
    );
    handshake.tool_list_changed_capable = false;
    let initialize = json!({"jsonrpc": "2.0", "id": 2, "method": "initialize"}).to_string();
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &initialize)
            .await
            .is_none(),
        "fresh initialize marks the generation current without notifying"
    );
    handshake.tool_list_changed_capable = true;
    handshake.catalog_version = super::super::binary_version().to_string();
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none(),
        "the initialized client must not get a redundant refresh"
    );

    handshake.client_instance_id = test_client_instance_id(3);
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_some()
    );
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none()
    );

    let next_generation = super::super::DaemonEngine::default();
    assert!(
        next_generation
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_some(),
        "a new daemon generation must notify the same long-lived client once"
    );

    handshake.catalog_version = super::super::binary_version().to_string();
    let same_version_generation = super::super::DaemonEngine::default();
    assert!(
        same_version_generation
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_some(),
        "generation identity, not a reused package version, controls refresh"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn catalog_refresh_rejects_untrusted_ids_and_stops_at_capacity() {
    let engine = super::super::DaemonEngine::default();
    let mut handshake = test_handshake_defaults();
    handshake.tool_list_changed_capable = true;
    handshake.catalog_version = "0.0.0-old".to_string();
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    assert!(super::super::valid_client_instance_id(
        &test_client_instance_id(0)
    ));
    assert!(super::super::valid_client_instance_id("mcp-1234567890"));
    for invalid_id in [
        "A".repeat(32),
        "x".repeat(4_096),
        "mcp-".to_string(),
        "mcp-not-a-timestamp".to_string(),
    ] {
        handshake.client_instance_id = invalid_id;
        assert!(
            engine
                .claim_catalog_refresh(&handshake, &ping)
                .await
                .is_none()
        );
    }
    assert!(
        engine
            .catalog_refresh_notified_clients
            .lock()
            .await
            .is_empty()
    );

    for value in 0..super::super::MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION {
        handshake.client_instance_id = test_client_instance_id(value as u128);
        assert!(
            engine
                .claim_catalog_refresh(&handshake, &ping)
                .await
                .is_some()
        );
    }
    handshake.client_instance_id =
        test_client_instance_id(super::super::MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION as u128);
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none(),
        "capacity saturation must skip rather than evicting an existing client"
    );
    assert_eq!(
        engine.catalog_refresh_notified_clients.lock().await.len(),
        super::super::MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION
    );
    handshake.client_instance_id = test_client_instance_id(0);
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none(),
        "saturation must preserve existing dedupe entries"
    );
    assert!(
        engine
            .catalog_refresh_saturation_logged
            .load(std::sync::atomic::Ordering::Relaxed)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_refreshes_once_only_after_generation_change() {
    let profile = TempDir::new().unwrap();
    let profile_root = profile.path().join("profile");
    let mut handshake = test_handshake_defaults();
    handshake.client_identity = DaemonClientIdentity {
        global_db_path: profile_root.join("global.db"),
        profile_root: profile_root.clone(),
    };
    handshake.client_instance_id = test_client_instance_id(4);
    let engine = test_daemon_engine_for_profile(&profile_root);

    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
    let initialize_responses =
        daemon_round_trip(engine.clone(), &handshake, initialize.clone()).await;
    assert_eq!(initialize_responses.len(), 1);
    let initialize_response_lines: Vec<String> = initialize_responses
        .iter()
        .map(serde_json::Value::to_string)
        .collect();
    let metadata = super::super::proxy_initialize_metadata(
        &initialize.to_string(),
        &initialize_response_lines,
    );
    super::super::apply_proxy_initialize_metadata(&mut handshake, metadata);
    assert!(handshake.tool_list_changed_capable);
    assert_eq!(handshake.catalog_version, super::super::binary_version());

    let same_generation = daemon_round_trip(
        engine,
        &handshake,
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .await;
    assert_eq!(
        same_generation.len(),
        1,
        "initialize already returned this generation's catalog"
    );
    assert_eq!(same_generation[0]["id"], json!(2));

    let next_generation = test_daemon_engine_for_profile(&profile_root);
    let first = daemon_round_trip(
        next_generation.clone(),
        &handshake,
        json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
    )
    .await;
    assert_eq!(
        first.len(),
        2,
        "notification must precede the ping response"
    );
    assert_eq!(first[0]["jsonrpc"], json!("2.0"));
    assert_eq!(
        first[0]["method"],
        json!("notifications/tools/list_changed")
    );
    assert!(first[0].get("id").is_none());
    assert_eq!(first[1]["id"], json!(3));

    let second = daemon_round_trip(
        next_generation,
        &handshake,
        json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
    )
    .await;
    assert_eq!(second.len(), 1, "the refresh must not loop");
    assert_eq!(second[0]["id"], json!(4));
}

#[cfg(unix)]
#[tokio::test]
async fn initialized_ack_preserves_pending_catalog_refresh_notification() {
    let profile = TempDir::new().unwrap();
    let profile_root = profile.path().join("profile");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let mut handshake = test_handshake_defaults();
    handshake.client_identity = DaemonClientIdentity {
        global_db_path: profile_root.join("global.db"),
        profile_root,
    };
    handshake.client_instance_id = test_client_instance_id(5);
    handshake.tool_list_changed_capable = true;
    handshake.catalog_version = "0.0.0-old".to_string();

    let initialized = daemon_round_trip(
        engine.clone(),
        &handshake,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(initialized.len(), 1);
    assert_eq!(
        initialized[0]["method"],
        json!("notifications/tools/list_changed")
    );

    let ping = daemon_round_trip(
        engine,
        &handshake,
        json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}),
    )
    .await;
    assert_eq!(ping.len(), 1, "refresh notification must be deduplicated");
    assert_eq!(ping[0]["id"], json!(6));
}

#[cfg(unix)]
#[test]
fn daemon_version_skew_warning_reads_initialize_server_info() {
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    })
    .to_string();
    let response = |version: &str| {
        vec![
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "serverInfo": { "name": "tracedecay", "version": version } }
            })
            .to_string(),
        ]
    };

    let warning =
        super::super::daemon_version_skew_warning(&initialize, &response("9.9.9"), "1.0.0")
            .expect("mismatched daemon version should warn");
    assert!(
        warning.contains("9.9.9") && warning.contains("1.0.0"),
        "warning should name both versions, got: {warning}"
    );
    assert!(
        warning.contains("MCP host") && !warning.contains("tracedecay daemon restart"),
        "a newer daemon should direct recovery at the stale host, got: {warning}"
    );

    let warning =
        super::super::daemon_version_skew_warning(&initialize, &response("1.0.0"), "9.9.9")
            .expect("newer client should warn about stale daemon");
    assert!(
        warning.contains("tracedecay daemon restart"),
        "a newer client should direct recovery at the stale daemon, got: {warning}"
    );

    assert_eq!(
        super::super::daemon_version_skew_warning(&initialize, &response("1.0.0"), "1.0.0"),
        None,
        "matching versions must not warn"
    );

    let tools_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {}
    })
    .to_string();
    assert_eq!(
        super::super::daemon_version_skew_warning(&tools_call, &response("9.9.9"), "1.0.0"),
        None,
        "only initialize responses advertise the daemon version"
    );
}

#[cfg(unix)]
#[test]
fn proxy_records_negotiated_catalog_capability_and_version() {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    })
    .to_string();
    let responses = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "tracedecay", "version": "2.0.0"}
            }
        })
        .to_string(),
    ];
    let metadata = super::super::proxy_initialize_metadata(&initialize, &responses);
    let mut handshake = test_handshake_defaults();
    super::super::apply_proxy_initialize_metadata(&mut handshake, metadata);

    assert!(handshake.tool_list_changed_capable);
    assert_eq!(handshake.catalog_version, "2.0.0");

    let legacy_responses = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tracedecay", "version": "1.0.0"}
            }
        })
        .to_string(),
    ];
    let metadata = super::super::proxy_initialize_metadata(&initialize, &legacy_responses);
    let mut legacy = test_handshake_defaults();
    super::super::apply_proxy_initialize_metadata(&mut legacy, metadata);
    assert!(!legacy.tool_list_changed_capable);
    assert!(legacy.catalog_version.is_empty());
}
