//! `tracedecay_files` as a host calls it: one `tools/call` on the production
//! MCP server, against a project whose files are written before the call.
//!
//! The census is the indexed symbol grain, not file nodes. `Cargo.toml` is
//! one `[package]` table plus its three keys. `src/lib.rs` declares one module
//! and one function. `src/greeting.rs` declares one function. `README.md` has
//! no heading, so it contributes no symbol and stays off the list.
//!
//! `glob::Pattern::matches` does not require a literal path separator, so `*`
//! crosses `/`. `*.rs` therefore matches `src/lib.rs`, not only a basename.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_first_json_content,
    production_composition_fixture_with_sources,
};

const TOOL: &str = "tracedecay_files";

const CARGO_TOML: &str =
    "[package]\nname = \"files_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const LIB_RS: &str =
    "pub mod greeting;\n\npub fn root_value() -> i32 {\n    greeting::hello()\n}\n";
const GREETING_RS: &str = "pub fn hello() -> i32 {\n    7\n}\n";
const README_MD: &str = "not indexed\n";

const GROUPED_ALL: &str = "\
## Files
**indexed files:** 3
**layout:** grouped

```text
Cargo.toml (4 symbols)
src/
  greeting.rs (1 symbols)
  lib.rs (2 symbols)
```
";

const FLAT_ALL: &str = "\
## Files
**indexed files:** 3
**layout:** flat

```text
- Cargo.toml (4 symbols, 66 bytes)
- src/greeting.rs (1 symbols, 32 bytes)
- src/lib.rs (2 symbols, 72 bytes)
```
";

const GROUPED_SRC: &str = "\
## Files
**indexed files:** 2
**layout:** grouped

```text
src/
  greeting.rs (1 symbols)
  lib.rs (2 symbols)
```
";

const GROUPED_GREETING: &str = "\
## Files
**indexed files:** 1
**layout:** grouped

```text
- src/greeting.rs (1 symbols)
```
";

const GROUPED_LIB: &str = "\
## Files
**indexed files:** 1
**layout:** grouped

```text
- src/lib.rs (2 symbols)
```
";

const GROUPED_CARGO: &str = "\
## Files
**indexed files:** 1
**layout:** grouped

```text
- Cargo.toml (4 symbols)
```
";

const EMPTY_GROUPED: &str = "\
## Files
**indexed files:** 0
**layout:** grouped

_No indexed files matched._
";

#[tokio::test]
async fn files_lists_the_indexed_census_and_filters() {
    let fixture = files_project().await;

    let grouped = call_markdown(&fixture, json!({})).await;
    assert_eq!(grouped, GROUPED_ALL, "default markdown\n{grouped}");
    assert_eq!(
        call_markdown(&fixture, json!({"layout": "grouped"})).await,
        GROUPED_ALL,
        "explicit grouped layout must match the default"
    );

    let grouped_census = listing(3, "grouped", all_files());
    assert_eq!(
        call_json(&fixture, json!({"format": "json"})).await,
        grouped_census,
        "json census"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "JSON"})).await,
        grouped_census,
        "format is case-insensitive"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "layout": "flat"})).await,
        listing(3, "flat", all_files()),
        "flat changes the layout field, not the file records"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"layout": "flat"})).await,
        FLAT_ALL,
        "flat markdown"
    );

    let src_files = json!([file("src/greeting.rs", 1, 32), file("src/lib.rs", 2, 72),]);
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "path": "src"})).await,
        listing(2, "grouped", src_files.clone()),
        "path src"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "path": "src/"})).await,
        listing(2, "grouped", src_files),
        "a trailing slash is the same directory"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"path": "src"})).await,
        GROUPED_SRC,
        "path src markdown"
    );

    assert_eq!(
        call_json(
            &fixture,
            json!({"format": "json", "path": "src/greeting.rs"})
        )
        .await,
        listing(1, "grouped", json!([file("src/greeting.rs", 1, 32)])),
        "an exact file path matches that file"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"path": "src/greeting.rs"})).await,
        GROUPED_GREETING,
        "a single file stays a bullet, not a tree"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"path": "src/lib.rs"})).await,
        GROUPED_LIB,
        "path src/lib.rs"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "path": "src/lib.rs/"})).await,
        listing(0, "grouped", json!([])),
        "a trailing slash after a file is a directory prefix, not that file"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"path": "src/lib"})).await,
        EMPTY_GROUPED,
        "a path prefix that is not a directory boundary matches nothing"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "path": ""})).await,
        listing(0, "grouped", json!([])),
        "an empty path is a prefix of nothing"
    );

    assert_eq!(
        call_json(&fixture, json!({"format": "json", "pattern": "*.rs"})).await,
        listing(
            2,
            "grouped",
            json!([file("src/greeting.rs", 1, 32), file("src/lib.rs", 2, 72),])
        ),
        "*.rs crosses directories"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "pattern": "*.toml"})).await,
        listing(1, "grouped", json!([file("Cargo.toml", 4, 66)])),
        "*.toml"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"pattern": "*.toml"})).await,
        GROUPED_CARGO,
        "*.toml markdown"
    );
    assert_eq!(
        call_json(&fixture, json!({"format": "json", "pattern": "*.md"})).await,
        listing(0, "grouped", json!([])),
        "a heading-less markdown file is not indexed"
    );
    assert_eq!(
        call_json(
            &fixture,
            json!({"format": "json", "path": "src", "pattern": "*.toml"})
        )
        .await,
        listing(0, "grouped", json!([])),
        "path and pattern both have to match"
    );
    assert_eq!(
        call_json(
            &fixture,
            json!({"format": "json", "path": "src", "pattern": "**/lib.rs"})
        )
        .await,
        listing(1, "grouped", json!([file("src/lib.rs", 2, 72)])),
        "path and glob intersect on src/lib.rs"
    );
    assert_eq!(
        call_markdown(&fixture, json!({"pattern": "nope*"})).await,
        EMPTY_GROUPED,
        "a glob that matches no indexed file"
    );

    assert_tool_error(
        &call(&fixture, json!({"pattern": "["})).await,
        "tool execution failed: config error: invalid file glob '[': Pattern syntax error near position 0: invalid range pattern",
    );
    assert_tool_error(
        &call(&fixture, json!([])).await,
        "tool execution failed: config error: invalid arguments: tracedecay_files expects a JSON object",
    );

    fixture.harness.shutdown().await;
}

async fn files_project() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|root| {
        write(root, "Cargo.toml", CARGO_TOML);
        write(root, "src/lib.rs", LIB_RS);
        write(root, "src/greeting.rs", GREETING_RS);
        write(root, "README.md", README_MD);
    })
    .await
}

fn all_files() -> Value {
    json!([
        file("Cargo.toml", 4, 66),
        file("src/greeting.rs", 1, 32),
        file("src/lib.rs", 2, 72),
    ])
}

fn file(path: &str, symbols: u64, bytes: u64) -> Value {
    json!({"path": path, "symbols": symbols, "bytes": bytes})
}

fn listing(count: u64, layout: &str, files: Value) -> Value {
    json!({"count": count, "layout": layout, "files": files})
}

async fn call_json(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = call(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "{TOOL} failed: {:?}",
        response.error
    );
    let result = response.result.as_ref().expect("tools/call result");
    extract_first_json_content(result)
}

async fn call_markdown(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let response = call(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "{TOOL} failed: {:?}",
        response.error
    );
    let result = response.result.as_ref().expect("tools/call result");
    result["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item.get("text").and_then(Value::as_str)?;
                text.starts_with("## Files\n").then_some(text)
            })
        })
        .unwrap_or_else(|| panic!("missing files markdown in {result}"))
        .to_owned()
}

async fn call(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
) -> tracedecay_mcp::JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, TOOL, arguments)
        .await
        .expect("production MCP answers a tools/call")
}

fn assert_tool_error(response: &tracedecay_mcp::JsonRpcResponse, message: &str) {
    assert!(response.result.is_none(), "{response:?}");
    let error = response.error.as_ref().expect("tool error");
    assert_eq!(error.code, -32603, "{error:?}");
    assert_eq!(error.message, message, "{error:?}");
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("tool"))
            .and_then(Value::as_str),
        Some(TOOL),
        "{error:?}"
    );
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    fs::write(path, contents).expect("write fixture file");
}
