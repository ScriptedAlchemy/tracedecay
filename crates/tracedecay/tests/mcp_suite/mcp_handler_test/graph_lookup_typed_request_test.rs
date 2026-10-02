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
    refusal_problem, warm_code_index_search,
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

async fn assert_refused(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
    message: &str,
) {
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
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    let problem = refusal_problem(&result);
    assert_eq!(problem["kind"], "invalid_request", "{tool_name}: {problem}");
    assert_eq!(
        problem["code"], "application.surface.invalid_request",
        "{tool_name}: {problem}"
    );
    assert_eq!(problem["message"], message, "{tool_name}: {problem}");
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

    assert_refused(
        &fixture,
        "tracedecay_grep",
        json!({"pattern": "TYPED_REQUEST_MARKER", "max_results": "5"}),
        "invalid arguments for tracedecay_grep: invalid type: string \"5\", expected u32",
    )
    .await;
    assert_refused(
        &fixture,
        "tracedecay_derives",
        json!({"qualified_name": "src/lib.rs::TypedWidget", "include_generated": true}),
        "invalid arguments for tracedecay_derives: unknown field `include_generated`, expected `node_id` or `qualified_name`",
    )
    .await;
    assert_refused(
        &fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": "fetch_typed_widget", "limit": "3"}),
        "invalid arguments for tracedecay_find_exact_symbol: invalid type: string \"3\", expected u32",
    )
    .await;

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn node_selectors_answer_node_id_and_refuse_the_id_spelling() {
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

    let widget = call_json(
        &fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": "TypedWidget", "format": "json"}),
    )
    .await;
    let widget_id = widget["matches"][0]["id"].clone();
    let fetch = call_json(
        &fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": "fetch_typed_widget", "format": "json"}),
    )
    .await;
    let fetch_id = fetch["matches"][0]["id"].clone();

    let derives = call_json(
        &fixture,
        "tracedecay_derives",
        json!({"node_id": widget_id, "format": "json"}),
    )
    .await;
    assert_eq!(
        (
            &derives[0]["name"],
            &derives[0]["derives"][0]["name"],
            &derives[0]["derives"][1]["name"],
        ),
        (&json!("TypedWidget"), &json!("Clone"), &json!("Debug"))
    );
    let signature = call_json(
        &fixture,
        "tracedecay_signature",
        json!({"node_id": fetch_id, "format": "json"}),
    )
    .await;
    assert_eq!(
        (&signature[0]["name"], &signature[0]["signature"]),
        (
            &json!("fetch_typed_widget"),
            &json!("pub fn fetch_typed_widget() -> u32")
        )
    );
    let test_map = call_json(
        &fixture,
        "tracedecay_test_map",
        json!({"node_id": fetch_id, "format": "json"}),
    )
    .await;
    assert_eq!(
        (
            &test_map["covered_symbols"],
            &test_map["uncovered_symbols"],
            &test_map["uncovered"][0]["name"],
            &test_map["uncovered"][0]["line"],
        ),
        (
            &json!(0),
            &json!(1),
            &json!("fetch_typed_widget"),
            &json!(6)
        )
    );

    for (tool_name, id, expected) in [
        (
            "tracedecay_derives",
            &widget_id,
            "`node_id` or `qualified_name`",
        ),
        (
            "tracedecay_signature",
            &fetch_id,
            "`node_id` or `qualified_name`",
        ),
        ("tracedecay_test_map", &fetch_id, "`file` or `node_id`"),
    ] {
        assert_refused(
            &fixture,
            tool_name,
            json!({"id": id}),
            &format!("invalid arguments for {tool_name}: unknown field `id`, expected {expected}"),
        )
        .await;
    }

    fixture.harness.shutdown().await;
}
