#![cfg(feature = "test-transport")]

//! `tracedecay_implementations` as an MCP client observes it.
//!
//! Calls go through the production server's `tools/call` path. Expected
//! records are the source text of this fixture, not values read back out of
//! the handler.

use crate::support::{
    ProductionCompositionFixture, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;

const LIB_RS: &str = r#"pub trait Widget {
    fn paint(&self) -> &'static str;
}

pub struct Solid;

impl Widget for Solid {
    fn paint(&self) -> &'static str {
        "solid"
    }
}

pub trait Unused {}

pub struct Outline;

impl Widget for Outline {
    fn paint(&self) -> &'static str {
        "outline"
    }
}

pub fn paint() -> &'static str {
    "free"
}

impl Solid {
    fn weight(&self) -> u8 {
        1
    }
}

mod decoy;
"#;

const DECOY_RS: &str = r#"pub fn Widget() -> u8 {
    7
}
"#;

const VIEW_TS: &str = r#"interface Drawable {
  draw(): string;
}

class Canvas implements Drawable {
  draw(): string {
    return "canvas";
  }
}

class Sketch {
  draw(): string {
    return "sketch";
  }
}
"#;

#[tokio::test]
async fn implementations_returns_literal_bodies_for_trait_interface_and_method() {
    let fixture = indexed_project().await;

    let widget = call_json(&fixture, json!({"trait": "Widget", "format": "json"})).await;
    assert_eq!(
        ordered(widget, &["type", "file", "line"]),
        json!({
            "match_count": 2,
            "implementations": [outline_widget(), solid_widget()]
        }),
        "trait lookup must return only Widget implementors and their method bodies"
    );

    let unused = call_json(&fixture, json!({"trait": "Unused", "format": "json"})).await;
    assert_eq!(
        unused,
        json!({"match_count": 0, "implementations": []}),
        "a trait with no implementors is an empty result, not a missing-name message"
    );

    let missing_trait =
        call_text(&fixture, json!({"trait": "AbsentTrait", "format": "json"})).await;
    assert_eq!(
        missing_trait,
        "No trait or interface named 'AbsentTrait' found."
    );

    let paint = call_json(&fixture, json!({"method": "paint", "format": "json"})).await;
    assert_eq!(
        ordered(paint, &["qualified_name"]),
        json!({
            "match_count": 4,
            "implementations": [
                method_body(
                    "src/lib.rs::<Outline as Widget>::paint",
                    "method",
                    "src/lib.rs",
                    18,
                    20,
                    "fn paint(&self) -> &'static str",
                    "    fn paint(&self) -> &'static str {\n        \"outline\"\n    }",
                ),
                method_body(
                    "src/lib.rs::<Solid as Widget>::paint",
                    "method",
                    "src/lib.rs",
                    8,
                    10,
                    "fn paint(&self) -> &'static str",
                    "    fn paint(&self) -> &'static str {\n        \"solid\"\n    }",
                ),
                method_body(
                    "src/lib.rs::Widget::paint",
                    "method",
                    "src/lib.rs",
                    2,
                    2,
                    "fn paint(&self) -> &'static str",
                    "    fn paint(&self) -> &'static str;",
                ),
                method_body(
                    "src/lib.rs::paint",
                    "function",
                    "src/lib.rs",
                    23,
                    25,
                    "pub fn paint() -> &'static str",
                    "pub fn paint() -> &'static str {\n    \"free\"\n}",
                ),
            ]
        }),
        "method lookup must return every paint body, including the trait declaration and free function"
    );

    let weight = call_json(&fixture, json!({"method": "weight", "format": "json"})).await;
    assert_eq!(
        weight,
        json!({
            "match_count": 1,
            "implementations": [
                method_body(
                    "src/lib.rs::Solid::weight",
                    "method",
                    "src/lib.rs",
                    28,
                    30,
                    "fn weight(&self) -> u8",
                    "    fn weight(&self) -> u8 {\n        1\n    }",
                )
            ]
        })
    );

    let same_name_function =
        call_json(&fixture, json!({"method": "Widget", "format": "json"})).await;
    assert_eq!(
        same_name_function,
        json!({
            "match_count": 1,
            "implementations": [
                method_body(
                    "src/decoy.rs::Widget",
                    "function",
                    "src/decoy.rs",
                    1,
                    3,
                    "pub fn Widget() -> u8",
                    "pub fn Widget() -> u8 {\n    7\n}",
                )
            ]
        }),
        "a function that only shares the trait's name is a method hit, not an implementor"
    );

    let missing_method = call_text(
        &fixture,
        json!({"method": "absent_method", "format": "json"}),
    )
    .await;
    assert_eq!(
        missing_method,
        "No function or method named 'absent_method' found."
    );

    let limited = call_json(
        &fixture,
        json!({"trait": "Widget", "limit": 1, "format": "json"}),
    )
    .await;
    assert_eq!(limited["match_count"], 1);
    assert_eq!(limited["implementations"].as_array().map(Vec::len), Some(1));
    let only = &limited["implementations"][0];
    assert!(
        only == &solid_widget() || only == &outline_widget(),
        "limit 1 must return one complete Widget implementor, got {only}"
    );

    let clamped = call_json(
        &fixture,
        json!({"trait": "Widget", "limit": 0, "format": "json"}),
    )
    .await;
    assert_eq!(clamped["match_count"], 1);
    assert_eq!(clamped["implementations"].as_array().map(Vec::len), Some(1));
    let clamped_only = &clamped["implementations"][0];
    assert!(
        clamped_only == &solid_widget() || clamped_only == &outline_widget(),
        "limit 0 is clamped to one complete Widget implementor, got {clamped_only}"
    );

    let drawable = call_json(&fixture, json!({"trait": "Drawable", "format": "json"})).await;
    assert_eq!(
        ordered(drawable, &["type", "file", "line"]),
        json!({
            "match_count": 1,
            "implementations": [{
                "type": "Canvas",
                "qualified_name": "src/view.ts::Canvas",
                "kind": "class",
                "file": "src/view.ts",
                "line": 5,
                "trait": "src/view.ts::Drawable",
                "methods": [{
                    "name": "draw",
                    "kind": "method",
                    "line": 6,
                    "signature": "draw(): string",
                    "body": "  draw(): string {\n    return \"canvas\";\n  }"
                }]
            }]
        }),
        "interface lookup must return the implementing class body and not Sketch"
    );

    let draw = call_json(&fixture, json!({"method": "draw", "format": "json"})).await;
    assert_eq!(
        ordered(draw, &["qualified_name"]),
        json!({
            "match_count": 3,
            "implementations": [
                method_body(
                    "src/view.ts::Canvas::draw",
                    "method",
                    "src/view.ts",
                    6,
                    8,
                    "draw(): string",
                    "  draw(): string {\n    return \"canvas\";\n  }",
                ),
                method_body(
                    "src/view.ts::Drawable::draw",
                    "method",
                    "src/view.ts",
                    2,
                    2,
                    "draw(): string",
                    "  draw(): string;",
                ),
                method_body(
                    "src/view.ts::Sketch::draw",
                    "method",
                    "src/view.ts",
                    12,
                    14,
                    "draw(): string",
                    "  draw(): string {\n    return \"sketch\";\n  }",
                ),
            ]
        })
    );

    let missing = call_raw(&fixture, json!({})).await;
    assert_eq!(
        missing["error"]["code"], -32602,
        "missing selector should be invalid params, got {missing}"
    );
    assert_eq!(
        missing["error"]["message"],
        "missing required parameter: 'trait' or 'method'"
    );
    assert_eq!(
        missing["error"]["data"]["reason_code"],
        "missing_required_parameter"
    );
    assert_eq!(
        missing["error"]["data"]["tool"],
        "tracedecay_implementations"
    );
    assert_eq!(missing["error"]["data"]["retryable"], false);

    let conflict = call_raw(&fixture, json!({"trait": "Widget", "method": "paint"})).await;
    assert_eq!(conflict["error"]["code"], -32603, "{conflict}");
    assert_eq!(
        conflict["error"]["message"],
        "tool execution failed: config error: tracedecay_implementations: 'trait' and 'method' are mutually exclusive"
    );
    assert_eq!(
        conflict["error"]["data"]["tool"],
        "tracedecay_implementations"
    );

    fixture.harness.shutdown().await;
}

fn solid_widget() -> Value {
    json!({
        "type": "Solid",
        "qualified_name": "src/lib.rs::Solid",
        "kind": "impl",
        "file": "src/lib.rs",
        "line": 7,
        "trait": "src/lib.rs::Widget",
        "methods": [{
            "name": "paint",
            "kind": "method",
            "line": 8,
            "signature": "fn paint(&self) -> &'static str",
            "body": "    fn paint(&self) -> &'static str {\n        \"solid\"\n    }"
        }]
    })
}

fn outline_widget() -> Value {
    json!({
        "type": "Outline",
        "qualified_name": "src/lib.rs::Outline",
        "kind": "impl",
        "file": "src/lib.rs",
        "line": 17,
        "trait": "src/lib.rs::Widget",
        "methods": [{
            "name": "paint",
            "kind": "method",
            "line": 18,
            "signature": "fn paint(&self) -> &'static str",
            "body": "    fn paint(&self) -> &'static str {\n        \"outline\"\n    }"
        }]
    })
}

fn method_body(
    qualified_name: &str,
    kind: &str,
    file: &str,
    line: u64,
    end_line: u64,
    signature: &str,
    body: &str,
) -> Value {
    json!({
        "name": qualified_name.rsplit("::").next().unwrap_or(qualified_name),
        "qualified_name": qualified_name,
        "kind": kind,
        "file": file,
        "line": line,
        "end_line": end_line,
        "signature": signature,
        "body": body,
    })
}

fn ordered(mut payload: Value, keys: &[&str]) -> Value {
    let Some(items) = payload
        .get_mut("implementations")
        .and_then(Value::as_array_mut)
    else {
        return payload;
    };
    for item in items.iter_mut() {
        if let Some(methods) = item.get_mut("methods").and_then(Value::as_array_mut) {
            methods.sort_by(|left, right| {
                (left["name"].as_str(), left["line"].as_u64())
                    .cmp(&(right["name"].as_str(), right["line"].as_u64()))
            });
        }
    }
    items.sort_by(|left, right| {
        keys.iter()
            .map(|key| left[*key].to_string())
            .collect::<Vec<_>>()
            .cmp(
                &keys
                    .iter()
                    .map(|key| right[*key].to_string())
                    .collect::<Vec<_>>(),
            )
    });
    payload
}

async fn indexed_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("src directory");
        fs::write(project.join("src/lib.rs"), LIB_RS).expect("lib.rs");
        fs::write(project.join("src/decoy.rs"), DECOY_RS).expect("decoy.rs");
        fs::write(project.join("src/view.ts"), VIEW_TS).expect("view.ts");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    warm_code_index_search(&server, "paint").await;
    fixture
}

async fn call_json(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    let result =
        handle_real_server_tool_call(&server, "tracedecay_implementations", arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

async fn call_text(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    let result =
        handle_real_server_tool_call(&server, "tracedecay_implementations", arguments).await;
    extract_real_server_text(&result).to_owned()
}

async fn call_raw(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    handle_real_server_tool_call_raw(&server, "tracedecay_implementations", arguments).await
}
