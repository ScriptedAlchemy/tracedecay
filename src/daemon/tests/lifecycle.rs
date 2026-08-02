use super::*;

#[test]
fn daemon_lifecycle_rejects_new_work_after_draining() {
    let lifecycle = DaemonLifecycle::default();
    assert!(lifecycle.accepting());

    lifecycle.begin_draining();

    assert!(!lifecycle.accepting());
}

#[test]
fn daemon_client_admission_reports_saturation_and_recovers() {
    let admission = super::super::DaemonClientAdmission::new(1);
    let permit = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => panic!("first client rejected"),
    };

    let response = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Saturated(response) => response,
        super::super::DaemonClientAdmissionOutcome::Admitted(_) => panic!("capacity exceeded"),
    };
    assert_eq!(
        response,
        super::super::DaemonClientSaturationResponse {
            kind: super::super::DaemonClientSaturationKind::ClientCapacityReached,
            retryable: true,
            capacity: 1,
        }
    );

    drop(permit);
    assert!(matches!(
        admission.try_admit(),
        super::super::DaemonClientAdmissionOutcome::Admitted(_)
    ));
}

#[test]
fn daemon_admission_preserves_reserved_health_capacity() {
    let admission = super::super::DaemonClientAdmission::with_reserved_capacity(3, 1);
    let first = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => panic!("first client rejected"),
    };
    let second = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => {
            panic!("second client rejected")
        }
    };
    assert_eq!(
        first.class(),
        super::super::DaemonClientAdmissionClass::General
    );
    assert_eq!(
        second.class(),
        super::super::DaemonClientAdmissionClass::General
    );

    let reserved = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => {
            panic!("reserved health capacity unavailable")
        }
    };
    assert_eq!(
        reserved.class(),
        super::super::DaemonClientAdmissionClass::ReservedControl
    );
    assert!(matches!(
        admission.try_admit(),
        super::super::DaemonClientAdmissionOutcome::Saturated(_)
    ));

    let status_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "tracedecay_status", "arguments": {}},
    })
    .to_string();
    let bulk_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "tracedecay_context", "arguments": {"task": "x"}},
    })
    .to_string();
    let shutdown_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": super::super::DAEMON_SHUTDOWN_METHOD,
    })
    .to_string();
    assert!(super::super::is_reserved_control_request(&status_request));
    assert!(super::super::is_reserved_control_request(&shutdown_request));
    assert!(!super::super::is_reserved_control_request(&bulk_request));
}

/// MCP discovery must never be rejected as bulk traffic.
///
/// A rejected `tools/call` costs one call; a rejected `initialize` or
/// `tools/list` costs the client its entire tracedecay tool registry for the
/// whole session, because hosts cache the catalog from that one exchange.
#[test]
fn mcp_discovery_requests_are_reserved_control_traffic() {
    for (id, method) in [(1, "initialize"), (2, "tools/list")] {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        })
        .to_string();
        assert!(
            super::super::is_reserved_control_request(&request),
            "{method} must be admitted as reserved control traffic"
        );
    }
    for method in ["notifications/initialized", "initialized"] {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
        })
        .to_string();
        assert!(
            super::super::is_reserved_control_request(&notification),
            "{method} must be admitted as reserved control traffic"
        );
    }
}

#[test]
fn daemon_shutdown_requires_a_response_id() {
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": super::super::DAEMON_SHUTDOWN_METHOD,
    })
    .to_string();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 17,
        "method": super::super::DAEMON_SHUTDOWN_METHOD,
    })
    .to_string();

    assert!(super::super::daemon_shutdown_response(&notification).is_none());
    let response = super::super::daemon_shutdown_response(&request).expect("shutdown response");
    assert_eq!(response.id, serde_json::json!(17));
    assert_eq!(response.result, Some(serde_json::json!({"accepted": true})));
}

#[tokio::test]
async fn authenticated_daemon_shutdown_acks_and_begins_draining() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let profile = TempDir::new().expect("profile");
    let mut client_identity = test_client_identity_for(profile.path().to_path_buf());
    client_identity.global_db_path = profile.path().join("mismatched.db");
    let store_administration = test_store_administration_for_profile(profile.path());
    let lifecycle = DaemonLifecycle::default();
    let server_lifecycle = lifecycle.clone();
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept shutdown client");
        super::super::serve_windows_broker_client(
            stream,
            TOKEN,
            &server_lifecycle,
            store_administration,
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            None,
        )
        .await
        .expect("serve shutdown client");
    });

    let stream = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect shutdown client");
    let (reader, mut writer) = stream.into_split();
    let preface = super::super::transport::DaemonAuthPreface::new(TOKEN)
        .to_line()
        .expect("auth preface");
    writer
        .write_all(format!("{preface}\n").as_bytes())
        .await
        .expect("write auth preface");
    writer
        .write_all(
            format!(
                "{}\n",
                DaemonHandshake {
                    client_identity,
                    ..test_handshake_defaults()
                }
                .to_line()
                .expect("handshake")
            )
            .as_bytes(),
        )
        .await
        .expect("write handshake");
    writer
        .write_all(
            format!(
                "{}\n",
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 23,
                    "method": super::super::DAEMON_SHUTDOWN_METHOD,
                })
            )
            .as_bytes(),
        )
        .await
        .expect("write shutdown");
    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut response)
        .await
        .expect("read shutdown response");
    let response: serde_json::Value = serde_json::from_str(response.trim()).expect("shutdown JSON");

    assert_eq!(response["id"], serde_json::json!(23));
    assert_eq!(response["result"], serde_json::json!({"accepted": true}));
    server.await.expect("shutdown server");
    assert!(!lifecycle.accepting());
}

#[test]
fn daemon_per_client_admission_is_fair_and_reconnects_after_release() {
    let admission = super::super::DaemonPerClientAdmission::new(2);
    let mut client_a = test_handshake_defaults();
    client_a.client_instance_id = "a".repeat(32);
    let mut client_b = test_handshake_defaults();
    client_b.client_instance_id = "b".repeat(32);

    let first = admission
        .try_admit(&client_a)
        .expect("first client-a request");
    let second = admission
        .try_admit(&client_a)
        .expect("second client-a request");
    let response = admission
        .try_admit(&client_a)
        .expect_err("one client must not consume an unbounded share");
    assert_eq!(
        response,
        super::super::DaemonClientSaturationResponse {
            kind: super::super::DaemonClientSaturationKind::PerClientCapacityReached,
            retryable: true,
            capacity: 2,
        }
    );

    let other = admission
        .try_admit(&client_b)
        .expect("another client retains a fair share");
    drop(first);
    assert!(
        admission.try_admit(&client_a).is_ok(),
        "released leases must allow reconnects"
    );
    drop(second);
    drop(other);
}

#[test]
fn daemon_per_client_admission_leaves_legacy_clients_compatible() {
    let admission = super::super::DaemonPerClientAdmission::new(1);
    let mut legacy = test_handshake_defaults();
    legacy.client_instance_id.clear();

    assert!(admission.try_admit(&legacy).is_ok());
    assert!(admission.try_admit(&legacy).is_ok());
    assert_eq!(
        admission.tracked_client_count(),
        0,
        "an absent stable instance id cannot be used as a fair identity"
    );
}

#[test]
fn daemon_per_client_fairness_never_blocks_reserved_health_requests() {
    let admission = super::super::DaemonPerClientAdmission::new(1);
    let mut client = test_handshake_defaults();
    client.client_instance_id = "c".repeat(32);
    let _bulk = admission.try_admit(&client).expect("initial bulk lease");
    let status_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "tracedecay_status", "arguments": {}},
    })
    .to_string();
    let context_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "tracedecay_context", "arguments": {"task": "x"}},
    })
    .to_string();

    assert!(
        admission
            .try_admit_request(&client, &status_request)
            .is_ok(),
        "reserved health requests must bypass per-client bulk fairness"
    );
    assert!(
        admission
            .try_admit_request(&client, &context_request)
            .is_err(),
        "bulk requests remain bounded"
    );
}

#[test]
fn daemon_client_saturation_response_is_typed_json_rpc_data() {
    let response = super::super::DaemonClientSaturationResponse {
        kind: super::super::DaemonClientSaturationKind::ClientCapacityReached,
        retryable: true,
        capacity: 3,
    }
    .into_json_rpc_with_id(serde_json::Value::Null);
    let data = response
        .error
        .expect("error response")
        .data
        .expect("typed data");

    assert_eq!(data["kind"], "client_capacity_reached");
    assert_eq!(data["retryable"], true);
    assert_eq!(data["capacity"], 3);
}

#[test]
fn daemon_per_client_saturation_response_is_typed_json_rpc_data() {
    let response = super::super::DaemonClientSaturationResponse {
        kind: super::super::DaemonClientSaturationKind::PerClientCapacityReached,
        retryable: true,
        capacity: 8,
    }
    .into_json_rpc_with_id(serde_json::Value::Null);
    let data = response
        .error
        .expect("error response")
        .data
        .expect("typed data");

    assert_eq!(data["kind"], "per_client_capacity_reached");
    assert_eq!(data["retryable"], true);
    assert_eq!(data["capacity"], 8);
}

#[test]
fn project_server_capacity_response_is_typed_json_rpc_data() {
    let response = super::super::project_open_error_response(
        serde_json::Value::Null,
        &super::super::project_server_capacity_error(),
    );
    let data = response
        .error
        .expect("error response")
        .data
        .expect("typed data");

    assert_eq!(data["kind"], "project_server_capacity_reached");
    assert_eq!(data["retryable"], true);
    assert_eq!(data["capacity"], super::super::MAX_CACHED_PROJECT_SERVERS);
}

#[tokio::test]
async fn reserved_doctor_request_answers_under_general_saturation() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let profile = TempDir::new().expect("profile");
    let client_identity = test_client_identity_for(profile.path().join("client"));
    let store_administration = test_store_administration_for_profile(&client_identity.profile_root);
    let _database_scope =
        enter_test_daemon_database_scope(&client_identity.profile_root, "reserved-doctor-test");

    let admission = super::super::DaemonClientAdmission::with_reserved_capacity(2, 1);
    let general = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => {
            panic!("general client rejected")
        }
    };
    let reserved = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => {
            panic!("reserved Doctor client rejected")
        }
    };
    assert_eq!(
        reserved.class(),
        super::super::DaemonClientAdmissionClass::ReservedControl
    );
    assert!(matches!(
        admission.try_admit(),
        super::super::DaemonClientAdmissionOutcome::Saturated(_)
    ));

    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept Doctor client");
        let lifecycle = DaemonLifecycle::default();
        super::super::serve_windows_broker_client_with_class(
            stream,
            TOKEN,
            &lifecycle,
            store_administration,
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            super::super::DaemonPerClientAdmission::default(),
            reserved.class(),
            None,
        )
        .await
        .expect("serve Doctor client");
    });

    let stream = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect Doctor client");
    let (reader, mut writer) = stream.into_split();
    let preface = super::super::transport::DaemonAuthPreface::new(TOKEN)
        .to_line()
        .expect("auth preface");
    writer
        .write_all(preface.as_bytes())
        .await
        .expect("write preface");
    writer.write_all(b"\n").await.expect("preface newline");
    writer
        .write_all(
            DaemonHandshake {
                client_identity,
                ..test_handshake_defaults()
            }
            .to_line()
            .expect("handshake")
            .as_bytes(),
        )
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 19,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_runtime",
            "arguments": {
                "authority_audit": true,
                "session_ingest_health": true,
                "format": "json"
            }
        }
    });
    writer
        .write_all(request.to_string().as_bytes())
        .await
        .expect("write Doctor request");
    writer
        .write_all(b"\n")
        .await
        .expect("Doctor request newline");
    let mut reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut response)
        .await
        .expect("read Doctor response");
    let response: serde_json::Value =
        serde_json::from_str(&response).expect("Doctor JSON-RPC response");
    assert_eq!(response["id"], 19);
    assert!(response.get("result").is_some());
    drop(general);
    server.await.expect("Doctor server task");
}

#[cfg(unix)]
#[tokio::test]
async fn one_shot_tool_call_receives_a_matching_saturation_response() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept tool call");
        super::super::reject_saturated_daemon_client(
            super::super::transport::BrokerStream::Unix(stream),
            super::super::DaemonClientSaturationResponse {
                kind: super::super::DaemonClientSaturationKind::ClientCapacityReached,
                retryable: true,
                capacity: 1,
            },
        )
        .await;
    });

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
            None,
        ),
    )
    .await
    .expect("saturation response timed out")
    .expect_err("saturated daemon must reject the tool call");
    let message = error.to_string();
    assert!(
        message.contains("daemon client capacity reached"),
        "expected a matching saturation response, got: {message}"
    );
    server.await.expect("saturation server task");
}

#[tokio::test]
async fn cancelling_daemon_client_releases_admission_capacity() {
    let admission = super::super::DaemonClientAdmission::new(1);
    let permit = match admission.try_admit() {
        super::super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::super::DaemonClientAdmissionOutcome::Saturated(_) => panic!("first client rejected"),
    };
    let task = tokio::spawn(async move {
        let _permit = permit;
        std::future::pending::<()>().await;
    });
    assert!(matches!(
        admission.try_admit(),
        super::super::DaemonClientAdmissionOutcome::Saturated(_)
    ));

    task.abort();
    task.await.expect_err("client task cancelled");
    assert!(matches!(
        admission.try_admit(),
        super::super::DaemonClientAdmissionOutcome::Admitted(_)
    ));
}

#[tokio::test]
async fn portable_broker_requests_reuse_one_authenticated_project_owner() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    gix::init(&project).expect("initialize project repository");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "portable-owner-cache-test")
            .expect("daemon database scope");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let route =
        super::super::ProjectRouteKey::from_handshake(&project, &handshake).expect("route key");
    let owners = std::sync::Arc::new(tokio::sync::Mutex::new(
        super::super::DatabaseOwnerRegistry::default(),
    ));
    prepare_test_profile_root(&profile_root);
    let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)
        .expect("load test profile identity");
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners))
        .with_profile_identity(profile_identity);
    let gates = std::sync::Arc::new(tokio::sync::Mutex::new(
        super::super::ProjectOpenGates::default(),
    ));
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");

    let server = {
        let store_administration = store_administration.clone();
        let gates = std::sync::Arc::clone(&gates);
        let attempts = std::sync::Arc::clone(&attempts);
        let lifecycle = lifecycle.clone();
        tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            for _ in 0..2 {
                let stream = listener.accept().await.expect("accept client");
                let store_administration = store_administration.clone();
                let gates = std::sync::Arc::clone(&gates);
                let attempts = std::sync::Arc::clone(&attempts);
                let lifecycle = lifecycle.clone();
                clients.spawn(async move {
                    Box::pin(super::super::serve_windows_broker_client(
                        stream,
                        TOKEN,
                        &lifecycle,
                        store_administration,
                        gates,
                        Some(attempts),
                    ))
                    .await
                });
            }
            while let Some(client) = clients.join_next().await {
                client.expect("client task").expect("serve client");
            }
        })
    };

    let request = |id: u64| {
        let endpoint = endpoint.clone();
        let handshake = handshake.clone();
        async move {
            let stream = super::super::transport::BrokerStream::connect(&endpoint)
                .await
                .expect("connect client");
            let (reader, mut writer) = stream.into_split();
            let preface = super::super::transport::DaemonAuthPreface::new(TOKEN)
                .to_line()
                .expect("auth preface");
            writer.write_all(preface.as_bytes()).await.expect("preface");
            writer.write_all(b"\n").await.expect("preface newline");
            writer
                .write_all(handshake.to_line().expect("handshake").as_bytes())
                .await
                .expect("handshake");
            writer.write_all(b"\n").await.expect("handshake newline");
            let initialize = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "portable-cache-test", "version": "1"}
                }
            });
            writer
                .write_all(initialize.to_string().as_bytes())
                .await
                .expect("initialize");
            writer.write_all(b"\n").await.expect("initialize newline");
            writer.shutdown().await.expect("shutdown request writer");
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let response = lines
                .next_line()
                .await
                .expect("read response")
                .expect("initialize response");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&response).unwrap()["id"],
                id
            );
        }
    };
    tokio::join!(request(1), request(2));
    server.await.expect("broker server");
    tokio::time::timeout(tokio::time::Duration::from_secs(20), async {
        loop {
            if owners.lock().await.get_route(&route).is_some() {
                return Ok(());
            }
            if let Some(failure) =
                super::super::portable_cached_project_open_failure(gates.as_ref(), &handshake)
                    .await?
            {
                return Err(failure.to_error());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("portable project warmup timed out")
    .expect("portable project warmup failed");

    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "same-route requests must singleflight one project open"
    );
    let owners = owners.lock().await;
    assert_eq!(owners.servers.len(), 1);
    assert_eq!(owners.aliases.len(), 1);
    let first = owners.get_route(&route).expect("first cached owner").1;
    let second = owners.get_route(&route).expect("second cached owner").1;
    assert!(std::sync::Arc::ptr_eq(first, second));
}

#[cfg(unix)]
#[tokio::test]
async fn client_drain_timeout_aborts_and_joins_remaining_work() {
    let mut clients = tokio::task::JoinSet::new();
    clients.spawn(async {
        std::future::pending::<()>().await;
        Ok(())
    });

    let drained = drain_client_tasks(&mut clients, tokio::time::Duration::from_millis(5)).await;

    assert!(!drained);
    assert!(clients.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn client_drain_waits_for_completed_work() {
    let mut clients = tokio::task::JoinSet::new();
    clients.spawn(async { Ok(()) });

    let drained = drain_client_tasks(&mut clients, tokio::time::Duration::from_secs(1)).await;

    assert!(drained);
    assert!(clients.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn one_shot_tool_call_aborts_when_daemon_liveness_fails_after_write() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept tool call");
        drop(listener);
        std::future::pending::<()>().await;
    });

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
            None,
        ),
    )
    .await
    .expect("liveness failure detection timed out")
    .expect_err("lost daemon liveness must abort the one-shot request");
    let message = error.to_string();
    assert!(message.contains("tracedecay_status"), "{message}");
    assert!(message.contains("unreachable"), "{message}");
    assert!(
        message.contains("already sent") && message.contains("not retried"),
        "{message}"
    );
    server.abort();
    let _ = server.await;
}

#[cfg(unix)]
#[tokio::test]
async fn proxied_request_uses_shared_liveness_boundary_after_write() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept proxied request");
        drop(listener);
        std::future::pending::<()>().await;
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/list",
    })
    .to_string();

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::send_daemon_request_line_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            &request,
            std::time::Duration::from_millis(10),
            None,
        ),
    )
    .await
    .expect("proxy liveness failure detection timed out")
    .expect_err("proxied response wait must stop when daemon liveness fails");
    let message = error.to_string();
    assert!(message.contains("tools/list"), "{message}");
    assert!(
        message.contains("already sent") && message.contains("not retried"),
        "{message}"
    );
    server.abort();
    let _ = server.await;
}

#[cfg(unix)]
#[tokio::test]
async fn post_write_disconnect_reports_ambiguous_outcome_without_retry() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept proxied request");
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
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
    })
    .to_string();

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::send_daemon_request_line_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            &request,
            std::time::Duration::from_millis(10),
            None,
        ),
    )
    .await
    .expect("post-write disconnect detection timed out")
    .expect_err("disconnect without a response must remain ambiguous");
    let message = error.to_string();
    assert!(message.contains("outcome is unknown"), "{message}");
    assert!(message.contains("not retried"), "{message}");
    assert!(!message.contains("retry the request"), "{message}");
    server.await.expect("fake daemon task");
}

#[cfg(unix)]
#[tokio::test]
async fn one_shot_tool_call_allows_long_response_while_daemon_stays_live() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept tool call");
        let (reader, mut writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake line");
        let request_line = lines
            .next_line()
            .await
            .expect("read request")
            .expect("request line");
        let request: Value = serde_json::from_str(&request_line).expect("request json");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), lines.next_line(),)
                .await
                .is_err(),
            "client write half must remain open while the response is in flight"
        );
        let (probe, _) = tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept())
            .await
            .expect("liveness probe timed out")
            .expect("accept liveness probe");
        drop(probe);
        let response = json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"status": "ok"},
        });
        writer
            .write_all(response.to_string().as_bytes())
            .await
            .expect("write response");
        writer.write_all(b"\n").await.expect("write newline");
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
            None,
        ),
    )
    .await
    .expect("healthy long-running request timed out")
    .expect("healthy long-running request must complete");
    assert_eq!(result["status"], json!("ok"));
    server.await.expect("fake daemon task");
}

#[cfg(unix)]
#[tokio::test]
async fn one_shot_tool_call_preserves_response_split_across_liveness_poll() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept tool call");
        let (reader, mut writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake line");
        let request_line = lines
            .next_line()
            .await
            .expect("read request")
            .expect("request line");
        let request: Value = serde_json::from_str(&request_line).expect("request json");
        let mut response = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"status": "split-across-poll"},
        }))
        .expect("encode response");
        response.push(b'\n');
        let split = response.len() / 2;
        writer
            .write_all(&response[..split])
            .await
            .expect("write response prefix");
        writer.flush().await.expect("flush response prefix");
        let (probe, _) = tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept())
            .await
            .expect("liveness probe timed out")
            .expect("accept liveness probe");
        drop(probe);
        writer
            .write_all(&response[split..])
            .await
            .expect("write response suffix");
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
            None,
        ),
    )
    .await
    .expect("split-frame response timed out")
    .expect("split-frame response must reassemble across liveness polls");
    assert_eq!(result["status"], json!("split-across-poll"));
    server.await.expect("fake daemon task");
}

#[cfg(unix)]
#[tokio::test]
async fn persistent_idle_client_closes_on_draining_without_timeout() {
    let lifecycle = DaemonLifecycle::default();
    let idle_lifecycle = lifecycle.clone();
    let mut clients = tokio::task::JoinSet::new();
    clients.spawn(async move {
        idle_lifecycle.wait_for_draining().await;
        Ok(())
    });

    lifecycle.begin_draining();
    let drained = drain_client_tasks(&mut clients, tokio::time::Duration::from_secs(1)).await;

    assert!(drained);
    assert!(lifecycle.try_enter().is_none());
}

#[tokio::test]
async fn draining_waits_for_one_bounded_in_flight_request() {
    let lifecycle = DaemonLifecycle::default();
    let activity = lifecycle.try_enter().expect("request should start");
    let client = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        drop(activity);
    });

    lifecycle.begin_draining();
    lifecycle.wait_for_idle().await;

    client.await.expect("client task should finish");
    assert!(lifecycle.try_enter().is_none());
}
