#![cfg(feature = "test-transport")]

//! `tracedecay_implementations` as an MCP client observes it.
//!
//! Calls go through the production server's `tools/call` path. Expected
//! records are the source text of this fixture, not values read back out of
//! the handler.

use crate::support::{
    ProductionCompositionFixture, dispatch_mcp_tool_call, extract_real_server_text,
    handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
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

/// `(qualified_name, kind, file, line, end_line, body)` of one match.
type Match = (String, String, String, u64, u64, String);

fn matched(qualified_name: &str, kind: &str, file: &str, lines: (u64, u64), body: &str) -> Match {
    (
        qualified_name.to_owned(),
        kind.to_owned(),
        file.to_owned(),
        lines.0,
        lines.1,
        body.to_owned(),
    )
}

fn trait_selector(name: &str) -> Value {
    json!({"selector": {"selector": "trait", "name": name}})
}

fn method_selector(name: &str) -> Value {
    json!({"selector": {"selector": "method", "name": name}})
}

#[tokio::test]
async fn implementations_returns_literal_bodies_for_trait_interface_and_method() {
    let fixture = indexed_project().await;

    let widget = call_matches(&fixture, trait_selector("Widget")).await;
    assert_eq!(
        widget,
        vec![
            matched(
                "src/lib.rs::Outline",
                "impl",
                "src/lib.rs",
                (17, 21),
                "impl Widget for Outline {\n    fn paint(&self) -> &'static str {\n        \"outline\"\n    }\n}",
            ),
            matched(
                "src/lib.rs::Solid",
                "impl",
                "src/lib.rs",
                (7, 11),
                "impl Widget for Solid {\n    fn paint(&self) -> &'static str {\n        \"solid\"\n    }\n}",
            ),
        ],
        "trait lookup returns only Widget implementors, each with its impl body"
    );

    assert!(
        call_matches(&fixture, trait_selector("Unused"))
            .await
            .is_empty(),
        "a trait with no implementors is an empty page"
    );
    assert!(
        call_matches(&fixture, trait_selector("AbsentTrait"))
            .await
            .is_empty(),
        "an absent trait is an empty page"
    );

    assert_eq!(
        call_matches(&fixture, method_selector("paint")).await,
        vec![
            matched(
                "src/lib.rs::<Outline as Widget>::paint",
                "method",
                "src/lib.rs",
                (18, 20),
                "    fn paint(&self) -> &'static str {\n        \"outline\"\n    }",
            ),
            matched(
                "src/lib.rs::<Solid as Widget>::paint",
                "method",
                "src/lib.rs",
                (8, 10),
                "    fn paint(&self) -> &'static str {\n        \"solid\"\n    }",
            ),
            matched(
                "src/lib.rs::Widget::paint",
                "method",
                "src/lib.rs",
                (2, 2),
                "    fn paint(&self) -> &'static str;",
            ),
            matched(
                "src/lib.rs::paint",
                "function",
                "src/lib.rs",
                (23, 25),
                "pub fn paint() -> &'static str {\n    \"free\"\n}",
            ),
        ],
        "method lookup returns every paint body, including the trait declaration and free function"
    );

    assert_eq!(
        call_matches(&fixture, method_selector("weight")).await,
        vec![matched(
            "src/lib.rs::Solid::weight",
            "method",
            "src/lib.rs",
            (28, 30),
            "    fn weight(&self) -> u8 {\n        1\n    }",
        )]
    );

    assert_eq!(
        call_matches(&fixture, method_selector("Widget")).await,
        vec![matched(
            "src/decoy.rs::Widget",
            "function",
            "src/decoy.rs",
            (1, 3),
            "pub fn Widget() -> u8 {\n    7\n}",
        )],
        "a function that only shares the trait's name is a method hit, not an implementor"
    );
    assert!(
        call_matches(&fixture, method_selector("absent_method"))
            .await
            .is_empty()
    );

    assert_eq!(
        call_matches(&fixture, trait_selector("Drawable")).await,
        vec![matched(
            "src/view.ts::Canvas",
            "class",
            "src/view.ts",
            (5, 9),
            "class Canvas implements Drawable {\n  draw(): string {\n    return \"canvas\";\n  }\n}",
        )],
        "interface lookup returns the implementing class body and not Sketch"
    );

    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    let markdown = dispatch_mcp_tool_call(
        &server,
        "tracedecay_implementations",
        trait_selector("Widget"),
    )
    .await;
    let markdown = markdown["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("markdown implementations text: {markdown}"));
    assert!(
        markdown.contains("src/lib.rs::Solid (impl) src/lib.rs:7")
            && markdown.contains("|         \"solid\""),
        "markdown carries each match and its body: {markdown}"
    );

    for (arguments, context) in [
        (json!({}), "a missing selector"),
        (json!({"trait": "Widget"}), "the retired trait argument"),
        (json!({"method": "paint"}), "the retired method argument"),
        (
            json!({"selector": {"selector": "trait", "name": "Widget"}, "limit": 1}),
            "the retired limit argument",
        ),
    ] {
        let refused = call_raw(&fixture, arguments).await;
        assert_eq!(
            refused["error"]["data"]["reason_code"], "application_surface_invalid_request",
            "{context} must be a typed invalid request: {refused}"
        );
    }

    fixture.harness.shutdown().await;
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

/// Every match on the served page, sorted by qualified name.
async fn call_matches(fixture: &ProductionCompositionFixture, mut arguments: Value) -> Vec<Match> {
    arguments["format"] = json!("json");
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    let result =
        handle_real_server_tool_call(&server, "tracedecay_implementations", arguments).await;
    let text = extract_real_server_text(&result);
    let payload: Value =
        serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}\n{text}"));
    let page = &payload["outcome"]["value"]["payload"];
    assert!(
        page["next_cursor"].is_null(),
        "fixture fits one page: {page}"
    );
    let mut matches = page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("implementations page has no items: {payload}"))
        .iter()
        .map(|item| {
            let symbol = &item["symbol"];
            (
                symbol["qualified_name"].as_str().unwrap().to_owned(),
                symbol["kind"].as_str().unwrap().to_owned(),
                symbol["file"].as_str().unwrap().to_owned(),
                symbol["line"].as_u64().unwrap(),
                symbol["end_line"].as_u64().unwrap(),
                item["body"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
}

async fn call_raw(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production implementations server");
    handle_real_server_tool_call_raw(&server, "tracedecay_implementations", arguments).await
}
