//! `tracedecay://files` admits the verified generation inventory.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::mcp_server_test::support::{jsonrpc_request, parse_response};
use crate::support::{
    CaptureTransport, ProductionCompositionFixture, extract_first_json_content,
    handle_real_server_tool_call, harness_wait_for_readiness, peer_production_composition,
    production_composition_fixture_with_sources,
};
use tracedecay::mcp::McpServer;

const CARGO_TOML: &str =
    "[package]\nname = \"files_resource_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const LIB_RS: &str =
    "pub mod greeting;\n\npub fn root_value() -> i32 {\n    greeting::hello()\n}\n";
const GREETING_RS: &str = "pub fn hello() -> i32 {\n    7\n}\n";
const README_MD: &str = "not indexed\n";

#[tokio::test]
async fn ready_repository_files_resource_matches_the_verified_disk_listing() {
    let fixture = ready_files_project().await;
    let server = server(&fixture);
    let text = read_files_resource(&server).await;
    let verified = verified_file_paths(&server).await;

    assert!(
        !text.contains("verified_generation_file_inventory_not_admitted"),
        "the advertised resource must admit a ready generation: {text}"
    );
    assert!(
        text.starts_with("freshness: fresh\n"),
        "ready inventory must name freshness: {text}"
    );
    assert_eq!(
        verified,
        vec![
            "Cargo.toml".to_owned(),
            "src/greeting.rs".to_owned(),
            "src/lib.rs".to_owned()
        ],
        "verified files tool census: {verified:?}"
    );
    for path in &verified {
        assert!(
            fixture.project_root.join(path).is_file(),
            "verified inventory path missing on disk: {path}"
        );
        let file_name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("indexed file name");
        assert!(
            text.contains(&format!("{file_name} (")),
            "files resource must list {path} from disk: {text}"
        );
    }
    assert!(
        !text.contains("README.md"),
        "unindexed files must stay off the admitted inventory: {text}"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn files_resource_does_not_leak_another_project_inventory() {
    let fixture = ready_files_project().await;
    let (peer, peer_isolation) = peer_production_composition(&fixture).await;
    let peer_root = peer_isolation.path().join("project");
    harness_wait_for_readiness(&peer, &peer_root, "ready", Duration::from_secs(20)).await;

    let owner_text = read_files_resource(&server(&fixture)).await;
    let peer_server = peer.server(&peer_root).expect("peer production MCP server");
    let peer_text = read_files_resource(&peer_server).await;

    assert!(
        owner_text.contains("greeting.rs ("),
        "owner inventory must keep its own files: {owner_text}"
    );
    assert!(
        !peer_text.contains("greeting.rs"),
        "a peer project must not receive the owner's verified files: {peer_text}"
    );
    assert!(
        peer_text.contains("main.rs (") || peer_text.contains("utils.rs ("),
        "peer inventory must keep its own verified files: {peer_text}"
    );
    assert!(
        !owner_text.contains("main.rs ("),
        "owner inventory must not leak the peer project: {owner_text}"
    );

    peer.shutdown().await;
    fixture.harness.shutdown().await;
}

async fn ready_files_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(|root| {
        write(root, "Cargo.toml", CARGO_TOML);
        write(root, "src/lib.rs", LIB_RS);
        write(root, "src/greeting.rs", GREETING_RS);
        write(root, "README.md", README_MD);
    })
    .await;
    harness_wait_for_readiness(
        &fixture.harness,
        &fixture.project_root,
        "ready",
        Duration::from_secs(20),
    )
    .await;
    fixture
}

fn server(fixture: &ProductionCompositionFixture) -> std::sync::Arc<McpServer> {
    fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server for the files resource")
}

async fn read_files_resource(server: &McpServer) -> String {
    let request = jsonrpc_request(
        json!(420),
        "resources/read",
        json!({ "uri": "tracedecay://files" }),
    );
    let mut transport = CaptureTransport {
        incoming: Some(request),
        output: String::new(),
    };
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("resources/read connection");
    let response = parse_response(transport.output.trim());
    assert!(response["error"].is_null(), "{response}");
    response["result"]["contents"][0]["text"]
        .as_str()
        .expect("files resource text")
        .to_owned()
}

async fn verified_file_paths(server: &McpServer) -> Vec<String> {
    let result = handle_real_server_tool_call(server, "tracedecay_files", json!({})).await;
    let payload = extract_first_json_content(&result);
    payload["files"]
        .as_array()
        .expect("files tool census")
        .iter()
        .map(|file| file["path"].as_str().expect("indexed file path").to_owned())
        .collect()
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("files resource fixture parent");
    }
    fs::write(path, contents).expect("files resource fixture file");
}
