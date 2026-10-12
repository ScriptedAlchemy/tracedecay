//! Runtime MCP dispatch coverage for sealed-generation census telemetry.

use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_test_support::registered_lib_fixture;
use super::*;

#[tokio::test]
async fn runtime_mcp_marks_missing_generation_census_authority_unavailable() {
    let dir = TempDir::new().expect("fixture root");
    let (cg, _runtime) = registered_lib_fixture(
        dir.path(),
        "runtime-generation-census-unavailable",
        "pub fn unavailable() {}\n",
        "project.mcp-runtime-generation-census-unavailable",
    )
    .await;

    let result = dispatch_on_graph_authority(
        &cg,
        "tracedecay_runtime",
        json!({ "format": "json" }),
        ToolCallRegistryOptions::default()
            .admit_opened_project(&cg)
            .expect("opened fixture admits"),
    )
    .await
    .expect("missing generation authority is an observed runtime state");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("runtime JSON text"),
    )
    .expect("parse runtime JSON");

    assert_eq!(
        payload["database"]["generation_census"],
        json!({
            "state": "unavailable",
            "reason": "authority_unavailable",
        }),
        "the v32 runtime database has no legacy code-graph tables to fall back to"
    );

    cg.checkpoint().await.expect("checkpoint fixture database");
    cg.close();
}
