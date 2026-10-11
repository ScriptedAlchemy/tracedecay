//! Shared helpers for `tracedecay serve` stdio integration tests
//! (`mcp_cli_serve_test`, `serve_template_path_test`,
//! `serve_tool_surface_test`): project fixtures, serve process spawners, a
//! live stdio session, and output parsers.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_project::project::TraceDecayOpenOptions;
#[cfg(unix)]
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

use crate::common::{TestChildProcess, canonical_existing_path, tracedecay_command_with_home};

const SERVE_CHILD_TIMEOUT: Duration = Duration::from_secs(20);
/// How long [`ServeSession::await_project_ready`] keeps asking. Each warming
/// answer already spent serve's own project-open grace, so this spans a few
/// readiness reads, not a poll interval.
const PROJECT_READY_BUDGET: Duration = Duration::from_secs(90);

pub fn profile_root(home: &Path) -> PathBuf {
    canonical_existing_path(home).join(".tracedecay")
}

pub async fn init_project_with_file(home: &Path, contents: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), contents).unwrap();
    init_project_direct(home, dir.path()).await;
    dir
}

#[cfg(unix)]
pub async fn init_project_under(home: &Path, parent: &Path, name: &str, contents: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), contents).unwrap();
    init_project_direct(home, &path).await;
    path
}

pub async fn init_project_direct(home: &Path, project: &Path) {
    let profile_root = profile_root(home);
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    crate::fixture::init_project_from_template_with_options(project, open_options)
        .await
        .expect("tracedecay project should initialize");
}

#[cfg(unix)]
pub async fn register_global_project(home: &Path, project: &Path) {
    use std::hash::{Hash, Hasher};

    let home = canonical_existing_path(home);
    let runtime = HostAdmissionTestRuntimeV1::profile(home.join(".tracedecay"))
        .await
        .unwrap();
    let canonical = RegisteredGlobalDb::canonical_project_key(project);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_id = format!("test_{:016x}", hasher.finish());
    runtime
        .upsert_code_project(&project_id, project, None, None, None)
        .await
        .expect("register checked test project identity");
    runtime.upsert(project, 0).await;
    runtime.checkpoint_profile_database_for_test().await;
}

/// A `tracedecay serve` command run from `cwd`, with `--path` when given.
pub fn serve_command(home: &Path, cwd: &Path, path_arg: Option<&OsStr>) -> Command {
    let mut command = tracedecay_command_with_home(home);
    command.arg("serve").current_dir(cwd);
    if let Some(path) = path_arg {
        command.arg("--path").arg(path);
    }
    command
}

/// Runs `command` as a serve session: `initialize` (id 1) with
/// `initialize_params`, a wait for the routed project through
/// [`ServeSession::await_project_ready`], then `requests`. Returns the
/// process output, the whole stdout included, once stdin closes.
pub fn run_serve_after_ready(
    command: Command,
    initialize_params: Value,
    requests: &[Value],
) -> Output {
    let mut session = ServeSession::start(command);
    session.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": initialize_params
    }));
    session.await_project_ready();
    for request in requests {
        session.send(request);
    }
    session.finish(SERVE_CHILD_TIMEOUT)
}

/// [`run_serve_after_ready`] from `cwd` (optionally with `--path`) with one
/// `tracedecay_runtime` tools/call (id 2).
#[cfg(unix)]
pub fn run_serve_runtime(
    home: &Path,
    cwd: &Path,
    path_arg: Option<&OsStr>,
    initialize_params: Value,
) -> Output {
    run_serve_after_ready(
        serve_command(home, cwd, path_arg),
        initialize_params,
        &[json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_runtime",
                "arguments": { "format": "json" }
            }
        })],
    )
}

#[cfg(unix)]
#[tokio::test]
async fn readiness_reissues_the_owner_wait_after_a_warming_refusal() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn warming_marker() {}\n").await;
    let hold = home.path().join("hold-project-sessions");
    fs::write(&hold, b"hold").unwrap();
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    let _daemon = crate::common::spawn_tracedecay_daemon_with(home.path(), |command| {
        command.env("TRACEDECAY_TEST_HOLD_AFTER_PROJECT_SESSIONS", &hold);
    });
    let release = std::thread::spawn(move || {
        crate::common::poll_until(
            Instant::now() + Duration::from_secs(30),
            Duration::from_millis(20),
            || entered.is_file().then_some(()),
            || "the project open never reached the session-store hold".to_owned(),
        );
        // The real serve grace is 15 seconds. Keep the owner mounted but
        // warming past that boundary, then release the same pending open.
        std::thread::sleep(Duration::from_secs(17));
        fs::remove_file(hold).unwrap();
    });
    let output = run_serve_runtime(home.path(), project.path(), None, json!({}));
    release.join().unwrap();
    assert!(output.status.success(), "{output:?}");
    let frames: Vec<Value> = String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        frames.iter().any(|frame| {
            frame["id"] == "project-ready"
                && frame["error"]["data"]["reason_code"] == "project_warming"
                && frame["error"]["data"]["retryable"] == true
        }),
        "the held open must actually exercise the typed retry: {frames:?}"
    );
    assert_eq!(
        runtime_project_root(&output.stdout, 2),
        canonical_existing_path(project.path()).to_string_lossy(),
        "the call after readiness must use the same held project's actual runtime"
    );
}

/// One live `tracedecay serve` session driven as an MCP stdio host drives
/// it: write one JSON-RPC line, then read lines until the expected response
/// or notification arrives. Every frame read stays in the transcript that
/// [`ServeSession::finish`] returns as stdout.
pub struct ServeSession {
    process: TestChildProcess,
    stdout: BufReader<ChildStdout>,
    transcript: Vec<u8>,
}

impl ServeSession {
    pub fn start(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tracedecay serve should start");
        let stdout = BufReader::new(child.stdout.take().expect("stdout should be piped"));
        Self {
            process: TestChildProcess::new(child),
            stdout,
            transcript: Vec::new(),
        }
    }

    pub fn send(&mut self, request: &Value) {
        let stdin = self
            .process
            .stdin_mut()
            .expect("stdin should stay open for the session");
        writeln!(stdin, "{request}").expect("write MCP request");
        stdin.flush().expect("flush MCP request");
    }

    pub fn read_message(&mut self) -> Value {
        loop {
            let mut line = String::new();
            match self.stdout.read_line(&mut line) {
                Ok(0) => panic!("serve closed stdout before answering the next MCP frame"),
                Ok(_) => {
                    self.transcript.extend_from_slice(line.as_bytes());
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    return serde_json::from_str(line)
                        .unwrap_or_else(|error| panic!("invalid MCP frame {line:?}: {error}"));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("failed to read MCP frame: {error}"),
            }
        }
    }

    pub fn wait_until(&mut self, mut done: impl FnMut(&[Value]) -> bool) -> Vec<Value> {
        let mut frames = Vec::new();
        while !done(&frames) {
            frames.push(self.read_message());
        }
        frames
    }

    pub fn request(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.send(&request);
        let frames = self.wait_until(|frames| frames.iter().any(|frame| frame["id"] == id));
        frames
            .into_iter()
            .find(|frame| frame["id"] == id)
            .expect("response id")
    }

    /// Holds the session until its routed project answers a
    /// `tracedecay_status` `wait_for` read at `graph_ready`, the readiness
    /// authority hosts wait on. A project still opening when serve's own
    /// open grace lapses answers with the typed retryable `project_warming`
    /// refusal, which names this same read as the way to wait, so only that
    /// refusal reissues it. Any other answer must be a reached wait.
    pub fn await_project_ready(&mut self) {
        let deadline = Instant::now() + PROJECT_READY_BUDGET;
        let status = loop {
            let status = self.request(json!({
                "jsonrpc": "2.0",
                "id": "project-ready",
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_status",
                    "arguments": {
                        "format": "json",
                        "wait_for": { "state": "graph_ready", "timeout_ms": 20_000 }
                    }
                }
            }));
            if status["error"]["data"]["reason_code"] != "project_warming"
                || Instant::now() >= deadline
            {
                break status;
            }
        };
        let text = status["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("project readiness read failed: {status}"));
        let readiness: Value = serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("status JSON was not an object: {error}; body={text}"));
        assert_eq!(
            readiness["wait"],
            json!({ "outcome": "reached" }),
            "the routed project never became graph_ready: {readiness}"
        );
    }

    /// Closes stdin and returns the process output once serve exits within
    /// `timeout`, with every frame this session read ahead of the rest of
    /// stdout.
    pub fn finish(self, timeout: Duration) -> Output {
        let Self {
            mut process,
            mut stdout,
            mut transcript,
        } = self;
        let reader =
            std::thread::spawn(move || stdout.read_to_end(&mut transcript).map(|_| transcript));
        let mut output = process
            .wait_with_output(timeout)
            .expect("tracedecay serve should exit after stdin closes");
        output.stdout = reader
            .join()
            .expect("stdout reader thread should not panic")
            .expect("serve stdout should drain");
        output
    }
}

/// The request lines [`assert_unenrolled_cwd_serve_session`] checks:
/// `initialize` (id 1), a project-bound `tools/call` (id 2), `tools/list`
/// (id 3), in the order a host issues them.
#[cfg(unix)]
pub fn unenrolled_cwd_serve_requests() -> [Value; 3] {
    [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "tracedecay_runtime", "arguments": { "format": "json" } }
        }),
    ]
}

/// Spawns `tracedecay serve` from `cwd` (optionally with `--path`), writes
/// each JSON-RPC `request` as one stdio line, and returns the process output
/// once stdin closes. Stdin writes ignore broken pipes so failure-path tests
/// can assert on the output instead of panicking when serve exits early.
pub fn run_serve_requests(
    home: &Path,
    cwd: &Path,
    path_arg: Option<&OsStr>,
    requests: &[Value],
) -> Output {
    let mut child = TestChildProcess::new(
        serve_command(home, cwd, path_arg)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tracedecay serve should start"),
    );

    {
        let stdin = child.stdin_mut().expect("stdin should be piped");
        for request in requests {
            let _ = writeln!(stdin, "{request}");
        }
    }

    child
        .wait_with_output(SERVE_CHILD_TIMEOUT)
        .expect("tracedecay serve should exit after stdin closes")
}

/// The JSON-RPC response line with the given `id`, or a panic naming the
/// full stdout so a missing response is diagnosable.
#[cfg(unix)]
pub fn json_rpc_response(stdout: &[u8], id: i64) -> Value {
    let stdout = String::from_utf8_lossy(stdout);
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response.get("id") == Some(&json!(id)))
        .unwrap_or_else(|| panic!("missing response id {id} in stdout:\n{stdout}"))
}

#[cfg(unix)]
pub fn canonical_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Asserts one `serve` session against a daemon whose handshake route is
/// `cwd`, a directory the profile never enrolled: `initialize` (id 1) and
/// `tools/list` (id 3) answer normally, and the `tools/call` (id 2) is the
/// typed `project_not_enrolled` refusal naming `cwd` and the repair. A
/// dropped daemon connection, a transport-flavoured error, or manufactured
/// project state under `cwd` all fail here.
#[cfg(unix)]
pub fn assert_unenrolled_cwd_serve_session(output: &Output, cwd: &Path) {
    assert!(
        output.status.success(),
        "tracedecay serve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let initialize = json_rpc_response(&output.stdout, 1);
    assert!(
        initialize.get("error").is_none(),
        "initialize must succeed for an unenrolled cwd: {initialize}"
    );
    assert_eq!(initialize["result"]["serverInfo"]["name"], "tracedecay");

    let tools = json_rpc_response(&output.stdout, 3);
    assert!(
        tools["result"]["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["name"] == "tracedecay_runtime")
        }),
        "tools/list must advertise the catalog, including the tool called next, \
         before a project exists: {tools}"
    );

    let refusal = json_rpc_response(&output.stdout, 2);
    let error = &refusal["error"];
    assert_eq!(
        error["data"]["reason_code"], "project_not_enrolled",
        "tools/call must answer with the typed not-enrolled state: {refusal}"
    );
    assert_eq!(error["data"]["retryable"], false);
    let message = error["message"].as_str().unwrap_or_default();
    let cwd = canonical_existing_path(cwd);
    assert!(
        message.contains(&cwd.to_string_lossy().into_owned())
            && message.contains("tracedecay init"),
        "the refusal must name the discovered directory and the repair: {refusal}"
    );
    assert!(
        !message.contains("daemon closed the connection"),
        "a missing project must not surface as a transport failure: {refusal}"
    );
    assert!(
        !cwd.join(".tracedecay").exists(),
        "an unenrolled serve session must not manufacture project state"
    );
}

/// Extracts `database.project_root` from the `tracedecay_runtime` tools/call
/// response with the given JSON-RPC id.
#[cfg(unix)]
pub fn runtime_project_root(stdout: &[u8], id: i64) -> String {
    let stdout = String::from_utf8(stdout.to_vec()).unwrap();
    let runtime_response: Value = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response.get("id") == Some(&json!(id)))
        .unwrap_or_else(|| panic!("missing runtime response in stdout:\n{stdout}"));
    let text = runtime_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("runtime tool should return text content: {runtime_response}"));
    let runtime: Value = serde_json::from_str(text).unwrap();
    runtime["database"]["project_root"]
        .as_str()
        .expect("runtime should include database.project_root")
        .to_string()
}
