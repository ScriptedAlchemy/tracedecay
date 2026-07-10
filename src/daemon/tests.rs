use std::path::PathBuf;

#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use tempfile::TempDir;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::task::JoinHandle;

#[cfg(unix)]
use super::drain_client_tasks;
use super::{DaemonClientIdentity, DaemonHandshake, DaemonLifecycle};

#[test]
fn daemon_lifecycle_rejects_new_work_after_draining() {
    let lifecycle = DaemonLifecycle::default();
    assert!(lifecycle.accepting());

    lifecycle.begin_draining();

    assert!(!lifecycle.accepting());
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
    }
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
        &socket,
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
        &socket,
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

    super::update_proxy_handshake_from_initialize(&base_handshake, &mut routed_handshake, &line)
        .await;

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
    super::update_proxy_handshake_from_initialize(
        &base_handshake,
        &mut routed_handshake,
        &rerun_without_roots,
    )
    .await;

    assert_eq!(
        routed_handshake.project_path.as_deref(),
        Some(project_a.as_path()),
        "reinitialize without a route must not keep the previous routed project"
    );
    assert_eq!(routed_handshake.scope_prefix.as_deref(), Some("src"));
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
    super::update_proxy_handshake_from_initialize(&base_handshake, &mut routed_handshake, &line)
        .await;
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
    super::update_proxy_handshake_from_initialize(&base_handshake, &mut routed_handshake, &line)
        .await;
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
async fn proxy_transport_carries_initialize_root_and_resets_on_reinitialize() {
    let dir = TempDir::new().expect("temp dir");
    let temp_root = dir.path().canonicalize().expect("canonical temp dir");
    let active_root = temp_root.join("active");
    let target_root = temp_root.join("target");
    std::fs::create_dir_all(active_root.join("src")).expect("active src");
    std::fs::create_dir_all(target_root.join("src")).expect("target src");
    let active = active_root.canonicalize().expect("active root");
    let target = target_root.canonicalize().expect("target root");
    let socket = temp_root.join("daemon.sock");
    let client_identity = test_client_identity_for(temp_root.join("profile"));
    let registry = crate::global_db::GlobalDb::open_at(&client_identity.global_db_path)
        .await
        .expect("registry");
    registry
        .upsert_code_project("proj_active_proxy", &active, None, None, Some("main"))
        .await
        .expect("active project registry");
    registry
        .upsert_code_project("proj_target_proxy", &target, None, None, Some("main"))
        .await
        .expect("target project registry");

    let listener = tokio::net::UnixListener::bind(&socket).expect("daemon socket");
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
            let project = handshake
                .project_path
                .as_ref()
                .map(|path| path.display().to_string());
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": { "project": project }
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
            projects.push(project);
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
            Some(target.clone()),
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

/// Old client → new daemon: handshakes without `client_version` (sent by
/// binaries predating the field) must still parse, with an empty version.
#[test]
fn daemon_handshake_accepts_old_client_without_version() {
    let encoded = serde_json::json!({
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

    let decoded = DaemonHandshake::from_line(&encoded).expect("old handshake should decode");

    assert_eq!(decoded.client_version, "");
}

/// New client → old daemon: the serde derive ignores unknown fields, so a
/// daemon predating `client_version` (same derive) parses new handshakes.
/// Adding another unknown field to a current handshake proves the
/// tolerance the old daemon relies on.
#[test]
fn daemon_handshake_ignores_unknown_fields_for_old_daemons() {
    let handshake = test_handshake_defaults();
    let mut value: serde_json::Value =
        serde_json::from_str(&handshake.to_line().expect("handshake should encode"))
            .expect("handshake json");
    value["field_from_a_future_version"] = serde_json::json!("ignored");

    let decoded = DaemonHandshake::from_line(&value.to_string())
        .expect("handshake with unknown fields should decode");

    assert_eq!(decoded, handshake);
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
        warning.contains("tracedecay daemon restart"),
        "warning should point at the restart command, got: {warning}"
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
    let engine = super::DaemonEngine::default();
    let key = super::ProjectServerKey::from_handshake(project.clone(), &handshake);

    engine
        .ensure_automation_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine.automation_schedulers.lock().await;
    assert!(!schedulers.contains_key(&key));
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
    let key = super::ProjectServerKey::from_handshake(project.clone(), &handshake);

    engine
        .ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone())
        .await;
    assert!(!engine.automation_schedulers.lock().await.contains_key(&key));

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

    engine
        .ensure_automation_scheduler(key.clone(), project, handshake)
        .await;

    let schedulers = engine.automation_schedulers.lock().await;
    assert!(schedulers.contains_key(&key));
    drop(schedulers);
    engine.shutdown_all().await;
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
async fn socket_client_serves_profile_scoped_lcm_without_project() {
    let hermes_home = TempDir::new().expect("hermes home");
    let hermes_home = hermes_home
        .path()
        .canonicalize()
        .expect("canonical hermes home");
    let client_identity = test_client_identity_for(hermes_home.join("client-profile"));
    std::fs::create_dir_all(hermes_home.join(".tracedecay")).expect("profile root");

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
                        "storage_scope": "hermes_profile",
                        "hermes_home": hermes_home,
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
        .expect("profile-scoped response should not time out")
        .expect("read response")
        .expect("profile-scoped response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(7));
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("profile result text");
    let payload: Value = serde_json::from_str(text).expect("profile payload json");
    assert_eq!(payload["status"], "not_ingested");
    assert_eq!(payload["storage_scope"], "hermes_profile");

    server_task.abort();
    let _ = server_task.await;
}
