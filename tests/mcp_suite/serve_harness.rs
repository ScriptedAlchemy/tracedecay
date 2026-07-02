//! Shared helpers for `tracedecay serve` stdio integration tests
//! (`mcp_cli_serve_test`, `serve_template_path_test`,
//! `serve_degraded_mode_test`): project fixtures, serve process spawners
//! (one-shot and interactive), and output parsers.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, ExitStatus, Output, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::tracedecay::TraceDecayOpenOptions;

use crate::common::{canonical_existing_path, tracedecay_command_with_home};

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

pub async fn register_global_project(home: &Path, project: &Path) {
    let home = canonical_existing_path(home);
    let db_path = home.join(".tracedecay/global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(project, 0).await;
    db.checkpoint().await;
}

/// Spawns `tracedecay serve` from `cwd` (optionally with `--path`), drives an
/// MCP `initialize` with the given params followed by a `tracedecay_runtime`
/// tools/call (id 2) over stdio, and returns the process output once stdin
/// closes. Stdin writes ignore broken pipes so failure-path tests can assert
/// on the output instead of panicking when serve exits early.
pub fn run_serve_runtime(
    home: &Path,
    cwd: &Path,
    path_arg: Option<&OsStr>,
    initialize_params: Value,
) -> Output {
    let mut command = tracedecay_command_with_home(home);
    command.arg("serve");
    if let Some(path) = path_arg {
        command.arg("--path").arg(path);
    }

    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tracedecay serve should start");

    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        let _ = writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": initialize_params
            })
        );
        let _ = writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_runtime",
                    "arguments": { "format": "json" }
                }
            })
        );
    }

    child
        .wait_with_output()
        .expect("tracedecay serve should exit after stdin closes")
}

/// An interactive `tracedecay serve` stdio session: requests are written and
/// responses read incrementally, so tests can change on-disk state (e.g. run
/// `tracedecay init`) between calls within one server lifetime.
pub struct ServeStdioSession {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
}

impl ServeStdioSession {
    pub fn spawn(home: &Path, cwd: &Path, path_arg: Option<&OsStr>) -> Self {
        let mut command = tracedecay_command_with_home(home);
        command.arg("serve");
        if let Some(path) = path_arg {
            command.arg("--path").arg(path);
        }
        let mut child = command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tracedecay serve should start");
        let stdin = child.stdin.take().expect("stdin should be piped");
        let reader = BufReader::new(child.stdout.take().expect("stdout should be piped"));
        Self {
            child,
            stdin: Some(stdin),
            reader,
        }
    }

    /// Writes one JSON-RPC request line.
    pub fn send(&mut self, request: &Value) {
        self.send_raw(&format!("{request}\n"));
    }

    /// Writes raw bytes in a single write, so multiple newline-separated
    /// requests arrive at the server as one pipelined chunk.
    pub fn send_raw(&mut self, payload: &str) {
        let stdin = self.stdin.as_mut().expect("session stdin already closed");
        stdin.write_all(payload.as_bytes()).expect("write request");
        stdin.flush().expect("flush request");
    }

    /// Builds the standard `tracedecay_runtime` tools/call used across serve
    /// tests.
    pub fn runtime_call(id: i64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_runtime",
                "arguments": { "format": "json" }
            }
        })
    }

    /// Reads stdout lines until the JSON-RPC response with `id` appears,
    /// skipping notifications and any interleaved non-response lines.
    pub fn response_with_id(&mut self, id: i64) -> Value {
        for _ in 0..50 {
            let mut line = String::new();
            assert!(
                self.reader.read_line(&mut line).expect("read serve stdout") > 0,
                "serve stdout closed while waiting for JSON-RPC response {id}"
            );
            if let Ok(response) = serde_json::from_str::<Value>(line.trim()) {
                if response.get("id") == Some(&json!(id)) {
                    return response;
                }
            }
        }
        panic!("no JSON-RPC response {id} within 50 stdout lines");
    }

    /// Closes stdin and waits for the server to exit.
    pub fn close_and_wait(mut self) -> ExitStatus {
        drop(self.stdin.take());
        self.child
            .wait()
            .expect("serve should exit after stdin closes")
    }
}

/// Finds the JSON-RPC response with the given id in a serve stdout dump.
/// Unlike [`runtime_project_root`] this makes no assumptions about the
/// response shape, so error/degraded responses can be asserted on.
pub fn json_rpc_response(stdout: &[u8], id: i64) -> Value {
    let stdout_text = String::from_utf8(stdout.to_vec()).unwrap();
    stdout_text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response.get("id") == Some(&json!(id)))
        .unwrap_or_else(|| panic!("missing JSON-RPC response {id} in stdout:\n{stdout_text}"))
}

/// Asserts that a tools/call response is the degraded-mode error result and
/// returns its notice text.
pub fn degraded_tool_error_text(response: &Value) -> String {
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "degraded tool calls must report an error result:\n{response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("degraded tool error should carry text content")
        .to_string()
}

/// Extracts `database.project_root` from the `tracedecay_runtime` tools/call
/// response with the given JSON-RPC id.
pub fn runtime_project_root(stdout: &[u8], id: i64) -> String {
    let stdout = String::from_utf8(stdout.to_vec()).unwrap();
    let runtime_response: Value = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response.get("id") == Some(&json!(id)))
        .unwrap_or_else(|| panic!("missing runtime response in stdout:\n{stdout}"));
    let text = runtime_response["result"]["content"][0]["text"]
        .as_str()
        .expect("runtime tool should return text content");
    let runtime: Value = serde_json::from_str(text).unwrap();
    runtime["database"]["project_root"]
        .as_str()
        .expect("runtime should include database.project_root")
        .to_string()
}

pub fn canonical_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
