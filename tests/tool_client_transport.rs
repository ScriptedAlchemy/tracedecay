#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Barrier, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{canonical_existing_path, tracedecay_command_with_home};
use serde_json::{Value, json};
use tempfile::TempDir;

const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);
const CHILD_TIMEOUT: Duration = Duration::from_secs(8);

struct ChildResult {
    output: Output,
    elapsed: Duration,
    killed_by_harness: bool,
}

fn run_command_with_timeout(mut command: Command, timeout: Duration) -> ChildResult {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn tracedecay");
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stderr = child.stderr.take().expect("stderr pipe");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read stdout");
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read stderr");
        bytes
    });
    let started = Instant::now();
    let (status, killed_by_harness) = loop {
        if let Some(status) = child.try_wait().expect("poll tracedecay") {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            child.kill().expect("kill hung tracedecay");
            break (child.wait().expect("reap hung tracedecay"), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    ChildResult {
        output: Output {
            status,
            stdout: stdout_reader.join().expect("join stdout reader"),
            stderr: stderr_reader.join().expect("join stderr reader"),
        },
        elapsed: started.elapsed(),
        killed_by_harness,
    }
}

fn init_project(home: &Path, project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("create project source");
    std::fs::write(project.join("src/lib.rs"), "pub fn marker() {}\n")
        .expect("write project source");
    crate::common::initialize_tracedecay_cli_project(home, project);
    // These journeys speak to a scripted daemon on an explicit socket; retire
    // the init daemon so its authority record cannot outrank that endpoint.
    crate::common::stop_managed_daemon(home);
}

fn tool_command(home: &Path, project: &Path, socket: &Path, query: &str) -> Command {
    let mut command = tracedecay_command_with_home(home);
    command
        .current_dir(project)
        .env("TRACEDECAY_DAEMON_SOCKET", socket)
        .args([
            "tool",
            "--project",
            project.to_string_lossy().as_ref(),
            "search",
            "--query",
            query,
            "--json",
        ]);
    command
}

fn response_bytes(request: &Value, text: &str) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": request["id"].clone(),
        "result": {
            "content": [{
                "type": "text",
                "text": text,
            }],
        },
    }))
    .expect("encode response");
    bytes.push(b'\n');
    bytes
}

fn spawn_scripted_daemon<F>(
    socket: PathBuf,
    connections: usize,
    script: F,
) -> (mpsc::Receiver<Value>, JoinHandle<()>)
where
    F: Fn(UnixStream, Value) + Send + Sync + 'static,
{
    let (ready_tx, ready_rx) = mpsc::channel();
    let (request_tx, request_rx) = mpsc::channel();
    let script = Arc::new(script);
    let server = std::thread::spawn(move || {
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        ready_tx.send(()).expect("signal daemon ready");
        let accept_deadline = Instant::now() + CHILD_TIMEOUT;
        let mut workers = Vec::new();
        while workers.len() < connections {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(LOCAL_TIMEOUT))
                        .expect("set read timeout");
                    stream
                        .set_write_timeout(Some(LOCAL_TIMEOUT))
                        .expect("set write timeout");
                    let mut reader =
                        BufReader::new(stream.try_clone().expect("clone fake daemon stream"));
                    let mut handshake = String::new();
                    reader.read_line(&mut handshake).expect("read handshake");
                    serde_json::from_str::<Value>(handshake.trim()).expect("decode handshake");
                    let mut request = String::new();
                    reader.read_line(&mut request).expect("read request");
                    let request: Value =
                        serde_json::from_str(request.trim()).expect("decode request");
                    assert_eq!(request["method"], "tools/call");
                    assert_eq!(request["params"]["name"], "tracedecay_search");
                    request_tx.send(request.clone()).expect("publish request");
                    let script = Arc::clone(&script);
                    workers.push(std::thread::spawn(move || script(stream, request)));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < accept_deadline,
                        "timed out waiting for {connections} client connections"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept fake daemon client: {error}"),
            }
        }
        for worker in workers {
            worker.join().expect("join fake daemon worker");
        }
    });
    ready_rx
        .recv_timeout(LOCAL_TIMEOUT)
        .expect("fake daemon ready");
    (request_rx, server)
}

fn fixture() -> (TempDir, TempDir, TempDir, PathBuf, PathBuf, PathBuf) {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let socket_dir = TempDir::new().expect("socket dir");
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    std::fs::create_dir(&profile_root).expect("create private profile root");
    std::fs::set_permissions(&profile_root, std::fs::Permissions::from_mode(0o700))
        .expect("secure profile root");
    init_project(&home_path, &project_path);
    let socket = socket_dir.path().join("tracedecay.sock");
    (home, project, socket_dir, home_path, project_path, socket)
}

#[test]
fn generic_tool_accepts_split_json_rpc_frame() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(socket.clone(), 1, |mut stream, request| {
        let bytes = response_bytes(&request, "split-ok");
        let split = bytes.len() / 2;
        stream.write_all(&bytes[..split]).expect("write prefix");
        stream.flush().expect("flush prefix");
        std::thread::sleep(Duration::from_millis(100));
        stream.write_all(&bytes[split..]).expect("write suffix");
    });
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "split"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join fake daemon");
    assert!(!result.killed_by_harness, "split response hung");
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(String::from_utf8_lossy(&result.output.stdout).contains("split-ok"));
}

#[test]
fn generic_tool_accepts_slow_byte_stream() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(socket.clone(), 1, |mut stream, request| {
        for byte in response_bytes(&request, "slow-ok") {
            stream.write_all(&[byte]).expect("write slow byte");
            stream.flush().expect("flush slow byte");
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "slow"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join fake daemon");
    assert!(!result.killed_by_harness, "slow response hung");
    assert!(result.output.status.success());
    assert!(String::from_utf8_lossy(&result.output.stdout).contains("slow-ok"));
}

#[test]
fn generic_tool_rejects_truncated_frame_without_output() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(socket.clone(), 1, |mut stream, _request| {
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":")
            .expect("write truncated response");
    });
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "truncated"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join fake daemon");
    assert!(!result.killed_by_harness, "truncated response hung");
    assert!(!result.output.status.success());
    assert!(result.output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&result.output.stderr).to_lowercase();
    assert!(stderr.contains("json") || stderr.contains("eof") || stderr.contains("decode"));
}

#[test]
fn generic_tool_rejects_semantic_truncation_envelope_without_output() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(socket.clone(), 1, |mut stream, request| {
        let envelope = json!({
            "truncated": true,
            "original_chars": 16000,
            "preview_chars": 2,
            "preview": "{}",
            "handle": "tool-trunc-1",
        });
        let mut bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": {
                "content": [{
                    "type": "text",
                    "text": envelope.to_string(),
                }],
            },
        }))
        .expect("encode truncation envelope");
        bytes.push(b'\n');
        stream.write_all(&bytes).expect("write truncation envelope");
    });
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "envelope"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join fake daemon");
    assert!(!result.killed_by_harness, "truncation envelope hung");
    assert!(!result.output.status.success());
    assert!(result.output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    assert!(
        stderr.contains("truncated JSON") && stderr.contains("tracedecay_retrieve"),
        "unexpected truncation error: {stderr}"
    );
}

/// The request deadline rides to the daemon, which enforces it; the client
/// reads for a bounded response grace beyond that deadline and never discards
/// an envelope it actually received. A reply arriving after the caller's
/// deadline but within the grace is therefore the authoritative outcome, not
/// an "outcome may be unknown" abort.
#[test]
fn generic_tool_preserves_late_reply_within_response_grace() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(socket.clone(), 1, |mut stream, request| {
        std::thread::sleep(Duration::from_secs(1));
        let _ = stream.write_all(&response_bytes(&request, "too-late"));
    });
    let mut command = tool_command(&home, &project, &socket, "never");
    command.env("TRACEDECAY_TOOL_DEADLINE_MS", "200");
    let result = run_command_with_timeout(command, CHILD_TIMEOUT);
    server.join().expect("join fake daemon");
    assert!(!result.killed_by_harness, "late reply was not read");
    assert!(
        result.output.status.success(),
        "received envelope must be honoured, not discarded: {}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.elapsed >= Duration::from_millis(200));
    assert!(result.elapsed < Duration::from_secs(5));
    let stdout = String::from_utf8_lossy(&result.output.stdout);
    assert!(
        stdout.contains("too-late"),
        "late payload must be printed: {stdout}"
    );
}

#[test]
fn generic_tool_rejects_unrepresentable_deadline() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let mut command = tool_command(&home, &project, &socket, "overflow");
    command.env("TRACEDECAY_TOOL_DEADLINE_MS", u64::MAX.to_string());
    let result = run_command_with_timeout(command, CHILD_TIMEOUT);
    assert!(!result.killed_by_harness);
    assert!(!result.output.status.success());
    assert!(result.output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    assert!(
        stderr.contains("TRACEDECAY_TOOL_DEADLINE_MS")
            && stderr.contains("monotonic deadline range"),
        "unexpected deadline validation error: {stderr}"
    );
}

#[test]
fn generic_tool_handles_concurrent_requests_without_crosstalk() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let server_barrier = Arc::clone(&barrier);
    let (_requests, server) =
        spawn_scripted_daemon(socket.clone(), 2, move |mut stream, request| {
            server_barrier.wait();
            let query = request["params"]["arguments"]["query"]
                .as_str()
                .expect("query argument");
            stream
                .write_all(&response_bytes(&request, query))
                .expect("write concurrent response");
        });
    let first = tool_command(&home, &project, &socket, "first");
    let second = tool_command(&home, &project, &socket, "second");
    let first = std::thread::spawn(move || run_command_with_timeout(first, CHILD_TIMEOUT));
    let second = std::thread::spawn(move || run_command_with_timeout(second, CHILD_TIMEOUT));
    let first = first.join().expect("join first CLI");
    let second = second.join().expect("join second CLI");
    server.join().expect("join fake daemon");
    assert!(!first.killed_by_harness && !second.killed_by_harness);
    assert!(first.output.status.success() && second.output.status.success());
    assert!(String::from_utf8_lossy(&first.output.stdout).contains("first"));
    assert!(String::from_utf8_lossy(&second.output.stdout).contains("second"));
}

#[test]
fn cancelling_generic_tool_reaps_child_and_closes_request() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (write_result_tx, write_result_rx) = mpsc::channel();
    let (requests, server) =
        spawn_scripted_daemon(socket.clone(), 1, move |mut stream, request| {
            std::thread::sleep(Duration::from_millis(200));
            write_result_tx
                .send(stream.write_all(&response_bytes(&request, "after-cancel")))
                .expect("publish post-cancel write");
        });
    let mut command = tool_command(&home, &project, &socket, "cancel");
    command
        .env("TRACEDECAY_TOOL_DEADLINE_MS", "30000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn cancellable CLI");
    requests
        .recv_timeout(LOCAL_TIMEOUT)
        .expect("observe cancellable request");
    child.kill().expect("cancel CLI child");
    let status = child.wait().expect("reap cancelled CLI child");
    assert!(!status.success());
    assert!(child.try_wait().expect("poll reaped child").is_some());
    let write_result = write_result_rx
        .recv_timeout(LOCAL_TIMEOUT)
        .expect("fake daemon observed cancellation");
    assert!(
        write_result.is_err(),
        "request socket remained open after cancellation"
    );
    server.join().expect("join fake daemon");
}
