//! `tracedecay_hotspots` through production MCP `tools/call`.
//!
//! The first run records the payloads the server actually returned. Literal
//! expectations replace that record once the ranking, the degree counts, and
//! the limit clamp have been read off those payloads.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{MountedProductionProject, close_test_graph, handle_tool_call, init_test_project};
use crate::support::{extract_text, test_temp_dir};

fn write_package(project: &Path, name: &str) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("package.json"),
        format!("{{\"name\":\"{name}\",\"private\":true,\"type\":\"module\"}}\n"),
    )
    .unwrap();
}

/// Four functions with a known call shape:
/// `hub` calls `mid`, `mid` calls `leaf`, `quiet` calls nothing.
fn write_chain_project(project: &Path) {
    write_package(project, "hotspots-chain");
    fs::write(
        project.join("src/calls.ts"),
        "export function quiet(): number {\n  return 0;\n}\n\nexport function leaf(): number {\n  return 1;\n}\n\nexport function mid(): number {\n  return leaf();\n}\n\nexport function hub(): number {\n  return mid();\n}\n",
    )
    .unwrap();
}

/// `hub` plus 101 direct callers, more symbols than the tool's 100-row cap.
fn write_fanout_project(project: &Path) {
    write_package(project, "hotspots-fanout");
    let mut source = String::from("export function hub(): number { return 1; }\n");
    for index in 0..101 {
        source.push_str(&format!(
            "export function caller{index}(): number {{ return hub(); }}\n"
        ));
    }
    fs::write(project.join("src/fanout.ts"), source).unwrap();
}

async fn hotspots_text(host: &MountedProductionProject, arguments: Value) -> String {
    let result = handle_tool_call(host, "tracedecay_hotspots", arguments, None, None)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_hotspots failed over production MCP: {error}"));
    extract_text(&result.value).to_owned()
}

async fn hotspots_json(host: &MountedProductionProject, arguments: Value) -> Value {
    let text = hotspots_text(host, arguments).await;
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("tracedecay_hotspots JSON payload did not parse: {error}\n{text}")
    })
}

fn record(name: &str, value: &Value) {
    let dir = Path::new("/tmp/hotspots-behavior-proof");
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_string_pretty(value).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn hotspots_ranks_symbols_by_edge_degree_and_clamps_limit() {
    let chain_dir = test_temp_dir();
    let chain_root = chain_dir.path().join("project");
    write_chain_project(&chain_root);
    let (chain, _env) = init_test_project(&chain_root).await;

    let chain_default = hotspots_json(&chain, json!({"format": "json"})).await;
    let chain_limit_one = hotspots_json(&chain, json!({"format": "json", "limit": 1})).await;
    let chain_markdown = hotspots_text(&chain, json!({"format": "markdown", "limit": 1})).await;
    let chain_rejected = chain
        .harness
        .call_tool(
            &chain.project_root,
            "tracedecay_hotspots",
            json!({"limit": 0, "format": "json"}),
        )
        .await
        .expect("zero limit still reaches the MCP server");
    close_test_graph(chain).await;

    let fanout_dir = test_temp_dir();
    let fanout_root = fanout_dir.path().join("project");
    write_fanout_project(&fanout_root);
    let (fanout, _env) = init_test_project(&fanout_root).await;
    let fanout_default = hotspots_json(&fanout, json!({"format": "json"})).await;
    let fanout_capped = hotspots_json(&fanout, json!({"format": "json", "limit": 250})).await;
    let fanout_one = hotspots_json(&fanout, json!({"format": "json", "limit": 1})).await;
    close_test_graph(fanout).await;

    let rejected = chain_rejected.error.expect("zero limit is a tool error");
    let proof = json!({
        "chain_default": chain_default,
        "chain_limit_one": chain_limit_one,
        "chain_markdown": chain_markdown,
        "rejected": {
            "code": rejected.code,
            "message": rejected.message,
            "data": rejected.data,
        },
        "fanout_default": fanout_default,
        "fanout_capped": fanout_capped,
        "fanout_one": fanout_one,
    });
    record("observed", &proof);

    // Replaced with the observed literals after the production call.
    assert_eq!(proof, json!("pending-observation"));
}
