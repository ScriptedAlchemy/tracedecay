//! Graph lookup and content-search tools decode their arguments against the typed
//! request over the production MCP `tools/call` path.
//!
//! Each refusal is paired with a valid call's literal answer on the same
//! fixture, so a refusal cannot pass by the tool refusing everything.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_json, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const SOURCE: &str = "#[derive(Debug, Clone)]\npub struct TypedWidget {\n    pub id: u32,\n}\n\npub fn fetch_typed_widget() -> u32 {\n    TYPED_REQUEST_MARKER\n}\n";

async fn call_json(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    extract_json(
        &response
            .result
            .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result")),
    )
}

async fn call_error(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(response.result.is_none(), "{:?}", response.result);
    response
        .error
        .unwrap_or_else(|| panic!("{tool_name} must refuse the request"))
        .message
}

#[tokio::test]
async fn graph_lookup_tools_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), SOURCE).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production graph server");
    warm_code_index_search(&server, "fetch_typed_widget").await;
    drop(server);

    let grep = call_json(
        &fixture,
        "tracedecay_grep",
        json!({"pattern": "TYPED_REQUEST_MARKER", "fixed_strings": true, "format": "json"}),
    )
    .await;
    assert_eq!(
        (
            &grep["match_count"],
            &grep["results"][0]["file"],
            &grep["results"][0]["line"],
            &grep["results"][0]["text"],
            &grep["results"][0]["symbol"],
        ),
        (
            &json!(1),
            &json!("src/lib.rs"),
            &json!(7),
            &json!("    TYPED_REQUEST_MARKER"),
            &json!("fetch_typed_widget"),
        )
    );
    let derives = call_json(
        &fixture,
        "tracedecay_derives",
        json!({"qualified_name": "src/lib.rs::TypedWidget", "format": "json"}),
    )
    .await;
    assert_eq!(
        (
            &derives[0]["name"],
            &derives[0]["line"],
            &derives[0]["derives"][0]["name"],
            &derives[0]["derives"][1]["name"],
        ),
        (
            &json!("TypedWidget"),
            &json!(2),
            &json!("Clone"),
            &json!("Debug"),
        )
    );
    let exact = call_json(
        &fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": "fetch_typed_widget", "format": "json"}),
    )
    .await;
    assert_eq!(
        (
            &exact["count"],
            &exact["matches"][0]["qualified_name"],
            &exact["matches"][0]["line"],
            &exact["matches"][0]["signature"],
        ),
        (
            &json!(1),
            &json!("src/lib.rs::fetch_typed_widget"),
            &json!(6),
            &json!("pub fn fetch_typed_widget() -> u32"),
        )
    );

    assert_eq!(
        call_error(
            &fixture,
            "tracedecay_grep",
            json!({"pattern": "TYPED_REQUEST_MARKER", "max_results": "5"}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_grep: invalid type: string \"5\", expected u32"
    );
    assert_eq!(
        call_error(
            &fixture,
            "tracedecay_derives",
            json!({"qualified_name": "src/lib.rs::TypedWidget", "include_generated": true}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_derives: unknown field `include_generated`, expected one of `id`, `node_id`, `qualified_name`"
    );
    assert_eq!(
        call_error(
            &fixture,
            "tracedecay_find_exact_symbol",
            json!({"name": "fetch_typed_widget", "limit": "3"}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_find_exact_symbol: invalid type: string \"3\", expected u32"
    );

    fixture.harness.shutdown().await;
}
