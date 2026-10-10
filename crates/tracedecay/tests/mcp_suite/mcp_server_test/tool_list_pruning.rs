//! Handshake token cost and reachability for the pruned MCP tool list.

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

use super::support::{
    jsonrpc_request, response_with_id, run_client_connection_with_messages,
    spec_initialize_request, successful_tool_text,
};

fn listed_tool_names(listed: &Value) -> BTreeSet<String> {
    listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result: {listed}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn default_handshake_is_cheaper_and_every_tool_stays_reachable() {
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production handshake server");

    let responses = run_client_connection_with_messages(
        Arc::clone(&server),
        vec![
            spec_initialize_request(json!(1)),
            jsonrpc_request(json!(2), "tools/list", json!({})),
            jsonrpc_request(
                json!(3),
                "tools/call",
                json!({
                    "name": "tracedecay_tool_search",
                    "arguments": { "format": "json" }
                }),
            ),
            jsonrpc_request(
                json!(4),
                "tools/call",
                json!({
                    "name": "tracedecay_runtime",
                    "arguments": { "format": "json" }
                }),
            ),
        ],
    )
    .await;

    let initialize = response_with_id(&responses, json!(1));
    assert!(
        initialize.get("error").is_none(),
        "initialize must succeed: {initialize}"
    );
    let instructions = initialize["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(
        instructions.contains("tracedecay_tool_search"),
        "initialize must steer hosts at the deferred-tool search: {instructions}"
    );

    let listed = response_with_id(&responses, json!(2));
    let handshake_names = listed_tool_names(&listed);
    assert!(
        handshake_names.contains("tracedecay_tool_search"),
        "default handshake must advertise tool search: {handshake_names:?}"
    );
    assert!(
        handshake_names.contains("tracedecay_search"),
        "default handshake must keep the always-loaded core: {handshake_names:?}"
    );
    assert!(
        !handshake_names.contains("tracedecay_runtime"),
        "deferred runtime must stay off the default handshake: {handshake_names:?}"
    );

    let default_tokens = tracedecay_mcp::tool_list_approx_tokens(&listed["result"])
        .expect("default handshake tokens");
    let full = tracedecay_mcp::tools::catalog_discovery::advertised_catalog_discovery_tools_list_payload_with_mode(
        Some(0),
        tracedecay_mcp::explore_call_budget(0),
        &tracedecay_tool_catalog::ProfileId::new(
            tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID,
        )
        .expect("default profile"),
        &tracedecay_mcp::tools::catalog_discovery::default_catalog_discovery_authority()
            .expect("discovery authority"),
        &tracedecay_mcp::project_catalog_discovery_scope(),
        tracedecay_mcp::ToolRegistryMode::HostAvailable,
        tracedecay_mcp::ToolListAdvertisement::Full,
    )
    .expect("full handshake catalog");
    let before_tokens = tracedecay_mcp::tool_list_approx_tokens(&full).expect("full tokens");
    assert!(
        default_tokens * 4 < before_tokens,
        "after={default_tokens} tokens / {} tools must be far cheaper than before={before_tokens} tokens / {} tools",
        handshake_names.len(),
        full["tools"].as_array().map(Vec::len).unwrap_or(0)
    );
    eprintln!(
        "mcp tool-list handshake tokens before={before_tokens} after={default_tokens} default_tools={} full_tools={}",
        handshake_names.len(),
        full["tools"].as_array().map(Vec::len).unwrap_or(0)
    );

    let search = response_with_id(&responses, json!(3));
    let catalog: Value =
        serde_json::from_str(successful_tool_text(&search, "tracedecay_tool_search"))
            .expect("tool search catalog JSON");
    let reachable: BTreeSet<String> = catalog["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tool search catalog: {catalog}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    let full_names: BTreeSet<String> = full["tools"]
        .as_array()
        .expect("full tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        reachable, full_names,
        "tool search must reach every catalog-filtered tool"
    );
    assert!(
        handshake_names.is_subset(&reachable),
        "default handshake must stay a subset of the reachable catalog"
    );

    let runtime = response_with_id(&responses, json!(4));
    assert!(
        runtime.get("error").is_none(),
        "a deferred tool must stay callable by name: {runtime}"
    );
    let runtime_text = successful_tool_text(&runtime, "tracedecay_runtime");
    assert!(
        !runtime_text.is_empty(),
        "deferred runtime call must return a body: {runtime}"
    );
}
