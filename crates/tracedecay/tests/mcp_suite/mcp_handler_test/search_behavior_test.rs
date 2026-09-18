#![cfg(feature = "test-transport")]

//! Host-visible `tracedecay_search` behavior through the production MCP server.
//!
//! The longer function name is a decoy: a prefix of the query must not outrank
//! the symbol whose name is the query.

use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use tracedecay::mcp::McpServer;

const EXACT_NAME: &str = "apply_invoice_discount";
const DECOY_NAME: &str = "apply_invoice_discount_preview";
const UNKNOWN_NAME: &str = "zz_not_in_billing_source";

#[tokio::test]
async fn exact_name_search_returns_that_function_and_rejects_a_missing_query() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/billing.rs"),
            "\
pub fn apply_invoice_discount(total: u32) -> u32 {
    total
}

pub fn apply_invoice_discount_preview(total: u32) -> u32 {
    total
}
",
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, EXACT_NAME).await;

    let page = search_json(&server, json!({"query": EXACT_NAME, "limit": 10})).await;
    assert_eq!(symbol_names(&page), vec![EXACT_NAME, DECOY_NAME], "{page}");
    assert_eq!(
        page["results"][0]["display"],
        json!({
            "name": EXACT_NAME,
            "qualified_name": "src/billing.rs::apply_invoice_discount",
            "kind": "function",
            "path": "src/billing.rs",
        }),
        "{page}"
    );
    assert_eq!(
        page["results"][1]["display"],
        json!({
            "name": DECOY_NAME,
            "qualified_name": "src/billing.rs::apply_invoice_discount_preview",
            "kind": "function",
            "path": "src/billing.rs",
        }),
        "{page}"
    );
    assert_ready(&page);

    let first_only = search_json(&server, json!({"query": EXACT_NAME, "limit": 1})).await;
    assert_eq!(symbol_names(&first_only), vec![EXACT_NAME], "{first_only}");
    assert_ready(&first_only);

    let miss = search_json(&server, json!({"query": UNKNOWN_NAME, "limit": 10})).await;
    assert_eq!(miss["results"], json!([]), "{miss}");
    assert_ready(&miss);

    let markdown = search_markdown(&server, json!({"query": EXACT_NAME, "limit": 1})).await;
    assert!(
        markdown.starts_with("freshness: fresh\n## Search Results\n"),
        "{markdown}"
    );
    let bullet = markdown
        .lines()
        .find(|line| line.starts_with("- **"))
        .unwrap_or_else(|| panic!("search markdown had no ranked hit: {markdown}"));
    assert_eq!(
        bullet.split(" · utility ").next(),
        Some("- **apply_invoice_discount** (function, exact_literal_phrase), rank 1"),
        "{markdown}"
    );

    let miss_markdown = search_markdown(&server, json!({"query": UNKNOWN_NAME})).await;
    assert!(
        miss_markdown.starts_with("freshness: fresh\n## Search Results\n_No matching symbols._\n"),
        "{miss_markdown}"
    );
    assert!(
        !miss_markdown.contains(EXACT_NAME),
        "an unknown name must not echo the indexed function: {miss_markdown}"
    );

    let missing = handle_real_server_tool_call_raw(&server, "tracedecay_search", json!({})).await;
    assert_eq!(missing["error"]["code"], -32602, "{missing}");
    assert_eq!(
        missing["error"]["message"], "missing required parameter: query",
        "{missing}"
    );
    assert_eq!(
        missing["error"]["data"]["tool"], "tracedecay_search",
        "{missing}"
    );
    assert_eq!(
        missing["error"]["data"]["reason_code"], "missing_required_parameter",
        "{missing}"
    );
    assert_eq!(missing["error"]["data"]["retryable"], false, "{missing}");
    assert_eq!(
        missing["error"]["data"]["detail"], "missing required parameter: query",
        "{missing}"
    );

    fixture.harness.shutdown().await;
}

async fn search_json(server: &McpServer, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("search arguments")
        .insert("format".to_owned(), json!("json"));
    let result = handle_real_server_tool_call(server, "tracedecay_search", arguments).await;
    serde_json::from_str(extract_real_server_text(&result)).expect("search JSON payload")
}

async fn search_markdown(server: &McpServer, mut arguments: Value) -> String {
    // The shared call helper defaults an omitted format to JSON. Agents that
    // omit `format` receive markdown, so this path sets that rendering
    // explicitly rather than inheriting the helper's JSON default.
    arguments
        .as_object_mut()
        .expect("search arguments")
        .insert("format".to_owned(), json!("markdown"));
    let response = handle_real_server_tool_call_raw(server, "tracedecay_search", arguments).await;
    assert!(
        response["error"].is_null(),
        "markdown search must succeed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("markdown search text")
        .to_owned()
}

fn symbol_names(payload: &Value) -> Vec<&str> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search results: {payload}"))
        .iter()
        .map(|item| {
            item["display"]["name"]
                .as_str()
                .unwrap_or_else(|| panic!("search result display name: {item}"))
        })
        .collect()
}

fn assert_ready(payload: &Value) {
    assert_eq!(payload["freshness"], json!({"state": "fresh"}), "{payload}");
    assert_eq!(
        payload["coverage"],
        json!({
            "exact": "complete",
            "lexical": "complete",
            "graph": "complete",
            "recall": "full",
        }),
        "{payload}"
    );
    assert!(payload["status"].is_null(), "{payload}");
    assert!(payload["reason"].is_null(), "{payload}");
}
