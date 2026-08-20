#[cfg(unix)]
use super::*;

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
        super::super::transport::DaemonAuthPreface::from_line(auth_line.trim()).expect("auth");
    assert!(
        preface.authenticate(expected_token),
        "proxy must reload current daemon authority"
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
        .write_all(serde_json::to_string(&response).unwrap().as_bytes())
        .await
        .expect("write response");
    writer.write_all(b"\n").await.expect("write newline");
    writer.shutdown().await.expect("shutdown fake daemon");
}

/// A slow or contended first Git probe defers the route; it must never be a
/// terminal failure. Both retry classifiers — the CLI's message check and the
/// proxy's JSON-RPC response check — have to accept the deferral, or a cold
/// first open on a slow volume fails `init`/`status` outright.
#[cfg(unix)]
#[test]
fn deferred_repository_discovery_is_project_open_retryable() {
    let error = super::super::core_proxy::repository_discovery_deferred(
        std::path::Path::new("/slow-volume/project"),
        tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown::DeadlineExceeded,
    );

    assert!(
        super::super::error_message_is_project_open_retryable(&error.to_string()),
        "a deferred discovery must classify as retryable, got: {error}"
    );

    let response = super::super::project_open_error_response(json!(7), &error);
    let refusal = serde_json::to_value(response.error.expect("deferred discovery refusal"))
        .expect("serialize deferred refusal");
    assert!(
        super::super::json_rpc_error_is_project_open_retryable(&refusal),
        "proxy retry classification must accept the deferred refusal: {refusal}"
    );
}

#[cfg(unix)]
#[test]
fn terminal_repository_discovery_failures_are_not_project_open_retryable() {
    for reason in [
        tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown::SpawnFailed,
        tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown::ProbeFailed,
    ] {
        let error = super::super::core_proxy::repository_discovery_deferred(
            std::path::Path::new("/broken-git/project"),
            reason,
        );

        assert!(
            !super::super::error_message_is_project_open_retryable(&error.to_string()),
            "a terminal discovery failure must not classify as retryable: {error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn transient_daemon_connect_errors_cover_restart_window_only() {
    assert!(super::super::is_transient_daemon_connect_error(
        std::io::ErrorKind::NotFound
    ));
    assert!(super::super::is_transient_daemon_connect_error(
        std::io::ErrorKind::ConnectionRefused
    ));
    assert!(!super::super::is_transient_daemon_connect_error(
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

    super::super::connect_with_restart_grace(
        &super::super::connection_for_socket_path(&socket),
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
    let grace = std::time::Duration::from_millis(300);
    let poll = std::time::Duration::from_millis(50);
    let started = tokio::time::Instant::now();

    let err = super::super::connect_with_restart_grace(
        &super::super::connection_for_socket_path(&socket),
        grace,
        poll,
    )
    .await
    .expect_err("connect should fail when no daemon ever binds");

    let elapsed = started.elapsed();
    assert!(elapsed >= grace, "restart grace must be fully observed");
    assert!(
        elapsed <= grace + poll,
        "restart retries must stop within one poll interval after grace"
    );

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
async fn long_lived_proxy_reloads_rotated_auth_after_daemon_restart() {
    let dir = TempDir::new().expect("temp dir");
    let profile = dir.path().canonicalize().expect("canonical profile");
    let socket = profile.join("daemon.sock");
    let endpoint = super::super::transport::DaemonEndpoint::Unix(socket.clone());
    let first_listener = tokio::net::UnixListener::bind(&socket).expect("bind first socket");
    let first_authority =
        super::super::authority::DaemonAuthority::acquire(&profile, &endpoint, "first")
            .expect("first daemon authority");
    let first_token = first_authority.auth_token().to_string();
    let rebound_socket = socket.clone();
    let rebound_profile = profile.clone();
    let rebound_endpoint = endpoint.clone();
    let (unbound_tx, unbound_rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(async move {
        answer_one_authenticated_proxy_request(first_listener, &first_token, 1).await;
        drop(first_authority);
        std::fs::remove_file(&rebound_socket).expect("unlink first socket");
        unbound_tx.send(()).expect("notify daemon outage");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let second_listener =
            tokio::net::UnixListener::bind(&rebound_socket).expect("bind second socket");
        let second_authority = super::super::authority::DaemonAuthority::acquire(
            &rebound_profile,
            &rebound_endpoint,
            "second",
        )
        .expect("second daemon authority");
        let second_token = second_authority.auth_token().to_string();
        assert_ne!(first_token, second_token, "daemon restart must rotate auth");
        answer_one_authenticated_proxy_request(second_listener, &second_token, 2).await;
    });

    let (mut transport, sender, mut receiver) = crate::mcp::transport::ChannelTransport::new();
    let proxy_socket = socket.clone();
    let proxy = tokio::spawn(async move {
        super::super::proxy_transport_to_daemon(
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
async fn initialize_root_routing_replaces_cached_project_and_scope() {
    let profile = TempDir::new().expect("profile temp dir");
    let project_a = TempDir::new().expect("project a temp dir");
    let project_b = TempDir::new().expect("project b temp dir");
    let project_a = project_a.path().canonicalize().expect("project a path");
    let project_b = project_b.path().canonicalize().expect("project b path");
    let registry = crate::host_admission::HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .expect("open retained profile runtime");
    let global_db_path = profile.path().join("global.db");
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
    let store_administration = test_store_administration_for_profile(profile.path());
    let _database_scope =
        enter_test_daemon_database_scope(profile.path(), "initialize-route-replacement-test");

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

    super::super::reset_proxy_handshake_for_initialize(
        &base_handshake,
        &mut routed_handshake,
        &line,
    );
    let route = super::super::apply_daemon_initialize_route(
        &mut routed_handshake,
        &line,
        &store_administration,
    )
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
    super::super::reset_proxy_handshake_for_initialize(
        &base_handshake,
        &mut routed_handshake,
        &rerun_without_roots,
    );
    assert!(
        super::super::apply_daemon_initialize_route(
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
    let registry = crate::host_admission::HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .expect("open retained profile runtime");
    let global_db_path = profile.path().join("global.db");
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
    let store_administration = test_store_administration_for_profile(profile.path());
    let _database_scope =
        enter_test_daemon_database_scope(profile.path(), "initialize-route-alias-test");
    let line = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "roots": [{ "uri": nested, "name": "alias" }] }
    })
    .to_string();

    let route =
        super::super::apply_daemon_initialize_route(&mut handshake, &line, &store_administration)
            .await
            .expect("daemon initialize routing should succeed")
            .expect("authenticated daemon should resolve registry alias");
    assert_eq!(route.project_path, alias);
    assert_eq!(handshake.project_path.as_deref(), Some(alias.as_path()));
    assert!(!route.allow_init);
}

#[cfg(unix)]
#[tokio::test]
async fn initialize_root_routing_fails_closed_without_pinned_configuration() {
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

    let config = crate::config::TraceDecayConfig {
        root_dir: project.display().to_string(),
        ..crate::config::TraceDecayConfig::default()
    };
    let config_path = crate::config::get_config_path(&project);
    std::fs::create_dir_all(config_path.parent().expect("legacy config parent"))
        .expect("create legacy config parent");
    let legacy_input = serde_json::to_string_pretty(&config).expect("serialize legacy config");
    std::fs::write(&config_path, &legacy_input).expect("write legacy config fixture");

    let mut routed_handshake = base_handshake.clone();
    let store_administration = test_store_administration_for_profile(profile.path());
    let _database_scope =
        enter_test_daemon_database_scope(profile.path(), "initialize-route-auto-init-test");
    super::super::reset_proxy_handshake_for_initialize(
        &base_handshake,
        &mut routed_handshake,
        &line,
    );
    super::super::apply_daemon_initialize_route(
        &mut routed_handshake,
        &line,
        &store_administration,
    )
    .await
    .expect("daemon should delegate auto-init");
    assert_eq!(
        routed_handshake.project_path.as_deref(),
        Some(project.as_path())
    );
    assert!(
        routed_handshake.allow_init,
        "fresh git root without a published snapshot follows SyncConfig::default().auto_init"
    );
    assert_eq!(
        std::fs::read_to_string(config_path).expect("legacy fixture remains readable"),
        legacy_input,
        "initialize routing must not rewrite legacy configuration input"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn serve_proxies_when_socket_already_exists() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");

    assert!(
        super::super::should_proxy_serve_to_daemon_with(
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
        super::super::should_proxy_serve_to_daemon_with(
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
        super::super::should_proxy_serve_to_daemon_with(
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
        super::super::should_proxy_serve_to_daemon_with(
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
        !super::super::should_proxy_serve_to_daemon_with(
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

    let responses = super::super::send_daemon_request_line(&socket, &handshake, &request)
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
async fn proxy_retries_bounded_project_warming_responses() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
    let daemon = tokio::spawn(async move {
        for response_kind in 0..2 {
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
            let response = if response_kind == 0 {
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {
                        "code": -32603,
                        "message": "config error: TraceDecay project '/tmp/project' is warming in the background; retry the same tool shortly"
                    }
                })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": { "generation": 2 }
                })
            };
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
    });

    let (mut transport, sender, mut receiver) = crate::mcp::transport::ChannelTransport::new();
    let proxy_socket = socket.clone();
    let proxy = tokio::spawn(async move {
        super::super::proxy_transport_to_daemon(
            &proxy_socket,
            &test_handshake_defaults(),
            None,
            &mut transport,
        )
        .await
    });

    sender
        .send(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 41,
                "method": "tools/call"
            }))
            .expect("request json"),
        )
        .expect("send request");
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
        .await
        .expect("proxy response timed out")
        .expect("proxy response");
    let response: Value = serde_json::from_str(response.trim()).expect("response json");
    assert_eq!(response["result"]["generation"], json!(2));

    drop(sender);
    await_test_task(proxy, "warming retry proxy task")
        .await
        .expect("proxy transport");
    await_test_task(daemon, "warming retry daemon task").await;
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
        super::super::proxy_transport_to_daemon(
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
        super::super::proxy_transport_to_daemon(&socket, &handshake, None, &mut transport),
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
