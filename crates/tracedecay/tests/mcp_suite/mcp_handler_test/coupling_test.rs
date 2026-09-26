//! Production MCP behavior of `tracedecay_coupling`.
//!
//! Coupling ranks files by the number of *other* files they share an admitted
//! relation with. Same-file calls do not count, and a `path` argument restricts
//! both the ranked files and the partners that contribute to those counts.
//!
//! The fixture's cross-file relations, independent of the tool:
//! - `src/shop/orders.rs` calls `quote` in `pricing.rs` and `stock` in `inventory.rs`
//! - `src/shop/reports.rs` calls `quote`
//! - `src/warehouse/auditor.rs` calls `quote`
//! - `src/shop/shipping.rs` only calls a helper in the same file
//!
//! Fan-in is therefore pricing=3, inventory=1. Fan-out is orders=2, then
//! reports=1 before auditor=1 because equal counts sort by path.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::support::{
    ProductionCompositionFixture, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const FAN_IN_JSON: &str = r#"{"direction":"fan_in","ranking":[{"coupled_files":3,"file":"src/shop/pricing.rs"},{"coupled_files":1,"file":"src/shop/inventory.rs"}],"result_count":2}"#;
const FAN_OUT_JSON: &str = r#"{"direction":"fan_out","ranking":[{"coupled_files":2,"file":"src/shop/orders.rs"},{"coupled_files":1,"file":"src/shop/reports.rs"},{"coupled_files":1,"file":"src/warehouse/auditor.rs"}],"result_count":3}"#;
const SHOP_FAN_IN_JSON: &str = r#"{"direction":"fan_in","ranking":[{"coupled_files":2,"file":"src/shop/pricing.rs"},{"coupled_files":1,"file":"src/shop/inventory.rs"}],"result_count":2}"#;
const SHOP_FAN_OUT_JSON: &str = r#"{"direction":"fan_out","ranking":[{"coupled_files":2,"file":"src/shop/orders.rs"},{"coupled_files":1,"file":"src/shop/reports.rs"}],"result_count":2}"#;
const TOP_FAN_IN_JSON: &str = r#"{"direction":"fan_in","ranking":[{"coupled_files":3,"file":"src/shop/pricing.rs"}],"result_count":1}"#;
const EMPTY_FAN_OUT_JSON: &str = r#"{"direction":"fan_out","ranking":[],"result_count":0}"#;
const EMPTY_FAN_IN_JSON: &str = r#"{"direction":"fan_in","ranking":[],"result_count":0}"#;

const FAN_IN_MARKDOWN: &str = "\
**direction:** fan_in
**result_count:** 2

## ranking
- **src/shop/pricing.rs**
  **coupled_files:** 3
- **src/shop/inventory.rs**
  **coupled_files:** 1
";

fn write_coupling_sources(project: &Path) {
    fs::create_dir_all(project.join("src/shop")).expect("shop dir");
    fs::create_dir_all(project.join("src/warehouse")).expect("warehouse dir");
    fs::write(
        project.join("src/lib.rs"),
        "pub mod shop;\npub mod warehouse;\n",
    )
    .expect("lib.rs");
    fs::write(
        project.join("src/shop/mod.rs"),
        "pub mod inventory;\npub mod orders;\npub mod pricing;\npub mod reports;\npub mod shipping;\n",
    )
    .expect("shop/mod.rs");
    fs::write(
        project.join("src/shop/pricing.rs"),
        "pub fn quote() -> u32 {\n    local_tax()\n}\n\nfn local_tax() -> u32 {\n    1\n}\n",
    )
    .expect("pricing.rs");
    fs::write(
        project.join("src/shop/inventory.rs"),
        "pub fn stock() -> u32 {\n    2\n}\n",
    )
    .expect("inventory.rs");
    fs::write(
        project.join("src/shop/shipping.rs"),
        "pub fn ship() -> u32 {\n    label()\n}\n\nfn label() -> u32 {\n    3\n}\n",
    )
    .expect("shipping.rs");
    fs::write(
        project.join("src/shop/orders.rs"),
        "use crate::shop::inventory::stock;\nuse crate::shop::pricing::quote;\n\npub fn place() -> u32 {\n    quote() + stock()\n}\n",
    )
    .expect("orders.rs");
    fs::write(
        project.join("src/shop/reports.rs"),
        "use crate::shop::pricing::quote;\n\npub fn report() -> u32 {\n    quote()\n}\n",
    )
    .expect("reports.rs");
    fs::write(project.join("src/warehouse/mod.rs"), "pub mod auditor;\n")
        .expect("warehouse/mod.rs");
    fs::write(
        project.join("src/warehouse/auditor.rs"),
        "use crate::shop::pricing::quote;\n\npub fn audit() -> u32 {\n    quote()\n}\n",
    )
    .expect("auditor.rs");
}

async fn call_coupling(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_coupling", arguments)
        .await
        .expect("production MCP tools/call")
}

fn tool_text(response: &JsonRpcResponse) -> &str {
    assert!(
        response.error.is_none(),
        "tracedecay_coupling failed over MCP: {response:?}"
    );
    response
        .result
        .as_ref()
        .and_then(|result| result["content"].as_array())
        .and_then(|content| content.iter().find_map(|item| item["text"].as_str()))
        .unwrap_or_else(|| panic!("tracedecay_coupling returned no text: {response:?}"))
}

fn assert_coupling_json(response: &JsonRpcResponse, expected: &str) {
    assert_eq!(tool_text(response), expected);
}

#[tokio::test]
async fn coupling_ranks_distinct_other_files_for_fan_in_and_fan_out() {
    let fixture = production_composition_fixture_with_sources(write_coupling_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production graph server");
    warm_code_index_search(&server, "quote").await;
    drop(server);

    let fan_in = call_coupling(&fixture, json!({"direction": "fan_in", "format": "json"})).await;
    assert_coupling_json(&fan_in, FAN_IN_JSON);

    let default_direction = call_coupling(&fixture, json!({"format": "json"})).await;
    assert_coupling_json(&default_direction, FAN_IN_JSON);

    let fan_out = call_coupling(&fixture, json!({"direction": "fan_out", "format": "json"})).await;
    assert_coupling_json(&fan_out, FAN_OUT_JSON);

    let capped_limit = call_coupling(
        &fixture,
        json!({"direction": "fan_in", "limit": 100, "format": "json"}),
    )
    .await;
    assert_coupling_json(&capped_limit, FAN_IN_JSON);

    let top = call_coupling(
        &fixture,
        json!({"direction": "fan_in", "limit": 1, "format": "json"}),
    )
    .await;
    assert_coupling_json(&top, TOP_FAN_IN_JSON);

    let shop_fan_in = call_coupling(
        &fixture,
        json!({"direction": "fan_in", "path": "src/shop", "format": "json"}),
    )
    .await;
    assert_coupling_json(&shop_fan_in, SHOP_FAN_IN_JSON);

    let shop_fan_out = call_coupling(
        &fixture,
        json!({"direction": "fan_out", "path": "src/shop", "format": "json"}),
    )
    .await;
    assert_coupling_json(&shop_fan_out, SHOP_FAN_OUT_JSON);

    let warehouse = call_coupling(
        &fixture,
        json!({"direction": "fan_out", "path": "src/warehouse", "format": "json"}),
    )
    .await;
    assert_coupling_json(&warehouse, EMPTY_FAN_OUT_JSON);

    let orders_only = call_coupling(
        &fixture,
        json!({"direction": "fan_out", "path": "src/shop/orders.rs", "format": "json"}),
    )
    .await;
    assert_coupling_json(&orders_only, EMPTY_FAN_OUT_JSON);

    let missing_path = call_coupling(
        &fixture,
        json!({"direction": "fan_in", "path": "src/closed", "format": "json"}),
    )
    .await;
    assert_coupling_json(&missing_path, EMPTY_FAN_IN_JSON);

    let markdown = call_coupling(&fixture, json!({"direction": "fan_in"})).await;
    assert_eq!(tool_text(&markdown), FAN_IN_MARKDOWN);

    let invalid = call_coupling(&fixture, json!({"direction": "sideways", "format": "json"})).await;
    let error = invalid
        .error
        .as_ref()
        .expect("invalid direction must fail the MCP call");
    assert!(invalid.result.is_none(), "{invalid:?}");
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: invalid arguments for tracedecay_coupling: unknown variant `sideways`, expected `fan_in` or `fan_out`"
    );

    fixture.harness.shutdown().await;
}
