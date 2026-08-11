//! Runtime MCP dispatch coverage for sealed-generation census telemetry.

use std::fs;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_test_support::SelectorEnv;
use super::*;
use crate::config::lock_user_data_dir_test_env;

#[tokio::test]
async fn runtime_mcp_marks_missing_generation_census_authority_unavailable() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture root");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("runtime-generation-census-unavailable");
    fs::create_dir_all(project.join("src")).expect("create fixture source root");
    fs::write(project.join("src/lib.rs"), "pub fn unavailable() {}\n")
        .expect("write fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-runtime-generation-census-unavailable",
    )
    .await
    .expect("open v32 mounted runtime fixture");

    let result = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_runtime",
        json!({ "format": "json" }),
        None,
        None,
        ToolCallRegistryOptions::default(),
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
