//! Production MCP path for `tracedecay_multi_root_scope_set_compare_and_swap`.
//!
//! The tool is daemon-owned. A recording executor only shows that the name
//! was forwarded. These calls use the same socket and `tools/call` framing
//! a host uses, and they assert the revision, frozen roots, and refusals
//! the caller can read back.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::{
    DaemonHandshake, enter_test_daemon_database_scope, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};

const TOOL_NAME: &str = "tracedecay_multi_root_scope_set_compare_and_swap";
const SCOPE_SET_ID: &str = "scope-set.mcp-cas";
const ALPHA_PROJECT_ID: &str = "project.mcp-cas-alpha";
const BETA_PROJECT_ID: &str = "project.mcp-cas-beta";
const BINDING_ID: &str = "binding.http.multi_root.scope_set_compare_and_swap.v1";
const RESULT_SCHEMA_ID: &str = "schema.tracedecay.multi-root.scope-set-compare-and-swap-result.v1";
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn multi_root_scope_set_compare_and_swap_reports_apply_conflict_and_refusal() {
    const STACK_SIZE: usize = 16 * 1024 * 1024;

    std::thread::Builder::new()
        .name("mcp-scope-set-cas".to_owned())
        .stack_size(STACK_SIZE)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(STACK_SIZE)
                .enable_all()
                .build()
                .expect("scope-set compare-and-swap runtime")
                .block_on(run_scope_set_compare_and_swap());
        })
        .expect("scope-set compare-and-swap thread")
        .join()
        .expect("scope-set compare-and-swap thread must not panic");
}

async fn run_scope_set_compare_and_swap() {
    let temp = TempDir::new().expect("scope-set fixture");
    let profile_root = temp.path().join("profile");
    let alpha = prepared_project(temp.path(), "alpha", ALPHA_PROJECT_ID);
    let beta = prepared_project(temp.path(), "beta", BETA_PROJECT_ID);
    let client_identity = test_client_identity_for(profile_root.clone());
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "mcp-scope-set-cas");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake = DaemonHandshake {
        project_path: Some(alpha.clone()),
        allow_init: true,
        client_identity,
        client_instance_id: "mcp-scope-set-cas".to_owned(),
        ..test_handshake_defaults()
    };
    // Both roots must be registered before the call. A selector the daemon
    // has not mounted answers `unavailable` instead of compare-and-swap
    // evidence, which would hide the revision the caller can observe.
    engine
        .open_project_server(&handshake)
        .await
        .expect("register alpha project");
    let beta_handshake = DaemonHandshake {
        project_path: Some(beta.clone()),
        ..handshake.clone()
    };
    engine
        .open_project_server(&beta_handshake)
        .await
        .expect("register beta project");

    let (server_stream, client_stream) =
        tokio::net::UnixStream::pair().expect("scope-set socket pair");
    let server_engine = engine.clone();
    let server_task = tokio::spawn(async move {
        Box::pin(super::super::serve_socket_client(
            server_stream,
            server_engine,
        ))
        .await
    });
    let (reader, mut writer) = client_stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    write_line(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "scope-set-cas-mcp", "version": "1"}
            }
        }),
    )
    .await;
    let initialized = read_response(&mut reader, 1, "initialize").await;
    assert!(
        initialized.get("error").is_none(),
        "initialize failed: {initialized}"
    );
    assert!(
        initialized.get("result").is_some(),
        "initialize omitted its result: {initialized}"
    );

    let unknown_field = call_tool(
        &mut reader,
        &mut writer,
        2,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": null,
            "roots": [root_selector(ALPHA_PROJECT_ID, &alpha)],
            "unexpected_field": true
        }),
    )
    .await;
    assert_invalid_request(&unknown_field);

    let empty_roots = call_tool(
        &mut reader,
        &mut writer,
        3,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": null,
            "roots": []
        }),
    )
    .await;
    assert_invalid_request(&empty_roots);

    let unsorted = call_tool(
        &mut reader,
        &mut writer,
        4,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": null,
            "roots": [
                root_selector(BETA_PROJECT_ID, &beta),
                root_selector(ALPHA_PROJECT_ID, &alpha)
            ]
        }),
    )
    .await;
    assert_invalid_request(&unsorted);

    let wrong_project = call_tool(
        &mut reader,
        &mut writer,
        5,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": null,
            "roots": [root_selector("project.mcp-cas-missing", &alpha)]
        }),
    )
    .await;
    assert_not_found(&wrong_project);

    let missing_revision = call_tool(
        &mut reader,
        &mut writer,
        6,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": 1,
            "roots": sorted_roots(&alpha, &beta)
        }),
    )
    .await;
    let missing_cas = assert_cas_evidence(&missing_revision);
    assert_eq!(missing_cas["status"], "conflict");
    assert_eq!(missing_cas["scope_set"], json!(null));

    let created = call_tool(
        &mut reader,
        &mut writer,
        7,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": null,
            "roots": sorted_roots(&alpha, &beta)
        }),
    )
    .await;
    let created_cas = assert_cas_evidence(&created);
    assert_eq!(created_cas["status"], "applied");
    assert_saved_scope_set(&created_cas["scope_set"], 1, &alpha, &beta);
    let created_digest = created_cas["scope_set"]["digest"]
        .as_str()
        .expect("applied scope set digest")
        .to_owned();

    let repeated_create = call_tool(
        &mut reader,
        &mut writer,
        8,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": null,
            "roots": sorted_roots(&alpha, &beta)
        }),
    )
    .await;
    let repeated_cas = assert_cas_evidence(&repeated_create);
    assert_eq!(repeated_cas["status"], "conflict");
    assert_saved_scope_set(&repeated_cas["scope_set"], 1, &alpha, &beta);
    assert_eq!(repeated_cas["scope_set"]["digest"], created_digest);

    let replaced = call_tool(
        &mut reader,
        &mut writer,
        9,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": 1,
            "roots": sorted_roots(&alpha, &beta)
        }),
    )
    .await;
    let replaced_cas = assert_cas_evidence(&replaced);
    assert_eq!(replaced_cas["status"], "applied");
    assert_saved_scope_set(&replaced_cas["scope_set"], 2, &alpha, &beta);
    let replaced_digest = replaced_cas["scope_set"]["digest"]
        .as_str()
        .expect("replaced scope set digest")
        .to_owned();
    assert_ne!(
        replaced_digest, created_digest,
        "revision 2 must seal a different scope-set digest than revision 1"
    );

    let stale = call_tool(
        &mut reader,
        &mut writer,
        10,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": 1,
            "roots": sorted_roots(&alpha, &beta)
        }),
    )
    .await;
    let stale_cas = assert_cas_evidence(&stale);
    assert_eq!(stale_cas["status"], "conflict");
    assert_saved_scope_set(&stale_cas["scope_set"], 2, &alpha, &beta);
    assert_eq!(stale_cas["scope_set"]["digest"], replaced_digest);

    let future_revision = call_tool(
        &mut reader,
        &mut writer,
        11,
        json!({
            "scope_set_id": SCOPE_SET_ID,
            "expected_revision": 99,
            "roots": sorted_roots(&alpha, &beta)
        }),
    )
    .await;
    let future_cas = assert_cas_evidence(&future_revision);
    assert_eq!(future_cas["status"], "conflict");
    assert_saved_scope_set(&future_cas["scope_set"], 2, &alpha, &beta);
    assert_eq!(future_cas["scope_set"]["digest"], replaced_digest);

    writer.shutdown().await.expect("close scope-set client");
    drop(writer);
    drop(reader);
    tokio::time::timeout(CALL_TIMEOUT, server_task)
        .await
        .expect("scope-set connection did not terminate")
        .expect("join scope-set connection")
        .expect("serve scope-set connection");
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("run Git fixture command");
    assert!(status.success(), "git {args:?} in {}", root.display());
}

fn prepared_project(root: &Path, name: &str, project_id: &str) -> PathBuf {
    let project = root.join(name);
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn mcp_cas() {}\n").expect("project source");
    git(&project, &["init", "--quiet"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project,
        &["config", "user.email", "tracedecay@example.com"],
    );
    git(&project, &["add", "."]);
    git(&project, &["commit", "--quiet", "-m", "base"]);
    let project = project.canonicalize().expect("canonical project root");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(&project, project_id)
        .expect("pin fixture project id");
    project
}

fn root_selector(project_id: &str, root: &Path) -> Value {
    json!({
        "project_id": project_id,
        "root": root
    })
}

fn sorted_roots(alpha: &Path, beta: &Path) -> Value {
    json!([
        root_selector(ALPHA_PROJECT_ID, alpha),
        root_selector(BETA_PROJECT_ID, beta)
    ])
}

async fn write_line(writer: &mut (impl AsyncWrite + Unpin), value: &Value) {
    writer
        .write_all(value.to_string().as_bytes())
        .await
        .expect("write JSON-RPC value");
    writer.write_all(b"\n").await.expect("write newline");
}

async fn read_response(reader: &mut (impl AsyncBufRead + Unpin), id: i64, context: &str) -> Value {
    let deadline = tokio::time::Instant::now() + CALL_TIMEOUT;
    let mut seen = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "{context}: timed out waiting for JSON-RPC id {id}\n{seen}"
        );
        let mut line = String::new();
        let read = tokio::time::timeout(remaining, reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| {
                panic!("{context}: timed out waiting for JSON-RPC id {id}\n{seen}")
            });
        let bytes = read.unwrap_or_else(|error| panic!("{context}: read failed: {error}"));
        assert_ne!(
            bytes, 0,
            "{context}: connection closed before id {id}\n{seen}"
        );
        seen.push_str(&line);
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id") == Some(&json!(id)) {
            return value;
        }
    }
}

async fn call_tool(
    reader: &mut (impl AsyncBufRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    id: i64,
    arguments: Value,
) -> Value {
    write_line(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": TOOL_NAME,
                "arguments": arguments
            }
        }),
    )
    .await;
    let response = read_response(reader, id, TOOL_NAME).await;
    assert!(
        response.get("error").is_none(),
        "{TOOL_NAME} must stay a completed JSON-RPC response: {response}"
    );
    assert_eq!(response["result"]["content"][0]["type"], "text");
    response
}

fn tool_body(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool response omitted text: {response}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool response text was not JSON ({error}): {text}"))
}

fn assert_tool_contract(body: &Value) {
    assert_eq!(body["binding_id"], BINDING_ID);
    assert_eq!(
        body["application"]["contract"]["schema_id"],
        RESULT_SCHEMA_ID
    );
    assert_eq!(body["application"]["contract"]["schema_revision"], 1);
}

fn assert_invalid_request(response: &Value) {
    assert_eq!(response["result"]["isError"], true);
    let body = tool_body(response);
    assert_tool_contract(&body);
    let problem = &body["application"]["problem"];
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "multi_root.invalid_request");
    assert_eq!(
        problem["message"],
        "The multi-root application request is invalid"
    );
    assert_eq!(problem["retry"], "never");
    assert_eq!(problem["owning_layer"], "runtime");
    assert_eq!(problem["legal_actions"], json!(["correct_request"]));
}

fn assert_not_found(response: &Value) {
    assert_eq!(response["result"]["isError"], true);
    let body = tool_body(response);
    assert_tool_contract(&body);
    let problem = &body["application"]["problem"];
    assert_eq!(problem["kind"], "not_found_or_not_authorized");
    assert_eq!(problem["code"], "not_found_or_not_authorized");
    assert_eq!(
        problem["message"],
        "The requested resource was not found or is not authorized"
    );
    assert_eq!(problem["retry"], "never");
    assert_eq!(problem["owning_layer"], "runtime");
    assert_eq!(problem["legal_actions"], json!([]));
}

fn assert_cas_evidence(response: &Value) -> Value {
    assert!(
        response["result"].get("isError").is_none(),
        "a settled compare-and-swap must not be an MCP error: {response}"
    );
    let body = tool_body(response);
    assert_tool_contract(&body);
    assert_eq!(body["application"]["outcome"]["outcome"], "evidence");
    body["application"]["outcome"]["value"]["payload"].clone()
}

fn assert_saved_scope_set(scope_set: &Value, revision: u64, alpha: &Path, beta: &Path) {
    assert_eq!(scope_set["scope_set_id"], SCOPE_SET_ID);
    assert_eq!(scope_set["revision"], revision);
    let roots = scope_set["roots"]
        .as_array()
        .unwrap_or_else(|| panic!("scope set omitted roots: {scope_set}"));
    assert_eq!(roots.len(), 2);
    let mut locators = BTreeSet::new();
    for root in roots {
        assert_eq!(root["scope"]["project_id"], root["locator"]["project_id"]);
        locators.insert((
            root["locator"]["project_id"]
                .as_str()
                .unwrap_or_else(|| panic!("locator omitted project_id: {root}"))
                .to_owned(),
            root["locator"]["canonical_root"]
                .as_str()
                .unwrap_or_else(|| panic!("locator omitted canonical_root: {root}"))
                .to_owned(),
        ));
    }
    assert_eq!(
        locators,
        BTreeSet::from([
            (ALPHA_PROJECT_ID.to_owned(), alpha.display().to_string()),
            (BETA_PROJECT_ID.to_owned(), beta.display().to_string()),
        ])
    );
    let digest = scope_set["digest"]
        .as_str()
        .unwrap_or_else(|| panic!("scope set omitted digest: {scope_set}"));
    assert!(
        digest.starts_with("sha256:") && digest.len() == "sha256:".len() + 64,
        "scope set digest must be a sha256 tag plus 64 hex characters, got {digest}"
    );
}
