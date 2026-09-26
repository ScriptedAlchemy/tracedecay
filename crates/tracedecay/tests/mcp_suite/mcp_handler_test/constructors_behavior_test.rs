//! User-visible `tracedecay_constructors` contract, exercised through the
//! production MCP `tools/call` path.
//!
//! The fixture includes a `Self` literal, a match pattern, a quoted lookalike,
//! and a function call so those shapes stay absent from the reported sites.
//! Line numbers below are the fixture's literal lines, not a second parser.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_mcp::JsonRpcResponse;

use crate::support::{
    ProductionCompositionFixture, extract_first_json_content, harness_wait_for_readiness,
    production_composition_fixture_with_sources,
};

const FIXTURE_SOURCE: &str = r#"#[derive(Default)]
pub struct BuildOptions {
    pub name: String,
    pub retries: u8,
    pub verbose: bool,
}

impl BuildOptions {
    pub fn via_self(name: String) -> Self {
        Self { name, retries: 1, verbose: false }
    }
}

pub fn explicit() -> BuildOptions {
    BuildOptions { name: String::new(), retries: 3, verbose: true }
}

pub fn shorthand(name: String, retries: u8, verbose: bool) -> BuildOptions {
    BuildOptions { name, retries, verbose }
}

pub fn updated() -> BuildOptions {
    BuildOptions { name: String::new(), ..Default::default() }
}

pub fn incomplete() -> BuildOptions {
    BuildOptions { name: String::new() }
}

pub fn qualified() -> BuildOptions {
    crate::BuildOptions { name: String::new(), retries: 1, verbose: false }
}

pub fn ignored(options: BuildOptions) -> bool {
    let _quoted = "BuildOptions { name: quoted }";
    // BuildOptions { name: comment }
    let _called = BuildOptions::new();
    match options {
        BuildOptions { verbose: true, .. } => true,
        _ => false,
    }
}

pub mod left {
    pub struct Options {
        pub one: u8,
    }
    pub fn build() -> Options {
        Options { one: 1 }
    }
}

pub mod right {
    pub struct Options {
        pub two: u8,
    }
    pub fn build() -> Options {
        Options { two: 2 }
    }
}

pub fn recovered() -> BuildOptions {
    BuildOptions { name: String::new(), retries: }
}
"#;

fn site(
    line: u64,
    fields: &[&str],
    update_fields: &[&str],
    missing_fields: &[&str],
    field_coverage: &str,
) -> Value {
    json!({
        "file": "src/lib.rs",
        "line": line,
        "fields": fields,
        "update_fields": update_fields,
        "missing_fields": missing_fields,
        "field_coverage": field_coverage,
    })
}

async fn wait_for_current_graph(fixture: &ProductionCompositionFixture) {
    harness_wait_for_readiness(
        &fixture.harness,
        &fixture.project_root,
        "ready",
        Duration::from_secs(20),
    )
    .await;
}

fn write_constructor_fixture(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), FIXTURE_SOURCE).unwrap();
}

async fn open_constructor_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(write_constructor_fixture).await;
    wait_for_current_graph(&fixture).await;
    fixture
}

async fn call_constructors(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_constructors", arguments)
        .await
        .expect("tracedecay_constructors tools/call")
}

fn json_payload(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "constructors tools/call failed: {:?}",
        response.error
    );
    extract_first_json_content(response.result.as_ref().expect("constructors result"))
}

fn assert_struct_argument_refused(response: &JsonRpcResponse, detail: &str) {
    let error = response
        .error
        .as_ref()
        .expect("a call without a struct name is a JSON-RPC error");
    assert!(response.result.is_none(), "{response:?}");
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        format!(
            "tool execution failed: config error: invalid arguments for tracedecay_constructors: {detail}"
        )
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("tool"))
            .and_then(Value::as_str),
        Some("tracedecay_constructors")
    );
}

#[tokio::test]
async fn constructors_reports_literal_sites_and_denies_a_missing_struct() {
    let fixture = open_constructor_project().await;

    let explicit = site(15, &["name", "retries", "verbose"], &[], &[], "complete");
    let shorthand = site(19, &["name", "retries", "verbose"], &[], &[], "complete");
    let updated = site(23, &["name"], &["retries", "verbose"], &[], "complete");
    let incomplete = site(27, &["name"], &[], &["retries", "verbose"], "complete");
    let qualified = site(31, &["name", "retries", "verbose"], &[], &[], "complete");
    let recovered = site(63, &["name", "retries"], &[], &[], "unknown");

    let report = json_payload(
        &call_constructors(
            &fixture,
            json!({"struct": "BuildOptions", "format": "json"}),
        )
        .await,
    );
    assert_eq!(
        report,
        json!({
            "struct": "BuildOptions",
            "candidate_count": 1,
            "resolution_status": "unverified",
            "resolution_reason": "syntax_only_simple_name",
            "expected_fields": ["name", "retries", "verbose"],
            "match_count": 6,
            "sites": [explicit, shorthand, updated, incomplete, qualified, recovered],
        }),
        "constructor report: {report}"
    );

    let first_only = json_payload(
        &call_constructors(
            &fixture,
            json!({"struct": "BuildOptions", "limit": 1, "format": "json"}),
        )
        .await,
    );
    assert_eq!(
        first_only,
        json!({
            "struct": "BuildOptions",
            "candidate_count": 1,
            "resolution_status": "unverified",
            "resolution_reason": "syntax_only_simple_name",
            "expected_fields": ["name", "retries", "verbose"],
            "match_count": 1,
            "sites": [site(15, &["name", "retries", "verbose"], &[], &[], "complete")],
        })
    );

    let clamped = json_payload(
        &call_constructors(
            &fixture,
            json!({"struct": "BuildOptions", "limit": 0, "format": "json"}),
        )
        .await,
    );
    assert_eq!(
        clamped,
        json!({
            "struct": "BuildOptions",
            "candidate_count": 1,
            "resolution_status": "unverified",
            "resolution_reason": "syntax_only_simple_name",
            "expected_fields": ["name", "retries", "verbose"],
            "match_count": 1,
            "sites": [site(15, &["name", "retries", "verbose"], &[], &[], "complete")],
        })
    );

    let missing = json_payload(
        &call_constructors(
            &fixture,
            json!({"struct": "MissingWidget", "format": "json"}),
        )
        .await,
    );
    assert_eq!(
        missing,
        json!({
            "found": false,
            "struct": "MissingWidget",
            "message": "No struct, class, or case-class named 'MissingWidget' found.",
            "match_count": 0,
            "sites": [],
        })
    );

    let function_is_not_a_struct = json_payload(
        &call_constructors(&fixture, json!({"struct": "explicit", "format": "json"})).await,
    );
    assert_eq!(
        function_is_not_a_struct,
        json!({
            "found": false,
            "struct": "explicit",
            "message": "No struct, class, or case-class named 'explicit' found.",
            "match_count": 0,
            "sites": [],
        })
    );

    let ambiguous = json_payload(
        &call_constructors(&fixture, json!({"struct": "Options", "format": "json"})).await,
    );
    assert_eq!(
        ambiguous,
        json!({
            "struct": "Options",
            "candidate_count": 2,
            "resolution_status": "unverified",
            "resolution_reason": "ambiguous_simple_name",
            "expected_fields": null,
            "match_count": 2,
            "sites": [
                site(49, &["one"], &[], &[], "unknown"),
                site(58, &["two"], &[], &[], "unknown"),
            ],
        })
    );

    assert_struct_argument_refused(
        &call_constructors(&fixture, json!({"format": "json"})).await,
        "missing field `struct`",
    );
    assert_struct_argument_refused(
        &call_constructors(&fixture, json!({"struct": 4, "format": "json"})).await,
        "invalid type: integer `4`, expected a string",
    );

    fixture.harness.shutdown().await;
}
