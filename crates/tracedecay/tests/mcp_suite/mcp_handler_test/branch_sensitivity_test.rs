#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::json;
use tracedecay_runtime_core::branch::{
    live_branch_resolution_count_for_root_for_test,
    reset_live_branch_resolution_count_for_root_for_test,
};

/// 25 warm retained-memory reads must not open git. A live checkout cannot
/// change fact-store list authority, so the dispatch path must serve the
/// snapshot branch instead of resolving HEAD.
#[tokio::test]
async fn warm_fact_store_list_calls_perform_no_live_branch_resolutions() {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");

    let warm = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_list",
        json!({ "format": "json" }),
    )
    .await;
    assert!(warm["error"].is_null(), "{warm}");

    let root = production.project_root.as_path();
    reset_live_branch_resolution_count_for_root_for_test(root);
    for _ in 0..25 {
        let response = handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_list",
            json!({ "format": "json" }),
        )
        .await;
        assert!(response["error"].is_null(), "{response}");
    }
    assert_eq!(
        live_branch_resolution_count_for_root_for_test(root),
        0,
        "branch-independent memory reads must not resolve the live git branch"
    );

    reset_live_branch_resolution_count_for_root_for_test(root);
    let sensitive =
        handle_real_server_tool_call_raw(&server, "tracedecay_status", json!({ "format": "json" }))
            .await;
    assert!(sensitive["error"].is_null(), "{sensitive}");
    assert!(
        live_branch_resolution_count_for_root_for_test(root) >= 1,
        "a graph/info tool must still resolve the live branch so checkout drift is visible on the next request"
    );
}
