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

use super::scheduler::{AutomationSchedulerExitBarrier, AutomationSchedulerLifecycle};
#[cfg(unix)]
use super::{
    AutomationSchedulerHandle, DaemonEngine, MemoryRepairSchedulerHandle, drain_client_tasks,
};
use super::{
    DaemonClientIdentity, DaemonHandshake, DaemonLifecycle, DatabaseOwnerRegistry, ProjectRouteKey,
    ProjectServerKey, StoreAdministration, StoreOwnerKey,
};

#[cfg(unix)]
const MAINTENANCE_TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

mod compatibility;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_identity_startup_replays_retained_profile_receipts() {
    let temp = TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let identity = DaemonClientIdentity {
        profile_root: profile_root.clone(),
        global_db_path: profile_root.join("global.db"),
    };

    let first_admin = StoreAdministration::default();
    let user_db = first_admin
        .user_session_database(&identity.global_db_path)
        .await
        .unwrap();
    let broker = first_admin
        .host_admission_broker(&user_db)
        .await
        .unwrap()
        .broker()
        .cloned()
        .expect("fresh host admission spool");
    let plan = crate::mcp::hook_events::HookEventPlan::RecordTerminalReceipt {
        route: Some(crate::daemon::HookRouteMetadata {
            session_id: Some("startup-session".to_string()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        }),
        receipt: crate::daemon::HookTerminalReceipt {
            tool_call_id: Some("startup-call".to_string()),
            turn_id: Some("startup-turn".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(1),
            transcript_watermark: Some("startup-watermark".to_string()),
        },
    };
    let payload = crate::mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap();
    first_admin.shutdown_host_admission_replay().await;
    broker.admit("hermes:startup-test", &payload).await.unwrap();
    // Retain the pending record after the first daemon's replay authority has
    // drained, so restart replay remains the acceptance path under test.
    drop(broker);
    drop(user_db);
    drop(first_admin);

    let restarted = StoreAdministration::default();
    super::replay_user_profile_host_admission_for_identity(&restarted, &identity)
        .await
        .unwrap();
    let recovered_db = restarted
        .user_session_database(&identity.global_db_path)
        .await
        .unwrap();
    let recovered = restarted
        .host_admission_broker(&recovered_db)
        .await
        .unwrap()
        .broker()
        .cloned()
        .expect("reopened host admission spool");
    let broker_path = super::authority::canonical_identity_path(
        &crate::sessions::user_sessions_db_path(&profile_root),
    )
    .unwrap();
    assert!(
        restarted
            .wait_user_profile_host_admission_replay_idle(
                &broker_path,
                std::time::Duration::from_secs(5),
            )
            .await,
        "restart replay worker must become idle"
    );
    assert_eq!(recovered.pending_count().await, 0);
    assert!(
        crate::automation::runner::user_automation_root(&profile_root)
            .join("host_receipts.json")
            .is_file()
    );
}

#[test]
fn tool_json_payload_requires_exactly_one_json_block() {
    let valid = serde_json::json!({
        "content": [
            {"text": "status"},
            {"text": "{\"ok\":true}"}
        ]
    });
    assert_eq!(
        super::tool_json_payload(&valid, "test").unwrap(),
        serde_json::json!({"ok": true})
    );

    for (content, expected) in [
        (
            serde_json::json!([{"text": "{\"first\":1}"}, {"text": "[2]"}]),
            "returned multiple JSON payloads",
        ),
        (
            serde_json::json!([{"text": "status"}, {"type": "image"}]),
            "returned no JSON payload",
        ),
    ] {
        let error =
            super::tool_json_payload(&serde_json::json!({"content": content}), "test").unwrap_err();
        assert!(error.to_string().contains(expected));
    }
}

#[test]
fn daemon_lifecycle_rejects_new_work_after_draining() {
    let lifecycle = DaemonLifecycle::default();
    assert!(lifecycle.accepting());

    lifecycle.begin_draining();

    assert!(!lifecycle.accepting());
}

#[test]
fn daemon_client_admission_reports_saturation_and_recovers() {
    let admission = super::DaemonClientAdmission::new(1);
    let permit = match admission.try_admit() {
        super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::DaemonClientAdmissionOutcome::Saturated(_) => panic!("first client rejected"),
    };

    let response = match admission.try_admit() {
        super::DaemonClientAdmissionOutcome::Saturated(response) => response,
        super::DaemonClientAdmissionOutcome::Admitted(_) => panic!("capacity exceeded"),
    };
    assert_eq!(
        response,
        super::DaemonClientSaturationResponse {
            kind: super::DaemonClientSaturationKind::ClientCapacityReached,
            retryable: true,
            capacity: 1,
        }
    );

    drop(permit);
    assert!(matches!(
        admission.try_admit(),
        super::DaemonClientAdmissionOutcome::Admitted(_)
    ));
}

#[test]
fn daemon_client_saturation_response_is_typed_json_rpc_data() {
    let response = super::DaemonClientSaturationResponse {
        kind: super::DaemonClientSaturationKind::ClientCapacityReached,
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

#[cfg(unix)]
#[tokio::test]
async fn one_shot_tool_call_receives_a_matching_saturation_response() {
    let temp = TempDir::new().expect("temp dir");
    let socket = temp.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept tool call");
        super::reject_saturated_daemon_client(
            super::transport::BrokerStream::Unix(stream),
            super::DaemonClientSaturationResponse {
                kind: super::DaemonClientSaturationKind::ClientCapacityReached,
                retryable: true,
                capacity: 1,
            },
        )
        .await;
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
    let admission = super::DaemonClientAdmission::new(1);
    let permit = match admission.try_admit() {
        super::DaemonClientAdmissionOutcome::Admitted(permit) => permit,
        super::DaemonClientAdmissionOutcome::Saturated(_) => panic!("first client rejected"),
    };
    let task = tokio::spawn(async move {
        let _permit = permit;
        std::future::pending::<()>().await;
    });
    assert!(matches!(
        admission.try_admit(),
        super::DaemonClientAdmissionOutcome::Saturated(_)
    ));

    task.abort();
    task.await.expect_err("client task cancelled");
    assert!(matches!(
        admission.try_admit(),
        super::DaemonClientAdmissionOutcome::Admitted(_)
    ));
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
        super::call_tool_with_liveness_poll(
            &socket,
            &test_handshake_defaults(),
            "tracedecay_status",
            json!({}),
            std::time::Duration::from_millis(10),
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
        .insert(key, test_automation_scheduler_handle(task));

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
#[tokio::test]
async fn daemon_memory_repair_scheduler_shutdown_aborts_and_joins_every_loop() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/memory-repair-shutdown-test"),
            global_db_path: PathBuf::from("/profiles/memory-repair-shutdown-test/global.db"),
            project_id: Some("memory-repair-shutdown-test".to_string()),
            store_root: PathBuf::from("/stores/memory-repair-shutdown-test"),
            graph_db_path: PathBuf::from("/stores/memory-repair-shutdown-test/graph.db"),
        },
        scope_prefix: None,
    };
    let task = tokio::spawn(std::future::pending::<()>());
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(key, MemoryRepairSchedulerHandle::for_test(task));

    engine.lifecycle.begin_draining();
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.shutdown_memory_repair_schedulers(),
    )
    .await
    .expect("memory-repair shutdown should not wait for its retry delay");

    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn automation_shutdown_timeout_keeps_unfinished_task_tracked() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/automation-shutdown-timeout-test"),
            global_db_path: PathBuf::from("/profiles/automation-shutdown-timeout-test/global.db"),
            project_id: Some("automation-shutdown-timeout-test".to_string()),
            store_root: PathBuf::from("/stores/automation-shutdown-timeout-test"),
            graph_db_path: PathBuf::from("/stores/automation-shutdown-timeout-test/graph.db"),
        },
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative automation owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));

    engine.lifecycle.begin_draining();
    engine.shutdown_automation_schedulers().await;

    assert!(
        !stale_task.is_finished(),
        "noncooperative automation owner must remain live until released"
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must transfer scheduler-map ownership to the tracked reaper"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "timed-out automation shutdown must retain one tracked join reaper"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("noncooperative automation owner completion timed out")
        .expect("noncooperative automation owner completed");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .is_empty()
                && engine.store_administration.retirement_reaper_count().await == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("automation shutdown reaper did not release owner state");
    assert!(stale_task.is_finished());
}

#[cfg(unix)]
#[tokio::test]
async fn repair_shutdown_timeout_keeps_unfinished_task_tracked() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/repair-shutdown-timeout-test"),
            global_db_path: PathBuf::from("/profiles/repair-shutdown-timeout-test/global.db"),
            project_id: Some("repair-shutdown-timeout-test".to_string()),
            store_root: PathBuf::from("/stores/repair-shutdown-timeout-test"),
            graph_db_path: PathBuf::from("/stores/repair-shutdown-timeout-test/graph.db"),
        },
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(key.clone(), MemoryRepairSchedulerHandle::for_test(task));

    engine.lifecycle.begin_draining();
    engine.shutdown_memory_repair_schedulers().await;

    assert!(
        !stale_task.is_finished(),
        "noncooperative repair owner must remain live until released"
    );
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must transfer repair-map ownership to the tracked reaper"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "timed-out repair shutdown must retain one tracked join reaper"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("noncooperative repair owner completion timed out")
        .expect("noncooperative repair owner completed");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if engine
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .is_empty()
                && engine.store_administration.retirement_reaper_count().await == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("repair shutdown reaper did not release owner state");
    assert!(stale_task.is_finished());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_contended_automation_retirement_remains_shutdown_owned() {
    use crate::dashboard::AutomationSchedulerReconcileOutcome;

    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/automation-registration-cancel-test"),
            global_db_path: PathBuf::from(
                "/profiles/automation-registration-cancel-test/global.db",
            ),
            project_id: Some("automation-registration-cancel-test".to_string()),
            store_root: PathBuf::from("/stores/automation-registration-cancel-test"),
            graph_db_path: PathBuf::from("/stores/automation-registration-cancel-test/old.db"),
        },
        scope_prefix: None,
    };
    let mut replacement = key.clone();
    replacement.owner.graph_db_path =
        PathBuf::from("/stores/automation-registration-cancel-test/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, started_rx)
        .await
        .expect("noncooperative automation owner start timed out")
        .expect("noncooperative automation owner started");
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));
    let barrier = engine
        .store_administration
        .install_retirement_reaper_registration_barrier_for_test();
    let retirement_engine = engine.clone();
    let retirement_key = key.clone();
    let retirement = tokio::spawn(async move {
        retirement_engine
            .retire_automation_scheduler_locked(&retirement_key)
            .await
    });
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, barrier.wait_until_reached())
        .await
        .expect("automation registration barrier was not reached");

    retirement.abort();
    barrier.release();
    let _ = tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, retirement)
        .await
        .expect("cancelled automation retirement did not unwind");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(1),
    )
    .await
    .expect("automation reaper was not registered after caller cancellation");
    let repeated = engine
        .retire_automation_scheduler_locked(&key)
        .await
        .expect("repeated automation retirement must reuse the tombstone");
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "repeated retirement must not add a second reaper"
    );
    assert_eq!(
        engine
            .ensure_automation_scheduler(
                replacement,
                PathBuf::from("/moved-project"),
                test_handshake_defaults(),
            )
            .await,
        AutomationSchedulerReconcileOutcome::Retiring,
        "restart must remain blocked while the old task is live"
    );

    let first_pass = engine
        .store_administration
        .retirement_reaper_shutdown_passes_for_test();
    let shutdown_administration = engine.store_administration.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_administration.shutdown_retirement_reapers().await;
    });
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_shutdown_pass_for_test(first_pass),
    )
    .await
    .expect("reaper shutdown did not observe registered automation ownership");
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the noncooperative automation owner"
    );
    shutdown.abort();
    let shutdown_result = tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, shutdown)
        .await
        .expect("cancelled reaper shutdown did not unwind");
    assert!(
        matches!(shutdown_result, Err(error) if error.is_cancelled()),
        "the first reaper shutdown must be cancelled at its wait point"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "cancelled shutdown must leave registry ownership intact"
    );

    let retry_pass = engine
        .store_administration
        .retirement_reaper_shutdown_passes_for_test();
    let retry_administration = engine.store_administration.clone();
    let retry = tokio::spawn(async move {
        retry_administration.shutdown_retirement_reapers().await;
    });
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_shutdown_pass_for_test(retry_pass),
    )
    .await
    .expect("repeated reaper shutdown did not rediscover automation ownership");
    assert!(!retry.is_finished());

    release.release();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, completed_rx)
        .await
        .expect("automation owner completion timed out")
        .expect("automation owner completion sender dropped");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, retry)
        .await
        .expect("repeated reaper shutdown timed out")
        .expect("repeated reaper shutdown panicked");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, repeated.wait())
        .await
        .expect("repeated automation retirement did not complete");
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        0
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine.store_administration.shutdown_retirement_reapers(),
    )
    .await
    .expect("idempotent reaper shutdown timed out");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_contended_repair_retirement_blocks_restart_until_join() {
    use super::memory_repair_scheduler::MemoryRepairSchedulerReconcileOutcome;

    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/repair-registration-cancel-test"),
            global_db_path: PathBuf::from("/profiles/repair-registration-cancel-test/global.db"),
            project_id: Some("repair-registration-cancel-test".to_string()),
            store_root: PathBuf::from("/stores/repair-registration-cancel-test"),
            graph_db_path: PathBuf::from("/stores/repair-registration-cancel-test/old.db"),
        },
        scope_prefix: None,
    };
    let mut replacement = key.clone();
    replacement.owner.graph_db_path =
        PathBuf::from("/stores/repair-registration-cancel-test/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, started_rx)
        .await
        .expect("noncooperative repair owner start timed out")
        .expect("noncooperative repair owner started");
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(key.clone(), MemoryRepairSchedulerHandle::for_test(task));
    let barrier = engine
        .store_administration
        .install_retirement_reaper_registration_barrier_for_test();
    let retirement_engine = engine.clone();
    let retirement_key = key.clone();
    let retirement = tokio::spawn(async move {
        retirement_engine
            .retire_memory_repair_scheduler_locked(&retirement_key)
            .await
    });
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, barrier.wait_until_reached())
        .await
        .expect("repair registration barrier was not reached");

    retirement.abort();
    barrier.release();
    let _ = tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, retirement)
        .await
        .expect("cancelled repair retirement did not unwind");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(1),
    )
    .await
    .expect("repair reaper was not registered after caller cancellation");
    let repeated = engine
        .retire_memory_repair_scheduler_locked(&key)
        .await
        .expect("repeated repair retirement must reuse the tombstone");
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1
    );
    assert_eq!(
        engine
            .ensure_memory_repair_scheduler(
                replacement,
                PathBuf::from("/moved-project"),
                test_handshake_defaults(),
            )
            .await,
        MemoryRepairSchedulerReconcileOutcome::Retiring,
        "repair restart must remain blocked until the old task joins"
    );

    release.release();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, completed_rx)
        .await
        .expect("repair owner completion timed out")
        .expect("repair owner completion sender dropped");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, repeated.wait())
        .await
        .expect("repeated repair retirement did not complete");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(0),
    )
    .await
    .expect("repair reaper ownership did not converge to zero");
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn panicked_retired_tasks_release_both_scheduler_registrations() {
    let engine = DaemonEngine::default();
    let automation_key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/panicked-automation-retirement-test"),
            global_db_path: PathBuf::from(
                "/profiles/panicked-automation-retirement-test/global.db",
            ),
            project_id: Some("panicked-automation-retirement-test".to_string()),
            store_root: PathBuf::from("/stores/panicked-automation-retirement-test"),
            graph_db_path: PathBuf::from("/stores/panicked-automation-retirement-test/graph.db"),
        },
        scope_prefix: None,
    };
    let repair_key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/panicked-repair-retirement-test"),
            global_db_path: PathBuf::from("/profiles/panicked-repair-retirement-test/global.db"),
            project_id: Some("panicked-repair-retirement-test".to_string()),
            store_root: PathBuf::from("/stores/panicked-repair-retirement-test"),
            graph_db_path: PathBuf::from("/stores/panicked-repair-retirement-test/graph.db"),
        },
        scope_prefix: None,
    };
    let automation_task = tokio::spawn(async {
        panic!("panicked automation owner");
    });
    let repair_task = tokio::spawn(async {
        panic!("panicked repair owner");
    });
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(
            automation_key.clone(),
            test_automation_scheduler_handle(automation_task),
        );
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(
            repair_key.clone(),
            MemoryRepairSchedulerHandle::for_test(repair_task),
        );

    let automation_retirement = engine
        .retire_automation_scheduler_locked(&automation_key)
        .await
        .expect("panicked automation retirement");
    let repair_retirement = engine
        .retire_memory_repair_scheduler_locked(&repair_key)
        .await
        .expect("panicked repair retirement");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, async {
        automation_retirement.wait().await;
        repair_retirement.wait().await;
    })
    .await
    .expect("panicked scheduler retirements did not complete");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(0),
    )
    .await
    .expect("panicked task reapers did not converge to zero");
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
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
    let (direct_server, alias_server) = tokio::time::timeout(
        PHASE_TIMEOUT,
        Box::pin(async {
            tokio::join!(
                engine.project_server(&direct),
                engine.project_server(&aliased)
            )
        }),
    )
    .await
    .expect("cache-test concurrent-open phase timed out");
    eprintln!("[cache-test] phase=concurrent-open done");
    let direct_server = direct_server.expect("direct project server");
    let alias_server = alias_server.expect("aliased project server");
    assert!(std::sync::Arc::ptr_eq(&direct_server, &alias_server));
    tokio::time::timeout(PHASE_TIMEOUT, async {
        while engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial maintenance activation timed out");
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "canonical aliases must singleflight the first project open"
    );
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "concurrent insertion must acquire one repair owner"
    );
    tokio::time::timeout(PHASE_TIMEOUT, async {
        loop {
            if engine
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .is_empty()
                && engine
                    .automation_config_probe_attempts
                    .load(std::sync::atomic::Ordering::Relaxed)
                    == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial memory-repair pass timed out");
    engine
        .memory_repair_start_attempts
        .store(0, std::sync::atomic::Ordering::Relaxed);
    engine
        .automation_config_probe_attempts
        .store(0, std::sync::atomic::Ordering::Relaxed);

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
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "cache hits must not restart completed maintenance"
    );
    assert_eq!(
        engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "cache hits must not re-probe unchanged automation config"
    );
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
#[tokio::test]
async fn interrupted_post_insert_activation_retains_maintenance_ownership() {
    let temp = TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("project dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let client_identity = test_client_identity_for(profile_root.clone());
    let initialized = crate::tracedecay::TraceDecay::init_with_options(
        &project,
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(client_identity.global_db_path.clone()),
        },
    )
    .await
    .expect("initialize project");
    drop(initialized);
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        ..test_handshake_defaults()
    };
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "interrupted-activation-test")
            .expect("daemon database scope");
    let engine = DaemonEngine::default();

    let (published_tx, published_rx) = tokio::sync::oneshot::channel();
    let request_engine = engine.clone();
    let request_handshake = handshake.clone();
    let request = tokio::spawn(async move {
        let (_, _, server, inserted) = request_engine
            .store_administration
            .with_writer(|| request_engine.open_project_server(&request_handshake))
            .await
            .expect("insert project server");
        assert!(inserted);
        published_tx.send(()).expect("signal cache publication");
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        server
    });
    published_rx.await.expect("cache publication signal");
    request.abort();
    let cancellation = request.await;
    assert!(
        matches!(cancellation, Err(error) if error.is_cancelled()),
        "requesting future must actually be cancelled"
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon-owned maintenance activation timed out");
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache publication must acquire repair ownership before activation can be interrupted"
    );
    assert_eq!(
        engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache publication must perform its one initial automation reconciliation"
    );

    engine.shutdown_all().await;
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

#[cfg(unix)]
#[tokio::test]
async fn project_rekey_cancels_stale_repair_owner_and_acquires_new_owner_once() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let task = tokio::spawn(std::future::pending::<()>());
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(old.clone(), MemoryRepairSchedulerHandle::for_test(task));

    engine
        .rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            true,
        )
        .await;
    assert!(
        stale_task.is_finished(),
        "rekey must await stale repair shutdown before returning"
    );
    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert!(!schedulers.contains_key(&old));
    assert!(schedulers.contains_key(&new));
    assert_eq!(schedulers.len(), 1);
    drop(schedulers);
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn project_rekey_awaits_stale_automation_owner_before_replacement() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let task = tokio::spawn(std::future::pending::<()>());
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(old.clone(), test_automation_scheduler_handle(task));

    engine
        .rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        )
        .await;

    assert!(
        stale_task.is_finished(),
        "rekey must await stale automation shutdown before returning"
    );
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_waits_for_blocked_automation_retirement_reaper_and_is_idempotent() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/automation.db"),
        },
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative automation owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));

    let retirement = engine
        .retire_automation_scheduler_locked(&key)
        .await
        .expect("automation owner retirement");
    let shutdown_engine = engine.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_engine.shutdown_all().await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown did not drain automation ownership");

    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the blocked automation retirement reaper"
    );
    assert!(
        !stale_task.is_finished(),
        "blocked automation owner must still be live before release"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("automation owner completion timed out")
        .expect("automation owner completion sender dropped");
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown did not reap automation retirement")
        .expect("shutdown task panicked");
    retirement.wait().await;

    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        0,
        "shutdown must leave no automation reaper ownership record"
    );
    assert!(
        stale_task.is_finished(),
        "shutdown must not leave the retired automation owner orphaned"
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must clear the retired automation tombstone"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), engine.shutdown_all())
        .await
        .expect("repeated shutdown must be idempotent");
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_waits_for_blocked_repair_retirement_reaper_and_is_idempotent() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/repair.db"),
        },
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(
            key.clone(),
            super::memory_repair_scheduler::MemoryRepairSchedulerHandle::for_test(task),
        );

    let retirement = engine
        .retire_memory_repair_scheduler_locked(&key)
        .await
        .expect("repair owner retirement");
    let shutdown_engine = engine.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_engine.shutdown_all().await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if engine
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown did not drain repair ownership");

    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the blocked repair retirement reaper"
    );
    assert!(
        !stale_task.is_finished(),
        "blocked repair owner must still be live before release"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("repair owner completion timed out")
        .expect("repair owner completion sender dropped");
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown did not reap repair retirement")
        .expect("shutdown task panicked");
    retirement.wait().await;

    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        0,
        "shutdown must leave no repair reaper ownership record"
    );
    assert!(
        stale_task.is_finished(),
        "shutdown must not leave the retired repair owner orphaned"
    );
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must clear the retired repair tombstone"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), engine.shutdown_all())
        .await
        .expect("repeated shutdown must be idempotent");
}

#[cfg(unix)]
#[tokio::test]
async fn automation_retirement_timeout_retains_owner_tombstone_until_join_finishes() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx.await.expect("noncooperative owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(old.clone(), test_automation_scheduler_handle(task));

    let rekey = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;

    let retained = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .get(&old)
        .is_some_and(|owner| owner.lifecycle == AutomationSchedulerLifecycle::Retiring);
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_automation_scheduler(
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
        ),
    )
    .await
    .ok();
    let owner_count = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .len();

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let joined = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(
            &old,
            new,
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;
    let owners_after_join = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .len();
    let reapers_after_join = engine.store_administration.retirement_reaper_count().await;
    engine.shutdown_all().await;

    assert_eq!(
        rekey,
        Ok(super::MaintenanceRekeyOutcome::Retiring),
        "noncooperative retirement must return its bounded timeout outcome"
    );
    assert!(
        retained,
        "retirement timeout must retain a tombstone until the JoinHandle terminates"
    );
    assert_eq!(
        reconcile,
        Some(crate::dashboard::AutomationSchedulerReconcileOutcome::Retiring),
        "replacement must remain unavailable while the old JoinHandle is live"
    );
    assert_eq!(
        owner_count, 1,
        "retirement must retain exactly one ownership record"
    );
    assert!(completed.is_ok(), "noncooperative owner was not released");
    assert_eq!(
        joined,
        Ok(super::MaintenanceRekeyOutcome::Completed),
        "released owner must be joined by the next retirement attempt"
    );
    assert!(
        stale_task.is_finished(),
        "stale owner task must be terminated"
    );
    assert_eq!(owners_after_join, 0, "the reaper must clear its tombstone");
    assert_eq!(
        reapers_after_join, 0,
        "normal automation reaper completion must release daemon ownership"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn repair_retirement_timeout_retains_owner_tombstone_until_join_finishes() {
    use super::memory_repair_scheduler::{
        MemoryRepairSchedulerLifecycle, MemoryRepairSchedulerReconcileOutcome,
    };

    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(old.clone(), MemoryRepairSchedulerHandle::for_test(task));

    let rekey = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;
    let retained = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .get(&old)
        .is_some_and(|owner| owner.lifecycle == MemoryRepairSchedulerLifecycle::Retiring);
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_memory_repair_scheduler(
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
        ),
    )
    .await
    .ok();
    let owner_count = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .len();

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let joined = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(
            &old,
            new,
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        ),
    )
    .await;
    let owners_after_join = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .len();
    let reapers_after_join = engine.store_administration.retirement_reaper_count().await;
    engine.shutdown_all().await;

    assert_eq!(rekey, Ok(super::MaintenanceRekeyOutcome::Retiring));
    assert!(retained, "repair timeout must retain a retiring owner");
    assert_eq!(
        reconcile,
        Some(MemoryRepairSchedulerReconcileOutcome::Retiring),
        "repair replacement must remain blocked by the live tombstone"
    );
    assert_eq!(owner_count, 1, "repair retirement must keep one owner");
    assert!(
        completed.is_ok(),
        "noncooperative repair owner was not released"
    );
    assert_eq!(joined, Ok(super::MaintenanceRekeyOutcome::Completed));
    assert!(stale_task.is_finished(), "stale repair task must terminate");
    assert_eq!(
        owners_after_join, 0,
        "repair reaper must clear its tombstone"
    );
    assert_eq!(
        reapers_after_join, 0,
        "normal repair reaper completion must release daemon ownership"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn released_repair_tombstone_allows_one_eventual_replacement() {
    use super::memory_repair_scheduler::{
        MemoryRepairSchedulerLifecycle, MemoryRepairSchedulerReconcileOutcome,
    };

    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(old.clone(), MemoryRepairSchedulerHandle::for_test(task));

    let timed_out = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            true,
        ),
    )
    .await;
    let retained = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .get(&old)
        .is_some_and(|owner| owner.lifecycle == MemoryRepairSchedulerLifecycle::Retiring);
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_memory_repair_scheduler(
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
        ),
    )
    .await
    .ok();
    let no_overlap = {
        let schedulers = engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await;
        schedulers.len() == 1 && schedulers.contains_key(&old) && !schedulers.contains_key(&new)
    };

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let replaced = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            true,
        ),
    )
    .await;
    let (owner_count, owns_new, live_replacement) = {
        let schedulers = engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await;
        (
            schedulers.len(),
            schedulers.contains_key(&new),
            schedulers
                .get(&new)
                .and_then(|owner| owner.task.as_ref())
                .is_some_and(|task| !task.is_finished()),
        )
    };
    let start_attempts = engine
        .memory_repair_start_attempts
        .load(std::sync::atomic::Ordering::Relaxed);
    engine.shutdown_all().await;

    assert_eq!(timed_out, Ok(super::MaintenanceRekeyOutcome::Retiring));
    assert!(
        retained,
        "retirement timeout must retain one Retiring repair tombstone"
    );
    assert_eq!(
        reconcile,
        Some(MemoryRepairSchedulerReconcileOutcome::Retiring),
        "ensure of the logical new key must stay blocked while the tombstone is live"
    );
    assert!(no_overlap, "a retiring repair owner must block replacement");
    assert!(
        completed.is_ok(),
        "noncooperative repair owner was not released"
    );
    assert_eq!(replaced, Ok(super::MaintenanceRekeyOutcome::Completed));
    assert_eq!(owner_count, 1, "exactly one repair owner must remain");
    assert!(owns_new, "the released tombstone must permit replacement");
    assert!(
        live_replacement,
        "the replacement repair owner must be live"
    );
    assert_eq!(
        start_attempts, 1,
        "repair replacement must start exactly once"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn released_automation_tombstone_allows_one_eventual_replacement() {
    use crate::automation::scheduler::{AutomationSchedulerControl, save_scheduler_control};
    use crate::dashboard::AutomationSchedulerReconcileOutcome;

    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    let profile_root = dir.path().join("profile");
    let client_identity = test_client_identity_for(profile_root);
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
    save_scheduled_automation(&dashboard_root, true).await;
    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl {
            paused: true,
            ..AutomationSchedulerControl::default()
        },
    )
    .await
    .expect("pause scheduler work");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let new = ProjectServerKey::from_open_project(&cg, &handshake).expect("new owner key");
    let mut old = new.clone();
    old.owner.graph_db_path = old.owner.graph_db_path.with_extension("retiring.db");

    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx.await.expect("noncooperative owner started");
    let engine = DaemonEngine::default();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(old.clone(), test_automation_scheduler_handle(task));

    let timed_out = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            project.clone(),
            handshake.clone(),
            true,
        ),
    )
    .await;
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.ensure_automation_scheduler(new.clone(), project.clone(), handshake.clone()),
    )
    .await
    .ok();
    let no_overlap = {
        let schedulers = engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await;
        schedulers.len() == 1 && schedulers.contains_key(&old) && !schedulers.contains_key(&new)
    };

    release.release();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx).await;
    let replaced = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        engine.rekey_project_maintenance(
            &old,
            new.clone(),
            project.clone(),
            handshake.clone(),
            true,
        ),
    )
    .await;
    let (owner_count, owns_new, live_replacement) = {
        let schedulers = engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await;
        (
            schedulers.len(),
            schedulers.contains_key(&new),
            schedulers
                .get(&new)
                .and_then(|owner| owner.task.as_ref())
                .is_some_and(|task| !task.is_finished()),
        )
    };
    engine.shutdown_all().await;

    assert_eq!(timed_out, Ok(super::MaintenanceRekeyOutcome::Retiring));
    assert_eq!(
        reconcile,
        Some(AutomationSchedulerReconcileOutcome::Retiring)
    );
    assert!(
        no_overlap,
        "replacement must not overlap the retiring owner"
    );
    assert!(completed.is_ok(), "noncooperative owner was not released");
    assert_eq!(replaced, Ok(super::MaintenanceRekeyOutcome::Completed));
    assert_eq!(owner_count, 1, "exactly one scheduler owner must remain");
    assert!(owns_new, "the released tombstone must permit replacement");
    assert!(
        live_replacement,
        "exactly one live replacement must own the scheduler"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_project_rekeys_are_bounded_and_keep_one_repair_owner() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut new = old.clone();
    new.owner.graph_db_path = PathBuf::from("/store/new.db");
    let first = engine.rekey_project_maintenance(
        &old,
        new.clone(),
        PathBuf::from("/moved-project"),
        test_handshake_defaults(),
        true,
    );
    let second = engine.rekey_project_maintenance(
        &old,
        new.clone(),
        PathBuf::from("/moved-project"),
        test_handshake_defaults(),
        true,
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(first, second);
    })
    .await
    .expect("concurrent rekey must not deadlock");

    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert_eq!(schedulers.len(), 1);
    assert!(schedulers.contains_key(&new));
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stale_cache_retirement_does_not_duplicate_canonical_repair_owner() {
    let engine = DaemonEngine::default();
    let old = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/store"),
            graph_db_path: PathBuf::from("/store/old.db"),
        },
        scope_prefix: None,
    };
    let mut canonical = old.clone();
    canonical.owner.graph_db_path = PathBuf::from("/store/canonical.db");
    let stale_task = tokio::spawn(std::future::pending::<()>());
    let stale_abort = stale_task.abort_handle();
    let canonical_task = tokio::spawn(std::future::pending::<()>());
    let canonical_abort = canonical_task.abort_handle();
    {
        let mut schedulers = engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await;
        schedulers.insert(
            old.clone(),
            MemoryRepairSchedulerHandle::for_test(stale_task),
        );
        schedulers.insert(
            canonical.clone(),
            MemoryRepairSchedulerHandle::for_test(canonical_task),
        );
    }

    engine
        .rekey_project_maintenance(
            &old,
            canonical.clone(),
            PathBuf::from("/moved-project"),
            test_handshake_defaults(),
            false,
        )
        .await;
    tokio::task::yield_now().await;

    assert!(stale_abort.is_finished());
    assert!(!canonical_abort.is_finished());
    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert!(!schedulers.contains_key(&old));
    assert!(schedulers.contains_key(&canonical));
    assert_eq!(schedulers.len(), 1);
    drop(schedulers);
    assert_eq!(
        engine
            .memory_repair_start_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "retirement must preserve an existing canonical owner"
    );
    engine.shutdown_all().await;
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
fn test_automation_scheduler_handle(task: JoinHandle<()>) -> AutomationSchedulerHandle {
    AutomationSchedulerHandle::for_test(task)
}

#[cfg(unix)]
#[derive(Clone)]
struct NoncooperativeTaskRelease {
    state: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(unix)]
impl NoncooperativeTaskRelease {
    fn release(&self) {
        let (released, changed) = &*self.state;
        *released.lock().unwrap_or_else(|error| error.into_inner()) = true;
        changed.notify_all();
    }
}

#[cfg(unix)]
impl Drop for NoncooperativeTaskRelease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(unix)]
struct AutomationExitBarrierRelease(Arc<AutomationSchedulerExitBarrier>);

#[cfg(unix)]
impl Drop for AutomationExitBarrierRelease {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(unix)]
fn spawn_noncooperative_test_task() -> (
    JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Receiver<()>,
    NoncooperativeTaskRelease,
) {
    let release = NoncooperativeTaskRelease {
        state: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
    };
    let task_release = release.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _ = started_tx.send(());
        let (released, changed) = &*task_release.state;
        let mut ready = released.lock().unwrap_or_else(|error| error.into_inner());
        while !*ready {
            ready = changed
                .wait(ready)
                .unwrap_or_else(|error| error.into_inner());
        }
        let _ = completed_tx.send(());
    });
    (task, started_rx, completed_rx, release)
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
    let cg = crate::tracedecay::TraceDecay::init_with_options(&project, handshake.open_options())
        .await
        .expect("project init");
    let engine = super::DaemonEngine::default();
    let key = super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");

    engine
        .ensure_automation_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(!schedulers.contains_key(&key));
}

#[cfg(unix)]
fn scheduled_automation_patch(enabled: bool) -> crate::automation::config::AutomationConfigPatch {
    crate::automation::config::AutomationConfigPatch {
        enabled: Some(enabled),
        backend: Some(crate::automation::config::AutomationBackend::CodexAppServer),
        memory_curator: crate::automation::config::AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("every:5m".to_string())),
            ..crate::automation::config::AutomationTaskPatch::default()
        },
        ..crate::automation::config::AutomationConfigPatch::default()
    }
}

#[cfg(unix)]
async fn save_scheduled_automation(dashboard_root: &std::path::Path, enabled: bool) {
    crate::automation::config::save_project_config(
        dashboard_root,
        &scheduled_automation_patch(enabled),
    )
    .await
    .expect("save scheduled automation config");
}

#[cfg(unix)]
#[tokio::test]
async fn disabled_finished_scheduler_reenables_with_a_fresh_owner() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    let profile_root = dir.path().join("profile");
    let client_identity = test_client_identity_for(profile_root);
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
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    save_scheduled_automation(&dashboard_root, false).await;
    let finished = tokio::spawn(async {});
    tokio::task::yield_now().await;
    assert!(finished.is_finished());
    let engine = DaemonEngine::default();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(finished));

    save_scheduled_automation(&dashboard_root, true).await;
    engine
        .ensure_automation_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    let owner = schedulers.get(&key).expect("re-enabled scheduler owner");
    assert!(
        !owner
            .task
            .as_ref()
            .expect("live scheduler task")
            .is_finished(),
        "re-enable must replace a finished scheduler handle"
    );
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_reenable_creates_one_live_scheduler_owner() {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().join("project");
    let profile_root = dir.path().join("profile");
    let client_identity = test_client_identity_for(profile_root);
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
    save_scheduled_automation(&dashboard_root, true).await;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    let finished = tokio::spawn(async {});
    tokio::task::yield_now().await;
    let engine = DaemonEngine::default();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(finished));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(
            engine.ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone()),
            engine.ensure_automation_scheduler(key.clone(), project, handshake)
        );
    })
    .await
    .expect("concurrent re-enable must not deadlock");

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert_eq!(schedulers.len(), 1);
    assert!(
        !schedulers
            .get(&key)
            .unwrap()
            .task
            .as_ref()
            .expect("live scheduler task")
            .is_finished()
    );
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[test]
fn memory_repair_retries_only_incomplete_progress() {
    use tracedecay_store::{
        CompatibilityFeedbackRepairProgressV1, CompatibilityLegacyMemoryCutoverProgressV1,
    };

    assert_eq!(
        super::memory_repair_tick_outcome(CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed: 1,
            remaining: Some(1),
        })
        .expect("incomplete repair must retry"),
        super::MemoryRepairTickOutcome::Incomplete,
    );
    assert_eq!(
        super::memory_repair_tick_outcome(CompatibilityFeedbackRepairProgressV1::Complete {
            processed: 1
        })
        .expect("complete repair must stop"),
        super::MemoryRepairTickOutcome::Complete,
    );
    assert_eq!(
        super::memory_repair_tick_outcome(CompatibilityFeedbackRepairProgressV1::NotRequired)
            .expect("unneeded repair must stop"),
        super::MemoryRepairTickOutcome::NotRequired,
    );
    assert!(
        super::memory_repair_tick_outcome(CompatibilityFeedbackRepairProgressV1::Unknown).is_err()
    );
    assert!(super::legacy_memory_cutover_should_retry(
        CompatibilityLegacyMemoryCutoverProgressV1::Incomplete { processed: 1 },
    ));
    assert!(!super::legacy_memory_cutover_should_retry(
        CompatibilityLegacyMemoryCutoverProgressV1::Complete,
    ));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn daemon_memory_repair_scheduler_starts_without_automation_configuration() {
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
    let cg = crate::tracedecay::TraceDecay::init_with_options(&project, handshake.open_options())
        .await
        .expect("project init");
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    let engine = DaemonEngine::default();

    engine
        .ensure_memory_repair_scheduler(key.clone(), project.clone(), handshake.clone())
        .await;
    engine
        .ensure_memory_repair_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await;
    assert!(schedulers.contains_key(&key));
    assert_eq!(schedulers.len(), 1);
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_memory_repair_tick_runs_without_automation_configuration() {
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
    let cg = crate::tracedecay::TraceDecay::init_with_options(&project, handshake.open_options())
        .await
        .expect("project init");
    drop(cg);

    let decision = super::run_memory_repair_scheduler_tick(&project, &handshake)
        .await
        .expect("memory repair tick must not depend on automation configuration");

    assert!(
        matches!(decision, super::MemoryRepairPassDecision::Idle),
        "a fresh project has no repair backlog"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unavailable_host_admission_spool_does_not_block_project_server_open() {
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
    let session_db_path = cg.store_layout().sessions_db_path.clone();
    drop(cg);
    let spool_path = session_db_path.parent().unwrap().join(format!(
        ".{}.host-admission",
        session_db_path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&spool_path, "not a directory").expect("block spool directory open");
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let _database_scope = crate::db::enter_daemon_database_scope(
        &client_identity.profile_root,
        1,
        "unavailable-host-admission-test",
    )
    .expect("daemon database scope");
    let engine = super::DaemonEngine::default();

    engine
        .project_server(&handshake)
        .await
        .expect("read/query server must open when admission spool is unavailable");
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn profile_reconcile_broadcasts_to_cached_projects_without_opening_uncached_projects() {
    let dir = TempDir::new().expect("temp dir");
    let profile_root = dir.path().join("profile");
    let first_project = dir.path().join("first");
    let second_project = dir.path().join("second");
    let uncached_project = dir.path().join("uncached");
    for project in [&first_project, &second_project, &uncached_project] {
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    }
    let client_identity = test_client_identity_for(profile_root.clone());
    for project in [&first_project, &second_project, &uncached_project] {
        let initialized = crate::tracedecay::TraceDecay::init_with_options(
            project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");
        drop(initialized);
    }
    let first_handshake = DaemonHandshake {
        project_path: Some(first_project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let second_handshake = DaemonHandshake {
        project_path: Some(second_project.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let engine = DaemonEngine::default();
    let first_server = engine
        .project_server(&first_handshake)
        .await
        .expect("cache first project");
    let second_server = engine
        .project_server(&second_handshake)
        .await
        .expect("cache second project");
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial scheduler probes timed out");
    let opens_before = engine
        .project_open_attempts
        .load(std::sync::atomic::Ordering::Relaxed);

    let mut user_config = crate::user_config::UserConfig::default();
    user_config.automation = crate::automation::config::effective_config(
        &user_config.automation,
        Some(&scheduled_automation_patch(true)),
    )
    .expect("effective global automation config");
    std::fs::write(
        profile_root.join("config.toml"),
        toml::to_string_pretty(&user_config).expect("serialize global config"),
    )
    .expect("write global config");

    let params = json!({
        "name": "tracedecay_admin_project",
        "arguments": {
            "action": "automation_reconcile",
            "scope": "profile"
        }
    });
    let response = super::projectless_tools_call_response(
        json!(73),
        Some(&params),
        &client_identity,
        &engine.store_administration,
    )
    .await;
    assert!(
        response.error.is_none(),
        "projectless profile reconcile must succeed: {:?}",
        response.error
    );
    let result = response.result.expect("profile reconcile result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("profile reconcile JSON text");
    let report: Value = serde_json::from_str(text).expect("profile reconcile report");
    assert_eq!(report["scope"], "profile");
    assert_eq!(report["cached_owners"], 2);
    assert_eq!(
        report["uncached_projects"],
        "deferred_until_project_startup"
    );
    let outcomes = report["outcomes"]
        .as_array()
        .expect("profile reconcile owner outcomes");
    assert_eq!(outcomes.len(), 2);
    for owner in outcomes {
        assert!(
            owner["project_id"].as_str().is_some(),
            "each profile outcome must carry stable project identity: {owner}"
        );
        assert!(
            owner["store_root"].as_str().is_some(),
            "each profile outcome must carry stable store identity: {owner}"
        );
        assert!(
            owner["graph_db_path"].as_str().is_some(),
            "each profile outcome must identify its physical graph store: {owner}"
        );
        assert!(
            owner["outcome"].as_str().is_some(),
            "each profile owner must carry its typed reconcile outcome: {owner}"
        );
    }
    assert_eq!(
        engine
            .project_open_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        opens_before,
        "profile reconcile must not open uncached projects"
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .len()
                == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("profile scheduler broadcast timed out");

    drop(first_server);
    drop(second_server);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cached_project_reconciles_cli_enabled_automation_without_cache_probe() {
    use crate::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch, save_project_config,
    };

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
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = super::DaemonEngine::default();
    let key = super::ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");

    let server = engine
        .project_server(&handshake)
        .await
        .expect("cache unconfigured project");
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial automation config reconciliation timed out");
    let cached = engine
        .project_server(&handshake)
        .await
        .expect("cached project server");
    assert!(Arc::ptr_eq(&server, &cached));
    assert_eq!(
        engine
            .automation_config_probe_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "cache hits must not provide config reconciliation"
    );
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

    let finished = tokio::spawn(async {});
    tokio::task::yield_now().await;
    assert!(finished.is_finished());
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(finished));
    let request = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_admin_project",
            "arguments": {
                "action": "automation_reconcile",
                "scope": "project"
            }
        }
    }))
    .expect("automation reconcile request");
    let response = server
        .handle_request(&request)
        .await
        .expect("automation reconcile response");
    assert!(response.error.is_none(), "daemon reconcile must succeed");
    let result = response.result.as_ref().expect("reconcile result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("reconcile JSON text");
    let report: Value = serde_json::from_str(text).expect("reconcile report");
    assert_eq!(
        report["outcome"], "started",
        "a finished handle must not be reported as a successful notification"
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .contains_key(&key)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("automation scheduler reconciliation timed out");

    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    assert!(
        !schedulers
            .get(&key)
            .expect("reconciled scheduler")
            .task
            .as_ref()
            .expect("live scheduler task")
            .is_finished(),
        "finished handle notification must be replaced, not reported as success"
    );
    drop(schedulers);
    engine.shutdown_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn disabled_scheduler_reconcile_cannot_acknowledge_an_owner_that_then_exits() {
    use crate::automation::config::{
        AutomationBackend, AutomationConfigPatch, AutomationTaskPatch, save_project_config,
    };
    use crate::automation::scheduler::{AutomationSchedulerControl, save_scheduler_control};
    use crate::dashboard::AutomationSchedulerReconcileOutcome;

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
    let scheduled = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        memory_curator: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("every:5m".to_string())),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    save_project_config(&cg.store_layout().dashboard_root, &scheduled)
        .await
        .expect("enable automation");
    save_scheduler_control(
        &cg.store_layout().dashboard_root,
        &AutomationSchedulerControl {
            paused: true,
            ..AutomationSchedulerControl::default()
        },
    )
    .await
    .expect("pause scheduler work");

    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    let engine = DaemonEngine::default();
    let barrier = Arc::new(AutomationSchedulerExitBarrier::new());
    let barrier_release = AutomationExitBarrierRelease(Arc::clone(&barrier));
    *engine.automation_scheduler_exit_barrier.lock().await = Some(Arc::clone(&barrier));
    // ensure starts the loop without MCP project_server open (schema-contract
    // reject on session_temporal_generations unique(session_id) vs partial index).
    assert_eq!(
        engine
            .ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone())
            .await,
        AutomationSchedulerReconcileOutcome::Started
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let changed = engine.automation_scheduler_state_changed.notified();
            tokio::pin!(changed);
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .contains_key(&key)
            {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("scheduler start timed out");

    save_project_config(
        &cg.store_layout().dashboard_root,
        &AutomationConfigPatch {
            enabled: Some(false),
            ..AutomationConfigPatch::default()
        },
    )
    .await
    .expect("disable automation");
    let wake = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .get(&key)
        .expect("live scheduler")
        .wake
        .clone();
    wake.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        barrier.wait_until_reached(),
    )
    .await
    .expect("scheduler did not reach disabled-read barrier");

    save_project_config(&cg.store_layout().dashboard_root, &scheduled)
        .await
        .expect("re-enable automation");
    let reconcile = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        engine.ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone()),
    )
    .await;
    barrier.release();
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        barrier.wait_for_decision(),
    )
    .await;
    let schedulers = engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await;
    let owner_count = schedulers.len();
    let owner_live = schedulers
        .get(&key)
        .and_then(|owner| owner.task.as_ref())
        .is_some_and(|task| !task.is_finished());
    drop(schedulers);
    engine.shutdown_all().await;
    drop(barrier_release);

    assert_eq!(
        reconcile,
        Ok(AutomationSchedulerReconcileOutcome::RunningNotified)
    );
    assert_eq!(
        decision,
        Ok(AutomationSchedulerExitBarrier::CONTINUE),
        "the generation acknowledged by reconcile must cancel the pending exit"
    );
    assert_eq!(owner_count, 1, "exactly one scheduler owner must remain");
    assert!(
        owner_live,
        "the acknowledged scheduler owner must still be live"
    );
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
    let mut config = crate::config::load_config(&linked).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&linked, &config)
        .expect("disable unrelated startup transcript ingestion");

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
