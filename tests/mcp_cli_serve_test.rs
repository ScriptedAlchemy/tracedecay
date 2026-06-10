use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::tokensave::TokenSave;

async fn init_project_with_file(contents: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), contents).unwrap();
    let cg = TokenSave::init(dir.path()).await.unwrap();
    cg.index_all().await.unwrap();
    dir
}

async fn init_project_under(parent: &Path, name: &str, contents: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), contents).unwrap();
    let cg = TokenSave::init(&path).await.unwrap();
    cg.index_all().await.unwrap();
    path
}

async fn register_global_project(home: &Path, project: &Path) {
    let db_path = home.join(".tokensave/global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(project, 0).await;
    db.checkpoint().await;
}

fn tokensave_command_with_home(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tokensave"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TOKENSAVE_GLOBAL_DB", home.join(".tokensave/global.db"));
    command
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
fn file_uri_localhost_percent_encoded(path: &Path) -> String {
    let encoded = path.to_string_lossy().replace(' ', "%20");
    format!("file://localhost{encoded}")
}

#[tokio::test]
async fn explicit_uninitialized_path_reports_error_instead_of_global_fallback() {
    let home = TempDir::new().unwrap();
    let explicit = TempDir::new().unwrap();
    let active = init_project_with_file("pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), active.path()).await;

    let output = tokensave_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(explicit.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tokensave serve should run");

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
async fn no_explicit_path_prefers_initialize_roots_over_global_fallback() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let stale = init_project_with_file("pub fn stale_project_marker() {}\n").await;
    let active = init_project_with_file("pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), stale.path()).await;

    let mut child = tokensave_command_with_home(home.path())
        .arg("serve")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tokensave serve should start");

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
                        "uri": format!("file://{}", active.path().display()),
                        "name": "active"
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
                    "name": "tokensave_runtime",
                    "arguments": {}
                }
            })
        )
        .unwrap();
    }

    let output = child
        .wait_with_output()
        .expect("tokensave serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tokensave serve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        runtime_project_root(&output.stdout, 2),
        active.path().to_str().unwrap(),
        "serve should prefer MCP initialize roots over stale global DB fallback"
    );
}

#[tokio::test]
async fn no_explicit_path_prefers_discovered_cwd_over_initialize_roots() {
    let home = TempDir::new().unwrap();
    let cwd_project = init_project_with_file("pub fn cwd_project_marker() {}\n").await;
    let nested_cwd = cwd_project.path().join("src");
    let active = init_project_with_file("pub fn active_project_marker() {}\n").await;

    let mut child = tokensave_command_with_home(home.path())
        .arg("serve")
        .current_dir(&nested_cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tokensave serve should start");

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
                        "uri": format!("file://{}", active.path().display()),
                        "name": "active"
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
                    "name": "tokensave_runtime",
                    "arguments": {}
                }
            })
        )
        .unwrap();
    }

    let output = child
        .wait_with_output()
        .expect("tokensave serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tokensave serve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        runtime_project_root(&output.stdout, 2),
        cwd_project.path().to_str().unwrap(),
        "discovered cwd project should be preferred over MCP initialize roots"
    );
}

#[tokio::test]
async fn explicit_initialized_path_ignores_initialize_roots() {
    let home = TempDir::new().unwrap();
    let explicit = init_project_with_file("pub fn explicit_project_marker() {}\n").await;
    let active = init_project_with_file("pub fn active_project_marker() {}\n").await;

    let mut child = tokensave_command_with_home(home.path())
        .arg("serve")
        .arg("--path")
        .arg(explicit.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tokensave serve should start");

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
                        "uri": format!("file://{}", active.path().display()),
                        "name": "active"
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
                    "name": "tokensave_runtime",
                    "arguments": {}
                }
            })
        )
        .unwrap();
    }

    let output = child
        .wait_with_output()
        .expect("tokensave serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tokensave serve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        runtime_project_root(&output.stdout, 2),
        explicit.path().to_str().unwrap(),
        "explicit --path should be authoritative over MCP initialize roots"
    );
}

#[tokio::test]
async fn no_explicit_path_without_roots_still_uses_global_fallback() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let active = init_project_with_file("pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), active.path()).await;

    let output = tokensave_command_with_home(home.path())
        .arg("serve")
        .current_dir(cwd.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tokensave serve should run");

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
        projects.path(),
        "stale-project",
        "pub fn stale_project_marker() {}\n",
    )
    .await;
    let active = init_project_under(
        projects.path(),
        "active project",
        "pub fn active_project_marker() {}\n",
    )
    .await;
    register_global_project(home.path(), &stale).await;
    register_global_project(home.path(), &active).await;

    let mut child = tokensave_command_with_home(home.path())
        .arg("serve")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tokensave serve should start");

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
                        "uri": file_uri_localhost_percent_encoded(&active),
                        "name": "active"
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
                    "name": "tokensave_runtime",
                    "arguments": {}
                }
            })
        )
        .unwrap();
    }

    let output = child
        .wait_with_output()
        .expect("tokensave serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tokensave serve should accept encoded file://localhost MCP roots\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        runtime_project_root(&output.stdout, 2),
        active.to_str().unwrap(),
        "serve should use the decoded MCP root project"
    );
}

#[tokio::test]
async fn same_depth_descendant_global_fallback_is_ambiguous() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let alpha = init_project_under(cwd.path(), "alpha", "pub fn alpha_marker() {}\n").await;
    let beta = init_project_under(cwd.path(), "beta", "pub fn beta_marker() {}\n").await;
    register_global_project(home.path(), &alpha).await;
    register_global_project(home.path(), &beta).await;

    let output = tokensave_command_with_home(home.path())
        .arg("serve")
        .current_dir(cwd.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tokensave serve should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "ambiguous same-depth descendants should not select an arbitrary project\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        stderr.contains("Multiple tokensave projects found"),
        "stderr should explain the ambiguity:\n{stderr}"
    );
    assert!(
        !stderr.contains("no projects registered in the global database"),
        "stderr should not contradict ambiguity with a no-projects error:\n{stderr}"
    );
}
