#![cfg(feature = "test-transport")]

//! `tracedecay_callers_for` over the real MCP `tools/call` path.
//!
//! The production fixture indexes `src/main.rs`, where `main` calls `helper`,
//! and `src/utils.rs`, where `helper` calls `format_greeting`. Caller
//! identities below are those symbols, not a property of the handler.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    ProductionCompositionFixture, extract_real_server_text, handle_real_server_tool_call_raw,
    production_composition_fixture, warm_code_index_search,
};

/// Accepted occurrence shape that is not in the fixture graph.
const UNMATCHED_OCCURRENCE: &str = "function:0000000000000000000000000000ffff";

struct IndexedGraph {
    server: Arc<McpServer>,
    _fixture: ProductionCompositionFixture,
}

impl IndexedGraph {
    async fn open() -> Self {
        let fixture = production_composition_fixture().await;
        let server = fixture
            .harness
            .server(&fixture.project_root)
            .expect("production graph server");
        warm_code_index_search(&server, "helper").await;
        Self {
            server,
            _fixture: fixture,
        }
    }
}

async fn call_callers_for(server: &McpServer, arguments: Value) -> Value {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_callers_for", arguments).await;
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "tracedecay_callers_for should succeed, got {response}"
    );
    let text = extract_real_server_text(&response["result"]);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_callers_for payload is not JSON ({error}): {text}")
    })
}

async fn symbol_id(server: &McpServer, name: &str, file: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20}),
    )
    .await;
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "exact-symbol lookup for {name} failed: {response}"
    );
    let payload: Value = serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("exact-symbol JSON");
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|item| item["name"] == name && item["file"] == file)
        })
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("expected {name} in {file}, got {payload}"))
        .to_owned()
}

fn caller_ids(payload: &Value, node_id: &str) -> Vec<String> {
    payload["callers"][node_id]
        .as_array()
        .unwrap_or_else(|| panic!("missing callers[{node_id}] in {payload}"))
        .iter()
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| panic!("caller id is not a string in {payload}"))
                .to_owned()
        })
        .collect()
}

async fn describe_callers(server: &McpServer, ids: &[String]) -> Vec<String> {
    let mut described = Vec::with_capacity(ids.len());
    for id in ids {
        let response =
            handle_real_server_tool_call_raw(server, "tracedecay_node", json!({"node_id": id}))
                .await;
        if response.get("error").is_some_and(|error| !error.is_null()) {
            described.push(format!(
                "{id} (node lookup failed: {})",
                response["error"]["message"]
            ));
            continue;
        }
        let node: Value = serde_json::from_str(extract_real_server_text(&response["result"]))
            .unwrap_or_else(|error| panic!("node payload for {id} is not JSON: {error}"));
        described.push(format!(
            "{id} {} {}",
            node["name"].as_str().unwrap_or("?"),
            node["file"].as_str().unwrap_or("?")
        ));
    }
    described
}

async fn assert_callers(server: &McpServer, actual: &[String], expected: &[String], context: &str) {
    if actual != expected {
        let described = describe_callers(server, actual).await;
        assert_eq!(
            actual, expected,
            "{context}; resolved callers: {described:?}"
        );
    }
}

async fn expect_rejection(server: &McpServer, arguments: Value, message: &str) {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_callers_for", arguments).await;
    assert_eq!(
        response["error"]["code"],
        json!(-32603),
        "JSON-RPC error code, response: {response}"
    );
    assert_eq!(
        response["error"]["message"].as_str(),
        Some(message),
        "JSON-RPC error message, response: {response}"
    );
}

#[tokio::test]
async fn callers_for_maps_each_node_to_its_direct_callers() {
    let graph = IndexedGraph::open().await;
    let server = &graph.server;
    let main_id = symbol_id(server, "main", "src/main.rs").await;
    let helper_id = symbol_id(server, "helper", "src/utils.rs").await;
    let format_id = symbol_id(server, "format_greeting", "src/utils.rs").await;

    let payload = call_callers_for(
        server,
        json!({
            "node_ids": [
                helper_id.clone(),
                format_id.clone(),
                main_id.clone(),
                UNMATCHED_OCCURRENCE
            ]
        }),
    )
    .await;

    assert_eq!(payload["truncated"], json!(false), "{payload}");
    assert_eq!(payload["max_per_item"], json!(1000), "{payload}");

    let helper_callers = caller_ids(&payload, &helper_id);
    let format_callers = caller_ids(&payload, &format_id);
    assert_callers(
        server,
        &helper_callers,
        &[main_id.clone()],
        "helper is called by main",
    )
    .await;
    assert_callers(
        server,
        &format_callers,
        &[helper_id.clone()],
        "format_greeting is called by helper",
    )
    .await;
    assert_eq!(
        caller_ids(&payload, &main_id),
        Vec::<String>::new(),
        "main has no callers: {payload}"
    );
    assert_eq!(
        caller_ids(&payload, UNMATCHED_OCCURRENCE),
        Vec::<String>::new(),
        "an unknown occurrence is present and empty: {payload}"
    );
    let mut keys: Vec<&str> = payload["callers"]
        .as_object()
        .expect("callers map")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected_keys = vec![
        helper_id.as_str(),
        format_id.as_str(),
        main_id.as_str(),
        UNMATCHED_OCCURRENCE,
    ];
    expected_keys.sort_unstable();
    assert_eq!(
        keys, expected_keys,
        "every requested id is a key: {payload}"
    );

    let explicit_calls = call_callers_for(
        server,
        json!({"node_ids": [format_id.clone()], "kind": "calls"}),
    )
    .await;
    assert_eq!(
        explicit_calls["truncated"],
        json!(false),
        "{explicit_calls}"
    );
    assert_eq!(
        explicit_calls["max_per_item"],
        json!(1000),
        "{explicit_calls}"
    );
    assert_eq!(
        caller_ids(&explicit_calls, &format_id),
        vec![helper_id.clone()],
        "kind calls is the same direct caller: {explicit_calls}"
    );

    let implements = call_callers_for(
        server,
        json!({
            "node_ids": [format_id.clone(), helper_id.clone()],
            "kind": "implements"
        }),
    )
    .await;
    assert_eq!(implements["truncated"], json!(false), "{implements}");
    assert_eq!(implements["max_per_item"], json!(1000), "{implements}");
    assert_eq!(
        caller_ids(&implements, &format_id),
        Vec::<String>::new(),
        "format_greeting has no implements edges: {implements}"
    );
    assert_eq!(
        caller_ids(&implements, &helper_id),
        Vec::<String>::new(),
        "helper has no implements edges: {implements}"
    );

    let anchor = format!("code-symbol:{format_id}");
    let anchored = call_callers_for(server, json!({"node_ids": [anchor.clone()]})).await;
    assert_eq!(anchored["truncated"], json!(false), "{anchored}");
    assert_eq!(anchored["max_per_item"], json!(1000), "{anchored}");
    assert_eq!(
        caller_ids(&anchored, &anchor),
        vec![helper_id.clone()],
        "a code-symbol anchor unwraps to the same caller set and keeps the request string as the key: {anchored}"
    );

    let truncated = call_callers_for(
        server,
        json!({"node_ids": [helper_id.clone()], "max_per_item": 0}),
    )
    .await;
    assert_eq!(truncated["truncated"], json!(true), "{truncated}");
    assert_eq!(truncated["max_per_item"], json!(0), "{truncated}");
    assert_eq!(
        caller_ids(&truncated, &helper_id),
        Vec::<String>::new(),
        "max_per_item 0 drops main, the caller shown without the cap: {truncated}"
    );

    let clamped = call_callers_for(
        server,
        json!({"node_ids": [helper_id.clone()], "max_per_item": 100_000}),
    )
    .await;
    assert_eq!(clamped["max_per_item"], json!(10_000), "{clamped}");
    assert_eq!(clamped["truncated"], json!(false), "{clamped}");
    assert_eq!(
        caller_ids(&clamped, &helper_id),
        vec![main_id],
        "a cap above 10000 is clamped and still returns main: {clamped}"
    );
}

#[tokio::test]
async fn callers_for_rejects_empty_unknown_kind_and_blank_ids() {
    let graph = IndexedGraph::open().await;
    let server = &graph.server;

    expect_rejection(
        server,
        json!({"node_ids": []}),
        "tool execution failed: config error: callers_for requires non-empty node_ids",
    )
    .await;
    expect_rejection(
        server,
        json!({}),
        "tool execution failed: config error: callers_for requires non-empty node_ids",
    )
    .await;
    expect_rejection(
        server,
        json!({
            "node_ids": [UNMATCHED_OCCURRENCE],
            "kind": "not_a_real_kind"
        }),
        "tool execution failed: config error: unknown edge kind: not_a_real_kind",
    )
    .await;
    expect_rejection(
        server,
        json!({"node_ids": [""]}),
        "tool execution failed: config error: invalid parameter: node_id must not be empty",
    )
    .await;
    expect_rejection(
        server,
        json!({"node_ids": ["code-chunk:not-a-symbol"]}),
        "tool execution failed: config error: invalid parameter: node_id `code-chunk:not-a-symbol` is an evidence anchor, not a graph symbol occurrence",
    )
    .await;
}
