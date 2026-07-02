use crate::common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use crate::common::{canonical_existing_path, tracedecay_command_with_home};
#[cfg(unix)]
use crate::serve_harness::runtime_project_root;
#[cfg(unix)]
use crate::serve_harness::{canonical_path_string, run_serve_runtime};
use crate::serve_harness::{
    init_project_under, init_project_with_file, profile_root, register_global_project,
};
use libsql::Builder;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
#[cfg(unix)]
use tokio::sync::Mutex;
use tracedecay::automation::managed_skills::{
    approve_managed_skill, create_managed_skill_draft, ManagedSkillDraft, ManagedSkillProvenance,
    ManagedSkillSource, ManagedSupportFile,
};
use tracedecay::automation::run_ledger::{
    append_run_record, write_run_artifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger,
};
use tracedecay::db::Database;
use tracedecay::mcp::handle_tool_call;
use tracedecay::serve;
use tracedecay::storage::{
    default_profile_sharded_layout, write_enrollment_marker, EnrollmentMarker, StorageMode,
};
use tracedecay::tracedecay::TraceDecay;

#[cfg(unix)]
static READ_ONLY_SERVE_ENV_LOCK: Mutex<()> = Mutex::const_new(());

fn json_rpc_tool_payload(stdout: &[u8], id: i64) -> Value {
    let stdout_text = String::from_utf8(stdout.to_vec()).unwrap();
    let response: Value = stdout_text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response.get("id") == Some(&json!(id)))
        .unwrap_or_else(|| panic!("missing JSON-RPC response {id} in stdout:\n{stdout_text}"));
    assert!(
        response.get("error").is_none(),
        "JSON-RPC response {id} should not be an error: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .expect("tool result should include content items");
    for item in content {
        let Some(text) = item["text"].as_str() else {
            continue;
        };
        let Some(json_start) = text.find('{').or_else(|| text.find('[')) else {
            continue;
        };
        if let Ok(payload) = serde_json::from_str(&text[json_start..]) {
            return payload;
        }
    }
    panic!("tool response {id} should include a JSON payload:\n{response}")
}

fn managed_skill_stdio_draft(id: &str, title: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: title.to_string(),
        summary: format!("{title} summary."),
        category: "maintenance".to_string(),
        targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
        body_markdown: format!("Use {title} before applying repository changes."),
        support_files: vec![ManagedSupportFile::new(
            "references/checklist.md",
            b"- inspect context\n- run focused tests\n".to_vec(),
        )
        .unwrap()],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay-stdio-test".to_string(),
            run_id: Some("run_mcp_stdio_skill".to_string()),
        },
    }
}

#[cfg(unix)]
fn run_serve_runtime_with_initialize_root(
    home: &Path,
    cwd: &Path,
    explicit_path: Option<&Path>,
    root_uri: String,
    root_name: &str,
) -> Output {
    let output = run_serve_runtime(
        home,
        cwd,
        explicit_path.map(Path::as_os_str),
        json!({
            "roots": [{
                "uri": root_uri,
                "name": root_name
            }]
        }),
    );
    assert!(
        output.status.success(),
        "tracedecay serve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
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

#[tokio::test]
async fn serve_stdio_smokes_managed_skill_list_and_view() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn skill_stdio_marker() {}\n").await;
    let profile_root = profile_root(home.path());

    create_managed_skill_draft(
        &profile_root,
        managed_skill_stdio_draft("pending-stdio-skill", "Pending stdio skill"),
    )
    .await
    .unwrap();
    create_managed_skill_draft(
        &profile_root,
        managed_skill_stdio_draft("active-stdio-skill", "Active stdio skill"),
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "active-stdio-skill")
        .await
        .unwrap();

    let mut child = tracedecay_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(project.path())
        .env_remove("TRACEDECAY_DAEMON_SOCKET")
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
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_skill_list",
                    "arguments": { "state": "active" }
                }
            })
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_skill_view",
                    "arguments": {
                        "id": "active-stdio-skill",
                        "include_support_files": false
                    }
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
        "serve skill stdio smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let list = json_rpc_tool_payload(&output.stdout, 2);
    assert_eq!(list["status"], "ok");
    assert_eq!(list["count"], 1);
    assert_eq!(list["skills"][0]["metadata"]["id"], "active-stdio-skill");
    assert_eq!(list["skills"][0]["metadata"]["state"], "active");
    assert_eq!(list["skills"][0]["support_file_count"], 1);
    assert!(list["skills"][0].get("body_markdown").is_none());

    let view = json_rpc_tool_payload(&output.stdout, 3);
    assert_eq!(view["status"], "ok");
    assert_eq!(view["skill"]["metadata"]["id"], "active-stdio-skill");
    assert!(view["skill"]["body_markdown"]
        .as_str()
        .unwrap()
        .contains("Active stdio skill"));
    assert_eq!(view["skill"]["support_files"].as_array().unwrap().len(), 0);
    assert_eq!(view["support_files_included"], false);
}

#[tokio::test]
async fn serve_stdio_smokes_automation_run_artifact_view() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn artifact_stdio_marker() {}\n").await;
    let dashboard_root = default_profile_sharded_layout(project.path(), &profile_root(home.path()))
        .unwrap()
        .dashboard_root;
    let run_id = "run-stdio-artifact";
    let artifact = write_run_artifact(
        &dashboard_root,
        run_id,
        AutomationRunArtifactKind::CodexHandoff,
        &json!({
            "status": "ready_for_review",
            "next_actions": ["inspect stdio artifact payload"],
        }),
        Some("stdio handoff ready".to_string()),
        "1782283200",
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: run_id.to_string(),
            trigger: AutomationTrigger::Dashboard,
            task: tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
            task_key: Some("memory_curator".to_string()),
            backend: "codex_app_server".to_string(),
            host_mode: Some("standalone".to_string()),
            prompt_version: Some("memory_curator:v1".to_string()),
            response_schema: None,
            strict_json: Some(true),
            model: Some("test-model".to_string()),
            status: AutomationRunStatus::Succeeded,
            evidence_hash: Some("sha256:evidence".to_string()),
            input_hash: Some("sha256:input".to_string()),
            output_hash: Some("sha256:output".to_string()),
            proposed_ops: Some(json!({"ops": []})),
            applied_ops: None,
            rejected_ops: None,
            validation_report: None,
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 0,
            error: None,
            error_classification: None,
            error_retryable: None,
            fallback_status: None,
            report_ref: None,
            artifacts: vec![artifact],
            started_at: "1782283199".to_string(),
            completed_at: "1782283200".to_string(),
        },
    )
    .await
    .unwrap();

    let mut child = tracedecay_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(project.path())
        .env_remove("TRACEDECAY_DAEMON_SOCKET")
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
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_automation_run_artifact_view",
                    "arguments": {
                        "run_id": run_id,
                        "kind": "codex_handoff"
                    }
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
        "serve artifact stdio smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let payload = json_rpc_tool_payload(&output.stdout, 2);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["artifact"]["kind"], "codex_handoff");
    assert_eq!(payload["payload"]["status"], "ready_for_review");
    assert_eq!(
        payload["payload"]["next_actions"][0],
        "inspect stdio artifact payload"
    );
}

/// Regression test for the serve/daemon-restart race: a serve process started
/// while `tracedecay update` is restarting the daemon sees no socket file, but
/// must not silently commit to in-process mode when an installed service is
/// about to rebind the socket.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn serve_started_during_daemon_restart_window_proxies_to_restarted_daemon() {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn restart_window_marker() {}\n").await;
    let socket_path = common::daemon_socket_path(home.path());
    fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

    // An installed service unit claims the socket, but the socket file is
    // missing — exactly the window between daemon shutdown and rebind.
    let unit_dir = canonical_existing_path(home.path()).join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).unwrap();
    fs::write(
        unit_dir.join("tracedecay.service"),
        format!(
            "[Service]\nExecStart=/opt/tracedecay/bin/tracedecay daemon run --socket {}\n",
            socket_path.display()
        ),
    )
    .unwrap();

    // The "restarted daemon" binds the socket only after serve has started.
    // It answers `initialize` with a sentinel server name and a skewed
    // version, so a proxied response is distinguishable from the in-process
    // fallback and exercises the client-side version-skew warning.
    let listener_path = socket_path.clone();
    let fake_daemon = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let listener = UnixListener::bind(&listener_path).expect("bind restarted daemon socket");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fake daemon listener");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for serve to proxy a request to the restarted daemon"
            );
            let mut stream = match listener.accept() {
                Ok((stream, _addr)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("accept serve connection: {e}"),
            };
            stream
                .set_nonblocking(false)
                .expect("blocking fake daemon stream");
            let mut reader = BufReader::new(stream.try_clone().expect("clone fake daemon stream"));
            let mut handshake_line = String::new();
            if reader
                .read_line(&mut handshake_line)
                .expect("read handshake")
                == 0
            {
                // The transport probe connects and hangs up without a handshake.
                continue;
            }
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).expect("read request") == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(request_line.trim()).expect("request json");
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "sentinel-restarted-daemon",
                        "version": "0.0.1-sentinel"
                    }
                }
            });
            writeln!(stream, "{response}").expect("write fake daemon response");
            // Drain until the proxy hangs up so the response is not lost to a
            // connection reset.
            let mut scratch = [0_u8; 64];
            while matches!(reader.read(&mut scratch), Ok(n) if n > 0) {}
            return;
        }
    });

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
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("tracedecay serve should exit after stdin closes");
    fake_daemon.join().expect("fake daemon thread should exit");

    assert!(
        output.status.success(),
        "serve should ride out the daemon restart window\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let response: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout should contain one JSON-RPC response: {err}\n{stdout}")
    });
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        json!("sentinel-restarted-daemon"),
        "initialize must be answered by the restarted daemon, not an in-process fallback:\n{response}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("0.0.1-sentinel") && stderr.contains("tracedecay daemon restart"),
        "proxy should warn about the daemon/client version skew\nstderr:\n{stderr}"
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
