use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::tokensave::TokenSave;

struct ProjectFixture {
    _tmp: TempDir,
    path: PathBuf,
}

async fn init_project_named(name: &str, source: &str) -> ProjectFixture {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(name);
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), source).unwrap();

    let cg = TokenSave::init(&path).await.unwrap();
    cg.index_all().await.unwrap();

    ProjectFixture { _tmp: tmp, path }
}

async fn init_project_under(parent: &Path, name: &str, source: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), source).unwrap();

    let cg = TokenSave::init(&path).await.unwrap();
    cg.index_all().await.unwrap();
    path
}

async fn register_global_project(home: &Path, project: &Path) {
    let db_path = home.join(".tokensave").join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(project, 0).await;
}

fn tokensave_command_with_home(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tokensave"));
    command.env("HOME", home).env("USERPROFILE", home);
    command
}

fn serve_with_initialize_root(home: &Path, cwd: &Path, root_uri: String) -> Output {
    let mut child = tokensave_command_with_home(home)
        .arg("serve")
        .current_dir(cwd)
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
                        "uri": root_uri,
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

    child
        .wait_with_output()
        .expect("tokensave serve should exit after stdin closes")
}

fn runtime_project_root(stdout: &[u8], response_id: i64) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let response: Value = serde_json::from_str(line).unwrap();
        if response["id"].as_i64() != Some(response_id) {
            continue;
        }
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool response should include text content");
        let runtime: Value = serde_json::from_str(text).unwrap();
        let db_path = runtime["database"]["db_path"]
            .as_str()
            .expect("runtime snapshot should include database path");
        return Path::new(db_path)
            .parent()
            .and_then(Path::parent)
            .expect("database path should live under .tokensave")
            .to_string_lossy()
            .into_owned();
    }
    panic!("response id {response_id} not found in stdout:\n{stdout}");
}

#[cfg(unix)]
fn file_uri_localhost_percent_encoded(path: &Path) -> String {
    let encoded_path = path.to_string_lossy().replace(' ', "%20");
    format!("file://localhost{encoded_path}")
}

#[cfg(unix)]
#[tokio::test]
async fn initialize_roots_decode_file_uri_localhost_and_percent_escapes() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let stale = init_project_named("stale-project", "pub fn stale_project_marker() {}\n").await;
    let active = init_project_named("active project", "pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), &stale.path).await;
    register_global_project(home.path(), &active.path).await;

    let output = serve_with_initialize_root(
        home.path(),
        cwd.path(),
        file_uri_localhost_percent_encoded(&active.path),
    );

    assert!(
        output.status.success(),
        "tokensave serve should accept encoded file://localhost MCP roots\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        runtime_project_root(&output.stdout, 2),
        active.path.to_string_lossy(),
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
        "stderr should not contradict the ambiguity with a no-projects error:\n{stderr}"
    );
}
