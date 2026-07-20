use super::*;

#[test]
fn bootstrap_tool_catalog_uses_project_node_count() {
    let request: super::super::JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    }))
    .expect("tools/list request");
    let response = super::super::daemon_bootstrap_response(&request, None, Some(65_395))
        .expect("bootstrap response")
        .expect("tools/list response");
    let result = response.result.expect("tools/list result");
    let context_description = result["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .find(|tool| tool["name"] == serde_json::json!("tracedecay_context"))
        .and_then(|tool| tool["description"].as_str())
        .expect("context tool description");

    assert!(context_description.contains("5 calls maximum"));
    assert!(context_description.contains("65395 nodes"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portable_broker_bootstrap_bypasses_project_writer_gate() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);

    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let client_identity = test_client_identity_for(profile_root.clone());
    let options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(client_identity.global_db_path.clone()),
    };
    drop(
        crate::tracedecay::TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize project"),
    );
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "portable-bootstrap-cache-test")
            .expect("daemon database scope");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let route =
        super::super::ProjectRouteKey::from_handshake(&project, &handshake).expect("project route");
    let owners = Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default()));
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners));
    let gates = Arc::new(tokio::sync::Mutex::new(
        super::super::ProjectOpenGates::default(),
    ));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");

    let blocker_administration = store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        blocker_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    let server_administration = store_administration.clone();
    let server_gates = Arc::clone(&gates);
    let server_attempts = Arc::clone(&attempts);
    let server_lifecycle = lifecycle.clone();
    let server = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let stream = listener.accept().await.expect("accept client");
            let administration = server_administration.clone();
            let gates = Arc::clone(&server_gates);
            let attempts = Arc::clone(&server_attempts);
            let lifecycle = server_lifecycle.clone();
            clients.spawn(async move {
                Box::pin(super::super::serve_windows_broker_client(
                    stream,
                    TOKEN,
                    &lifecycle,
                    administration,
                    gates,
                    Some(attempts),
                ))
                .await
            });
        }
        while let Some(client) = clients.join_next().await {
            client.expect("client task").expect("serve client");
        }
    });

    let request = |id: u64, method: &'static str| {
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
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": (method == "initialize").then_some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "portable-bootstrap-test", "version": "1"}
                }))
            });
            writer
                .write_all(request.to_string().as_bytes())
                .await
                .expect("request");
            writer.write_all(b"\n").await.expect("request newline");
            writer.shutdown().await.expect("shutdown request writer");
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let response = lines
                .next_line()
                .await
                .expect("read response")
                .expect("response line");
            serde_json::from_str::<serde_json::Value>(&response).expect("response json")
        }
    };
    let mut initialize_task = tokio::spawn(request(1, "initialize"));
    let mut tools_list_task = tokio::spawn(request(2, "tools/list"));
    let (initialize_within_bound, tools_list_within_bound) = tokio::join!(
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut initialize_task),
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut tools_list_task),
    );

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if initialize_within_bound.is_err() {
        let _ = initialize_task.await;
    }
    if tools_list_within_bound.is_err() {
        let _ = tools_list_task.await;
    }
    server.await.expect("portable broker server");

    let initialize_response = initialize_within_bound
        .expect("portable initialize must not wait for project writer gate")
        .expect("initialize client task");
    assert_eq!(
        initialize_response["result"]["protocolVersion"],
        serde_json::json!("2024-11-05")
    );
    let tools_list_response = tools_list_within_bound
        .expect("portable tools/list must not wait for project writer gate")
        .expect("tools/list client task");
    assert!(
        tools_list_response["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "portable bootstrap tool catalog must not be empty"
    );
    let portable_context_description = tools_list_response["result"]["tools"]
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool["name"] == serde_json::json!("tracedecay_context"))
        })
        .and_then(|tool| tool["description"].as_str())
        .expect("portable context tool description");
    assert!(portable_context_description.contains("10 calls maximum"));
    assert!(portable_context_description.contains("project graph is warming"));

    tokio::time::timeout(PHASE_TIMEOUT, async {
        loop {
            if owners.lock().await.get_route(&route).is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("portable initialize background warmup timed out");
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "portable initialize warmup must singleflight one project open"
    );
    lifecycle.begin_draining();
    tokio::time::timeout(PHASE_TIMEOUT, lifecycle.wait_for_idle())
        .await
        .expect("portable warmup lifecycle drain timed out");
    super::super::shutdown_project_servers(&store_administration).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_server_warmup_drops_lifecycle_activity_on_draining() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let engine = DaemonEngine::default();
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity: test_client_identity_for(profile_root),
        ..test_handshake_defaults()
    };
    let initialize_request = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }))
    .expect("initialize request");

    let store_administration = engine.store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        store_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    engine.spawn_project_server_warmup(handshake, initialize_request);
    engine.lifecycle.begin_draining();
    let idle_while_writer_held = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.lifecycle.wait_for_idle(),
    )
    .await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if idle_while_writer_held.is_err() {
        tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            engine.lifecycle.wait_for_idle(),
        )
        .await
        .expect("warmup cleanup after writer release");
    }

    idle_while_writer_held.expect("draining must cancel project warmup before writer release");
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_activation_drain_wins_when_discovery_is_simultaneously_ready() {
    for _ in 0..32 {
        let lifecycle = DaemonLifecycle::default();
        let discovery_polled = Arc::new(tokio::sync::Notify::new());
        let discovery_polled_by_future = Arc::clone(&discovery_polled);
        let discovery_lifecycle = lifecycle.clone();
        let discovery_won = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let discovery_won_by_future = Arc::clone(&discovery_won);
        super::super::spawn_lifecycle_automation_scheduler_activation(
            lifecycle.clone(),
            async move {
                discovery_polled_by_future.notify_one();
                discovery_lifecycle.wait_for_draining().await;
                discovery_won_by_future.store(true, std::sync::atomic::Ordering::Release);
            },
        );
        discovery_polled.notified().await;

        lifecycle.begin_draining();
        tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            lifecycle.wait_for_idle(),
        )
        .await
        .expect("simultaneous scheduler discovery drain timed out");
        assert!(
            !discovery_won.load(std::sync::atomic::Ordering::Acquire),
            "draining must win when scheduler discovery becomes ready on the same tick"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portable_project_warmup_cancels_before_shutdown_snapshot() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity: test_client_identity_for(profile_root),
        ..test_handshake_defaults()
    };
    let initialize_request: crate::mcp::JsonRpcRequest =
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .expect("initialize request");
    let owners = Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default()));
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners));
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(
        super::super::ProjectOpenGates::default(),
    ));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();

    let blocker_administration = store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        blocker_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    super::super::spawn_portable_project_server_warmup(
        lifecycle.clone(),
        store_administration,
        project_open_gates,
        handshake,
        initialize_request,
        Some(Arc::clone(&attempts)),
    );
    tokio::task::yield_now().await;
    lifecycle.begin_draining();
    let idle_before_writer_release = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        lifecycle.wait_for_idle(),
    )
    .await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");

    idle_before_writer_release
        .expect("portable warmup must release lifecycle activity before writer release");
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "draining portable warmup must not start a project open"
    );
    assert!(
        owners.lock().await.values().next().is_none(),
        "draining portable warmup must not insert a server after shutdown snapshot"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_warmup_drain_wins_when_open_is_simultaneously_ready() {
    let initialize_request: crate::mcp::JsonRpcRequest =
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .expect("initialize request");

    for _ in 0..32 {
        let lifecycle = DaemonLifecycle::default();
        let open_polled = Arc::new(tokio::sync::Notify::new());
        let open_polled_by_future = Arc::clone(&open_polled);
        let open_lifecycle = lifecycle.clone();
        let open_won = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let open_won_by_future = Arc::clone(&open_won);
        super::super::spawn_lifecycle_project_server_warmup(
            lifecycle.clone(),
            initialize_request.clone(),
            async move {
                open_polled_by_future.notify_one();
                open_lifecycle.wait_for_draining().await;
                open_won_by_future.store(true, std::sync::atomic::Ordering::Release);
                Err(crate::errors::TraceDecayError::Config {
                    message: "simultaneous warmup completion".to_string(),
                })
            },
        );
        open_polled.notified().await;

        lifecycle.begin_draining();
        tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            lifecycle.wait_for_idle(),
        )
        .await
        .expect("simultaneous warmup drain timed out");
        assert!(
            !open_won.load(std::sync::atomic::Ordering::Acquire),
            "draining must win when project open becomes ready on the same tick"
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_bootstrap_catalog_bypasses_project_writer_gate() {
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let project = project.canonicalize().expect("canonical project");
    let client_identity = test_client_identity_for(profile_root.clone());
    let options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(client_identity.global_db_path.clone()),
    };
    drop(
        crate::tracedecay::TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize project"),
    );
    let registry = crate::global_db::GlobalDb::open_at(&client_identity.global_db_path)
        .await
        .expect("open global registry");
    registry
        .upsert_code_project("mcp-bootstrap-route-project", &project, None, None, None)
        .await
        .expect("register initialize root");
    drop(registry);
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "mcp-bootstrap-cache-test")
            .expect("daemon database scope");
    let engine = DaemonEngine::default();
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        allow_initialize_root_routing: true,
        ..test_handshake_defaults()
    };

    let store_administration = engine.store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        store_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bootstrap-cache-test", "version": "1"},
            "roots": [{"uri": project, "name": "registered-project"}]
        }
    });
    let tools_list = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let initialize_engine = engine.clone();
    let initialize_handshake = handshake.clone();
    let mut initialize_task = tokio::spawn(async move {
        super::handshake::daemon_round_trip(initialize_engine, &initialize_handshake, initialize)
            .await
    });
    let tools_list_engine = engine.clone();
    let tools_list_handshake = handshake.clone();
    let mut tools_list_task = tokio::spawn(async move {
        super::handshake::daemon_round_trip(tools_list_engine, &tools_list_handshake, tools_list)
            .await
    });
    let (initialize_within_bound, tools_list_within_bound) = tokio::join!(
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut initialize_task),
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut tools_list_task),
    );

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if initialize_within_bound.is_err() {
        let _ = initialize_task.await;
    }
    if tools_list_within_bound.is_err() {
        let _ = tools_list_task.await;
    }

    let initialize_responses = initialize_within_bound
        .expect("initialize must not wait for project writer gate")
        .expect("initialize client task");
    let initialize_response = initialize_responses
        .iter()
        .find(|response| response["id"] == json!(1))
        .expect("initialize response");
    assert_eq!(
        initialize_response["result"]["protocolVersion"],
        json!("2024-11-05")
    );
    assert_eq!(
        initialize_response["result"]["serverInfo"]["name"],
        json!("tracedecay")
    );
    assert_eq!(
        initialize_response["result"]["_meta"]["tracedecayInitializeRoute"],
        json!({
            "projectPath": handshake.project_path,
            "allowInit": false,
        })
    );

    let tools_list_responses = tools_list_within_bound
        .expect("tools/list must not wait for project writer gate")
        .expect("tools/list client task");
    let tools = tools_list_responses
        .iter()
        .find(|response| response["id"] == json!(2))
        .and_then(|response| response["result"]["tools"].as_array())
        .expect("tools/list result catalog");
    assert!(
        !tools.is_empty(),
        "bootstrap tool catalog must not be empty"
    );
    let context_description = tools
        .iter()
        .find(|tool| tool["name"] == json!("tracedecay_context"))
        .and_then(|tool| tool["description"].as_str())
        .expect("context tool description");
    assert!(context_description.contains("10 calls maximum"));
    assert!(context_description.contains("project graph is warming"));

    let project_path = handshake.project_path.as_ref().expect("project path");
    let route =
        super::super::ProjectRouteKey::from_handshake(project_path, &handshake).expect("route");
    tokio::time::timeout(PHASE_TIMEOUT, async {
        loop {
            let warmed = engine
                .store_administration
                .project_servers()
                .lock()
                .await
                .get_route(&route)
                .is_some();
            if warmed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initialize background warmup timed out");
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "initialize warmup must singleflight one project open"
    );

    tokio::time::timeout(PHASE_TIMEOUT, engine.shutdown_all())
        .await
        .expect("bootstrap-cache shutdown timed out");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_tool_cache_miss_returns_warming_while_project_opens_in_background() {
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    let project = project.canonicalize().expect("canonical project");
    let client_identity = test_client_identity_for(profile_root.clone());
    let options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(client_identity.global_db_path.clone()),
    };
    drop(
        crate::tracedecay::TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize project"),
    );
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "direct-warmup-test")
            .expect("daemon database scope");
    let engine = DaemonEngine::default();
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };

    let store_administration = engine.store_administration.clone();
    let writer_held = Arc::new(tokio::sync::Notify::new());
    let writer_held_by_blocker = Arc::clone(&writer_held);
    let (release_writer, writer_release) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        store_administration
            .with_writer(|| async move {
                writer_held_by_blocker.notify_one();
                writer_release.await.expect("release writer gate");
            })
            .await;
    });
    writer_held.notified().await;

    let request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_status",
            "arguments": {"format": "json"}
        }
    });
    let request_engine = engine.clone();
    let request_handshake = handshake.clone();
    let mut request_task = tokio::spawn(async move {
        super::handshake::daemon_round_trip(request_engine, &request_handshake, request).await
    });
    let response_within_bound =
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut request_task).await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if response_within_bound.is_err() {
        let _ = request_task.await;
    }

    let responses = response_within_bound
        .expect("direct tool cache miss must return a bounded warming response")
        .expect("direct tool client task");
    let response = responses
        .iter()
        .find(|response| response["id"] == json!(3))
        .expect("direct tool response");
    let message = response["error"]["message"]
        .as_str()
        .expect("warming error message");
    assert!(message.contains("warming in the background"), "{message}");
    assert!(message.contains("retry"), "{message}");

    let route =
        super::super::ProjectRouteKey::from_handshake(&project, &handshake).expect("project route");
    tokio::time::timeout(PHASE_TIMEOUT, async {
        loop {
            let warmed = engine
                .store_administration
                .project_servers()
                .lock()
                .await
                .get_route(&route)
                .is_some();
            if warmed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached project warmup timed out");
    tokio::time::timeout(PHASE_TIMEOUT, engine.shutdown_all())
        .await
        .expect("direct warmup shutdown timed out");
}
