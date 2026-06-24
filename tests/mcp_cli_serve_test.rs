mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use common::{canonical_existing_path, tracedecay_command_with_home};
use libsql::Builder;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
#[cfg(unix)]
use tokio::sync::Mutex;
use tracedecay::db::Database;
use tracedecay::global_db::GlobalDb;
use tracedecay::mcp::handle_tool_call;
use tracedecay::serve;
use tracedecay::storage::{write_enrollment_marker, EnrollmentMarker, StorageMode};
use tracedecay::tracedecay::TraceDecay;

#[cfg(unix)]
static READ_ONLY_SERVE_ENV_LOCK: Mutex<()> = Mutex::const_new(());

async fn init_project_with_file(home: &Path, contents: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), contents).unwrap();
    init_project_with_cli(home, dir.path());
    dir
}

async fn init_project_under(home: &Path, parent: &Path, name: &str, contents: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), contents).unwrap();
    init_project_with_cli(home, &path);
    path
}

fn init_project_with_cli(home: &Path, project: &Path) {
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
        "tracedecay init failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn register_global_project(home: &Path, project: &Path) {
    let home = canonical_existing_path(home);
    let db_path = home.join(".tracedecay/global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(project, 0).await;
    db.checkpoint().await;
}

fn runtime_project_root(stdout: &[u8], id: i64) -> String {
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

#[cfg(unix)]
fn run_serve_runtime_with_initialize_root(
    home: &Path,
    cwd: &Path,
    explicit_path: Option<&Path>,
    root_uri: String,
    root_name: &str,
) -> Output {
    let mut command = tracedecay_command_with_home(home);
    command.arg("serve");
    if let Some(path) = explicit_path {
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
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "roots": [{
                        "uri": root_uri,
                        "name": root_name
                    }]
                }
            })
        )
        .unwrap();
        writeln!(
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
        )
        .unwrap();
    }

    let output = child
        .wait_with_output()
        .expect("tracedecay serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tracedecay serve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn canonical_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn profile_root(home: &Path) -> PathBuf {
    canonical_existing_path(home).join(".tracedecay")
}

async fn set_user_version(db_path: &Path, version: u32) {
    let db = Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(&format!("PRAGMA user_version = {version}"), ())
        .await
        .unwrap();
}

#[cfg(unix)]
async fn drop_memory_facts(db_path: &Path) {
    let mut permissions = fs::metadata(db_path).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(db_path, permissions).unwrap();

    let db = Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute("DROP TABLE memory_facts", ()).await.unwrap();

    let mut permissions = fs::metadata(db_path).unwrap().permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(db_path, permissions).unwrap();
}

fn extract_tool_text(value: &Value) -> &str {
    value["content"][0]["text"]
        .as_str()
        .expect("tool result should include text content")
}

#[cfg(unix)]
async fn create_read_only_project_db(
    home: &Path,
    project: &Path,
    project_id: &str,
    user_version: Option<u32>,
) -> (PathBuf, PathBuf) {
    let project_root = canonical_existing_path(project);
    let profile_root = profile_root(home);
    let data_root = profile_root.join(format!("projects/{project_id}"));
    let db_path = data_root.join("tracedecay.db");

    unsafe {
        std::env::set_var("HOME", canonical_existing_path(home));
        std::env::set_var("USERPROFILE", canonical_existing_path(home));
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("TRACEDECAY_DATA_DIR", &profile_root);
        std::env::set_var("TRACEDECAY_GLOBAL_DB", profile_root.join("global.db"));
    }

    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let (db, _) = Database::initialize(&db_path).await.unwrap();
    db.checkpoint().await.unwrap();
    db.close();
    if let Some(version) = user_version {
        set_user_version(&db_path, version).await;
    }

    let mut permissions = fs::metadata(&db_path).unwrap().permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(&db_path, permissions).unwrap();

    (project_root, db_path)
}

#[cfg(unix)]
fn file_uri_localhost_percent_encoded(path: &Path) -> String {
    let encoded = path.to_string_lossy().replace(' ', "%20");
    format!("file://localhost{encoded}")
}

/// Builds a portable `file://` URI: `/tmp/x` → `file:///tmp/x` on Unix,
/// `C:\Users\x` → `file:///C:/Users/x` on Windows.
fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

#[tokio::test]
async fn explicit_uninitialized_path_reports_error_instead_of_global_fallback() {
    let home = TempDir::new().unwrap();
    let explicit = TempDir::new().unwrap();
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), active.path()).await;

    let output = tracedecay_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(explicit.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tracedecay serve should run");

    assert!(
        !output.status.success(),
        "explicit uninitialized --path should fail instead of serving a global fallback\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&explicit.path().display().to_string()),
        "error should name the explicit project path\nstderr:\n{stderr}"
    );
}

#[tokio::test]
async fn serve_without_daemon_socket_falls_back_to_in_process_mcp() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn client_only_marker() {}\n").await;

    let mut child = tracedecay_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tracedecay serve should run");
    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {}
            })
        )
        .unwrap();
    }
    let output = child
        .wait_with_output()
        .expect("tracedecay serve should exit after stdin closes");

    assert!(
        output.status.success(),
        "serve should fall back to an in-process MCP engine when the daemon socket is missing\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"protocolVersion\":\"2024-11-05\""),
        "serve fallback should answer initialize over stdio\nstdout:\n{stdout}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn serve_daemon_proxy_reports_daemon_disconnect_as_json_rpc_error() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn disconnect_marker() {}\n").await;
    let socket_dir = TempDir::new().unwrap();
    let socket_path = socket_dir.path().join("tracedecay.sock");
    let (ready_tx, ready_rx) = mpsc::channel();

    let listener_path = socket_path.clone();
    let fake_daemon = std::thread::spawn(move || {
        let listener = UnixListener::bind(&listener_path).expect("bind fake daemon socket");
        ready_tx.send(()).expect("notify fake daemon readiness");
        if let Ok((mut stream, _addr)) = listener.accept() {
            let mut scratch = [0_u8; 512];
            let _ = stream.read(&mut scratch);
        }
    });
    ready_rx.recv().expect("fake daemon should be ready");

    let mut child = tracedecay_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(project.path())
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tracedecay serve should run");
    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {}
            })
        )
        .unwrap();
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("tracedecay serve should exit after stdin closes");
    fake_daemon.join().expect("fake daemon thread should exit");

    assert!(
        output.status.success(),
        "serve should keep stdio healthy after daemon disconnect\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let response: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout should contain one JSON-RPC response: {err}\n{stdout}")
    });
    assert_eq!(response["id"], json!(1));
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("TraceDecay daemon connection failed")),
        "disconnect should be reported as a JSON-RPC error response, got:\n{response}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_initialized_rejects_read_only_db_with_pending_migrations() {
    let _env_guard = READ_ONLY_SERVE_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let (project_root, db_path) = create_read_only_project_db(
        home.path(),
        project.path(),
        "proj_serve_readonly_old_schema",
        Some(14),
    )
    .await;
    drop_memory_facts(&db_path).await;

    assert!(
        TraceDecay::open(&project_root).await.is_err(),
        "normal TraceDecay::open should fail against the read-only DB fixture"
    );

    let error = match serve::ensure_initialized(&project_root).await {
        Ok(_) => panic!("read-only fallback must reject old schemas instead of serving them"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("schema") && message.contains("migrat"),
        "error should explain that the read-only DB needs migration, got: {message}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_initialized_read_only_fallback_reports_and_guards_read_only_store() {
    let _env_guard = READ_ONLY_SERVE_ENV_LOCK.lock().await;
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "pub fn readonly_marker() {}\n",
    )
    .unwrap();
    let (project_root, _db_path) = create_read_only_project_db(
        home.path(),
        project.path(),
        "proj_serve_readonly_current_schema",
        None,
    )
    .await;

    let cg = TraceDecay::open_read_only(&project_root)
        .await
        .expect("current-schema read-only DB should open for read-only serving");

    let status = handle_tool_call(
        &cg,
        "tracedecay_storage_status",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_tool_text(&status.value)).unwrap();
    assert_eq!(payload["status"].as_str(), Some("ok"));
    assert_eq!(payload["writable"].as_bool(), Some(false));
    assert_eq!(payload["read_only"].as_bool(), Some(true));

    let error = match cg.index_all().await {
        Ok(_) => panic!("mutating operations should be guarded before SQLite rejects writes"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("read-only"),
        "write guard should report read-only state, got: {message}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn no_explicit_path_prefers_initialize_roots_over_global_fallback() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let stale = init_project_with_file(home.path(), "pub fn stale_project_marker() {}\n").await;
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), stale.path()).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let output = run_serve_runtime_with_initialize_root(
        home.path(),
        cwd.path(),
        None,
        file_uri(active.path()),
        "active",
    );

    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
        canonical_path_string(active.path()),
        "serve should prefer MCP initialize roots over stale global DB fallback"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn no_explicit_path_prefers_discovered_cwd_over_initialize_roots() {
    let home = TempDir::new().unwrap();
    let cwd_project = init_project_with_file(home.path(), "pub fn cwd_project_marker() {}\n").await;
    let nested_cwd = cwd_project.path().join("src");
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let output = run_serve_runtime_with_initialize_root(
        home.path(),
        &nested_cwd,
        None,
        file_uri(active.path()),
        "active",
    );

    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
        canonical_path_string(cwd_project.path()),
        "discovered cwd project should be preferred over MCP initialize roots"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_initialized_path_ignores_initialize_roots() {
    let home = TempDir::new().unwrap();
    let explicit =
        init_project_with_file(home.path(), "pub fn explicit_project_marker() {}\n").await;
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let output = run_serve_runtime_with_initialize_root(
        home.path(),
        explicit.path(),
        Some(explicit.path()),
        file_uri(active.path()),
        "active",
    );

    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
        canonical_path_string(explicit.path()),
        "explicit --path should be authoritative over MCP initialize roots"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn no_explicit_path_without_roots_still_uses_global_fallback() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), active.path()).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let output = tracedecay_command_with_home(home.path())
        .arg("serve")
        .current_dir(cwd.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tracedecay serve should run");

    assert!(
        output.status.success(),
        "no explicit path should keep global DB fallback when MCP roots are unavailable\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn initialize_roots_decode_file_uri_localhost_and_percent_escapes() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let projects = TempDir::new().unwrap();
    let stale = init_project_under(
        home.path(),
        projects.path(),
        "stale-project",
        "pub fn stale_project_marker() {}\n",
    )
    .await;
    let active = init_project_under(
        home.path(),
        projects.path(),
        "active project",
        "pub fn active_project_marker() {}\n",
    )
    .await;
    register_global_project(home.path(), &stale).await;
    register_global_project(home.path(), &active).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let output = run_serve_runtime_with_initialize_root(
        home.path(),
        cwd.path(),
        None,
        file_uri_localhost_percent_encoded(&active),
        "active",
    );
    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
        canonical_path_string(&active),
        "serve should use the decoded MCP root project"
    );
}

#[tokio::test]
async fn same_depth_descendant_global_fallback_is_ambiguous() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let alpha = init_project_under(
        home.path(),
        cwd.path(),
        "alpha",
        "pub fn alpha_marker() {}\n",
    )
    .await;
    let beta =
        init_project_under(home.path(), cwd.path(), "beta", "pub fn beta_marker() {}\n").await;
    register_global_project(home.path(), &alpha).await;
    register_global_project(home.path(), &beta).await;

    let output = tracedecay_command_with_home(home.path())
        .arg("serve")
        .current_dir(cwd.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tracedecay serve should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "ambiguous same-depth descendants should not select an arbitrary project\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        stderr.contains("Multiple tracedecay projects found"),
        "stderr should explain the ambiguity:\n{stderr}"
    );
    assert!(
        !stderr.contains("no projects registered in the global database"),
        "stderr should not contradict ambiguity with a no-projects error:\n{stderr}"
    );
}
