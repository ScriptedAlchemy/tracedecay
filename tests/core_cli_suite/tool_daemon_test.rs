use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::common;
use crate::common::{
    canonical_existing_path, spawn_tracedecay_daemon, tracedecay_command_with_home,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::storage::{
    EnrollmentMarker, StorageMode, default_profile_project_id, profile_sharded_data_root,
    write_enrollment_marker,
};

/// Bound for waits that depend on spawning and running the real `tracedecay`
/// CLI as a child process: connecting to the fake daemon socket and forwarding
/// the observed request back to the test thread. Under nextest's
/// process-per-test parallelism the fork/exec + init of that child can be
/// scheduled slowly on a loaded runner, so a 2s bound false-fires. This is a
/// generous ceiling that still fails fast on a genuine hang (the CLI normally
/// connects in well under a second).
const CLI_ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(20);

/// Bound for local, in-process readiness signals (a spawned thread binding a
/// socket and sending on an mpsc channel). These do not spawn external
/// processes, but the thread can still be scheduled slowly under load.
const LOCAL_READY_TIMEOUT: Duration = Duration::from_secs(10);

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

fn git(project: &Path, args: &[&str]) {
    let git = crate::common::git_program();
    // Retry a transient spawn ENOENT under heavy parallel load.
    let mut last_err: Option<std::io::Error> = None;
    let mut output = None;
    for attempt in 0..5 {
        match std::process::Command::new(&git)
            .args(args)
            .current_dir(project)
            .output()
        {
            Ok(out) => {
                output = Some(out);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt < 4 => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
            }
            Err(e) => panic!("git {args:?} should run (program {git:?}): {e}"),
        }
    }
    let output =
        output.unwrap_or_else(|| panic!("git {args:?} should run after retries: {last_err:?}"));
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
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

        let deadline = Instant::now() + CLI_ROUNDTRIP_TIMEOUT;
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
            .set_nonblocking(false)
            .expect("set accepted stream blocking");
        stream
            .set_write_timeout(Some(CLI_ROUNDTRIP_TIMEOUT))
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
        .recv_timeout(LOCAL_READY_TIMEOUT)
        .expect("fake daemon should become ready");
    request_rx
}

fn spawn_hook_event_daemon(socket_path: PathBuf) -> mpsc::Receiver<Value> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon socket");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        ready_tx.send(()).expect("notify fake daemon readiness");

        let deadline = Instant::now() + CLI_ROUNDTRIP_TIMEOUT;
        let (stream, _) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        panic!("timed out waiting for hook to connect to fake daemon");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept fake daemon client: {e}"),
            }
        };
        // The listener above is intentionally nonblocking so the accept loop
        // can poll against a deadline. The accepted stream must be switched
        // back to blocking (matching spawn_sentinel_daemon_with_notification)
        // before reading: on some platforms an accepted Unix socket can carry
        // over the listener's nonblocking flag, which would otherwise make
        // read_line() return a transient WouldBlock-shaped I/O error instead
        // of blocking until the CLI child has written its handshake line.
        stream
            .set_nonblocking(false)
            .expect("set accepted stream blocking");
        // Best-effort: fire-and-forget hook CLIs (the cursor notifications)
        // connect, write, and exit before this line runs, and on macOS
        // setsockopt(SO_RCVTIMEO) fails with EINVAL once the peer has hung
        // up — while the written bytes remain buffered and readable. The
        // timeout is only defense-in-depth (nextest's per-test timeout is
        // the real backstop), so a failure here must not panic the helper.
        let _ = stream.set_read_timeout(Some(CLI_ROUNDTRIP_TIMEOUT));
        let mut reader = BufReader::new(stream);
        let mut handshake = String::new();
        reader
            .read_line(&mut handshake)
            .expect("read daemon handshake");
        let handshake: Value = serde_json::from_str(handshake.trim()).expect("handshake JSON");
        assert!(
            handshake["project_path"].is_string(),
            "hook notifications must be scoped to a project"
        );
        assert!(
            !handshake
                .get("allow_init")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            "hook notifications must not permit daemon-side init"
        );

        let mut request = String::new();
        reader
            .read_line(&mut request)
            .expect("read hook JSON-RPC notification");
        let request: Value = serde_json::from_str(request.trim()).expect("request JSON");
        assert_eq!(request["method"], "tracedecay/hookEvent");
        assert!(
            request.get("id").is_none(),
            "hook event must be a notification, not a request"
        );
        request_tx
            .send(request)
            .expect("send observed hook event notification");
    });

    ready_rx
        .recv_timeout(LOCAL_READY_TIMEOUT)
        .expect("fake daemon should become ready");
    request_rx
}

fn assert_hook_notification(
    command_arg: &str,
    fail_open_label: &str,
    expected_agent: &str,
    expected_event: &str,
    build_event: impl FnOnce(&Path) -> Value,
    assert_extra_params: impl FnOnce(&Value, &Path),
) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed_request = spawn_hook_event_daemon(socket_path.clone());
    let event = build_event(&project_path).to_string();

    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .arg(command_arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("stdin should be piped")
                .write_all(event.as_bytes())?;
            child.wait_with_output()
        })
        .expect("hook command should run");

    assert!(
        output.status.success(),
        "{fail_open_label} hook should fail open\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let request = observed_request
        .recv_timeout(CLI_ROUNDTRIP_TIMEOUT)
        .expect("fake daemon should receive hook event");
    assert_eq!(request["params"]["agent"], expected_agent);
    assert_eq!(request["params"]["event"], expected_event);
    assert_extra_params(&request, &project_path);
}

#[test]
fn cursor_after_file_edit_hook_notifies_daemon() {
    assert_hook_notification(
        "hook-cursor-after-file-edit",
        "afterFileEdit",
        "cursor",
        "afterFileEdit",
        |project_path| {
            let edited = project_path.join("src/lib.rs");
            std::fs::write(&edited, "pub fn answer() -> u32 { 43 }\n").unwrap();
            json!({
                "hook_event_name": "afterFileEdit",
                "file_path": edited,
                "workspace_roots": [project_path],
            })
        },
        |request, _| {
            assert_eq!(request["params"]["rel_paths"], json!(["src/lib.rs"]));
        },
    );
}

#[test]
fn cursor_after_shell_hook_notifies_daemon() {
    assert_hook_notification(
        "hook-cursor-after-shell",
        "afterShellExecution",
        "cursor",
        "afterShellExecution",
        |project_path| {
            json!({
                "hook_event_name": "afterShellExecution",
                "command": "git pull --rebase",
                "cwd": project_path,
                "workspace_roots": [project_path],
            })
        },
        |request, project_path| {
            assert_eq!(request["params"]["command"], "git pull --rebase");
            assert_eq!(
                request["params"]["cwd"],
                project_path.to_string_lossy().to_string()
            );
        },
    );
}

#[test]
fn cursor_workspace_open_hook_notifies_daemon() {
    assert_hook_notification(
        "hook-cursor-workspace-open",
        "workspaceOpen",
        "cursor",
        "workspaceOpen",
        |project_path| {
            json!({
                "hook_event_name": "workspaceOpen",
                "cwd": project_path,
                "workspace_roots": [project_path],
            })
        },
        |request, project_path| {
            assert_eq!(
                request["params"]["cwd"],
                project_path.to_string_lossy().to_string()
            );
        },
    );
}

#[test]
fn kiro_post_tool_use_hook_notifies_daemon() {
    assert_hook_notification(
        "hook-kiro-post-tool-use",
        "Kiro postToolUse",
        "kiro",
        "postToolUse",
        |project_path| {
            let edited = project_path.join("src/lib.rs");
            std::fs::write(&edited, "pub fn answer() -> u32 { 44 }\n").unwrap();
            json!({
                "hook_event_name": "postToolUse",
                "cwd": project_path,
                "tool_name": "fs_write",
                "tool_input": {
                    "path": "src/lib.rs"
                },
            })
        },
        |request, project_path| {
            assert_eq!(
                request["params"]["cwd"],
                project_path.to_string_lossy().to_string()
            );
            assert_eq!(request["params"]["rel_paths"], json!(["src/lib.rs"]));
        },
    );
}

#[test]
fn daemon_sigterm_exits_while_authenticated_project_client_is_connected() {
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
    let authority: Value = serde_json::from_slice(
        &std::fs::read(home_path.join(".tracedecay/daemon-authority.json"))
            .expect("read daemon authority"),
    )
    .expect("parse daemon authority");
    let auth_token = authority["auth_token"]
        .as_str()
        .expect("daemon authority auth token");
    writeln!(
        client,
        "{}",
        json!({
            "protocol": "tracedecay-daemon-v1",
            "auth_token": auth_token
        })
    )
    .expect("write daemon auth preface");
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
        .recv_timeout(CLI_ROUNDTRIP_TIMEOUT)
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
        .recv_timeout(CLI_ROUNDTRIP_TIMEOUT)
        .expect("fake daemon should receive tools/call request");
}

#[test]
fn tool_cli_rejects_invalid_storage_scope_argument() {
    let home = TempDir::new().unwrap();
    let outside_cwd = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let outside_cwd_path = canonical_existing_path(outside_cwd.path());
    let args = json!({
        "provider": "cursor",
        "storage_scope": "hermes_profile",
    })
    .to_string();

    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&outside_cwd_path)
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
        !output.status.success(),
        "invalid storage_scope must fail before dispatch\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("storage-scope")
            && (stderr.contains("invalid value") || stderr.contains("project, user")),
        "rejection should name the valid scopes:\n{stderr}"
    );
}

#[test]
fn tool_cli_rejects_removed_hermes_home_argument() {
    let home = TempDir::new().unwrap();
    let hermes_home = TempDir::new().unwrap();
    let outside_cwd = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let hermes_home_path = canonical_existing_path(hermes_home.path());
    let outside_cwd_path = canonical_existing_path(outside_cwd.path());
    let args = json!({
        "provider": "cursor",
        "hermes_home": hermes_home_path,
    })
    .to_string();

    let output = tracedecay_command_with_home(&home_path)
        .current_dir(&outside_cwd_path)
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
        !output.status.success(),
        "removed hermes_home must fail before dispatch\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown parameter") && stderr.contains("--hermes-home"),
        "rejection should name the removed argument:\n{stderr}"
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
            r#"{"action":"add","content":"first touch via daemon","category":"decision"}"#,
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
        .recv_timeout(CLI_ROUNDTRIP_TIMEOUT)
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
fn doctor_keeps_live_daemon_database_healthy_without_compaction() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let data_root = profile_sharded_data_root(
        &home_path.join(".tracedecay"),
        &default_profile_project_id(&project_path),
    );
    let db_path = data_root.join(tracedecay::config::db_filename(&data_root));
    common::create_runtime().block_on(async {
        let (db, _) = crate::common::open_test_database(&db_path)
            .await
            .expect("open graph database");
        db.execute_write_batch(
            "seed doctor daemon reclaimable pages fixture",
            "CREATE TABLE doctor_daemon_probe (payload BLOB);\
                 WITH RECURSIVE count(x) AS (\
                     VALUES(1) UNION ALL SELECT x + 1 FROM count WHERE x < 128\
                 )\
                 INSERT INTO doctor_daemon_probe SELECT zeroblob(8192) FROM count;\
                 DELETE FROM doctor_daemon_probe;",
        )
        .await
        .expect("seed reclaimable pages");
        db.checkpoint().await.expect("checkpoint fixture");
    });

    let _daemon = spawn_tracedecay_daemon(&home_path);
    let first_tool_calls = tool_status_server_tool_calls(&home_path, &project_path);
    let output = tracedecay_command_with_home(&home_path)
        .arg("doctor")
        .args(["--agent", "claude"])
        .current_dir(&project_path)
        .output()
        .expect("doctor should run");
    assert!(
        output.status.success(),
        "doctor failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Compacting database") && !stderr.contains("VACUUM"),
        "doctor must stay read-only while daemon owns the database:\n{stderr}"
    );

    let second_tool_calls = tool_status_server_tool_calls(&home_path, &project_path);
    assert!(
        second_tool_calls > first_tool_calls,
        "daemon project engine must remain usable after doctor"
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
fn daemon_first_touch_init_does_not_mask_existing_profile_config_errors() {
    let daemon_home = TempDir::new().unwrap();
    let client_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let daemon_home_path = canonical_existing_path(daemon_home.path());
    let client_home_path = canonical_existing_path(client_home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&client_home_path, &project_path);

    let project_id = default_profile_project_id(&project_path);
    let config_path = client_home_path
        .join(".tracedecay/projects")
        .join(project_id)
        .join("config.json");
    std::fs::write(&config_path, b"{not json").unwrap();

    let _daemon = spawn_tracedecay_daemon(&daemon_home_path);
    let socket_path = common::daemon_socket_path(&daemon_home_path);
    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&client_home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args([
            "tool",
            "--project",
            &project_arg,
            "fact_store",
            "--json",
            "--args",
            r#"{"action":"add","content":"do not hide config errors","category":"decision"}"#,
        ])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        !output.status.success(),
        "first-touch daemon dispatch must not reinitialize over an existing bad config\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to parse config file"),
        "expected malformed config error, got:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(config_path).unwrap(),
        "{not json",
        "bad config should remain unchanged after rejected first-touch init"
    );
}

#[test]
fn daemon_project_handshake_uses_registry_backed_profile_store_without_marker() {
    let daemon_home = TempDir::new().unwrap();
    let client_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let daemon_home_path = canonical_existing_path(daemon_home.path());
    let client_home_path = canonical_existing_path(client_home.path());
    let project_path = canonical_existing_path(project.path());

    std::fs::create_dir_all(project_path.join("src")).unwrap();
    std::fs::write(
        project_path.join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    write_enrollment_marker(
        &project_path,
        &EnrollmentMarker {
            project_id: "proj_daemon_registry".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let output = tracedecay_command_with_home(&client_home_path)
        .arg("init")
        .current_dir(&project_path)
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
    std::fs::remove_dir_all(project_path.join(".tracedecay")).unwrap();

    let _daemon = spawn_tracedecay_daemon(&daemon_home_path);
    let socket_path = common::daemon_socket_path(&daemon_home_path);
    let project_arg = project_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&client_home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "active_project"])
        .output()
        .expect("tracedecay tool active_project should run");

    assert!(
        output.status.success(),
        "daemon should open registry-backed profile store without a checkout marker\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("proj_daemon_registry"),
        "active_project should report the registered profile store id\nstdout:\n{stdout}"
    );
}

#[test]
fn daemon_project_handshake_uses_registered_remote_store_after_rename() {
    let daemon_home = TempDir::new().unwrap();
    let client_home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let daemon_home_path = canonical_existing_path(daemon_home.path());
    let client_home_path = canonical_existing_path(client_home.path());
    let original_path = workspace.path().join("repo-before-rename");
    let renamed_path = workspace.path().join("repo-after-rename");

    std::fs::create_dir_all(&original_path).unwrap();
    git(&original_path, &["init"]);
    git(
        &original_path,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:ScriptedAlchemy/tracedecay.git",
        ],
    );
    init_project_with_cli(&client_home_path, &original_path);
    let original_project_id = default_profile_project_id(&canonical_existing_path(&original_path));
    std::fs::rename(&original_path, &renamed_path).unwrap();
    let renamed_path = canonical_existing_path(&renamed_path);

    let _daemon = spawn_tracedecay_daemon(&daemon_home_path);
    let socket_path = common::daemon_socket_path(&daemon_home_path);
    let project_arg = renamed_path.to_string_lossy().to_string();
    let output = tracedecay_command_with_home(&client_home_path)
        .current_dir(&renamed_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "active_project"])
        .output()
        .expect("tracedecay tool active_project should run");

    assert!(
        output.status.success(),
        "daemon should open renamed checkout through the registered git remote store\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        client_home_path
            .join(".tracedecay/projects")
            .join(&original_project_id)
            .join("tracedecay.db")
            .exists(),
        "original profile shard should remain the selected initialized store"
    );
    assert!(
        !client_home_path
            .join(".tracedecay/projects")
            .join(default_profile_project_id(&renamed_path))
            .join("tracedecay.db")
            .exists(),
        "daemon must not create a second path-hash profile shard for the renamed checkout"
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
fn tool_cli_without_daemon_socket_reports_daemon_unavailable() {
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

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TraceDecay daemon socket") && stderr.contains("is not available"),
        "expected explicit daemon-unavailable error, got:\n{stderr}"
    );
}
