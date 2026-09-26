#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

const TODO_MARKDOWN: &str = r#"## Risky Patterns
**Match count:** 1
**By kind:** todo: 1

### Findings
- **TODO at src/audit.rs:14**
  **Snippet:** todo!("later");
  **Enclosing:** src/audit.rs::unfinished
"#;

const EMPTY_MARKDOWN: &str = r#"## Risky Patterns
**Match count:** 0

_No risky patterns found._
"#;

const AUDIT_RS: &str = r#"pub fn checked_len(value: Option<u64>) -> u64 {
    value.unwrap()
}

pub fn required_label(value: Option<&str>) -> &str {
    value.expect("label")
}

pub fn fail_closed() {
    panic!("boom");
}

pub fn unfinished() {
    todo!("later");
}

pub fn missing_impl() {
    unimplemented!("nope");
}

pub fn raw_total_len(total: u64) -> usize {
    let ptr = &total as *const u64;
    unsafe { *ptr as usize }
}

pub unsafe fn raw_byte(ptr: *const u8) -> u8 {
    unsafe { *ptr }
}

pub struct Marker;

unsafe impl Send for Marker {}

unsafe trait Zeroable {}

pub fn decoys() {
    let fallback = Some(1).unwrap_or(0);
    let _ = Some(1).expect_err("fine");
    let unsafely = 1;
    let quoted = "do not unwrap() or panic!(\"x\") or todo!() or unsafe { 1 }";
    let ok = 1; // panic!("hidden");
    let _ = (fallback, unsafely, quoted, ok);
}
"#;

const WIDGET_TEST_RS: &str = "pub fn helper() {\n    let _ = Some(1).unwrap();\n}\n";

const SHIP_TEST_RS: &str = "fn integration_site() {\n    panic!(\"from tests\");\n}\n";

const SAFE_RS: &str = "pub fn safe_add(a: u64, b: u64) -> u64 {\n    a + b\n}\n";

/// `tools/call` for this tool, the path an MCP client uses.
async fn tool_text(fixture: &ProductionCompositionFixture, args: Value) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_unsafe_patterns", args)
        .await
        .unwrap_or_else(|error| panic!("tools/call failed: {error}"));
    assert!(
        response.error.is_none(),
        "tools/call returned an error: {:?}",
        response.error
    );
    extract_text(&response.result.expect("tools/call result")).to_owned()
}

/// The refusal message of a `tools/call` the tool must reject.
async fn tool_error(fixture: &ProductionCompositionFixture, args: Value) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_unsafe_patterns", args)
        .await
        .unwrap_or_else(|error| panic!("tools/call failed: {error}"));
    assert!(response.result.is_none(), "{:?}", response.result);
    response.error.expect("tools/call must refuse").message
}

const UNKNOWN_KIND_REFUSAL: &str = "tool execution failed: config error: invalid arguments for tracedecay_unsafe_patterns: unknown variant `not_a_kind`, expected one of `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `unsafe_block`";

fn parse_json(text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool text was not JSON: {error}\n{text}"))
}

fn site(kind: &str, file: &str, line: u64, snippet: &str, enclosing: &str, in_test: bool) -> Value {
    json!({
        "kind": kind,
        "file": file,
        "line": line,
        "snippet": snippet,
        "enclosing": enclosing,
        "in_test": in_test,
    })
}

fn report(match_count: u64, by_kind: Value, matches: Vec<Value>) -> Value {
    json!({
        "match_count": match_count,
        "by_kind": by_kind,
        "matches": matches,
    })
}

fn empty_report() -> Value {
    report(0, json!({}), Vec::new())
}

fn checked_len() -> Value {
    site(
        "unwrap",
        "src/audit.rs",
        2,
        "value.unwrap()",
        "src/audit.rs::checked_len",
        false,
    )
}

fn required_label() -> Value {
    site(
        "expect",
        "src/audit.rs",
        6,
        "value.expect(\"label\")",
        "src/audit.rs::required_label",
        false,
    )
}

fn fail_closed() -> Value {
    site(
        "panic",
        "src/audit.rs",
        10,
        "panic!(\"boom\");",
        "src/audit.rs::fail_closed",
        false,
    )
}

fn unfinished() -> Value {
    site(
        "todo",
        "src/audit.rs",
        14,
        "todo!(\"later\");",
        "src/audit.rs::unfinished",
        false,
    )
}

fn missing_impl() -> Value {
    site(
        "unimplemented",
        "src/audit.rs",
        18,
        "unimplemented!(\"nope\");",
        "src/audit.rs::missing_impl",
        false,
    )
}

fn raw_total_len() -> Value {
    site(
        "unsafe_block",
        "src/audit.rs",
        23,
        "unsafe { *ptr as usize }",
        "src/audit.rs::raw_total_len",
        false,
    )
}

fn raw_byte_fn() -> Value {
    site(
        "unsafe_block",
        "src/audit.rs",
        26,
        "pub unsafe fn raw_byte(ptr: *const u8) -> u8 {",
        "src/audit.rs::raw_byte",
        false,
    )
}

fn raw_byte_block() -> Value {
    site(
        "unsafe_block",
        "src/audit.rs",
        27,
        "unsafe { *ptr }",
        "src/audit.rs::raw_byte",
        false,
    )
}

fn marker_impl() -> Value {
    site(
        "unsafe_block",
        "src/audit.rs",
        32,
        "unsafe impl Send for Marker {}",
        "src/audit.rs::Marker",
        false,
    )
}

fn zeroable_trait() -> Value {
    site(
        "unsafe_block",
        "src/audit.rs",
        34,
        "unsafe trait Zeroable {}",
        "src/audit.rs::Zeroable",
        false,
    )
}

fn widget_helper() -> Value {
    site(
        "unwrap",
        "src/widget_test.rs",
        2,
        "let _ = Some(1).unwrap();",
        "src/widget_test.rs::helper",
        true,
    )
}

fn ship_panic() -> Value {
    site(
        "panic",
        "tests/ship_test.rs",
        2,
        "panic!(\"from tests\");",
        "tests/ship_test.rs::integration_site",
        true,
    )
}

fn production_sites() -> Vec<Value> {
    vec![
        checked_len(),
        required_label(),
        fail_closed(),
        unfinished(),
        missing_impl(),
        raw_total_len(),
        raw_byte_fn(),
        raw_byte_block(),
        marker_impl(),
        zeroable_trait(),
    ]
}

/// Every default kind, the decoys that must not appear, and the parameter
/// axes a caller actually sends.
#[tokio::test]
async fn unsafe_patterns_reports_literal_sites_for_each_kind() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("tests")).unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "pub mod audit;\npub mod safe;\nmod widget_test;\n",
        )
        .unwrap();
        fs::write(project.join("src/audit.rs"), AUDIT_RS).unwrap();
        fs::write(project.join("src/safe.rs"), SAFE_RS).unwrap();
        fs::write(project.join("src/widget_test.rs"), WIDGET_TEST_RS).unwrap();
        fs::write(project.join("tests/ship_test.rs"), SHIP_TEST_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let markdown = tool_text(&fixture, json!({"path": "src/audit.rs", "kinds": ["todo"]})).await;
    assert_eq!(markdown, TODO_MARKDOWN);

    let todo_json = tool_text(
        &fixture,
        json!({"path": "src/audit.rs", "kinds": ["todo"], "format": "json"}),
    )
    .await;
    assert_eq!(
        parse_json(&todo_json),
        report(1, json!({"todo": 1}), vec![unfinished()])
    );

    let empty_markdown = tool_text(&fixture, json!({"path": "src/safe.rs"})).await;
    assert_eq!(empty_markdown, EMPTY_MARKDOWN);
    let empty_json = tool_text(&fixture, json!({"path": "src/safe.rs", "format": "json"})).await;
    assert_eq!(parse_json(&empty_json), empty_report());

    // An unknown kind is refused rather than silently matching nothing.
    assert_eq!(
        tool_error(&fixture, json!({"kinds": ["not_a_kind"], "format": "json"})).await,
        UNKNOWN_KIND_REFUSAL
    );

    let mut all_sites = production_sites();
    all_sites.push(widget_helper());
    all_sites.push(ship_panic());
    let full = tool_text(&fixture, json!({"format": "json"})).await;
    assert_eq!(
        parse_json(&full),
        report(
            12,
            json!({
                "expect": 1,
                "panic": 2,
                "todo": 1,
                "unimplemented": 1,
                "unsafe_block": 5,
                "unwrap": 2,
            }),
            all_sites,
        )
    );

    let excluded = tool_text(&fixture, json!({"exclude_tests": true, "format": "json"})).await;
    assert_eq!(
        parse_json(&excluded),
        report(
            10,
            json!({
                "expect": 1,
                "panic": 1,
                "todo": 1,
                "unimplemented": 1,
                "unsafe_block": 5,
                "unwrap": 1,
            }),
            production_sites(),
        )
    );

    let widget_only = tool_text(
        &fixture,
        json!({"path": "src/widget_test.rs", "format": "json"}),
    )
    .await;
    assert_eq!(
        parse_json(&widget_only),
        report(1, json!({"unwrap": 1}), vec![widget_helper()])
    );
    let widget_hidden = tool_text(
        &fixture,
        json!({"path": "src/widget_test.rs", "exclude_tests": true, "format": "json"}),
    )
    .await;
    assert_eq!(parse_json(&widget_hidden), empty_report());

    let panics = tool_text(&fixture, json!({"kinds": ["panic"], "format": "json"})).await;
    assert_eq!(
        parse_json(&panics),
        report(2, json!({"panic": 2}), vec![fail_closed(), ship_panic()])
    );

    assert_eq!(
        tool_error(
            &fixture,
            json!({"kinds": ["unwrap", "not_a_kind"], "format": "json"}),
        )
        .await,
        UNKNOWN_KIND_REFUSAL
    );
    let unwraps = tool_text(&fixture, json!({"kinds": ["unwrap"], "format": "json"})).await;
    assert_eq!(
        parse_json(&unwraps),
        report(
            2,
            json!({"unwrap": 2}),
            vec![checked_len(), widget_helper()]
        )
    );

    let limited = tool_text(&fixture, json!({"limit": 1, "format": "json"})).await;
    assert_eq!(
        parse_json(&limited),
        report(1, json!({"unwrap": 1}), vec![checked_len()])
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn unsafe_patterns_classifies_inline_rust_test_scopes() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/lib.rs"),
            r#"pub fn production_risk() { Some(1).unwrap(); }

pub mod tests {
    pub fn production_module_named_tests() { Some(2).unwrap(); }
}

mod support {
    #[cfg(test)]
    mod nested_cfg {
        fn test_only_helper() { Some(3).unwrap(); }
    }
}

#[test]
fn attributed_test() { Some(4).unwrap(); }

#[test] fn adjacent_test() { Some(5).unwrap(); } pub fn adjacent_production() { panic!(); }
"#,
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let shared_line = "#[test] fn adjacent_test() { Some(5).unwrap(); } pub fn adjacent_production() { panic!(); }";
    // Two declarations share this line, so each site is attributed by where it
    // sits in the line: the unwrap is inside the test fn, the panic is inside
    // the production fn that follows it.
    let unwrap_enclosing = "src/lib.rs::adjacent_test";
    let panic_enclosing = "src/lib.rs::adjacent_production";
    let included_matches = vec![
        site(
            "unwrap",
            "src/lib.rs",
            1,
            "pub fn production_risk() { Some(1).unwrap(); }",
            "src/lib.rs::production_risk",
            false,
        ),
        site(
            "unwrap",
            "src/lib.rs",
            4,
            "pub fn production_module_named_tests() { Some(2).unwrap(); }",
            "src/lib.rs::tests::production_module_named_tests",
            false,
        ),
        site(
            "unwrap",
            "src/lib.rs",
            10,
            "fn test_only_helper() { Some(3).unwrap(); }",
            "src/lib.rs::support::nested_cfg::test_only_helper",
            true,
        ),
        site(
            "unwrap",
            "src/lib.rs",
            15,
            "fn attributed_test() { Some(4).unwrap(); }",
            "src/lib.rs::attributed_test",
            true,
        ),
        site(
            "unwrap",
            "src/lib.rs",
            17,
            shared_line,
            unwrap_enclosing,
            false,
        ),
        site(
            "panic",
            "src/lib.rs",
            17,
            shared_line,
            panic_enclosing,
            false,
        ),
    ];
    let included = tool_text(
        &fixture,
        json!({"kinds": ["unwrap", "panic"], "exclude_tests": false, "format": "json"}),
    )
    .await;
    assert_eq!(
        parse_json(&included),
        report(6, json!({"panic": 1, "unwrap": 5}), included_matches)
    );

    let excluded_matches = vec![
        site(
            "unwrap",
            "src/lib.rs",
            1,
            "pub fn production_risk() { Some(1).unwrap(); }",
            "src/lib.rs::production_risk",
            false,
        ),
        site(
            "unwrap",
            "src/lib.rs",
            4,
            "pub fn production_module_named_tests() { Some(2).unwrap(); }",
            "src/lib.rs::tests::production_module_named_tests",
            false,
        ),
        site(
            "unwrap",
            "src/lib.rs",
            17,
            shared_line,
            unwrap_enclosing,
            false,
        ),
        site(
            "panic",
            "src/lib.rs",
            17,
            shared_line,
            panic_enclosing,
            false,
        ),
    ];
    let excluded = tool_text(
        &fixture,
        json!({"kinds": ["unwrap", "panic"], "exclude_tests": true, "format": "json"}),
    )
    .await;
    assert_eq!(
        parse_json(&excluded),
        report(4, json!({"panic": 1, "unwrap": 3}), excluded_matches)
    );

    fixture.harness.shutdown().await;
}
