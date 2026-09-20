//! Behavior of `tracedecay_largest` through the production MCP `tools/call` path.
//!
//! Expected spans are the 1-based inclusive ranges of the sources written
//! below. They are not read back from the tool.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

const LIB_RS: &str = r#"pub fn tiny() -> u32 {
    1
}

pub struct Wide {
    first: u32,
    second: u32,
    third: u32,
    fourth: u32,
}

pub fn medium() -> u32 {
    let a = 1;
    let b = 2;
    a + b
}

pub fn huge() -> u32 {
    let one = 1;
    let two = 2;
    let three = 3;
    let four = 4;
    let five = 5;
    one + two + three + four + five
}
"#;

const ELSEWHERE_RS: &str = r#"pub fn elsewhere_giant() -> u32 {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
    let e = 5;
    let f = 6;
    let g = 7;
    let h = 8;
    let i = 9;
    a + b + c + d + e + f + g + h + i
}
"#;

fn visible_ranking(payload: &Value) -> Vec<Value> {
    payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking array missing: {payload}"))
        .iter()
        .map(|item| {
            json!({
                "name": item["name"],
                "kind": item["kind"],
                "file": item["file"],
                "start_line": item["start_line"],
                "end_line": item["end_line"],
                "lines": item["lines"],
            })
        })
        .collect()
}

async fn call_largest(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_largest", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.error.is_none(),
        "tracedecay_largest failed: {:?}",
        response.error
    );
    let result = response.result.expect("tracedecay_largest result");
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "{result}"
    );
    let text = extract_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("largest JSON ({error}): {text}"))
}

async fn call_largest_text(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_largest", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.error.is_none(),
        "tracedecay_largest failed: {:?}",
        response.error
    );
    let result = response.result.expect("tracedecay_largest result");
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "{result}"
    );
    extract_text(&result).to_owned()
}

#[tokio::test]
async fn largest_ranks_by_inclusive_line_span() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
        fs::write(project.join("src/elsewhere.rs"), ELSEWHERE_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let functions = call_largest(
        &fixture,
        json!({"node_kind": "function", "path": "src/lib.rs", "format": "json"}),
    )
    .await;
    assert_eq!(functions["node_kind_filter"], "function", "{functions}");
    assert_eq!(functions["result_count"], 3, "{functions}");
    assert_eq!(
        visible_ranking(&functions),
        vec![
            json!({
                "name": "huge",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 18,
                "end_line": 25,
                "lines": 8
            }),
            json!({
                "name": "medium",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 12,
                "end_line": 16,
                "lines": 5
            }),
            json!({
                "name": "tiny",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 1,
                "end_line": 3,
                "lines": 3
            }),
        ],
        "{functions}"
    );

    let limited = call_largest(
        &fixture,
        json!({
            "node_kind": "function",
            "path": "src/lib.rs",
            "limit": 2,
            "format": "json"
        }),
    )
    .await;
    assert_eq!(limited["result_count"], 2, "{limited}");
    assert_eq!(
        visible_ranking(&limited),
        vec![
            json!({
                "name": "huge",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 18,
                "end_line": 25,
                "lines": 8
            }),
            json!({
                "name": "medium",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 12,
                "end_line": 16,
                "lines": 5
            }),
        ],
        "{limited}"
    );

    let structs = call_largest(
        &fixture,
        json!({"node_kind": "struct", "path": "src/lib.rs", "format": "json"}),
    )
    .await;
    assert_eq!(structs["node_kind_filter"], "struct", "{structs}");
    assert_eq!(structs["result_count"], 1, "{structs}");
    assert_eq!(
        visible_ranking(&structs),
        vec![json!({
            "name": "Wide",
            "kind": "struct",
            "file": "src/lib.rs",
            "start_line": 5,
            "end_line": 10,
            "lines": 6
        })],
        "{structs}"
    );

    let in_file = call_largest(
        &fixture,
        json!({"path": "src/lib.rs", "limit": 1, "format": "json"}),
    )
    .await;
    assert_eq!(in_file["node_kind_filter"], Value::Null, "{in_file}");
    assert_eq!(in_file["result_count"], 1, "{in_file}");
    assert_eq!(
        visible_ranking(&in_file),
        vec![json!({
            "name": "huge",
            "kind": "function",
            "file": "src/lib.rs",
            "start_line": 18,
            "end_line": 25,
            "lines": 8
        })],
        "the 8-line function outranks the 6-line struct: {in_file}"
    );

    let anywhere = call_largest(&fixture, json!({"limit": 1, "format": "json"})).await;
    assert_eq!(anywhere["result_count"], 1, "{anywhere}");
    assert_eq!(
        visible_ranking(&anywhere),
        vec![json!({
            "name": "elsewhere_giant",
            "kind": "function",
            "file": "src/elsewhere.rs",
            "start_line": 1,
            "end_line": 12,
            "lines": 12
        })],
        "{anywhere}"
    );

    let elsewhere = call_largest(
        &fixture,
        json!({
            "node_kind": "function",
            "path": "src/elsewhere.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(elsewhere["result_count"], 1, "{elsewhere}");
    assert_eq!(
        visible_ranking(&elsewhere),
        vec![json!({
            "name": "elsewhere_giant",
            "kind": "function",
            "file": "src/elsewhere.rs",
            "start_line": 1,
            "end_line": 12,
            "lines": 12
        })],
        "{elsewhere}"
    );

    let absent = call_largest(
        &fixture,
        json!({"path": "src/does-not-exist.rs", "format": "json"}),
    )
    .await;
    assert_eq!(
        absent,
        json!({
            "node_kind_filter": null,
            "result_count": 0,
            "ranking": []
        }),
        "a path with no symbols is an empty ranking, not a hidden hit: {absent}"
    );

    let markdown = call_largest_text(
        &fixture,
        json!({"node_kind": "function", "path": "src/lib.rs", "limit": 1}),
    )
    .await;
    assert!(
        markdown.contains("**node_kind_filter:** function\n"),
        "{markdown}"
    );
    assert!(markdown.contains("**result_count:** 1\n"), "{markdown}");
    assert!(
        markdown.contains("- **huge**\n  **kind:** function\n  **file:** src/lib.rs\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("  **end_line:** 25\n  **lines:** 8\n  **start_line:** 18\n"),
        "{markdown}"
    );
    assert!(!markdown.contains("medium"), "{markdown}");
    assert!(!markdown.contains("elsewhere_giant"), "{markdown}");

    fixture.harness.shutdown().await;
}
