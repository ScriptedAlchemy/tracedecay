#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{canonical_existing_path, spawn_tracedecay_daemon, tracedecay_command_with_home};
use serde_json::{json, Value};
use tempfile::TempDir;

fn init_project_with_cli(home: &Path, project: &Path) {
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();

    let output = tracedecay_command_with_home(home)
        .arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tracedecay init should run");
    assert!(
        output.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tool_status_server_tool_calls(home: &Path, project: &Path) -> u64 {
    let project_arg = project.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            &project_arg,
            "status",
            "--json",
            "--format",
            "json",
        ])
        .output()
        .expect("tracedecay tool status should run");
    assert!(
        output.status.success(),
        "status should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).expect("tool result json");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("status result text");
    let payload: Value = serde_json::from_str(text).expect("status payload json");
    payload["server"]["tool_calls"]
        .as_u64()
        .unwrap_or_else(|| panic!("missing server.tool_calls in {payload}"))
}

fn wait_for_daemon_socket(socket_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if UnixStream::connect(socket_path).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for daemon socket at {}",
            socket_path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_child_exit(child: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .expect("child status should be readable")
            .is_some()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_sentinel_daemon(
    socket_path: PathBuf,
    expected_tool_name: &'static str,
    expect_project_path: bool,
    expect_allow_init: bool,
    sentinel: &'static str,
) -> mpsc::Receiver<Value> {
    spawn_sentinel_daemon_with_notification(
        socket_path,
        expected_tool_name,
        expect_project_path,
        expect_allow_init,
        sentinel,
        false,
    )
}

fn spawn_sentinel_daemon_with_notification(
    socket_path: PathBuf,
    expected_tool_name: &'static str,
    expect_project_path: bool,
    expect_allow_init: bool,
    sentinel: &'static str,
    emit_notification: bool,
) -> mpsc::Receiver<Value> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon socket");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        ready_tx.send(()).expect("notify fake daemon readiness");

        let deadline = Instant::now() + Duration::from_secs(2);
        let (stream, _) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        panic!("timed out waiting for tool CLI to connect to fake daemon");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept fake daemon client: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("write timeout");

        let mut reader = BufReader::new(stream.try_clone().expect("clone fake daemon stream"));
        let mut handshake = String::new();
        reader
            .read_line(&mut handshake)
            .expect("read daemon handshake");
        let handshake: Value = serde_json::from_str(handshake.trim()).expect("handshake JSON");
        assert_eq!(handshake["project_path"].is_string(), expect_project_path);
        assert_eq!(
            handshake
                .get("allow_init")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            expect_allow_init
        );

        let mut request = String::new();
        reader
            .read_line(&mut request)
            .expect("read JSON-RPC request");
        let request: Value = serde_json::from_str(request.trim()).expect("request JSON");
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], expected_tool_name);
        request_tx
            .send(request.clone())
            .expect("send observed JSON-RPC request");

        let response = json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": {
                "content": [{
                    "type": "text",
                    "text": sentinel
                }]
            }
        });
        let mut writer = stream;
        if emit_notification {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {
                    "level": "warning",
                    "data": "daemon notice before response"
                }
            });
            writeln!(writer, "{}", serde_json::to_string(&notification).unwrap())
                .expect("write fake daemon notification");
        }
        writeln!(writer, "{}", serde_json::to_string(&response).unwrap())
            .expect("write fake daemon response");
    });

    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("fake daemon should become ready");
    request_rx
}

#[test]
fn daemon_sigterm_exits_while_project_client_is_connected() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let socket_path = common::daemon_socket_path(&home_path);
    let _ = std::fs::remove_file(&socket_path);
    let mut child = tracedecay_command_with_home(&home_path)
        .arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("tracedecay daemon should start");
    wait_for_daemon_socket(&socket_path);

    let mut client = UnixStream::connect(&socket_path).expect("client should connect to daemon");
    let mut reader = BufReader::new(client.try_clone().expect("clone daemon client stream"));
    let handshake = json!({
        "project_path": project_path,
        "scope_prefix": null,
        "timings": false,
        "allow_init": false,
        "client_identity": {
            "profile_root": home_path.join(".tracedecay"),
            "global_db_path": home_path.join(".tracedecay/global.db")
        }
    });
    writeln!(client, "{handshake}").expect("write daemon handshake");
    writeln!(
        client,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })
    )
    .expect("write initialize request");
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .expect("read initialize response");
    assert!(
        response.contains("\"id\":1"),
        "daemon should answer initialize before SIGTERM, got: {response}"
    );

    let pid = child.id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-TERM", pid.as_str()])
        .status()
        .expect("send SIGTERM to daemon");
    assert!(status.success(), "kill -TERM should succeed");

    if !wait_for_child_exit(&mut child, Duration::from_secs(3)) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("daemon should exit on SIGTERM even with a connected project client");
    }
}

#[test]
fn daemon_socket_is_owner_only() {
    let home = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let socket_path = common::daemon_socket_path(&home_path);
    let _ = std::fs::remove_file(&socket_path);
    let mut child = tracedecay_command_with_home(&home_path)
        .arg("daemon")
        .arg("run")
        .arg("--socket")
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("tracedecay daemon should start");

    wait_for_daemon_socket(&socket_path);
    let mode = std::fs::metadata(&socket_path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "daemon socket should be owner-only");

    let pid = child.id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-TERM", pid.as_str()])
        .status()
        .expect("send SIGTERM to daemon");
    assert!(status.success(), "kill -TERM should succeed");

    if !wait_for_child_exit(&mut child, Duration::from_secs(3)) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("daemon should exit after socket permission test");
    }
}

#[test]
fn tool_cli_invokes_mcp_tool_through_daemon_socket() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let sentinel = "daemon-backed tool response";
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed_request = spawn_sentinel_daemon(
        socket_path.clone(),
        "tracedecay_status",
        true,
        false,
        sentinel,
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "status", "--json"])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        output.status.success(),
        "tool CLI should accept daemon JSON-RPC response\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(sentinel),
        "tool CLI should print daemon response, got:\n{stdout}"
    );
    observed_request
        .recv_timeout(Duration::from_secs(2))
        .expect("fake daemon should receive tools/call request");
}

#[test]
fn tool_cli_skips_daemon_notifications_until_matching_response() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let sentinel = "daemon response after notification";
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed_request = spawn_sentinel_daemon_with_notification(
        socket_path.clone(),
        "tracedecay_status",
        true,
        false,
        sentinel,
        true,
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "status", "--json"])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        output.status.success(),
        "tool CLI should skip daemon notifications before the response\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(sentinel),
        "tool CLI should print daemon response after notification, got:\n{stdout}"
    );
    observed_request
        .recv_timeout(Duration::from_secs(2))
        .expect("fake daemon should receive tools/call request");
}

#[test]
fn profile_scoped_tool_cli_invokes_daemon_without_project_handshake() {
    let home = TempDir::new().unwrap();
    let hermes_home = TempDir::new().unwrap();
    let outside_cwd = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let hermes_home_path = canonical_existing_path(hermes_home.path());
    let outside_cwd_path = canonical_existing_path(outside_cwd.path());

    let sentinel = "profile-scoped daemon response";
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed_request = spawn_sentinel_daemon(
        socket_path.clone(),
        "tracedecay_lcm_status",
        false,
        false,
        sentinel,
    );
    let args = json!({
        "provider": "cursor",
        "storage_scope": "hermes_profile",
        "hermes_home": hermes_home_path,
    })
    .to_string();

    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&outside_cwd_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args([
            "tool",
            "tracedecay_lcm_status",
            "--json",
            "--args",
            args.as_str(),
        ])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        output.status.success(),
        "profile-scoped tool CLI should accept daemon response\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(sentinel),
        "tool CLI should print daemon response, got:\n{stdout}"
    );
    let request = observed_request
        .recv_timeout(Duration::from_secs(2))
        .expect("fake daemon should receive profile-scoped tools/call request");
    assert_eq!(
        request["params"]["arguments"]["storage_scope"],
        "hermes_profile"
    );
}

#[test]
fn first_touch_store_tool_cli_invokes_daemon_with_init_permission() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());

    let sentinel = "first-touch daemon response";
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed_request = spawn_sentinel_daemon(
        socket_path.clone(),
        "tracedecay_fact_store",
        true,
        true,
        sentinel,
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args([
            "tool",
            "--project",
            &project_arg,
            "fact_store",
            "--json",
            "--args",
            r#"{"action":"add","content":"first touch via daemon","fact_type":"decision"}"#,
        ])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        output.status.success(),
        "first-touch store tool CLI should accept daemon response\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(sentinel),
        "tool CLI should print daemon response, got:\n{stdout}"
    );
    let request = observed_request
        .recv_timeout(Duration::from_secs(2))
        .expect("fake daemon should receive first-touch tools/call request");
    assert_eq!(request["params"]["arguments"]["action"], "add");
}

#[test]
fn daemon_reuses_project_engine_across_tool_clients() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);
    let _daemon = spawn_tracedecay_daemon(&home_path);

    let first_tool_calls = tool_status_server_tool_calls(&home_path, &project_path);
    let second_tool_calls = tool_status_server_tool_calls(&home_path, &project_path);

    assert_eq!(
        first_tool_calls, 1,
        "first status call should see itself counted"
    );
    assert!(
        second_tool_calls >= 2,
        "second status call should reuse daemon engine and see accumulated tool calls, got {second_tool_calls}"
    );
}

#[test]
fn daemon_project_handshake_uses_client_profile_identity() {
    let daemon_home = TempDir::new().unwrap();
    let client_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let daemon_home_path = canonical_existing_path(daemon_home.path());
    let client_home_path = canonical_existing_path(client_home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&client_home_path, &project_path);
    let _daemon = spawn_tracedecay_daemon(&daemon_home_path);

    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&client_home_path)
        .current_dir(&project_path)
        .env(
            "TRACEDECAY_DAEMON_SOCKET",
            common::daemon_socket_path(&daemon_home_path),
        )
        .args(["tool", "--project", &project_arg, "status", "--json"])
        .output()
        .expect("tracedecay tool status should run");

    assert!(
        output.status.success(),
        "daemon should open the client's profile-sharded project\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn daemon_project_cache_is_scoped_by_client_identity() {
    let daemon_home = TempDir::new().unwrap();
    let client_a_home = TempDir::new().unwrap();
    let client_b_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let daemon_home_path = canonical_existing_path(daemon_home.path());
    let client_a_home_path = canonical_existing_path(client_a_home.path());
    let client_b_home_path = canonical_existing_path(client_b_home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&client_a_home_path, &project_path);
    let _daemon = spawn_tracedecay_daemon(&daemon_home_path);

    // Both clients use one daemon socket; only handshake identity should
    // distinguish project cache entries.
    let socket_path = common::daemon_socket_path(&daemon_home_path);
    assert_ne!(socket_path, common::daemon_socket_path(&client_a_home_path));
    assert_ne!(socket_path, common::daemon_socket_path(&client_b_home_path));
    let project_arg = project_path.to_string_lossy().to_string();
    let client_a_output = tracedecay_command_with_home(&client_a_home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "status", "--json"])
        .output()
        .expect("client A tool status should run");
    assert!(
        client_a_output.status.success(),
        "client A should open its initialized project through the shared daemon\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&client_a_output.stdout),
        String::from_utf8_lossy(&client_a_output.stderr)
    );

    let client_b_output = tracedecay_command_with_home(&client_b_home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "status", "--json"])
        .output()
        .expect("client B tool status should run");
    assert!(
        !client_b_output.status.success(),
        "client B should not reuse client A's cached project server\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&client_b_output.stdout),
        String::from_utf8_lossy(&client_b_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&client_b_output.stderr);
    let expected_project_path = project_path.to_string_lossy();
    let stderr_lower = stderr.to_lowercase();
    assert!(
        stderr.contains("daemon tool call failed")
            && stderr_lower.contains("no tracedecay index found")
            && stderr.contains(expected_project_path.as_ref()),
        "expected client B to fail because its profile has not initialized the project, got:\n{stderr}"
    );
}

#[test]
fn tool_cli_without_daemon_socket_reports_client_only_error() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let missing_socket = socket_dir.path().join("missing.sock");
    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &missing_socket)
        .args(["tool", "--project", &project_arg, "status", "--json"])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        !output.status.success(),
        "tool CLI should fail instead of opening MCP handlers in-process"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TraceDecay daemon socket") && stderr.contains("is not available"),
        "expected daemon availability guidance, got:\n{stderr}"
    );
}
