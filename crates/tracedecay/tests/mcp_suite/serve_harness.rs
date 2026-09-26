//! Shared helpers for `tracedecay serve` stdio integration tests
//! (`mcp_cli_serve_test`, `serve_template_path_test`): project fixtures,
//! one-shot serve process spawners, and output parsers.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_project::project::TraceDecayOpenOptions;
#[cfg(unix)]
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

use crate::common::{TestChildProcess, canonical_existing_path, tracedecay_command_with_home};

const SERVE_CHILD_TIMEOUT: Duration = Duration::from_secs(20);

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
    let canonical = HostAdmissionTestRuntimeV1::canonical_project_key(project);
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
    run_serve_requests(
        home,
        cwd,
        path_arg,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": initialize_params
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_runtime",
                    "arguments": { "format": "json" }
                }
            }),
        ],
    )
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
/// once stdin closes.
pub fn run_serve_requests(
    home: &Path,
    cwd: &Path,
    path_arg: Option<&OsStr>,
    requests: &[Value],
) -> Output {
    let mut command = tracedecay_command_with_home(home);
    command.arg("serve");
    if let Some(path) = path_arg {
        command.arg("--path").arg(path);
    }

    let mut child = TestChildProcess::new(
        command
            .current_dir(cwd)
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
        .expect("runtime tool should return text content");
    let runtime: Value = serde_json::from_str(text).unwrap();
    runtime["database"]["project_root"]
        .as_str()
        .expect("runtime should include database.project_root")
        .to_string()
}
