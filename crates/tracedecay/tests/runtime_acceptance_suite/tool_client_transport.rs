use crate::common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Barrier, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::common::{canonical_existing_path, tracedecay_command_with_home};
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{FilesLayoutV1, FilesResultV1, IndexedFileV1};
use tracedecay_daemon_identity::authority::DaemonAuthority;
use tracedecay_daemon_protocol::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonAuthPreface, DaemonEndpoint,
    DaemonHandshake, DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationRequest,
    DaemonInvocationResponse,
};
use tracedecay_domain::ProjectId;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

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
    let git = Command::new(common::git_program())
        .current_dir(project)
        .args(["init", "--quiet"])
        .status()
        .expect("initialize fixture repository");
    assert!(git.success(), "initialize fixture repository");
    crate::common::initialize_tracedecay_cli_project(home, project);
    // These journeys speak to a scripted daemon on an explicit socket; retire
    // the init daemon so its authority record cannot outrank that endpoint.
    crate::common::stop_managed_daemon(home);
}

fn tool_command(home: &Path, project: &Path, socket: &Path, pattern: &str) -> Command {
    let mut command = tracedecay_command_with_home(home);
    command
        .current_dir(project)
        .env("TRACEDECAY_DAEMON_SOCKET", socket)
        .args([
            "tool",
            "--project",
            project.to_string_lossy().as_ref(),
            "files",
            "--pattern",
            pattern,
            "--format",
            "json",
            "--json",
        ]);
    command
}

fn files_result(paths: impl IntoIterator<Item = String>) -> FilesResultV1 {
    let files = paths
        .into_iter()
        .map(|path| IndexedFileV1 {
            path,
            symbols: 1,
            bytes: 20,
        })
        .collect::<Vec<_>>();
    FilesResultV1 {
        count: files.len(),
        layout: FilesLayoutV1::Flat,
        files,
        worktree_omitted_sources: None,
        freshness: None,
    }
}

fn files_response(
    request: &DaemonInvocationRequest,
    scope: ResolvedScope,
    result: FilesResultV1,
) -> Vec<u8> {
    let response = DaemonInvocationResponse {
        protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
        revision: DAEMON_INVOCATION_REVISION,
        request_id: request.request_id.clone(),
        outcome: DaemonInvocationOutcome::GraphTool {
            scope,
            completion: GraphToolCompletionV1 {
                result: GraphToolResultV1::Files(result),
                touched_files: Vec::new(),
                code_graph: None,
                analytics: None,
                cost: None,
            },
        },
    };
    let mut bytes = serde_json::to_vec(&response).expect("encode canonical response");
    bytes.push(b'\n');
    bytes
}

fn response_bytes(request: &DaemonInvocationRequest, scope: ResolvedScope, text: &str) -> Vec<u8> {
    files_response(request, scope, files_result([text.to_owned()]))
}

fn output_payload(result: &ChildResult) -> Value {
    let envelope = serde_json::from_slice(&result.output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid CLI JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&result.output.stderr)
        )
    });
    tracedecay::daemon::tool_json_payload(&envelope, "transport fixture")
        .expect("tool JSON payload")
}

fn assert_problem(result: &ChildResult, kind: &str) {
    assert!(!result.killed_by_harness, "CLI failed to settle");
    assert!(!result.output.status.success(), "problem must fail the CLI");
    let envelope: Value =
        serde_json::from_slice(&result.output.stdout).expect("typed problem JSON");
    assert_eq!(envelope["isError"], true, "{envelope}");
    assert_eq!(
        envelope["structuredContent"]["problem"]["kind"], kind,
        "{envelope}"
    );
}

fn spawn_scripted_daemon<F>(
    socket: PathBuf,
    home: &Path,
    project: &Path,
    connections: usize,
    script: F,
) -> (mpsc::Receiver<()>, JoinHandle<()>)
where
    F: Fn(UnixStream, DaemonInvocationRequest, ResolvedScope) + Send + Sync + 'static,
{
    let layout = tracedecay_runtime_core::storage::resolve_persisted_layout(
        project,
        &home.join(".tracedecay"),
    )
    .expect("resolve enrolled project")
    .expect("project is enrolled");
    let project_id = ProjectId::new(layout.identity.project_id.expect("enrolled project id"))
        .expect("valid enrolled project id");
    let scope =
        tracedecay_code_index_runtime::code_index_scheduler::identity::resolved_scope_for_project(
            project,
            &project_id,
        )
        .expect("production project scope");
    let expected_project = canonical_existing_path(project);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (request_tx, request_rx) = mpsc::channel();
    let script = Arc::new(script);
    let server = std::thread::spawn(move || {
        let _ = std::fs::remove_file(&socket);
        let authority = DaemonAuthority::acquire(
            socket.parent().expect("socket parent"),
            &DaemonEndpoint::Unix(socket.clone()),
            env!("CARGO_PKG_VERSION"),
        )
        .expect("seed fake daemon authority");
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
                    // macOS `accept` inherits `O_NONBLOCK` from a non-blocking
                    // listener, so the first `read` returns `WouldBlock`.
                    stream
                        .set_nonblocking(false)
                        .expect("accepted fake daemon stream must be blocking");
                    stream
                        .set_read_timeout(Some(LOCAL_TIMEOUT))
                        .expect("set read timeout");
                    stream
                        .set_write_timeout(Some(LOCAL_TIMEOUT))
                        .expect("set write timeout");
                    let mut reader =
                        BufReader::new(stream.try_clone().expect("clone fake daemon stream"));
                    let mut preface = String::new();
                    reader.read_line(&mut preface).expect("read auth preface");
                    assert!(
                        DaemonAuthPreface::from_line(preface.trim())
                            .expect("decode auth preface")
                            .authenticate(authority.auth_token())
                    );
                    let mut handshake = String::new();
                    reader.read_line(&mut handshake).expect("read handshake");
                    let handshake: DaemonHandshake =
                        serde_json::from_str(handshake.trim()).expect("decode handshake");
                    assert_eq!(
                        handshake
                            .project_path
                            .as_deref()
                            .map(canonical_existing_path),
                        Some(expected_project.clone()),
                        "CLI must request the enrolled project"
                    );
                    let mut request = String::new();
                    reader.read_line(&mut request).expect("read request");
                    let request: DaemonInvocationRequest =
                        serde_json::from_str(request.trim()).expect("decode canonical request");
                    assert_eq!(request.protocol, DAEMON_INVOCATION_PROTOCOL);
                    assert_eq!(request.revision, DAEMON_INVOCATION_REVISION);
                    assert!(matches!(
                        &request.payload,
                        DaemonInvocationPayload::GraphTool {
                            surface_operation: ApplicationSurfaceOperation::Files,
                            ..
                        }
                    ));
                    request_tx.send(()).expect("publish request receipt");
                    let script = Arc::clone(&script);
                    let scope = scope.clone();
                    workers.push(std::thread::spawn(move || script(stream, request, scope)));
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
fn generic_tool_accepts_slow_byte_stream() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        |mut stream, request, scope| {
            for byte in response_bytes(&request, scope, "slow-ok") {
                stream.write_all(&[byte]).expect("write slow byte");
                stream.flush().expect("flush slow byte");
                std::thread::sleep(Duration::from_millis(2));
            }
        },
    );
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "slow"),
        CHILD_TIMEOUT,
    );
    let served = server.join();
    assert!(
        !result.killed_by_harness && result.output.status.success(),
        "slow response failed after {:?} (killed={}): status={} stdout={} stderr={}",
        result.elapsed,
        result.killed_by_harness,
        result.output.status,
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr),
    );
    served.expect("join fake daemon");
    assert!(String::from_utf8_lossy(&result.output.stdout).contains("slow-ok"));
}

#[test]
fn generic_tool_rejects_truncated_frame_as_typed_failure() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        |mut stream, request, scope| {
            let bytes = response_bytes(&request, scope, "partial-must-not-escape");
            stream
                .write_all(&bytes[..bytes.len() / 2])
                .expect("write truncated response");
        },
    );
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "truncated"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join scripted daemon");
    assert_problem(&result, "unavailable");
    let envelope: Value = serde_json::from_slice(&result.output.stdout).expect("problem envelope");
    assert_eq!(
        envelope["structuredContent"]["problem"]["diagnostic"]["code"],
        "daemon_unavailable"
    );
    assert!(!String::from_utf8_lossy(&result.output.stdout).contains("partial-must-not-escape"));
}

fn oversized_files() -> FilesResultV1 {
    let files =
        files_result((0..1024).map(|index| format!("src/transport_large_result_{index:04}.rs")));
    assert!(
        serde_json::to_vec(&files).expect("files JSON").len() > tracedecay_mcp::MAX_RESPONSE_CHARS
    );
    files
}

#[test]
fn generic_tool_reports_unavailable_truncation_storage() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let layout = tracedecay_runtime_core::storage::resolve_persisted_layout(
        &project,
        &home.join(".tracedecay"),
    )
    .expect("resolve fixture layout")
    .expect("initialized layout");
    if layout.response_handle_root.exists() {
        std::fs::remove_dir(&layout.response_handle_root).expect("unused fixture handle cache");
    }
    std::fs::write(&layout.response_handle_root, b"cache path is a file")
        .expect("block handle cache storage");
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        |mut stream, request, scope| {
            stream
                .write_all(&files_response(&request, scope, oversized_files()))
                .expect("write oversized result");
        },
    );
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "large"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join scripted daemon");
    assert!(!result.killed_by_harness, "oversized response hung");
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    let payload = output_payload(&result);
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(payload["handle_available"], false, "{payload}");
    assert!(
        payload.get("handle").is_none(),
        "unwritten content must not receive a handle"
    );
    assert_eq!(
        payload["handle_status"]["reason_code"],
        "handle_store_failed"
    );
    assert_eq!(payload["handle_status"]["retryable"], true);
    assert!(
        payload["handle_status"]["retry_instruction"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        payload["preview_chars"].as_u64().unwrap() < payload["original_chars"].as_u64().unwrap()
    );
}

#[test]
fn generic_tool_retrieves_oversized_typed_result() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let expected = serde_json::to_value(oversized_files()).expect("expected file listing");
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        |mut stream, request, scope| {
            stream
                .write_all(&files_response(&request, scope, oversized_files()))
                .expect("write oversized result");
        },
    );
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "large"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join scripted daemon");
    assert!(!result.killed_by_harness, "oversized response hung");
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    let preview = output_payload(&result);
    assert_eq!(preview["truncated"], true, "{preview}");
    let handle = preview["handle"].as_str().expect("stored response handle");
    assert_eq!(preview["retrieve_tool"], "tracedecay_retrieve");

    // The CLI renderer wrote the real enrolled project's cache. Recover its
    // pages through the shipped daemon's retrieve owner, not scripted pages.
    let _daemon = common::spawn_tracedecay_daemon(&home);
    let mut content = String::new();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let arguments = json!({ "handle": handle, "offset": offset, "format": "json" });
        let mut command = tracedecay_command_with_home(&home);
        command.current_dir(&project).args([
            "tool",
            "--project",
            project.to_str().expect("project path"),
            "retrieve",
            "--args",
            &arguments.to_string(),
            "--json",
        ]);
        let retrieved = run_command_with_timeout(command, CHILD_TIMEOUT);
        assert!(!retrieved.killed_by_harness, "retrieval hung");
        assert!(
            retrieved.output.status.success(),
            "{}",
            String::from_utf8_lossy(&retrieved.output.stderr)
        );
        let page = output_payload(&retrieved);
        pages += 1;
        assert_eq!(page["handle"], handle);
        assert_eq!(page["offset"], offset);
        content.push_str(page["content"].as_str().expect("retained page content"));
        if page["has_more"] == false {
            assert!(page["next_offset"].is_null());
            assert_eq!(
                page["total_chars"].as_u64(),
                Some(content.chars().count() as u64)
            );
            break;
        }
        assert_eq!(page["has_more"], true);
        let next = page["next_offset"].as_u64().expect("next retained offset");
        assert!(next > offset, "retrieval must advance");
        offset = next;
    }
    assert!(
        pages > 1,
        "the oversized result must require multiple bounded pages"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&content).expect("complete retained JSON"),
        expected
    );
}

/// Files listing whose compact `--json` document stays under the Linux pipe
/// buffer while the pretty-printed form does not. Agents that `wait()` before
/// reading stdout deadlock on the pretty form and capture ~65KiB of truncated
/// JSON; compact output plus an explicit flush lets that parent exit.
fn pipe_boundary_files() -> FilesResultV1 {
    files_result((0..520).map(|index| format!("src/pipe_boundary_{index:04}.rs")))
}

fn hold_stream_after_files(
    mut stream: UnixStream,
    request: DaemonInvocationRequest,
    scope: ResolvedScope,
    files: FilesResultV1,
) {
    stream
        .write_all(&files_response(&request, scope, files))
        .expect("write files result");
    stream.flush().expect("flush files result");
    // The production daemon stays in its retained-connection read loop after
    // a graph-tool response. Close only when the CLI drops the stream.
    let mut extra = String::new();
    let _ = BufReader::new(stream).read_line(&mut extra);
}

/// `wait()` the child before reading stdout, the classic pipe-deadlock parent.
fn run_command_wait_then_read(mut command: Command, timeout: Duration) -> ChildResult {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn tracedecay");
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stderr = child.stderr.take().expect("stderr pipe");
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
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    stdout.read_to_end(&mut stdout_bytes).expect("read stdout");
    stderr.read_to_end(&mut stderr_bytes).expect("read stderr");
    ChildResult {
        output: Output {
            status,
            stdout: stdout_bytes,
            stderr: stderr_bytes,
        },
        elapsed: started.elapsed(),
        killed_by_harness,
    }
}

fn assert_complete_json_document(result: &ChildResult) -> Value {
    assert!(
        !result.killed_by_harness,
        "CLI --json hung after writing files: elapsed={:?} stdout_len={} stderr={}",
        result.elapsed,
        result.output.stdout.len(),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    serde_json::from_slice(&result.output.stdout).unwrap_or_else(|error| {
        panic!(
            "--json must be one complete parseable document ({error}); bytes={} head={}",
            result.output.stdout.len(),
            String::from_utf8_lossy(&result.output.stdout[..result.output.stdout.len().min(200)])
        )
    })
}

#[test]
fn generic_tool_json_exits_while_daemon_holds_the_stream() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let files = oversized_files();
    let expected = files.count;
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        move |stream, request, scope| {
            hold_stream_after_files(stream, request, scope, files.clone());
        },
    );
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "large"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join scripted daemon");
    assert!(
        result.elapsed < Duration::from_secs(3),
        "held daemon stream must not keep --json alive: {:?}",
        result.elapsed
    );
    let envelope = assert_complete_json_document(&result);
    assert_eq!(envelope["isError"], false, "{envelope}");
    assert_eq!(envelope["structuredContent"]["count"], expected);
    let files = envelope["structuredContent"]["files"]
        .as_array()
        .unwrap_or_else(|| panic!("held-stream --json must keep the full listing: {envelope}"));
    assert_eq!(files.len(), expected);
    assert!(
        result.output.stdout.len() > 65_536,
        "held-stream --json must keep the full listing above 64KiB: {}",
        result.output.stdout.len()
    );
}

#[test]
fn generic_tool_json_completes_when_parent_waits_before_reading() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let files = pipe_boundary_files();
    let expected = files.count;
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        move |stream, request, scope| {
            hold_stream_after_files(stream, request, scope, files.clone());
        },
    );
    let result = run_command_wait_then_read(
        tool_command(&home, &project, &socket, "boundary"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join scripted daemon");
    let envelope = assert_complete_json_document(&result);
    assert_eq!(envelope["isError"], false, "{envelope}");
    assert!(
        result.output.stdout.len() <= 65_536,
        "pipe-boundary --json must fit a wait-then-read parent: {}",
        result.output.stdout.len()
    );
    let files = envelope["structuredContent"]["files"]
        .as_array()
        .unwrap_or_else(|| panic!("pipe-boundary --json must keep inline files: {envelope}"));
    assert_eq!(files.len(), expected);
}

#[test]
fn generic_tool_json_keeps_full_oversized_files_when_parent_drains() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let files = oversized_files();
    let expected = files.count;
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        move |stream, request, scope| {
            hold_stream_after_files(stream, request, scope, files.clone());
        },
    );
    let result = run_command_with_timeout(
        tool_command(&home, &project, &socket, "large"),
        CHILD_TIMEOUT,
    );
    server.join().expect("join scripted daemon");
    let envelope = assert_complete_json_document(&result);
    assert_eq!(envelope["structuredContent"]["count"], expected);
    let files = envelope["structuredContent"]["files"]
        .as_array()
        .unwrap_or_else(|| panic!("drained --json must keep the full listing: {envelope}"));
    assert_eq!(files.len(), expected);
    assert!(
        result.output.stdout.len() > 65_536,
        "drained --json must keep the full listing above 64KiB: {}",
        result.output.stdout.len()
    );
}

#[test]
fn generic_read_only_tool_times_out_without_late_success() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        |mut stream, request, scope| {
            std::thread::sleep(Duration::from_secs(1));
            let _ = stream.write_all(&response_bytes(&request, scope, "too-late"));
        },
    );
    let mut command = tool_command(&home, &project, &socket, "never");
    command.env("TRACEDECAY_TOOL_DEADLINE_MS", "200");
    let result = run_command_with_timeout(command, CHILD_TIMEOUT);
    server.join().expect("join scripted daemon");
    // Read-only invocations cancel at their deadline; only authoritative
    // effects retain the response-grace settlement policy.
    assert_problem(&result, "timed_out");
    assert!(result.elapsed >= Duration::from_millis(200));
    assert!(result.elapsed < Duration::from_secs(5));
    assert!(!String::from_utf8_lossy(&result.output.stdout).contains("too-late"));
}

#[test]
fn generic_tool_rejects_unrepresentable_deadline() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let mut command = tool_command(&home, &project, &socket, "overflow");
    command.env("TRACEDECAY_TOOL_DEADLINE_MS", u64::MAX.to_string());
    let result = run_command_with_timeout(command, CHILD_TIMEOUT);
    assert!(!result.killed_by_harness);
    assert!(!result.output.status.success());
    let refusal: Value = serde_json::from_slice(&result.output.stdout).unwrap_or_else(|error| {
        panic!(
            "--json deadline refusal must be one JSON document ({error}): {}",
            String::from_utf8_lossy(&result.output.stdout)
        )
    });
    assert_eq!(
        (&refusal["problem"]["kind"], &refusal["problem"]["code"],),
        (
            &json!("invalid_request"),
            &json!("application.surface.invalid_request"),
        ),
        "deadline validation must be a typed invalid request: {refusal}"
    );
    assert!(
        refusal["problem"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("TRACEDECAY_TOOL_DEADLINE_MS")
                && message.contains("monotonic deadline range")),
        "typed deadline refusal must retain its diagnostic: {refusal}"
    );
}

#[test]
fn generic_tool_handles_concurrent_requests_without_crosstalk() {
    let (_home, _project, _socket_dir, home, project, socket) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let server_barrier = Arc::clone(&barrier);
    let (_requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        2,
        move |mut stream, request, scope| {
            server_barrier.wait();
            let DaemonInvocationPayload::GraphTool { arguments, .. } = &request.payload else {
                panic!("expected graph tool request");
            };
            let query = arguments["pattern"].as_str().expect("pattern argument");
            stream
                .write_all(&response_bytes(&request, scope, query))
                .expect("write concurrent response");
        },
    );
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
    let (requests, server) = spawn_scripted_daemon(
        socket.clone(),
        &home,
        &project,
        1,
        move |mut stream, request, scope| {
            std::thread::sleep(Duration::from_millis(200));
            write_result_tx
                .send(stream.write_all(&response_bytes(&request, scope, "after-cancel")))
                .expect("publish post-cancel write");
        },
    );
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
