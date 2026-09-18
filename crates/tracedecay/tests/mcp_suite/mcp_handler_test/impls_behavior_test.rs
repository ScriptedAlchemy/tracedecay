#![cfg(feature = "test-transport")]

//! `tracedecay_impls` through the production MCP `tools/call` path.
//!
//! The fixture is a small Rust crate with local trait impls, an inherent
//! impl, a cross-file impl (`badge.rs` imports `Show`), and an impl whose
//! trait (`std::fmt::Display`) is not a project symbol. Expected rows are
//! the census a caller observes, not extractor internals.

use crate::support::{
    ProductionCompositionFixture, extract_text, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;

const LIB_RS: &str = include_str!("../../fixtures/impls_behavior/src/lib.rs");
const BADGE_RS: &str = include_str!("../../fixtures/impls_behavior/src/badge.rs");

struct ImplsFixture {
    production: ProductionCompositionFixture,
}

fn impl_row(
    ty: &str,
    qualified_name: &str,
    trait_name: Option<&str>,
    trait_qualified_name: Option<&str>,
    file: &str,
    start_line: u64,
    end_line: u64,
    signature: &str,
) -> Value {
    json!({
        "type": ty,
        "qualified_name": qualified_name,
        "trait": trait_name,
        "trait_qualified_name": trait_qualified_name,
        "file": file,
        "start_line": start_line,
        "end_line": end_line,
        "signature": signature,
    })
}

fn badge_show() -> Value {
    impl_row(
        "Badge",
        "src/badge.rs::Badge",
        Some("Show"),
        Some("src/lib.rs::Show"),
        "src/badge.rs",
        5,
        9,
        "impl Show for Badge",
    )
}

fn widget_inherent() -> Value {
    impl_row(
        "Widget",
        "src/lib.rs::Widget",
        None,
        None,
        "src/lib.rs",
        7,
        11,
        "impl Widget",
    )
}

fn widget_show() -> Value {
    impl_row(
        "Widget",
        "src/lib.rs::Widget",
        Some("Show"),
        Some("src/lib.rs::Show"),
        "src/lib.rs",
        17,
        21,
        "impl Show for Widget",
    )
}

fn widget_display() -> Value {
    impl_row(
        "Widget",
        "src/lib.rs::Widget",
        None,
        None,
        "src/lib.rs",
        23,
        27,
        "impl std::fmt::Display for Widget",
    )
}

fn counter_show() -> Value {
    impl_row(
        "Counter",
        "src/lib.rs::Counter",
        Some("Show"),
        Some("src/lib.rs::Show"),
        "src/lib.rs",
        31,
        35,
        "impl Show for Counter",
    )
}

fn show_rows() -> Value {
    json!([badge_show(), widget_show(), counter_show()])
}

fn widget_rows() -> Value {
    json!([widget_inherent(), widget_show(), widget_display()])
}

fn all_rows() -> Value {
    json!([
        badge_show(),
        widget_inherent(),
        widget_show(),
        widget_display(),
        counter_show()
    ])
}

fn stable_rows(payload: &Value) -> Value {
    let mut rows = payload["impls"]
        .as_array()
        .unwrap_or_else(|| panic!("tracedecay_impls response has no impls array: {payload}"))
        .iter()
        .map(|item| {
            json!({
                "type": item["type"],
                "qualified_name": item["qualified_name"],
                "trait": item["trait"],
                "trait_qualified_name": item["trait_qualified_name"],
                "file": item["file"],
                "start_line": item["start_line"],
                "end_line": item["end_line"],
                "signature": item["signature"],
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left["file"]
            .as_str()
            .unwrap_or("")
            .cmp(right["file"].as_str().unwrap_or(""))
            .then(
                left["start_line"]
                    .as_u64()
                    .cmp(&right["start_line"].as_u64()),
            )
            .then(
                left["signature"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(right["signature"].as_str().unwrap_or("")),
            )
    });
    Value::Array(rows)
}

fn assert_impls(payload: &Value, count: u64, truncated: bool, expected: Value) {
    assert_eq!(
        payload["count"],
        json!(count),
        "tracedecay_impls count: {payload}"
    );
    assert_eq!(
        payload["truncated"],
        json!(truncated),
        "tracedecay_impls truncated: {payload}"
    );
    assert_eq!(
        stable_rows(payload),
        expected,
        "tracedecay_impls rows: {payload}"
    );
}

async fn open_impls_fixture() -> ImplsFixture {
    let production = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
        fs::write(project.join("src/badge.rs"), BADGE_RS).unwrap();
    })
    .await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production graph server");
    warm_code_index_search(&server, "Show").await;
    ImplsFixture { production }
}

async fn call_impls(fixture: &ImplsFixture, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("tool arguments are an object")
        .insert("format".to_owned(), json!("json"));
    let server = fixture
        .production
        .harness
        .server(&fixture.production.project_root)
        .expect("production graph server");
    let response = handle_real_server_tool_call_raw(&server, "tracedecay_impls", arguments).await;
    assert!(
        response["error"].is_null(),
        "tracedecay_impls MCP call failed: {response}"
    );
    let text = extract_text(&response["result"]);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_impls did not return JSON ({error}): {text}"))
}

#[tokio::test]
async fn tracedecay_impls_lists_filters_and_truncates_impl_blocks() {
    let fixture = open_impls_fixture().await;

    let census = call_impls(&fixture, json!({})).await;
    assert_impls(&census, 5, false, all_rows());

    let by_trait = call_impls(&fixture, json!({"trait": "Show"})).await;
    assert_impls(&by_trait, 3, false, show_rows());

    let by_trait_case = call_impls(&fixture, json!({"trait": "show"})).await;
    assert_impls(&by_trait_case, 3, false, show_rows());

    let by_qualified_trait = call_impls(&fixture, json!({"trait": "src/lib.rs::Show"})).await;
    assert_impls(&by_qualified_trait, 3, false, show_rows());

    let by_type = call_impls(&fixture, json!({"type": "Widget"})).await;
    assert_impls(&by_type, 3, false, widget_rows());

    let by_qualified_type = call_impls(&fixture, json!({"type": "src/lib.rs::Counter"})).await;
    assert_impls(&by_qualified_type, 1, false, json!([counter_show()]));

    let both = call_impls(&fixture, json!({"trait": "Show", "type": "Widget"})).await;
    assert_impls(&both, 1, false, json!([widget_show()]));

    let missing_trait = call_impls(&fixture, json!({"trait": "MissingTrait"})).await;
    assert_impls(&missing_trait, 0, false, json!([]));

    let trait_name_is_not_a_type = call_impls(&fixture, json!({"type": "Show"})).await;
    assert_impls(&trait_name_is_not_a_type, 0, false, json!([]));

    let unresolved_display = call_impls(&fixture, json!({"trait": "Display"})).await;
    assert_impls(&unresolved_display, 0, false, json!([]));

    let limited = call_impls(&fixture, json!({"trait": "Show", "limit": 1})).await;
    // Occurrence order is not part of the tool contract, so the kept row is
    // one of the Show impls rather than a particular catalog key.
    assert_eq!(limited["count"], json!(1), "limit payload: {limited}");
    assert_eq!(
        limited["truncated"],
        json!(true),
        "limit payload: {limited}"
    );
    let limited_rows = stable_rows(&limited);
    assert_eq!(limited_rows.as_array().map(Vec::len), Some(1));
    let kept = &limited_rows[0];
    assert!(
        show_rows()
            .as_array()
            .expect("show rows")
            .iter()
            .any(|row| row == kept),
        "limit 1 kept {kept}, which is not a Show impl"
    );

    fixture.production.harness.shutdown().await;
}
