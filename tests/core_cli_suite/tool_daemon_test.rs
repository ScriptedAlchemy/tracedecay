use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

/// Outer kill ceiling for CLI children under hang-regression tests. Product
/// deadlines (for example `TRACEDECAY_STATUS_DEADLINE_MS`) must expire first.
const CLI_CHILD_KILL_TIMEOUT: Duration = Duration::from_secs(12);

/// Spawn a CLI child with stdin closed and drain stdout/stderr on dedicated
/// threads so a large or stalled response cannot deadlock the pipe buffers.
fn run_command_with_timeout(mut command: Command, timeout: Duration) -> Output {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn tracedecay: {e}"));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            out.read_to_end(&mut buf)
                .unwrap_or_else(|e| panic!("failed to read stdout: {e}"));
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr {
            err.read_to_end(&mut buf)
                .unwrap_or_else(|e| panic!("failed to read stderr: {e}"));
        }
        buf
    });
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("failed to poll child: {e}"))
        {
            let stdout = stdout_handle.join().expect("stdout reader");
            let stderr = stderr_handle.join().expect("stderr reader");
            return Output {
                status,
                stdout,
                stderr,
            };
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child
                .wait()
                .unwrap_or_else(|e| panic!("failed to wait for timed out child: {e}"));
            let stdout = stdout_handle.join().unwrap_or_default();
            let stderr = stderr_handle.join().unwrap_or_default();
            panic!(
                "tracedecay hung after {:?}\nstdout:\n{}\nstderr:\n{}",
                started.elapsed(),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

enum FakeDaemonResponse {
    Complete { text: String },
    TruncatedJsonRpc,
    HoldOpen,
}

fn spawn_scripted_daemon(
    socket_path: PathBuf,
    expected_tool_name: &'static str,
    response: FakeDaemonResponse,
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
        let (stream, _) = common::poll_until(
            deadline,
            Duration::from_millis(10),
            || match listener.accept() {
                Ok(accepted) => Some(accepted),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(e) => panic!("accept fake daemon client: {e}"),
            },
            || "timed out waiting for tool CLI to connect to fake daemon".to_string(),
        );
        stream
            .set_nonblocking(false)
            .expect("set accepted stream blocking");
        stream
            .set_write_timeout(Some(CLI_ROUNDTRIP_TIMEOUT))
            .expect("write timeout");
        let _ = stream.set_read_timeout(Some(CLI_ROUNDTRIP_TIMEOUT));

        let mut reader = BufReader::new(stream.try_clone().expect("clone fake daemon stream"));
        let mut handshake = String::new();
        reader
            .read_line(&mut handshake)
            .expect("read daemon handshake");
        let _handshake: Value = serde_json::from_str(handshake.trim()).expect("handshake JSON");

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

        match response {
            FakeDaemonResponse::Complete { text } => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": text
                        }]
                    }
                });
                let mut writer = stream;
                writeln!(writer, "{}", serde_json::to_string(&response).unwrap())
                    .expect("write fake daemon response");
            }
            FakeDaemonResponse::TruncatedJsonRpc => {
                let mut writer = stream;
                write!(writer, "{{\"jsonrpc\":\"2.0\",\"id\":").expect("write truncated frame");
                // Close without a terminating newline so the client sees EOF mid-frame.
            }
            FakeDaemonResponse::HoldOpen => {
                // Keep the accepted socket open without writing a matching response.
                std::thread::sleep(CLI_CHILD_KILL_TIMEOUT + Duration::from_secs(2));
            }
        }
    });

    ready_rx
        .recv_timeout(LOCAL_READY_TIMEOUT)
        .expect("fake daemon should become ready");
    request_rx
}

fn minimal_status_payload() -> String {
    serde_json::to_string(&json!({
        "node_count": 1,
        "edge_count": 0,
        "file_count": 1,
        "nodes_by_kind": {},
        "edges_by_kind": {},
        "db_size_bytes": 128,
        "last_updated": 1,
        "total_source_bytes": 32,
        "files_by_language": { "Rust": 1 },
        "last_sync_at": 1,
        "last_full_sync_at": 1,
        "last_sync_duration_ms": 1,
        "serving_branch": "master",
    }))
    .expect("status payload")
}

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

fn configuration_tool_success(
    home: &Path,
    project: &Path,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let project_arg = project.to_string_lossy().to_string();
    let arguments = serde_json::to_string(&arguments).expect("configuration arguments");
    let deadline = Instant::now() + CLI_ROUNDTRIP_TIMEOUT;
    loop {
        let mut command = tracedecay_command_with_home(home);
        command.current_dir(project).args([
            "tool",
            "--project",
            project_arg.as_str(),
            tool_name,
            "--args",
            arguments.as_str(),
            "--json",
        ]);
        let output = run_command_with_timeout(command, CLI_ROUNDTRIP_TIMEOUT);
        let payload: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{tool_name} returned invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        if payload.get("outcome").is_some() {
            assert!(
                output.status.success(),
                "{tool_name} returned an outcome with a failing status\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return payload;
        }
        if Instant::now() >= deadline {
            panic!(
                "{tool_name} never became available\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_daemon_socket(socket_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    common::poll_until(
        deadline,
        Duration::from_millis(25),
        || UnixStream::connect(socket_path).is_ok().then_some(()),
        || {
            format!(
                "timed out waiting for daemon socket at {}",
                socket_path.display()
            )
        },
    );
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
        let (stream, _) = common::poll_until(
            deadline,
            Duration::from_millis(10),
            || match listener.accept() {
                Ok(accepted) => Some(accepted),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(e) => panic!("accept fake daemon client: {e}"),
            },
            || "timed out waiting for tool CLI to connect to fake daemon".to_string(),
        );
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
        let (stream, _) = common::poll_until(
            deadline,
            Duration::from_millis(10),
            || match listener.accept() {
                Ok(accepted) => Some(accepted),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(e) => panic!("accept fake daemon client: {e}"),
            },
            || "timed out waiting for hook to connect to fake daemon".to_string(),
        );
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
            // Shell text is never forwarded: `DaemonHookEvent` carries the
            // working directory so native Git reconciliation owns the effect,
            // and command text cannot become sync or branch authority.
            assert!(
                request["params"]["command"].is_null(),
                "shell command text must not reach the daemon: {request}"
            );
            assert_eq!(
                request["params"]["cwd"],
                project_path.to_string_lossy().to_string()
            );
        },
    );
}

#[test]
fn cursor_after_shell_missing_daemon_exits_promptly_without_children() {
    const SAMPLE_COUNT: usize = 10;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);
    let missing_socket = socket_dir.path().join("missing.sock");
    let event = json!({
        "hook_event_name": "afterShellExecution",
        "command": "git status",
        "cwd": project_path,
        "workspace_roots": [project_path],
    })
    .to_string();
    let mut samples = Vec::with_capacity(SAMPLE_COUNT);

    for _ in 0..SAMPLE_COUNT {
        let started = Instant::now();
        let output = tracedecay_command_with_home(&home_path)
            .current_dir(&project_path)
            .env("TRACEDECAY_DAEMON_SOCKET", &missing_socket)
            .arg("hook-cursor-after-shell")
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
                #[cfg(target_os = "linux")]
                {
                    let children =
                        std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", child.id()))
                            .unwrap_or_default();
                    assert!(
                        children.trim().is_empty(),
                        "after-shell hook spawned unexpected children: {children}"
                    );
                }
                child.wait_with_output()
            })
            .expect("missing-daemon hook command should run");
        samples.push(started.elapsed());
        assert!(
            output.status.success(),
            "missing-daemon hook must fail open\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    samples.sort_unstable();
    let min = samples[0];
    let median = samples[SAMPLE_COUNT / 2];
    let max = samples[SAMPLE_COUNT - 1];
    eprintln!(
        "cursor missing-daemon after-shell samples={SAMPLE_COUNT} min_us={} median_us={} max_us={}",
        min.as_micros(),
        median.as_micros(),
        max.as_micros()
    );
    assert!(
        max < Duration::from_secs(2),
        "missing-daemon hook exceeded its bounded fail-fast ceiling: {max:?}"
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
    let mut daemon = common::DaemonProcess::new(
        tracedecay_command_with_home(&home_path)
            .arg("daemon")
            .arg("run")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("tracedecay daemon should start"),
    );
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

    let pid = daemon.id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-TERM", pid.as_str()])
        .status()
        .expect("send SIGTERM to daemon");
    assert!(status.success(), "kill -TERM should succeed");

    assert!(
        daemon
            .wait_for_exit(Duration::from_secs(3))
            .expect("daemon status should be readable")
            .is_some(),
        "daemon should exit on SIGTERM even with a connected project client"
    );
}

#[test]
fn daemon_socket_is_owner_only() {
    let home = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let socket_path = common::daemon_socket_path(&home_path);
    let _ = std::fs::remove_file(&socket_path);
    let mut daemon = common::DaemonProcess::new(
        tracedecay_command_with_home(&home_path)
            .arg("daemon")
            .arg("run")
            .arg("--socket")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("tracedecay daemon should start"),
    );

    wait_for_daemon_socket(&socket_path);
    let mode = std::fs::metadata(&socket_path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "daemon socket should be owner-only");

    let pid = daemon.id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-TERM", pid.as_str()])
        .status()
        .expect("send SIGTERM to daemon");
    assert!(status.success(), "kill -TERM should succeed");

    assert!(
        daemon
            .wait_for_exit(Duration::from_secs(3))
            .expect("daemon status should be readable")
            .is_some(),
        "daemon should exit after socket permission test"
    );
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
fn configuration_tool_cli_persists_effects_and_fails_on_stale_cas() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);
    let daemon = spawn_tracedecay_daemon(&home_path);

    let observed = configuration_tool_success(
        &home_path,
        &project_path,
        "configuration_observed_state",
        json!({}),
    );
    let project_id = observed["scope"]["project_id"]
        .as_str()
        .expect("configuration scope project id")
        .to_owned();
    let initial_revision = observed["outcome"]["value"]["payload"][0]["desired_revision_id"]
        .as_str()
        .expect("initial configuration revision")
        .to_owned();
    let initial = configuration_tool_success(
        &home_path,
        &project_path,
        "configuration_get",
        json!({ "key": "diagnostics.prewarm.v1" }),
    );
    let initial_value = initial["outcome"]["value"]["payload"]["effective_value"]["value"]
        .as_bool()
        .expect("initial diagnostics prewarm value");
    let next_value = !initial_value;
    let mutation = json!({
        "layer": {
            "kind": "project",
            "project_id": project_id,
        },
        "key": "diagnostics.prewarm.v1",
        "value": {
            "kind": "boolean",
            "value": next_value,
        },
        "expected_revision": initial_revision,
    });

    configuration_tool_success(
        &home_path,
        &project_path,
        "configuration_set",
        mutation.clone(),
    );
    let advanced = configuration_tool_success(
        &home_path,
        &project_path,
        "configuration_observed_state",
        json!({}),
    );
    let advanced_revision = advanced["outcome"]["value"]["payload"][0]["desired_revision_id"]
        .as_str()
        .expect("advanced configuration revision")
        .to_owned();
    assert_ne!(advanced_revision, initial_revision);

    drop(daemon);
    let _restarted_daemon = spawn_tracedecay_daemon(&home_path);
    let reloaded = configuration_tool_success(
        &home_path,
        &project_path,
        "configuration_get",
        json!({ "key": "diagnostics.prewarm.v1" }),
    );
    assert_eq!(
        reloaded["outcome"]["value"]["payload"]["effective_value"]["value"], next_value,
        "the CLI mutation must survive a daemon restart and affect the resolved setting"
    );

    let stale_mutation = json!({
        "layer": {
            "kind": "project",
            "project_id": project_id,
        },
        "key": "diagnostics.prewarm.v1",
        "value": {
            "kind": "boolean",
            "value": initial_value,
        },
        "expected_revision": initial_revision,
    });
    let project_arg = project_path.to_string_lossy().to_string();
    let stale_arguments = serde_json::to_string(&stale_mutation).unwrap();
    let mut command = tracedecay_command_with_home(&home_path);
    command.current_dir(&project_path).args([
        "tool",
        "--project",
        project_arg.as_str(),
        "configuration_set",
        "--args",
        stale_arguments.as_str(),
        "--json",
    ]);
    let stale = run_command_with_timeout(command, CLI_ROUNDTRIP_TIMEOUT);
    assert!(
        !stale.status.success(),
        "a stale configuration write must fail the shell gate\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stale.stdout),
        String::from_utf8_lossy(&stale.stderr)
    );
    let stale_payload: Value =
        serde_json::from_slice(&stale.stdout).expect("stale write problem JSON");
    assert_eq!(stale_payload["problem"]["kind"], "conflict");
    assert_eq!(stale_payload["problem"]["code"], "configuration.conflict");
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
fn daemon_first_touch_uses_registered_runtime_without_rewriting_legacy_config() {
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
        output.status.success(),
        "daemon dispatch must use the registered configuration revision instead of re-entering \
         legacy first-touch migration\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Fact Store"),
        "registered runtime should execute the requested tool\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
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

#[test]
fn status_json_requests_compact_daemon_payload_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed = spawn_scripted_daemon(
        socket_path.clone(),
        "tracedecay_status",
        FakeDaemonResponse::Complete {
            text: minimal_status_payload(),
        },
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let mut command = tracedecay_command_with_home(&home_path);
    command
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .env("TERM", "dumb")
        // Positional path avoids a preliminary registry admin_cli round-trip
        // through --project-path / --project-id resolution.
        .args(["status", "--json", project_arg.as_str()]);
    let output = run_command_with_timeout(command, CLI_ROUNDTRIP_TIMEOUT);

    assert!(
        output.status.success(),
        "status --json should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains('\u{1b}'),
        "noninteractive status --json must not emit ANSI, got:\n{stdout}"
    );
    let payload: Value = serde_json::from_slice(&output.stdout).expect("one JSON status object");
    assert_eq!(payload["node_count"], 1);
    assert_ne!(payload.get("truncated"), Some(&json!(true)));

    let request = observed
        .recv_timeout(CLI_ROUNDTRIP_TIMEOUT)
        .expect("fake daemon should receive tools/call request");
    assert_eq!(request["params"]["name"], "tracedecay_status");
    let args = &request["params"]["arguments"];
    assert_eq!(args["format"], "json");
    assert_eq!(args["include_branch_diagnostics"], false);
    assert_eq!(args["include_storage_health"], false);
    assert_eq!(args["include_session_ingest"], false);
    assert_eq!(args["include_staleness"], false);
}

#[test]
fn status_json_rejects_truncated_tool_payload_without_partial_stdout() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let truncated = json!({
        "truncated": true,
        "original_chars": 20_000,
        "preview_chars": 32,
        "preview": "{\"node_count\":1}",
        "handle": "rh_status_test",
    })
    .to_string();
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let _observed = spawn_scripted_daemon(
        socket_path.clone(),
        "tracedecay_status",
        FakeDaemonResponse::Complete { text: truncated },
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let mut command = tracedecay_command_with_home(&home_path);
    command
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["status", "--json", project_arg.as_str()]);
    let output = run_command_with_timeout(command, CLI_ROUNDTRIP_TIMEOUT);

    assert!(
        !output.status.success(),
        "truncated status envelope must fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "must not print a partial/wrong-schema status object, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("truncated JSON") && stderr.contains("tracedecay_status"),
        "expected truncation diagnostic, got:\n{stderr}"
    );
}

#[test]
fn tool_cli_rejects_truncated_json_rpc_response_without_hanging() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let socket_path = socket_dir.path().join("tracedecay.sock");
    let _observed = spawn_scripted_daemon(
        socket_path.clone(),
        "tracedecay_status",
        FakeDaemonResponse::TruncatedJsonRpc,
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let mut command = tracedecay_command_with_home(&home_path);
    command
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args(["tool", "--project", &project_arg, "status", "--json"]);
    let output = run_command_with_timeout(command, CLI_CHILD_KILL_TIMEOUT);

    assert!(
        !output.status.success(),
        "truncated JSON-RPC must fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        (stderr.contains("json") || stderr.contains("eof") || stderr.contains("decode"))
            && (stderr.contains("daemon") || stderr.contains("tool")),
        "expected a decode/EOF protocol diagnostic, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn status_command_times_out_when_daemon_never_replies() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);

    let socket_path = socket_dir.path().join("tracedecay.sock");
    let _observed = spawn_scripted_daemon(
        socket_path.clone(),
        "tracedecay_status",
        FakeDaemonResponse::HoldOpen,
    );
    let project_arg = project_path.to_string_lossy().to_string();
    let mut command = tracedecay_command_with_home(&home_path);
    command
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .env("TRACEDECAY_STATUS_DEADLINE_MS", "2000")
        .args(["status", "--json", project_arg.as_str()]);
    let started = Instant::now();
    let output = run_command_with_timeout(command, CLI_CHILD_KILL_TIMEOUT);
    let elapsed = started.elapsed();

    assert!(
        !output.status.success(),
        "stalled daemon must not succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "status should fail under the absolute deadline, took {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("timed out") || stderr.contains("deadline"),
        "expected deadline diagnostic, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn spawn_handshake_capturing_daemon(socket_path: PathBuf) -> mpsc::Receiver<Value> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (handshake_tx, handshake_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon socket");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        ready_tx.send(()).expect("notify fake daemon readiness");

        let deadline = Instant::now() + CLI_ROUNDTRIP_TIMEOUT;
        let (stream, _) = common::poll_until(
            deadline,
            Duration::from_millis(10),
            || match listener.accept() {
                Ok(accepted) => Some(accepted),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(e) => panic!("accept fake daemon client: {e}"),
            },
            || "timed out waiting for tool CLI to connect to fake daemon".to_string(),
        );
        stream
            .set_nonblocking(false)
            .expect("set accepted stream blocking");
        let _ = stream.set_read_timeout(Some(CLI_ROUNDTRIP_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CLI_ROUNDTRIP_TIMEOUT));

        let mut reader = BufReader::new(stream.try_clone().expect("clone fake daemon stream"));
        let mut handshake = String::new();
        reader
            .read_line(&mut handshake)
            .expect("read daemon handshake");
        let handshake: Value = serde_json::from_str(handshake.trim()).expect("handshake JSON");
        handshake_tx
            .send(handshake)
            .expect("send observed handshake");

        let mut request = String::new();
        reader
            .read_line(&mut request)
            .expect("read JSON-RPC request");
        let request: Value = serde_json::from_str(request.trim()).expect("request JSON");
        let response = json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": {
                "content": [{
                    "type": "text",
                    "text": "{\"status\":\"ok\"}"
                }]
            }
        });
        let mut writer = stream;
        writeln!(writer, "{}", serde_json::to_string(&response).unwrap())
            .expect("write fake daemon response");
    });

    ready_rx
        .recv_timeout(LOCAL_READY_TIMEOUT)
        .expect("fake daemon should become ready");
    handshake_rx
}

#[test]
fn user_scoped_lcm_tool_cli_handshakes_projectless_from_filesystem_root_cwd() {
    let home = TempDir::new().unwrap();
    let socket_dir = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let observed_handshake = spawn_handshake_capturing_daemon(socket_path.clone());
    let args = json!({
        "provider": "hermes",
        "session_id": "stock-check-session",
        "storage_scope": "user",
        "transcript_projection": true,
        "messages": [
            {"role": "user", "content": "hello", "id": "m1"},
            {"role": "assistant", "content": "hi there", "id": "m2"}
        ],
    })
    .to_string();

    let output = tracedecay_command_with_home(&home_path)
        .current_dir(std::path::Path::new("/"))
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .args([
            "tool",
            "tracedecay_lcm_preflight",
            "--json",
            "--args",
            args.as_str(),
        ])
        .output()
        .expect("tracedecay tool should run");

    assert!(
        output.status.success(),
        "user-scoped LCM from cwd=/ must reach the daemon projectless\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let handshake = observed_handshake
        .recv_timeout(CLI_ROUNDTRIP_TIMEOUT)
        .expect("fake daemon should observe handshake");
    assert!(
        handshake.get("project_path").is_none()
            || handshake.get("project_path") == Some(&Value::Null),
        "Hermes user scope must not invent project=/; handshake was {handshake}"
    );
}

#[test]
fn hermes_stock_sync_turn_keeps_project_lcm_grep_available() {
    // Mirrors scripts/hermes_stock_check.py: user-scoped sync_turn (cwd=/) then
    // project-scoped lcm_grep must stay available without inventing project=/.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    init_project_with_cli(&home_path, &project_path);
    let _daemon = spawn_tracedecay_daemon(&home_path);
    let project_arg = project_path.to_string_lossy().to_string();
    let socket = common::daemon_socket_path(&home_path);

    let user_args = json!({
        "provider": "hermes",
        "session_id": "stock-check-session",
        "storage_scope": "user",
        "transcript_projection": true,
        "messages": [
            {
                "role": "user",
                "content": "hello",
                "id": "tracedecay_sync_1_user",
                "timestamp": 1.0,
                "associated_project_roots": [project_arg],
            },
            {
                "role": "assistant",
                "content": "hi there",
                "id": "tracedecay_sync_1_assistant",
                "timestamp": 1.0,
                "associated_project_roots": [project_arg],
            }
        ],
    })
    .to_string();
    let user_output = tracedecay_command_with_home(&home_path)
        .current_dir(std::path::Path::new("/"))
        .env("TRACEDECAY_DAEMON_SOCKET", &socket)
        .args([
            "tool",
            "tracedecay_lcm_preflight",
            "--json",
            "--args",
            user_args.as_str(),
        ])
        .output()
        .expect("user-scoped sync_turn preflight should run");
    assert!(
        user_output.status.success(),
        "user-scoped Hermes preflight must succeed projectless\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&user_output.stdout),
        String::from_utf8_lossy(&user_output.stderr)
    );

    let project_args = json!({
        "provider": "hermes",
        "session_id": "stock-check-session",
        "transcript_projection": true,
        "messages": [
            {
                "role": "user",
                "content": "hello",
                "id": "tracedecay_sync_1_user",
                "timestamp": 1.0,
                "associated_project_roots": [project_arg],
            },
            {
                "role": "assistant",
                "content": "hi there",
                "id": "tracedecay_sync_1_assistant",
                "timestamp": 1.0,
                "associated_project_roots": [project_arg],
            }
        ],
    })
    .to_string();
    let project_output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket)
        .args([
            "tool",
            "--project",
            &project_arg,
            "tracedecay_lcm_preflight",
            "--json",
            "--args",
            project_args.as_str(),
        ])
        .output()
        .expect("project-scoped sync_turn preflight should run");
    assert!(
        project_output.status.success(),
        "project-scoped Hermes preflight must succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&project_output.stdout),
        String::from_utf8_lossy(&project_output.stderr)
    );

    let grep_args = json!({
        "provider": "hermes",
        "session_id": "stock-check-session",
        "query": "hello",
        "scope": "all",
    })
    .to_string();
    let grep_output = tracedecay_command_with_home(&home_path)
        .current_dir(&project_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket)
        .args([
            "tool",
            "--project",
            &project_arg,
            "tracedecay_lcm_grep",
            "--json",
            "--args",
            grep_args.as_str(),
        ])
        .output()
        .expect("project-scoped lcm_grep should run");
    assert!(
        grep_output.status.success(),
        "lcm_grep after sync_turn must succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&grep_output.stdout),
        String::from_utf8_lossy(&grep_output.stderr)
    );
    let grep: Value = serde_json::from_slice(&grep_output.stdout).unwrap_or_else(|error| {
        panic!(
            "lcm_grep should return JSON ({error}): {}",
            String::from_utf8_lossy(&grep_output.stdout)
        )
    });
    let payload = grep
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(grep);
    assert!(
        payload.get("error").is_none(),
        "stock Hermes regression: grep must remain available after sync_turn, got {payload}"
    );
    assert_ne!(
        payload.get("status").and_then(Value::as_str),
        Some("unavailable"),
        "stock Hermes regression: temporal store must stay attached, got {payload}"
    );
}
