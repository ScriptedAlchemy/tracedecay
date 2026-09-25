//! `tracedecay_dead_code` as a caller sees it: one production MCP `tools/call`
//! against a source file whose uncalled symbols are known by reading the file.
//!
//! `src/lib.rs` is the census. These symbols are dead, in source order:
//!
//! | line | name             | why it is dead                                      |
//! |------|------------------|-----------------------------------------------------|
//! | 7    | `caller`         | private function, nothing calls it                  |
//! | 14   | `dead_inline`    | `#[inline]` is an annotation, not a caller         |
//! | 16   | `dead_plain`     | private function, nothing calls it                  |
//! | 34   | `unused_method`  | private method, nothing calls it                    |
//!
//! These are in the same file and must not appear unless an argument opts in:
//! `main` (entry-point name), `called_helper` (called from `main` and
//! `caller`), `published_unused` (public, and `include_public` defaults to
//! false), `slope_excludes_lfe` (`#[test]`), `test_prefixed` (name prefix),
//! `async_suite_case` (`#[tokio::test]`), `parameterized_case` (`#[rstest]`).

use std::collections::HashSet;
use std::fs;

use serde_json::{Value, json};

use crate::support::{
    extract_text, production_composition_fixture_with_sources, wait_for_current_graph,
};

const LIB_RS: &str = r#"fn main() {
    called_helper();
}

fn called_helper() {}

fn caller() {
    called_helper();
}

pub fn published_unused() {}

#[inline]
fn dead_inline() {}

fn dead_plain() -> i32 {
    1
}

#[test]
fn slope_excludes_lfe() {}

fn test_prefixed() {}

#[tokio::test]
async fn async_suite_case() {}

#[rstest]
fn parameterized_case() {}

struct Widget;

impl Widget {
    fn unused_method(&self) {}
}
"#;

fn private_dead_symbols() -> Value {
    json!([
        {
            "name": "caller",
            "kind": "function",
            "file": "src/lib.rs",
            "line": 7,
            "signature": "fn caller()"
        },
        {
            "name": "dead_inline",
            "kind": "function",
            "file": "src/lib.rs",
            "line": 14,
            "signature": "fn dead_inline()"
        },
        {
            "name": "dead_plain",
            "kind": "function",
            "file": "src/lib.rs",
            "line": 16,
            "signature": "fn dead_plain() -> i32"
        },
        {
            "name": "unused_method",
            "kind": "method",
            "file": "src/lib.rs",
            "line": 34,
            "signature": "fn unused_method(&self)"
        }
    ])
}

fn symbol_ids(payload: &Value) -> Vec<String> {
    payload["symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("dead_code must return symbols: {payload}"))
        .iter()
        .map(|symbol| {
            symbol["id"]
                .as_str()
                .unwrap_or_else(|| panic!("dead_code symbol has no id: {symbol}"))
                .to_owned()
        })
        .collect()
}

fn census(payload: &Value) -> Value {
    let mut symbols = payload["symbols"].clone();
    let Some(list) = symbols.as_array_mut() else {
        panic!("dead_code must return symbols: {payload}");
    };
    for symbol in list {
        let Some(symbol) = symbol.as_object_mut() else {
            panic!("dead_code symbol must be an object: {payload}");
        };
        symbol.remove("id");
    }
    symbols
}

fn assert_occurrence_ids(payload: &Value) {
    let ids = symbol_ids(payload);
    assert!(
        !ids.is_empty(),
        "a non-empty census must name each symbol with an occurrence id: {payload}"
    );
    for id in &ids {
        assert!(
            id.starts_with("symbol.v1.") && id.len() > "symbol.v1.".len(),
            "dead_code id must be a symbol occurrence, got {id}: {payload}"
        );
    }
    let unique = ids.iter().collect::<HashSet<_>>();
    assert_eq!(
        unique.len(),
        ids.len(),
        "dead_code ids must identify one symbol each: {ids:?}"
    );
}

fn markdown_without_ids(markdown: &str) -> String {
    let mut lines = markdown
        .lines()
        .filter(|line| !line.trim_start().starts_with("**id:**"))
        .collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

async fn call_dead_code(
    fixture: &crate::support::ProductionCompositionFixture,
    arguments: Value,
) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_dead_code", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.error.is_none(),
        "tracedecay_dead_code failed: {:?}",
        response.error
    );
    let result = response
        .result
        .expect("tracedecay_dead_code returned a production MCP result");
    let text = extract_text(&result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_dead_code JSON payload ({error}):\n{text}"))
}

#[tokio::test]
async fn dead_code_reports_only_the_uncalled_private_symbols() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production dead-code server");
    wait_for_current_graph(&server).await;

    let payload = call_dead_code(&fixture, json!({"format": "json"})).await;
    assert_eq!(payload["dead_code_count"], 4, "{payload}");
    assert_eq!(census(&payload), private_dead_symbols(), "{payload}");
    assert_occurrence_ids(&payload);

    let with_public =
        call_dead_code(&fixture, json!({"include_public": true, "format": "json"})).await;
    assert_eq!(with_public["dead_code_count"], 5, "{with_public}");
    assert_eq!(
        census(&with_public),
        json!([
            {
                "name": "caller",
                "kind": "function",
                "file": "src/lib.rs",
                "line": 7,
                "signature": "fn caller()"
            },
            {
                "name": "published_unused",
                "kind": "function",
                "file": "src/lib.rs",
                "line": 11,
                "signature": "pub fn published_unused()"
            },
            {
                "name": "dead_inline",
                "kind": "function",
                "file": "src/lib.rs",
                "line": 14,
                "signature": "fn dead_inline()"
            },
            {
                "name": "dead_plain",
                "kind": "function",
                "file": "src/lib.rs",
                "line": 16,
                "signature": "fn dead_plain() -> i32"
            },
            {
                "name": "unused_method",
                "kind": "method",
                "file": "src/lib.rs",
                "line": 34,
                "signature": "fn unused_method(&self)"
            }
        ]),
        "{with_public}"
    );

    let methods = call_dead_code(&fixture, json!({"kinds": ["method"], "format": "json"})).await;
    assert_eq!(methods["dead_code_count"], 1, "{methods}");
    assert_eq!(
        census(&methods),
        json!([
            {
                "name": "unused_method",
                "kind": "method",
                "file": "src/lib.rs",
                "line": 34,
                "signature": "fn unused_method(&self)"
            }
        ]),
        "{methods}"
    );

    let first = call_dead_code(&fixture, json!({"limit": 1, "format": "json"})).await;
    assert_eq!(first["dead_code_count"], 1, "{first}");
    assert_eq!(
        census(&first),
        json!([
            {
                "name": "caller",
                "kind": "function",
                "file": "src/lib.rs",
                "line": 7,
                "signature": "fn caller()"
            }
        ]),
        "{first}"
    );

    let clamped = call_dead_code(&fixture, json!({"limit": 0, "format": "json"})).await;
    assert_eq!(clamped["dead_code_count"], 1, "{clamped}");
    assert_eq!(
        census(&clamped),
        json!([
            {
                "name": "caller",
                "kind": "function",
                "file": "src/lib.rs",
                "line": 7,
                "signature": "fn caller()"
            }
        ]),
        "limit 0 is clamped to 1 and still returns the first dead symbol: {clamped}"
    );

    let markdown = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_dead_code", json!({}))
        .await
        .expect("production MCP tools/call");
    assert!(
        markdown.error.is_none(),
        "default dead_code failed: {:?}",
        markdown.error
    );
    let markdown = markdown
        .result
        .expect("default dead_code returned a production MCP result");
    let markdown = extract_text(&markdown);
    assert_eq!(
        markdown_without_ids(markdown),
        "\
**dead_code_count:** 4

## symbols
**file:** src/lib.rs

- **caller**
  **kind:** function
  **line:** 7
  **signature:** `fn caller()`
- **dead_inline**
  **kind:** function
  **line:** 14
  **signature:** `fn dead_inline()`
- **dead_plain**
  **kind:** function
  **line:** 16
  **signature:** `fn dead_plain() -> i32`
- **unused_method**
  **kind:** method
  **line:** 34
  **signature:** `fn unused_method(&self)`"
    );
    let id_lines = markdown
        .lines()
        .filter(|line| line.trim_start().starts_with("**id:**"))
        .count();
    assert_eq!(
        id_lines, 4,
        "default markdown must identify each symbol:\n{markdown}"
    );

    fixture.harness.shutdown().await;
}
