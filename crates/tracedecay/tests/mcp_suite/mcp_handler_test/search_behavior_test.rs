//! `tracedecay_search` as an agent calls it: one concrete query in, the symbol
//! the agent would open out. The production MCP server is the subject.

#![cfg(feature = "test-transport")]

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;

const LEDGER_SOURCE: &str = "\
pub fn ledger_post_entry(amount: u32) -> u32 {\n    \
    amount\n\
}\n\
\n\
pub fn unrelated_balance() -> u32 {\n    \
    0\n\
}\n";

fn search_displays(payload: &Value) -> Vec<Value> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search payload has no results array: {payload}"))
        .iter()
        .map(|item| item["display"].clone())
        .collect()
}

#[tokio::test]
async fn search_returns_the_named_symbol_and_rejects_a_missing_query() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("search fixture sources");
        fs::write(project.join("src/ledger.rs"), LEDGER_SOURCE).expect("write ledger source");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production search server");

    let missing = handle_real_server_tool_call_raw(&server, "tracedecay_search", json!({})).await;
    assert_eq!(missing["error"]["code"], -32602, "{missing}");
    assert_eq!(
        missing["error"]["message"], "missing required parameter: query",
        "{missing}"
    );
    assert_eq!(
        missing["error"]["data"],
        json!({
            "tool": "tracedecay_search",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: query",
        }),
        "{missing}"
    );

    warm_code_index_search(&server, "ledger_post_entry").await;

    // The warm-up proves the generation is current and every lane complete.
    // The seat can still have a continuation pass in flight after that, and
    // a search taken while it runs reports `verifying`. The literal payload
    // below is the settled one, so take the first search that reports it.
    let mut hit = Value::Null;
    for _ in 0..40 {
        let response = handle_real_server_tool_call(
            &server,
            "tracedecay_search",
            json!({
                "query": "ledger_post_entry",
                "prefer_symbol": true,
                "format": "json",
            }),
        )
        .await;
        hit = serde_json::from_str(extract_real_server_text(&response)).expect("search JSON");
        if hit["freshness"] == json!({ "state": "fresh" }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(hit["freshness"], json!({ "state": "fresh" }), "{hit}");
    assert_eq!(
        hit["coverage"],
        json!({
            "exact": "complete",
            "lexical": "complete",
            "graph": "complete",
            "recall": "full",
        }),
        "{hit}"
    );
    assert!(
        hit["status"].is_null(),
        "a completed search is not unavailable: {hit}"
    );
    assert_eq!(
        search_displays(&hit),
        vec![json!({
            "name": "ledger_post_entry",
            "qualified_name": "src/ledger.rs::ledger_post_entry",
            "kind": "function",
            "path": "src/ledger.rs",
        })],
        "{hit}"
    );
    assert_eq!(hit["results"][0]["final_ordinal"], 0, "{hit}");
    assert_eq!(
        hit["results"][0]["candidate"]["exact_class"], "exact_message",
        "{hit}"
    );
    assert_eq!(
        hit["lexical_routes"],
        json!([
            { "route": "query", "label": "query" },
            {
                "route": "preferred_symbol",
                "tokens": ["ledger_post_entry"],
                "label": "symbol:ledger_post_entry",
            },
            {
                "route": "identifier_split",
                "strict_query": "ledger_post_entry",
                "terms": ["ledger", "post", "entry"],
                "label": "split:ledger|post|entry",
            },
        ]),
        "{hit}"
    );

    let rendered = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "ledger_post_entry",
            "prefer_symbol": true,
            "format": "markdown",
        }),
    )
    .await;
    let rendered = extract_real_server_text(&rendered);
    let mut lines = rendered.lines();
    assert_eq!(lines.next(), Some("freshness: fresh"), "{rendered}");
    assert_eq!(lines.next(), Some("## Search Results"), "{rendered}");
    let bullet = lines
        .find(|line| line.starts_with("- **"))
        .unwrap_or_else(|| panic!("markdown search has no result bullet: {rendered}"));
    let (head, rest) = bullet
        .split_once(" · utility ")
        .unwrap_or_else(|| panic!("markdown bullet has no utility suffix: {bullet} in {rendered}"));
    assert_eq!(
        head,
        "- **ledger_post_entry** (function, exact_message), rank 1"
    );
    let via = rest
        .split_once(" · via ")
        .map(|(_, via)| via)
        .unwrap_or_else(|| panic!("markdown bullet has no route suffix: {bullet}"));
    // Each matching chunk discloses the same three routes. The bullet lists
    // every disclosure the host receives, in rank order, without collapsing
    // them.
    assert_eq!(
        via,
        "query, symbol:ledger_post_entry, split:ledger|post|entry, query, symbol:ledger_post_entry, split:ledger|post|entry"
    );

    let miss = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({ "query": "qxqvnomatch", "format": "json" }),
    )
    .await;
    let miss: Value = serde_json::from_str(extract_real_server_text(&miss)).expect("miss JSON");
    assert_eq!(miss["results"], json!([]), "{miss}");
    assert_eq!(miss["freshness"], json!({ "state": "fresh" }), "{miss}");
    assert_eq!(miss["coverage"]["recall"], "full", "{miss}");

    let anchored = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "qxqvnomatch",
            "lexical_anchors": ["ledger_post_entry"],
            "format": "json",
        }),
    )
    .await;
    let anchored: Value =
        serde_json::from_str(extract_real_server_text(&anchored)).expect("anchor JSON");
    assert_eq!(
        search_displays(&anchored),
        vec![json!({
            "name": "ledger_post_entry",
            "qualified_name": "src/ledger.rs::ledger_post_entry",
            "kind": "function",
            "path": "src/ledger.rs",
        })],
        "an anchor must rank the named symbol when the query text does not: {anchored}"
    );
    assert_eq!(
        anchored["lexical_routes"],
        json!([
            { "route": "query", "label": "query" },
            {
                "route": "anchor",
                "anchor": "ledger_post_entry",
                "label": "anchor:ledger_post_entry",
            },
        ]),
        "{anchored}"
    );

    fixture.harness.shutdown().await;
}
