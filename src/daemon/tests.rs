use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::sync::Arc;

#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::task::JoinHandle;

#[cfg(unix)]
use super::{AutomationSchedulerHandle, DaemonEngine, drain_client_tasks};
use super::{
    DaemonClientIdentity, DaemonHandshake, DaemonLifecycle, DatabaseOwnerRegistry, ProjectRouteKey,
    ProjectServerKey, StoreAdministration, StoreOwnerKey,
};

mod compatibility;

#[test]
fn daemon_lifecycle_rejects_new_work_after_draining() {
    let lifecycle = DaemonLifecycle::default();
    assert!(lifecycle.accepting());

    lifecycle.begin_draining();

    assert!(!lifecycle.accepting());
}

#[test]
fn bootstrap_tool_catalog_uses_project_node_count() {
    let request: super::JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    }))
    .expect("tools/list request");
    let super::DaemonBootstrap::Respond(response) =
        super::daemon_bootstrap_response(&request, None, Some(65_395)).expect("bootstrap response")
    else {
        panic!("tools/list must produce a response");
    };
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

#[tokio::test]
async fn portable_broker_requests_reuse_one_authenticated_project_owner() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

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
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "portable-owner-cache-test")
            .expect("daemon database scope");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let route = super::ProjectRouteKey::from_handshake(&project, &handshake).expect("route key");
    let owners = std::sync::Arc::new(tokio::sync::Mutex::new(
        super::DatabaseOwnerRegistry::default(),
    ));
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners));
    let gates = std::sync::Arc::new(tokio::sync::Mutex::new(super::ProjectOpenGates::default()));
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();
    let (listener, endpoint) =
        super::transport::BrokerListener::bind(&super::transport::default_loopback_endpoint())
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
                    Box::pin(super::serve_windows_broker_client(
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
            let stream = super::transport::BrokerStream::connect(&endpoint)
                .await
                .expect("connect client");
            let (reader, mut writer) = stream.into_split();
            let preface = super::transport::DaemonAuthPreface::new(TOKEN)
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
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("portable project warmup timed out");
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
    let route = ProjectRouteKey::from_handshake(&project, &handshake).expect("project route");
    let owners = Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default()));
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners));
    let gates = Arc::new(tokio::sync::Mutex::new(super::ProjectOpenGates::default()));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle = DaemonLifecycle::default();
    let (listener, endpoint) =
        super::transport::BrokerListener::bind(&super::transport::default_loopback_endpoint())
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
                Box::pin(super::serve_windows_broker_client(
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
            let stream = super::transport::BrokerStream::connect(&endpoint)
                .await
                .expect("connect client");
            let (reader, mut writer) = stream.into_split();
            let preface = super::transport::DaemonAuthPreface::new(TOKEN)
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
    super::shutdown_project_servers(&store_administration).await;
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
        super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
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
        super::send_daemon_request_line_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            &request,
            std::time::Duration::from_millis(10),
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
        super::send_daemon_request_line_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            &request,
            std::time::Duration::from_millis(10),
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
        super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
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

#[cfg(unix)]
#[tokio::test]
async fn draining_waits_for_one_bounded_in_flight_request() {
    let lifecycle = DaemonLifecycle::default();
    let activity = lifecycle.try_enter().expect("request should start");
    let mut clients = tokio::task::JoinSet::new();
    clients.spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        drop(activity);
        Ok(())
    });

    lifecycle.begin_draining();
    let drained = drain_client_tasks(&mut clients, tokio::time::Duration::from_secs(1)).await;
    lifecycle.wait_for_idle().await;

    assert!(drained);
    assert!(lifecycle.try_enter().is_none());
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

    engine
        .spawn_project_server_warmup(handshake, initialize_request)
        .await;
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
        super::spawn_lifecycle_automation_scheduler_activation(lifecycle.clone(), async move {
            discovery_polled_by_future.notify_one();
            discovery_lifecycle.wait_for_draining().await;
            discovery_won_by_future.store(true, std::sync::atomic::Ordering::Release);
        });
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
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(super::ProjectOpenGates::default()));
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

    super::spawn_portable_project_server_warmup(
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
        super::spawn_lifecycle_project_server_warmup(
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
#[tokio::test]
async fn daemon_scheduler_shutdown_aborts_and_joins_every_loop() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/shutdown-test"),
            global_db_path: PathBuf::from("/profiles/shutdown-test/global.db"),
            project_id: Some("shutdown-test".to_string()),
            store_root: PathBuf::from("/stores/shutdown-test"),
            graph_db_path: PathBuf::from("/stores/shutdown-test/graph.db"),
        },
        scope_prefix: None,
    };
    let task = tokio::spawn(std::future::pending::<()>());
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(
            key,
            AutomationSchedulerHandle {
                task,
                wake: std::sync::Arc::new(tokio::sync::Notify::new()),
            },
        );

    engine.lifecycle.begin_draining();
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.shutdown_automation_schedulers(),
    )
    .await
    .expect("scheduler shutdown should not wait for its tick interval");

    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_server_cache_hit_skips_open_and_singleflights_first_miss() {
    const PHASE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let project_alias = temp.path().join("project-alias");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&project).expect("project dir");
    std::os::unix::fs::symlink(&project, &project_alias).expect("project alias");
    let client_identity = test_client_identity_for(profile_root.clone());
    let options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(client_identity.global_db_path.clone()),
    };
    eprintln!("[cache-test] phase=init start");
    let initialized = crate::tracedecay::TraceDecay::init_with_options(&project, options)
        .await
        .expect("initialize project");
    drop(initialized);
    let mut config = crate::config::load_config(&project).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&project, &config)
        .expect("disable unrelated startup transcript ingestion");
    eprintln!("[cache-test] phase=init done");

    let direct = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let aliased = DaemonHandshake {
        project_path: Some(project_alias),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "project-server-cache-test")
            .expect("daemon database scope");
    let engine = DaemonEngine::default();
    let direct_route = super::ProjectRouteKey::from_handshake(&project, &direct).unwrap();
    let alias_route = super::ProjectRouteKey::from_handshake(
        &project.canonicalize().expect("canonical project"),
        &aliased,
    )
    .unwrap();
    assert_eq!(
        direct_route, alias_route,
        "aliases must share one route gate"
    );

    eprintln!("[cache-test] phase=concurrent-open start");
    let (direct_server, alias_server) = tokio::time::timeout(PHASE_TIMEOUT, async {
        tokio::join!(
            engine.project_server(&direct),
            engine.project_server(&aliased)
        )
    })
    .await
    .expect("cache-test concurrent-open phase timed out");
    eprintln!("[cache-test] phase=concurrent-open done");
    let direct_server = direct_server.expect("direct project server");
    let alias_server = alias_server.expect("aliased project server");
    assert!(std::sync::Arc::ptr_eq(&direct_server, &alias_server));
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "canonical aliases must singleflight the first project open"
    );

    eprintln!("[cache-test] phase=cached-open start");
    let cached = tokio::time::timeout(PHASE_TIMEOUT, engine.project_server(&direct))
        .await
        .expect("cache-test cached-open phase timed out")
        .expect("cached project server");
    eprintln!("[cache-test] phase=cached-open done");
    assert!(std::sync::Arc::ptr_eq(&direct_server, &cached));
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache hits must return before opening project databases"
    );

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

    let cached_while_writer_held = tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        engine.project_server(&direct),
    )
    .await;
    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");

    let cached_while_writer_held = cached_while_writer_held
        .expect("cached project server must not wait for writer gate")
        .expect("cached project server while writer gate held");
    assert!(std::sync::Arc::ptr_eq(
        &direct_server,
        &cached_while_writer_held
    ));
    drop(cached);
    drop(alias_server);
    drop(direct_server);
    eprintln!("[cache-test] phase=shutdown start");
    tokio::time::timeout(PHASE_TIMEOUT, engine.shutdown_all())
        .await
        .expect("cache-test shutdown phase timed out");
    eprintln!("[cache-test] phase=shutdown done");
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
        daemon_round_trip(initialize_engine, &initialize_handshake, initialize).await
    });
    let tools_list_engine = engine.clone();
    let tools_list_handshake = handshake.clone();
    let mut tools_list_task = tokio::spawn(async move {
        daemon_round_trip(tools_list_engine, &tools_list_handshake, tools_list).await
    });
    let (initialize_within_bound, tools_list_within_bound) = tokio::join!(
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut initialize_task),
        tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut tools_list_task),
    );

    let direct_tool = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_status",
            "arguments": {"format": "json"}
        }
    });
    let direct_tool_engine = engine.clone();
    let direct_tool_handshake = handshake.clone();
    let mut direct_tool_task = tokio::spawn(async move {
        daemon_round_trip(direct_tool_engine, &direct_tool_handshake, direct_tool).await
    });
    let direct_tool_before_warmup = tokio::time::timeout(
        super::CONTENDED_PROJECT_OPEN_GRACE + tokio::time::Duration::from_millis(250),
        &mut direct_tool_task,
    )
    .await;
    assert!(
        direct_tool_before_warmup.is_err(),
        "a same-route tool call must wait for initialize warmup"
    );

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    if initialize_within_bound.is_err() {
        let _ = initialize_task.await;
    }
    if tools_list_within_bound.is_err() {
        let _ = tools_list_task.await;
    }
    let direct_tool_responses = tokio::time::timeout(PHASE_TIMEOUT, &mut direct_tool_task)
        .await
        .expect("same-route tool call timed out after warmup")
        .expect("direct tool client task");
    let direct_tool_response = direct_tool_responses
        .iter()
        .find(|response| response["id"] == json!(3))
        .expect("direct tool response");
    assert!(
        direct_tool_response.get("result").is_some(),
        "{direct_tool_response}"
    );

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
    let route = ProjectRouteKey::from_handshake(project_path, &handshake).expect("project route");
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
        daemon_round_trip(request_engine, &request_handshake, request).await
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

    let route = ProjectRouteKey::from_handshake(&project, &handshake).expect("project route");
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

#[cfg(unix)]
#[test]
fn store_owner_key_collapses_profile_and_store_aliases() {
    let temp = TempDir::new().expect("temp dir");
    let profile = temp.path().join("profile");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&profile).expect("profile dir");
    std::fs::create_dir_all(&store).expect("store dir");
    let profile_alias = temp.path().join("profile-alias");
    let store_alias = temp.path().join("store-alias");
    std::os::unix::fs::symlink(&profile, &profile_alias).expect("profile alias");
    std::os::unix::fs::symlink(&store, &store_alias).expect("store alias");

    let direct = StoreOwnerKey::from_paths(
        &profile,
        &profile.join("global.db"),
        Some("project-id".to_string()),
        &store,
        &store.join("graph.db"),
    )
    .expect("direct owner");
    let aliased = StoreOwnerKey::from_paths(
        &profile_alias,
        &profile_alias.join("global.db"),
        Some("project-id".to_string()),
        &store_alias,
        &store_alias.join("graph.db"),
    )
    .expect("aliased owner");

    assert_eq!(direct, aliased);
}

#[cfg(unix)]
#[test]
fn database_owner_registry_rekeys_and_evicts_stale_routes() {
    let owner = StoreOwnerKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_id: Some("project".to_string()),
        store_root: PathBuf::from("/store"),
        graph_db_path: PathBuf::from("/store/main.db"),
    };
    let old = ProjectServerKey {
        owner: owner.clone(),
        scope_prefix: Some("src".to_string()),
    };
    let mut feature_owner = owner;
    feature_owner.graph_db_path = PathBuf::from("/store/feature.db");
    let new = ProjectServerKey {
        owner: feature_owner,
        scope_prefix: Some("src".to_string()),
    };
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project"),
        scope_prefix: Some("src".to_string()),
    };
    let mut registry = DatabaseOwnerRegistry::<u8>::default();
    registry.insert(old.clone(), 7);
    registry.bind_route(route.clone(), old.clone());

    assert!(registry.rekey(&old, &new));

    assert!(registry.get(&old).is_none());
    assert_eq!(registry.get(&new), Some(&7));
    assert_eq!(registry.get_route(&route), Some((&new, &7)));

    let mut collision = DatabaseOwnerRegistry::<u8>::default();
    collision.insert(old.clone(), 7);
    collision.insert(new.clone(), 9);
    collision.bind_route(route.clone(), old.clone());
    assert!(!collision.rekey(&old, &new));
    assert!(collision.get(&old).is_none());
    assert_eq!(collision.get(&new), Some(&9));
    assert!(collision.get_route(&route).is_none());
}

#[test]
fn database_owner_registry_race_keeps_first_server_and_binds_route() {
    let owner = StoreOwnerKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_id: Some("project".to_string()),
        store_root: PathBuf::from("/store"),
        graph_db_path: PathBuf::from("/store/main.db"),
    };
    let key = ProjectServerKey {
        owner,
        scope_prefix: None,
    };
    let route = ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from("/project-alias"),
        scope_prefix: None,
    };
    let mut registry = DatabaseOwnerRegistry::<u8>::default();
    registry.insert(key.clone(), 7);

    let (resolved, inserted) = registry.bind_or_insert_route(route.clone(), key.clone(), 9);

    assert_eq!(resolved, 7);
    assert!(!inserted);
    assert_eq!(registry.get_route(&route), Some((&key, &7)));
}

fn test_client_identity() -> DaemonClientIdentity {
    test_client_identity_for(PathBuf::from("/profiles/client"))
}

fn test_client_identity_for(profile_root: PathBuf) -> DaemonClientIdentity {
    DaemonClientIdentity {
        global_db_path: profile_root.join("global.db"),
        profile_root,
    }
}

fn test_handshake_defaults() -> DaemonHandshake {
    DaemonHandshake {
        project_path: None,
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: test_client_identity(),
        client_version: super::binary_version().to_string(),
        client_instance_id: crate::runtime_identity::process_run_id().to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
    }
}

#[cfg(unix)]
fn test_client_instance_id(value: u128) -> String {
    format!("{value:032x}")
}

#[cfg(unix)]
async fn await_test_task<T>(task: JoinHandle<T>, label: &str) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap_or_else(|_| panic!("{label} timed out"))
        .unwrap_or_else(|e| panic!("{label} panicked: {e}"))
}

#[cfg(unix)]
async fn answer_one_proxy_request(listener: tokio::net::UnixListener, generation: u64) {
    let (stream, _addr) = listener.accept().await.expect("accept proxied client");
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    let handshake_line = lines
        .next_line()
        .await
        .expect("read handshake")
        .expect("handshake line");
    DaemonHandshake::from_line(&handshake_line).expect("parse handshake");
    let request_line = lines
        .next_line()
        .await
        .expect("read request")
        .expect("request line");
    let request: Value = serde_json::from_str(&request_line).expect("request json");
    let response = json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": { "generation": generation }
    });
    writer
        .write_all(
            serde_json::to_string(&response)
                .expect("response json")
                .as_bytes(),
        )
        .await
        .expect("write response");
    writer.write_all(b"\n").await.expect("write newline");
    writer.shutdown().await.expect("shutdown fake daemon");
}

#[cfg(unix)]
async fn answer_one_authenticated_proxy_request(
    listener: tokio::net::UnixListener,
    expected_token: &str,
    generation: u64,
) {
    let (stream, _addr) = listener.accept().await.expect("accept proxied client");
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    let auth_line = lines
        .next_line()
        .await
        .expect("read auth preface")
        .expect("auth preface line");
    let preface =
        super::transport::DaemonAuthPreface::from_line(auth_line.trim()).expect("auth preface");
    assert!(
        preface.authenticate(expected_token),
        "proxy must reload the current daemon authority token"
    );
    let handshake_line = lines
        .next_line()
        .await
        .expect("read handshake")
        .expect("handshake line");
    DaemonHandshake::from_line(&handshake_line).expect("parse handshake");
    let request_line = lines
        .next_line()
        .await
        .expect("read request")
        .expect("request line");
    let request: Value = serde_json::from_str(&request_line).expect("request json");
    let response = json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": { "generation": generation }
    });
    writer
        .write_all(
            serde_json::to_string(&response)
                .expect("response json")
                .as_bytes(),
        )
        .await
        .expect("write response");
    writer.write_all(b"\n").await.expect("write newline");
    writer.shutdown().await.expect("shutdown fake daemon");
}

#[cfg(unix)]
async fn daemon_round_trip(
    engine: super::DaemonEngine,
    handshake: &DaemonHandshake,
    request: Value,
) -> Vec<Value> {
    let (server_stream, client_stream) =
        tokio::net::UnixStream::pair().expect("daemon socket pair");
    let server =
        tokio::spawn(
            async move { Box::pin(super::serve_socket_client(server_stream, engine)).await },
        );
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
fn daemon_log_line_formats_stable_key_value_fields() {
    let line = super::format_daemon_log_line(
        "scheduler_task",
        &[
            ("task", "memory_curator".to_string()),
            ("outcome", "not due yet".to_string()),
            ("project", "/tmp/example project".to_string()),
        ],
    );

    assert_eq!(
        line,
        "[tracedecay] event=scheduler_task task=memory_curator outcome=\"not due yet\" project=\"/tmp/example project\""
    );
}

#[test]
fn daemon_log_line_escapes_quotes_and_backslashes() {
    let line = super::format_daemon_log_line(
        "client_error",
        &[("error", r#"failed at "step" \ retry"#.to_string())],
    );

    assert_eq!(
        line,
        r#"[tracedecay] event=client_error error="failed at \"step\" \\ retry""#
    );
}

#[test]
fn daemon_log_line_escapes_control_characters() {
    let line = super::format_daemon_log_line(
        "client_error",
        &[("error", "first\nsecond\rthird\tfourth".to_string())],
    );

    assert_eq!(
        line,
        r#"[tracedecay] event=client_error error="first\nsecond\rthird\tfourth""#
    );
}

#[cfg(unix)]
#[test]
fn transient_daemon_connect_errors_cover_restart_window_only() {
    assert!(super::is_transient_daemon_connect_error(
        std::io::ErrorKind::NotFound
    ));
    assert!(super::is_transient_daemon_connect_error(
        std::io::ErrorKind::ConnectionRefused
    ));
    assert!(!super::is_transient_daemon_connect_error(
        std::io::ErrorKind::PermissionDenied
    ));
}

// start_paused: these restart-window tests only wait on tokio timers
// (sleep/poll intervals); paused time auto-advances them so each test
// finishes in milliseconds instead of real 200-300 ms waits.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn connect_with_restart_grace_reconnects_once_daemon_rebinds() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");

    // Simulate the `tracedecay update` restart window: the socket is
    // missing for a while, then the new daemon binds the same path.
    let bind_path = socket.clone();
    let daemon = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        tokio::net::UnixListener::bind(&bind_path).expect("bind restarted daemon socket")
    });

    super::connect_with_restart_grace(
        &super::connection_for_socket_path(&socket),
        std::time::Duration::from_secs(8),
        std::time::Duration::from_millis(50),
    )
    .await
    .expect("connect should succeed once the restarted daemon binds");
    daemon.await.expect("daemon bind task");
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn connect_with_restart_grace_gives_up_with_restart_hint() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");

    let err = super::connect_with_restart_grace(
        &super::connection_for_socket_path(&socket),
        std::time::Duration::from_millis(300),
        std::time::Duration::from_millis(50),
    )
    .await
    .expect_err("connect should fail when no daemon ever binds");

    let message = err.to_string();
    assert!(
        message.contains("tracedecay update"),
        "error should hint that the daemon may be restarting after an update, got: {message}"
    );
    assert!(
        message.contains(&socket.display().to_string()),
        "error should name the socket path, got: {message}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn initialize_root_routing_replaces_cached_project_and_scope() {
    let profile = TempDir::new().expect("profile temp dir");
    let project_a = TempDir::new().expect("project a temp dir");
    let project_b = TempDir::new().expect("project b temp dir");
    let project_a = project_a.path().canonicalize().expect("project a path");
    let project_b = project_b.path().canonicalize().expect("project b path");
    let global_db_path = profile.path().join("global.db");
    let registry = crate::global_db::GlobalDb::open_at(&global_db_path)
        .await
        .expect("open registry");
    registry
        .upsert_code_project("project-a", &project_a, None, None, None)
        .await
        .expect("register project a");
    registry
        .upsert_code_project("project-b", &project_b, None, None, None)
        .await
        .expect("register project b");
    drop(registry);

    let mut base_handshake = test_handshake_defaults();
    base_handshake.project_path = Some(project_a.clone());
    base_handshake.scope_prefix = Some("src".to_string());
    base_handshake.allow_initialize_root_routing = true;
    base_handshake.client_identity = test_client_identity_for(profile.path().to_path_buf());
    base_handshake.client_identity.global_db_path = global_db_path;
    let mut routed_handshake = base_handshake.clone();
    let store_administration = super::StoreAdministration::default();

    let line = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "roots": [{
                "uri": project_b.to_string_lossy(),
                "name": "project-b"
            }]
        }
    })
    .to_string();

    super::reset_proxy_handshake_for_initialize(&base_handshake, &mut routed_handshake, &line);
    let route =
        super::apply_daemon_initialize_route(&mut routed_handshake, &line, &store_administration)
            .await
            .expect("daemon initialize routing should succeed")
            .expect("registered initialize root should produce a route");
    assert_eq!(route.project_path, project_b);

    assert_eq!(
        routed_handshake.project_path.as_deref(),
        Some(project_b.as_path())
    );
    assert_eq!(routed_handshake.scope_prefix, None);

    let rerun_without_roots = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "initialize",
        "params": {}
    })
    .to_string();
    super::reset_proxy_handshake_for_initialize(
        &base_handshake,
        &mut routed_handshake,
        &rerun_without_roots,
    );
    assert!(
        super::apply_daemon_initialize_route(
            &mut routed_handshake,
            &rerun_without_roots,
            &store_administration,
        )
        .await
        .expect("daemon initialize reroute should succeed")
        .is_none()
    );

    assert_eq!(
        routed_handshake.project_path.as_deref(),
        Some(project_a.as_path()),
        "reinitialize without a route must not keep the previous routed project"
    );
    assert_eq!(routed_handshake.scope_prefix.as_deref(), Some("src"));
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_resolves_registry_only_initialize_root_alias() {
    let profile = TempDir::new().expect("profile temp dir");
    let canonical = TempDir::new().expect("canonical project temp dir");
    let alias = TempDir::new().expect("project alias temp dir");
    let canonical = canonical.path().canonicalize().expect("canonical project");
    let alias = alias.path().canonicalize().expect("canonical alias");
    let nested = alias.join("nested");
    std::fs::create_dir_all(&nested).expect("nested alias path");
    let global_db_path = profile.path().join("global.db");
    let registry = crate::global_db::GlobalDb::open_at(&global_db_path)
        .await
        .expect("open registry");
    registry
        .upsert_code_project("project-registry-only", &canonical, None, None, None)
        .await
        .expect("register canonical project");
    registry
        .upsert_project_alias(&alias, "project-registry-only")
        .await
        .expect("register project alias");
    drop(registry);

    let mut handshake = test_handshake_defaults();
    handshake.allow_initialize_root_routing = true;
    handshake.client_identity = test_client_identity_for(profile.path().to_path_buf());
    handshake.client_identity.global_db_path = global_db_path;
    let store_administration = super::StoreAdministration::default();
    let line = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "roots": [{ "uri": nested, "name": "alias" }] }
    })
    .to_string();

    let route = super::apply_daemon_initialize_route(&mut handshake, &line, &store_administration)
        .await
        .expect("daemon initialize routing should succeed")
        .expect("authenticated daemon should resolve registry alias");
    assert_eq!(route.project_path, alias);
    assert_eq!(handshake.project_path.as_deref(), Some(alias.as_path()));
    assert!(!route.allow_init);
}

#[cfg(unix)]
#[tokio::test]
async fn initialize_root_routing_delegates_config_gated_git_auto_init() {
    let profile = TempDir::new().expect("profile temp dir");
    let fallback = TempDir::new().expect("fallback temp dir");
    let project = TempDir::new().expect("git project temp dir");
    let git_status = std::process::Command::new(crate::git::git_program())
        .args(["init", "-q"])
        .current_dir(project.path())
        .status()
        .expect("git init");
    assert!(git_status.success(), "git init should succeed");
    let project = project
        .path()
        .canonicalize()
        .expect("canonical git project");

    let mut base_handshake = test_handshake_defaults();
    base_handshake.project_path = Some(fallback.path().to_path_buf());
    base_handshake.allow_initialize_root_routing = true;
    base_handshake.client_identity = test_client_identity_for(profile.path().to_path_buf());
    let line = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "roots": [{
                "uri": format!("file://{}", project.display()),
                "name": "unindexed-git-project"
            }]
        }
    })
    .to_string();

    let mut routed_handshake = base_handshake.clone();
    let store_administration = super::StoreAdministration::default();
    super::reset_proxy_handshake_for_initialize(&base_handshake, &mut routed_handshake, &line);
    super::apply_daemon_initialize_route(&mut routed_handshake, &line, &store_administration)
        .await
        .expect("daemon should delegate auto-init");
    assert_eq!(
        routed_handshake.project_path.as_deref(),
        Some(project.as_path())
    );
    assert!(routed_handshake.allow_init);

    let mut config = crate::config::TraceDecayConfig {
        root_dir: project.display().to_string(),
        ..crate::config::TraceDecayConfig::default()
    };
    config.sync.auto_init = false;
    crate::config::save_config(&project, &config).expect("disable auto-init");
    super::reset_proxy_handshake_for_initialize(&base_handshake, &mut routed_handshake, &line);
    super::apply_daemon_initialize_route(&mut routed_handshake, &line, &store_administration)
        .await
        .expect("daemon should resolve git root with auto-init disabled");
    assert_eq!(
        routed_handshake.project_path.as_deref(),
        Some(project.as_path())
    );
    assert!(!routed_handshake.allow_init);
}

#[cfg(unix)]
#[tokio::test]
async fn serve_proxies_when_socket_already_exists() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");

    assert!(
        super::should_proxy_serve_to_daemon_with(
            &socket,
            None,
            std::time::Duration::from_secs(8),
            std::time::Duration::from_millis(50),
        )
        .await
    );
}

#[cfg(unix)]
#[tokio::test]
async fn serve_stays_in_process_without_socket_or_installed_service() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let other_socket = dir.path().join("other.sock");

    // No socket and no service claiming it: fall back immediately, even
    // with a long grace configured — startup must not stall.
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::should_proxy_serve_to_daemon_with(
            &socket,
            None,
            std::time::Duration::from_secs(8),
            std::time::Duration::from_millis(50),
        ),
    )
    .await
    .expect("decision without daemon evidence should be immediate");
    assert!(!decision);

    // A service installed for a different socket is not evidence either.
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::should_proxy_serve_to_daemon_with(
            &socket,
            Some(&other_socket),
            std::time::Duration::from_secs(8),
            std::time::Duration::from_millis(50),
        ),
    )
    .await
    .expect("mismatched service socket should not delay the decision");
    assert!(!decision);
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn serve_waits_out_restart_window_when_service_owns_socket() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");

    // Simulate the `tracedecay update` restart window: the service is
    // installed but the old daemon already unlinked the socket; the new
    // daemon binds it shortly after serve starts.
    let bind_path = socket.clone();
    let daemon = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        tokio::net::UnixListener::bind(&bind_path).expect("bind restarted daemon socket")
    });

    assert!(
        super::should_proxy_serve_to_daemon_with(
            &socket,
            Some(&socket),
            std::time::Duration::from_secs(8),
            std::time::Duration::from_millis(50),
        )
        .await,
        "serve started during a daemon restart should still pick the daemon transport"
    );
    daemon.await.expect("daemon bind task");
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn serve_falls_back_when_installed_service_never_rebinds() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");

    assert!(
        !super::should_proxy_serve_to_daemon_with(
            &socket,
            Some(&socket),
            std::time::Duration::from_millis(200),
            std::time::Duration::from_millis(50),
        )
        .await,
        "a stopped service should fall back to in-process after the grace expires"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn proxied_request_survives_daemon_restart_window() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");

    let bind_path = socket.clone();
    let daemon = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let listener =
            tokio::net::UnixListener::bind(&bind_path).expect("bind restarted daemon socket");
        let (stream, _addr) = listener.accept().await.expect("accept proxied client");
        let (reader, mut writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let handshake_line = lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake line");
        DaemonHandshake::from_line(&handshake_line).expect("parse handshake");
        let request_line = lines
            .next_line()
            .await
            .expect("read request")
            .expect("request line");
        let request: Value = serde_json::from_str(&request_line).expect("request json");
        let response = json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": { "ok": true }
        });
        writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response json")
                    .as_bytes(),
            )
            .await
            .expect("write response");
        writer.write_all(b"\n").await.expect("write newline");
    });

    let handshake = test_handshake_defaults();
    let request = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/list"
    }))
    .expect("request json");

    let responses = super::send_daemon_request_line(&socket, &handshake, &request)
        .await
        .expect("request should succeed once the restarted daemon is back");

    assert_eq!(responses.len(), 1);
    let response: Value = serde_json::from_str(responses[0].trim()).expect("proxied response json");
    assert_eq!(response["id"], json!(42));
    assert_eq!(response["result"]["ok"], json!(true));
    daemon.await.expect("fake daemon task");
}

#[cfg(unix)]
#[tokio::test]
async fn long_lived_proxy_reconnects_after_daemon_socket_rebind() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let first_listener = tokio::net::UnixListener::bind(&socket).expect("bind first daemon socket");
    let rebound_socket = socket.clone();
    let (unbound_tx, unbound_rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        answer_one_proxy_request(first_listener, 1).await;
        std::fs::remove_file(&rebound_socket).expect("unlink first daemon socket");
        unbound_tx.send(()).expect("notify daemon outage");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let second_listener =
            tokio::net::UnixListener::bind(&rebound_socket).expect("bind second daemon socket");
        answer_one_proxy_request(second_listener, 2).await;
    });

    let (mut transport, sender, mut receiver) = crate::mcp::transport::ChannelTransport::new();
    let proxy_socket = socket.clone();
    let proxy = tokio::spawn(async move {
        super::proxy_transport_to_daemon(
            &proxy_socket,
            &test_handshake_defaults(),
            None,
            &mut transport,
        )
        .await
    });

    let request = |id| {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list"
        }))
        .expect("request json")
    };
    sender.send(request(1)).expect("send first request");
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await
        .expect("first response timed out")
        .expect("first response");
    let first: Value = serde_json::from_str(first.trim()).expect("first response json");
    assert_eq!(first["result"]["generation"], json!(1));

    unbound_rx.await.expect("first daemon should unlink socket");
    sender.send(request(2)).expect("send second request");
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await
        .expect("second response timed out")
        .expect("second response");
    let second: Value = serde_json::from_str(second.trim()).expect("second response json");
    assert_eq!(second["result"]["generation"], json!(2));

    drop(sender);
    await_test_task(proxy, "long-lived proxy task")
        .await
        .expect("proxy transport");
    await_test_task(daemon, "daemon rebind task").await;
}

#[cfg(unix)]
#[tokio::test]
async fn long_lived_proxy_reloads_rotated_auth_after_daemon_restart() {
    let dir = TempDir::new().expect("temp dir");
    let profile = dir.path().canonicalize().expect("canonical profile");
    let socket = profile.join("daemon.sock");
    let endpoint = super::transport::DaemonEndpoint::Unix(socket.clone());
    let first_listener = tokio::net::UnixListener::bind(&socket).expect("bind first daemon socket");
    let first_authority = super::authority::DaemonAuthority::acquire(&profile, &endpoint, "first")
        .expect("first daemon authority");
    let first_token = first_authority.auth_token().to_string();
    let rebound_socket = socket.clone();
    let rebound_profile = profile.clone();
    let rebound_endpoint = endpoint.clone();
    let (unbound_tx, unbound_rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        answer_one_authenticated_proxy_request(first_listener, &first_token, 1).await;
        drop(first_authority);
        std::fs::remove_file(&rebound_socket).expect("unlink first daemon socket");
        unbound_tx.send(()).expect("notify daemon outage");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let second_listener =
            tokio::net::UnixListener::bind(&rebound_socket).expect("bind second daemon socket");
        let second_authority = super::authority::DaemonAuthority::acquire(
            &rebound_profile,
            &rebound_endpoint,
            "second",
        )
        .expect("second daemon authority");
        let second_token = second_authority.auth_token().to_string();
        assert_ne!(first_token, second_token, "daemon restart must rotate auth");
        answer_one_authenticated_proxy_request(second_listener, &second_token, 2).await;
        drop(second_authority);
    });

    let (mut transport, sender, mut receiver) = crate::mcp::transport::ChannelTransport::new();
    let proxy_socket = socket.clone();
    let proxy = tokio::spawn(async move {
        super::proxy_transport_to_daemon(
            &proxy_socket,
            &test_handshake_defaults(),
            None,
            &mut transport,
        )
        .await
    });
    let request = |id| {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list"
        }))
        .expect("request json")
    };

    sender.send(request(1)).expect("send first request");
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await
        .expect("first response timed out")
        .expect("first response");
    let first: Value = serde_json::from_str(first.trim()).expect("first response json");
    assert_eq!(first["result"]["generation"], json!(1));

    unbound_rx.await.expect("first daemon should unlink socket");
    sender.send(request(2)).expect("send second request");
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await
        .expect("second response timed out")
        .expect("second response");
    let second: Value = serde_json::from_str(second.trim()).expect("second response json");
    assert_eq!(second["result"]["generation"], json!(2));

    drop(sender);
    await_test_task(proxy, "rotating-auth proxy task")
        .await
        .expect("proxy transport");
    await_test_task(daemon, "rotating-auth daemon task").await;
}

#[cfg(unix)]
#[tokio::test]
async fn proxy_uses_daemon_initialize_route_without_registry_access() {
    let dir = TempDir::new().expect("temp dir");
    let temp_root = dir.path().canonicalize().expect("canonical temp dir");
    let active_root = temp_root.join("active");
    let target_root = temp_root.join("target");
    std::fs::create_dir_all(active_root.join("src")).expect("active src");
    std::fs::create_dir_all(target_root.join("src")).expect("target src");
    let active = active_root.canonicalize().expect("active root");
    let target = target_root.canonicalize().expect("target root");
    let socket = temp_root.join("daemon.sock");
    let mut client_identity = test_client_identity_for(temp_root.join("profile"));
    client_identity.global_db_path = temp_root.join("proxy-cannot-open-this-directory");
    std::fs::create_dir_all(&client_identity.global_db_path).expect("non-database authority path");

    let listener = tokio::net::UnixListener::bind(&socket).expect("daemon socket");
    let daemon_target = target.clone();
    let accept_task = tokio::spawn(async move {
        let mut projects = Vec::new();
        for _ in 0..4 {
            let (stream, _addr) = listener.accept().await.expect("accept daemon client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let handshake_line = lines
                .next_line()
                .await
                .expect("read handshake")
                .expect("handshake line");
            let handshake =
                DaemonHandshake::from_line(&handshake_line).expect("daemon handshake json");
            let request_line = lines
                .next_line()
                .await
                .expect("read request")
                .expect("request line");
            let request: Value = serde_json::from_str(&request_line).expect("request json");
            let mut project = handshake
                .project_path
                .as_ref()
                .map(|path| path.display().to_string());
            let mut result = json!({ "project": project });
            if request["method"] == json!("initialize")
                && request
                    .pointer("/params/roots")
                    .and_then(Value::as_array)
                    .is_some_and(|roots| !roots.is_empty())
            {
                project = Some(daemon_target.display().to_string());
                result["project"] = json!(project);
                result["_meta"]["tracedecayInitializeRoute"] = json!({
                    "projectPath": daemon_target,
                    "allowInit": false,
                });
            }
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": result
            });
            writer
                .write_all(
                    serde_json::to_string(&response)
                        .expect("response json")
                        .as_bytes(),
                )
                .await
                .expect("write response");
            writer.write_all(b"\n").await.expect("write newline");
            writer.shutdown().await.expect("shutdown fake daemon");
            projects.push(
                handshake
                    .project_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            );
        }
        projects
    });

    let (mut transport, sender, mut receiver) = crate::mcp::transport::ChannelTransport::new();
    sender
        .send(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "codex", "version": "test"},
                    "roots": [{"uri": format!("file://{}", target.display()), "name": "target"}]
                }
            }))
            .expect("initialize json"),
        )
        .expect("send initialize");
    sender
        .send(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_files",
                    "arguments": {"layout": "flat"}
                }
            }))
            .expect("tools/call json"),
        )
        .expect("send tools/call");
    sender
        .send(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "codex", "version": "test"}
                }
            }))
            .expect("reinitialize json"),
        )
        .expect("send reinitialize");
    sender
        .send(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_files",
                    "arguments": {"layout": "flat"}
                }
            }))
            .expect("post-reinitialize tools/call json"),
        )
        .expect("send post-reinitialize tools/call");
    drop(sender);

    let handshake = DaemonHandshake {
        project_path: Some(active.clone()),
        allow_initialize_root_routing: true,
        client_identity,
        ..test_handshake_defaults()
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::proxy_transport_to_daemon(&socket, &handshake, None, &mut transport),
    )
    .await
    .expect("proxy transport timed out")
    .expect("proxy transport");

    let mut responses = Vec::new();
    while let Ok(Some(line)) =
        tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv()).await
    {
        responses.push(line);
    }
    let response_project = |id| {
        responses
            .iter()
            .map(|line| serde_json::from_str::<Value>(line.trim()).expect("response json"))
            .find(|response| response["id"] == json!(id))
            .and_then(|response| response["result"]["project"].as_str().map(str::to_string))
    };
    let target = target.display().to_string();
    let active = active.display().to_string();
    assert_eq!(response_project(1).as_deref(), Some(target.as_str()));
    assert_eq!(response_project(2).as_deref(), Some(target.as_str()));
    assert_eq!(response_project(3).as_deref(), Some(active.as_str()));
    assert_eq!(response_project(4).as_deref(), Some(active.as_str()));

    let served_projects = await_test_task(accept_task, "daemon accept task").await;
    assert_eq!(
        served_projects,
        vec![
            Some(active.clone()),
            Some(target),
            Some(active.clone()),
            Some(active),
        ]
    );
}

#[cfg(unix)]
#[test]
fn scheduler_task_start_log_uses_task_key_and_project() {
    let line = super::format_daemon_log_line(
        "scheduler_task",
        &super::scheduler_task_log_fields(
            std::path::Path::new("/tmp/project with spaces"),
            crate::automation::backend::AgentTaskKind::SkillWriter,
            "start",
        ),
    );

    assert_eq!(
        line,
        "[tracedecay] event=scheduler_task project=\"/tmp/project with spaces\" task=skill_writer outcome=start"
    );
}

#[cfg(unix)]
#[test]
fn scheduler_record_log_preserves_skipped_status_and_reason() {
    let record = crate::automation::run_ledger::AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: "run-123".to_string(),
        trigger: crate::automation::run_ledger::AutomationTrigger::Scheduler,
        task: crate::automation::backend::AgentTaskKind::MemoryCurator,
        task_key: Some("memory_curator".to_string()),
        backend: "codex_app_server".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some("memory_curator:v1".to_string()),
        response_schema: None,
        strict_json: None,
        model: None,
        status: crate::automation::run_ledger::AutomationRunStatus::Skipped,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 1,
        error: None,
        error_classification: None,
        error_retryable: None,
        fallback_status: Some("scheduler_interval_not_elapsed".to_string()),
        report_ref: None,
        artifacts: Vec::new(),
        started_at: "1000".to_string(),
        completed_at: "1001".to_string(),
    };

    let line =
        super::daemon_scheduler_record_log_line(std::path::Path::new("/tmp/project"), &record);

    assert_eq!(
        line,
        "[tracedecay] event=scheduler_task project=/tmp/project task=memory_curator outcome=skipped run_id=run-123 reason=scheduler_interval_not_elapsed"
    );
}

#[cfg(unix)]
#[test]
fn automation_staged_log_line_is_stable() {
    let line = super::format_daemon_log_line(
        "automation_staged",
        &super::automation_staged_log_fields(
            std::path::Path::new("/tmp/project"),
            crate::automation::staged_notice::AutomationPendingCounts {
                pending_fact_proposals: 2,
                pending_skills: 1,
            },
        ),
    );

    assert_eq!(
        line,
        "[tracedecay] event=automation_staged project=/tmp/project pending_fact_proposals=2 pending_skills=1"
    );
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
        super::DatabaseOwnerRegistry::default(),
    ));
    let store_administration = StoreAdministration::with_project_servers(Arc::clone(&owners));
    let gates = std::sync::Arc::new(tokio::sync::Mutex::new(super::ProjectOpenGates::default()));
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (listener, endpoint) =
        super::transport::BrokerListener::bind(&super::transport::default_loopback_endpoint())
            .await
            .expect("loopback listener");
    let server_administration = store_administration.clone();
    let server_attempts = std::sync::Arc::clone(&attempts);
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        Box::pin(super::serve_windows_broker_client(
            stream,
            TOKEN,
            &DaemonLifecycle::default(),
            server_administration,
            gates,
            Some(server_attempts),
        ))
        .await
    });
    let mut handshake = test_handshake_defaults();
    handshake.project_path = Some(PathBuf::from("/must-not-route"));
    let mut client = super::transport::BrokerStream::connect(&endpoint)
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
        serde_json::json!(env!("CARGO_PKG_VERSION"))
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
            super::is_missing_index_error(&error),
            "intentional missing-store state should permit config-gated auto-init: {message}"
        );
    }

    let unrelated = crate::errors::TraceDecayError::Config {
        message: "identity cutover conflict".to_string(),
    };
    assert!(!super::is_missing_index_error(&unrelated));
}

#[cfg(unix)]
#[test]
fn client_version_skew_flags_only_real_mismatches() {
    assert_eq!(super::client_version_skew("1.2.3", "1.2.3"), None);
    assert_eq!(super::client_version_skew("", "1.2.3"), None);
    assert_eq!(
        super::client_version_skew("1.3.0", "1.2.3"),
        Some("1.3.0".to_string())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_engine_logs_version_skew_once_per_client_version() {
    let engine = super::DaemonEngine::default();
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
    let engine = super::DaemonEngine::default();
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
    handshake.catalog_version = super::binary_version().to_string();
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

    let next_generation = super::DaemonEngine::default();
    assert!(
        next_generation
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_some(),
        "a new daemon generation must notify the same long-lived client once"
    );

    handshake.catalog_version = super::binary_version().to_string();
    let same_version_generation = super::DaemonEngine::default();
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
    let engine = super::DaemonEngine::default();
    let mut handshake = test_handshake_defaults();
    handshake.tool_list_changed_capable = true;
    handshake.catalog_version = "0.0.0-old".to_string();
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    assert!(super::valid_client_instance_id(&test_client_instance_id(0)));
    assert!(super::valid_client_instance_id("mcp-1234567890"));
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

    for value in 0..super::MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION {
        handshake.client_instance_id = test_client_instance_id(value as u128);
        assert!(
            engine
                .claim_catalog_refresh(&handshake, &ping)
                .await
                .is_some()
        );
    }
    handshake.client_instance_id =
        test_client_instance_id(super::MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION as u128);
    assert!(
        engine
            .claim_catalog_refresh(&handshake, &ping)
            .await
            .is_none(),
        "capacity saturation must skip rather than evicting an existing client"
    );
    assert_eq!(
        engine.catalog_refresh_notified_clients.lock().await.len(),
        super::MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION
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
    let mut handshake = test_handshake_defaults();
    handshake.client_instance_id = test_client_instance_id(4);
    let engine = super::DaemonEngine::default();

    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
    let initialize_responses =
        daemon_round_trip(engine.clone(), &handshake, initialize.clone()).await;
    assert_eq!(initialize_responses.len(), 1);
    let initialize_response_lines: Vec<String> = initialize_responses
        .iter()
        .map(serde_json::Value::to_string)
        .collect();
    let metadata =
        super::proxy_initialize_metadata(&initialize.to_string(), &initialize_response_lines);
    super::apply_proxy_initialize_metadata(&mut handshake, metadata);
    assert!(handshake.tool_list_changed_capable);
    assert_eq!(handshake.catalog_version, super::binary_version());

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

    let next_generation = super::DaemonEngine::default();
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
    let engine = super::DaemonEngine::default();
    let mut handshake = test_handshake_defaults();
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

    let warning = super::daemon_version_skew_warning(&initialize, &response("9.9.9"), "1.0.0")
        .expect("mismatched daemon version should warn");
    assert!(
        warning.contains("9.9.9") && warning.contains("1.0.0"),
        "warning should name both versions, got: {warning}"
    );
    assert!(
        warning.contains("MCP host") && !warning.contains("tracedecay daemon restart"),
        "a newer daemon should direct recovery at the stale host, got: {warning}"
    );

    let warning = super::daemon_version_skew_warning(&initialize, &response("1.0.0"), "9.9.9")
        .expect("newer client should warn about stale daemon");
    assert!(
        warning.contains("tracedecay daemon restart"),
        "a newer client should direct recovery at the stale daemon, got: {warning}"
    );

    assert_eq!(
        super::daemon_version_skew_warning(&initialize, &response("1.0.0"), "1.0.0"),
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
        super::daemon_version_skew_warning(&tools_call, &response("9.9.9"), "1.0.0"),
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
    let metadata = super::proxy_initialize_metadata(&initialize, &responses);
    let mut handshake = test_handshake_defaults();
    super::apply_proxy_initialize_metadata(&mut handshake, metadata);

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
    let metadata = super::proxy_initialize_metadata(&initialize, &legacy_responses);
    let mut legacy = test_handshake_defaults();
    super::apply_proxy_initialize_metadata(&mut legacy, metadata);
    assert!(!legacy.tool_list_changed_capable);
    assert!(legacy.catalog_version.is_empty());
}

#[cfg(unix)]
#[test]
fn automation_scheduler_starts_when_any_task_has_interval() {
    use crate::automation::config::{
        AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
    };

    let mut config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        ..AutomationConfig::default()
    };
    config.tasks.memory_curator = AutomationTaskConfig {
        enabled: true,
        schedule: Some("every:5m".to_string()),
        interval_secs: None,
        cooldown_secs: None,
        ..AutomationTaskConfig::default()
    };

    assert!(super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.schedule = Some("manual".to_string());
    assert!(!super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.schedule = Some("interval".to_string());
    config.tasks.memory_curator.interval_secs = None;
    assert!(!super::automation_scheduler_configured(&config));
    config.tasks.memory_curator.interval_secs = Some(300);
    assert!(super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.enabled = false;
    config.tasks.session_reflector = AutomationTaskConfig {
        enabled: true,
        schedule: Some("hourly".to_string()),
        interval_secs: None,
        cooldown_secs: None,
        ..AutomationTaskConfig::default()
    };
    assert!(super::automation_scheduler_configured(&config));

    config.tasks.session_reflector.enabled = false;
    config.tasks.skill_writer = AutomationTaskConfig {
        enabled: true,
        schedule: Some("daily".to_string()),
        interval_secs: None,
        cooldown_secs: None,
        ..AutomationTaskConfig::default()
    };
    assert!(super::automation_scheduler_configured(&config));

    config.tasks.memory_curator.schedule = Some("every:5m".to_string());
    config.backend = AutomationBackend::ExternalCommand;
    assert!(!super::automation_scheduler_configured(&config));

    config.backend = AutomationBackend::CodexAppServer;
    config.host_mode = AutomationHostMode::DelegatedHost;
    assert!(!super::automation_scheduler_configured(&config));

    config.host_mode = AutomationHostMode::Standalone;
    config.enabled = false;
    assert!(!super::automation_scheduler_configured(&config));
}

#[cfg(unix)]
#[test]
fn automation_scheduler_loads_client_profile_config() {
    let profile = TempDir::new().expect("profile temp dir");
    std::fs::write(
        profile.path().join("config.toml"),
        "[automation]\n\
             enabled = true\n\
             backend = \"codex_app_server\"\n\
             \n\
             [automation.tasks.memory_curator]\n\
             enabled = true\n\
             schedule = \"every:5m\"\n",
    )
    .expect("write config");
    let client_identity = test_client_identity_for(profile.path().to_path_buf());

    let config = super::user_config_for_client(&client_identity);

    assert!(config.automation.enabled);
    assert!(super::automation_scheduler_configured(&config.automation));
}

#[cfg(unix)]
#[tokio::test]
async fn automation_scheduler_tick_secs_loads_dashboard_project_config() {
    use crate::automation::config::{AutomationConfigPatch, save_project_config};

    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let cg = crate::tracedecay::TraceDecay::init_with_options(
        &project,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(client_identity.profile_root.clone()),
            global_db_path: Some(client_identity.global_db_path.clone()),
        },
    )
    .await
    .expect("project init");
    save_project_config(
        &cg.store_layout().dashboard_root,
        &AutomationConfigPatch {
            scheduler_tick_secs: Some(17),
            ..AutomationConfigPatch::default()
        },
    )
    .await
    .expect("save automation config");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };

    let tick_secs = Box::pin(super::automation_scheduler_tick_secs_for_project(
        &project, &handshake,
    ))
    .await;

    assert_eq!(tick_secs, 17);
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_ensure_scheduler_skips_before_project_has_configured_work() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let cg = Arc::new(
        crate::tracedecay::TraceDecay::init_with_options(&project, handshake.open_options())
            .await
            .expect("project init"),
    );
    let engine = super::DaemonEngine::default();
    let key = super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");

    engine
        .ensure_automation_scheduler(key.clone(), project, handshake, cg)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(!schedulers.contains_key(&key));
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_scheduler_discovery_without_work_does_not_wait_for_writer_gate() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let cg = Arc::new(
        crate::tracedecay::TraceDecay::init_with_options(&project, handshake.open_options())
            .await
            .expect("project init"),
    );
    let engine = super::DaemonEngine::default();
    let key = super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");

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

    let discovery = tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        engine.ensure_automation_scheduler(key, project, handshake, cg),
    )
    .await;

    release_writer.send(()).expect("signal writer gate release");
    blocker.await.expect("writer gate blocker task");
    discovery.expect("read-only scheduler discovery must not wait for the writer gate");
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_ensure_scheduler_starts_after_project_configures_work() {
    use crate::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch, save_project_config,
    };

    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let project_graph = crate::tracedecay::TraceDecay::init_with_options(
        &project,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(client_identity.profile_root.clone()),
            global_db_path: Some(client_identity.global_db_path.clone()),
        },
    )
    .await
    .expect("project init");
    let server = crate::mcp::McpServer::new_with_global_db(project_graph, None, None).await;
    let cg = server.cg().await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = super::DaemonEngine::default();
    let key = super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    engine
        .store_administration
        .project_servers()
        .lock()
        .await
        .insert(key.clone(), server);

    engine
        .ensure_automation_scheduler(
            key.clone(),
            project.clone(),
            handshake.clone(),
            Arc::clone(&cg),
        )
        .await;
    assert!(
        !engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .contains_key(&key)
    );

    save_project_config(
        &cg.store_layout().dashboard_root,
        &AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:5m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await
    .expect("save automation config");

    let first = engine.ensure_automation_scheduler(
        key.clone(),
        project.clone(),
        handshake.clone(),
        Arc::clone(&cg),
    );
    let second = engine.ensure_automation_scheduler(key.clone(), project, handshake, cg);
    tokio::join!(first, second);

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert_eq!(schedulers.len(), 1);
    assert!(schedulers.contains_key(&key));
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_scheduler_skips_stale_owner_key_after_rekey() {
    use crate::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch, save_project_config,
    };

    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let project_graph = crate::tracedecay::TraceDecay::init_with_options(
        &project,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(client_identity.profile_root.clone()),
            global_db_path: Some(client_identity.global_db_path.clone()),
        },
    )
    .await
    .expect("project init");
    let server = crate::mcp::McpServer::new_with_global_db(project_graph, None, None).await;
    let cg = server.cg().await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = super::DaemonEngine::default();
    let stale_key =
        super::ProjectServerKey::from_open_project(&cg, &handshake).expect("stale owner key");

    save_project_config(
        &cg.store_layout().dashboard_root,
        &AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:5m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await
    .expect("save automation config");

    let mut current_key = stale_key.clone();
    current_key.scope_prefix = Some("rekeyed".to_string());
    {
        let mut owners = engine.store_administration.project_servers().lock().await;
        owners.insert(stale_key.clone(), server);
        assert!(owners.rekey(&stale_key, &current_key));
    }

    engine
        .ensure_automation_scheduler(stale_key.clone(), project, handshake, cg)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(
        !schedulers.contains_key(&stale_key),
        "scheduler discovery must not start under a key that no longer owns the project server"
    );
    assert!(schedulers.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn automation_scheduler_tick_respects_pause_control_without_backend_call() {
    use crate::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch, save_project_config,
    };
    use crate::automation::run_ledger::load_run_records;
    use crate::automation::scheduler::{AutomationSchedulerControl, save_scheduler_control};

    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let cg = crate::tracedecay::TraceDecay::init_with_options(
        &project,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(client_identity.profile_root.clone()),
            global_db_path: Some(client_identity.global_db_path.clone()),
        },
    )
    .await
    .expect("project init");
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    save_project_config(
        &dashboard_root,
        &AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:1m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await
    .expect("save automation config");
    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: true },
    )
    .await
    .expect("save paused scheduler control");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };

    Box::pin(super::run_automation_scheduler_tick(&project, &handshake))
        .await
        .expect("paused scheduler tick should exit cleanly");

    let records = load_run_records(&dashboard_root, 10)
        .await
        .expect("load run ledger");
    assert!(
        records.is_empty(),
        "paused scheduler tick must not call backends or append run records"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn socket_client_rejects_tool_calls_without_project() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));

    let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
    let server_task = tokio::spawn(super::serve_socket_client(
        server,
        super::DaemonEngine::default(),
    ));

    let (reader, mut writer) = client.into_split();
    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("newline");
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_lcm_status",
                    "arguments": {
                        "provider": "cursor",
                        "format": "json"
                    }
                }
            }))
            .expect("tools/call json")
            .as_bytes(),
        )
        .await
        .expect("write tools/call");
    writer.write_all(b"\n").await.expect("newline");
    writer.shutdown().await.expect("shutdown writer");

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("projectless rejection should not time out")
        .expect("read response")
        .expect("projectless response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(7));
    assert_eq!(
        response["error"]["message"], "tracedecay_lcm_status requires an initialized code project",
        "projectless handshake should return the stable current contract"
    );

    server_task
        .await
        .expect("server task should complete")
        .expect("projectless client shutdown should be clean");
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_linked_worktree_route_repairs_primary_identity_and_keeps_alias() {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let primary = root.join("primary");
    let linked = root.join("linked");
    let profile_root = root.join("profile");
    std::fs::create_dir_all(&primary).expect("primary dir");
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.local")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.local")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&primary, &["init", "-b", "main", "--quiet"]);
    std::fs::write(primary.join("README.md"), "linked worktree route\n").expect("fixture");
    git(&primary, &["add", "."]);
    git(&primary, &["commit", "-m", "fixture", "--quiet"]);
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "feature/linked-route",
            linked.to_str().expect("utf-8 linked path"),
            "HEAD",
        ],
    );

    let client_identity = test_client_identity_for(profile_root.clone());
    let options = crate::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(client_identity.global_db_path.clone()),
    };
    let primary_cg = crate::tracedecay::TraceDecay::init_with_options(&primary, options.clone())
        .await
        .expect("primary init");
    primary_cg.index_all().await.expect("primary index");
    primary_cg
        .db()
        .checkpoint()
        .await
        .expect("primary checkpoint");
    let project_id = primary_cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("profile project id");
    drop(primary_cg);

    let registry = crate::global_db::GlobalDb::open_at(&client_identity.global_db_path)
        .await
        .expect("registry");
    registry
        .upsert_code_project(
            &project_id,
            &linked,
            crate::worktree::git_common_dir(&linked).as_deref(),
            None,
            Some("main"),
        )
        .await
        .expect("seed stale linked canonical root");

    let handshake = DaemonHandshake {
        project_path: Some(linked.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "linked-worktree-route-test")
            .expect("daemon database scope");
    let engine = super::DaemonEngine::default();
    engine
        .project_server(&handshake)
        .await
        .expect("daemon linked-worktree route");

    let context = registry
        .project_registry_context_by_id(&project_id)
        .await
        .expect("registry context");
    assert_eq!(
        context.project.canonical_root,
        crate::global_db::GlobalDb::canonical_project_key(&primary)
    );
    assert!(context.aliases.iter().any(|alias| {
        alias.alias_path == crate::global_db::GlobalDb::canonical_project_key(&linked)
    }));
}

#[test]
fn unsupported_daemon_transport_never_falls_back_to_local_sqlite() {
    assert!(super::proxy_required_by_platform(false, false));
    assert!(super::proxy_required_by_platform(false, true));
    assert!(!super::proxy_required_by_platform(true, false));
}
