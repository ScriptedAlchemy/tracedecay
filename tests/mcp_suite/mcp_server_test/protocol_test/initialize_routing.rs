use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;

use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_server_with_messages,
};

fn initialize_protocol_fixture(project: &Path, module: &str) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), format!("pub mod {module};\n")).unwrap();
    fs::write(
        project.join(format!("src/{module}.rs")),
        format!("pub fn {module}_marker() {{}}\n"),
    )
    .unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(project)
                .status()
                .unwrap()
                .success()
        );
    }
}

async fn fixture() -> (
    TempDir,
    ProductionProjectCompositionHarnessV1,
    Arc<McpServer>,
    PathBuf,
) {
    let isolation = TempDir::new().unwrap();
    let active_project = isolation.path().join("active-project");
    let target_project = isolation.path().join("target-project");
    initialize_protocol_fixture(&active_project, "active");
    initialize_protocol_fixture(&target_project, "target");
    let harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [active_project.clone(), target_project.clone()],
    )
    .await
    .unwrap();
    let server = harness.server(&active_project).unwrap();
    (isolation, harness, server, target_project)
}

fn initialize_request(target_project: &Path) -> String {
    let target_root_uri = url::Url::from_file_path(target_project)
        .expect("target project has a portable file URI")
        .to_string();
    jsonrpc_request(
        json!(1),
        "initialize",
        json!({
            "clientInfo": {"name": "codex", "version": "test"},
            "roots": [{"uri": target_root_uri, "name": "target-project"}]
        }),
    )
}

fn files_request(id: u64, arguments: Value) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({
            "name": "tracedecay_files",
            "arguments": arguments,
        }),
    )
}

pub(super) async fn assert_registered_reader_uses_initialize_root() {
    let (_isolation, _harness, server, target_project) = fixture().await;
    let responses = run_server_with_messages(
        server,
        vec![
            initialize_request(&target_project),
            files_request(2, json!({"layout": "flat"})),
        ],
    )
    .await;

    let files_response = response_with_id(&responses, json!(2));
    let text = files_response["result"]["content"][0]["text"]
        .as_str()
        .expect("files response text");
    assert!(
        text.contains("src/target.rs"),
        "initialize root should route reader tools to target project, got {text}"
    );
    assert!(
        !text.contains("src/active.rs"),
        "implicit initialize-root routing should not read the active project: {text}"
    );
}

pub(super) async fn assert_legacy_selectors_cannot_spoof_initialize_root() {
    let (_isolation, _harness, server, target_project) = fixture().await;
    let active_graph = server.cg().await;
    let active_root = active_graph.project_root().to_string_lossy().into_owned();
    let active_project_id = active_graph
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project has a registered identity");

    let spoof_cases = [
        (
            "top-level project_path",
            json!({"layout": "flat", "project_path": active_root.clone()}),
        ),
        (
            "top-level project_root",
            json!({"layout": "flat", "project_root": active_root.clone()}),
        ),
        (
            "nested selector path",
            json!({"layout": "flat", "project_selector": {"path": active_root.clone()}}),
        ),
        (
            "nested selector project_path",
            json!({"layout": "flat", "project_selector": {"project_path": active_root}}),
        ),
        (
            "top-level project_id alias",
            json!({"layout": "flat", "project_id": active_project_id}),
        ),
    ];
    let mut messages = vec![initialize_request(&target_project)];
    for (offset, (_, arguments)) in spoof_cases.iter().enumerate() {
        messages.push(files_request(10 + offset as u64, arguments.clone()));
    }
    messages.push(files_request(100, json!({"layout": "flat"})));

    let responses = run_server_with_messages(server, messages).await;
    for (offset, (case, _)) in spoof_cases.iter().enumerate() {
        let response = response_with_id(&responses, json!(10 + offset as u64));
        assert_eq!(
            response["error"]["code"], -32602,
            "{case} must be rejected as invalid parameters instead of overriding the initialize-root route: {response}"
        );
        assert!(
            response["result"].is_null(),
            "{case} must not return a tool result after invalid-parameter rejection: {response}"
        );
        assert!(
            !response.to_string().contains("src/active.rs"),
            "{case} must not serve spoof-project data: {response}"
        );
    }

    let clean_response = response_with_id(&responses, json!(100));
    let clean_text = clean_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("clean files response text: {clean_response}"));
    assert!(
        clean_text.contains("src/target.rs") && !clean_text.contains("src/active.rs"),
        "rejected spoof attempts must not disturb the initialize-root route: {clean_text}"
    );
}
