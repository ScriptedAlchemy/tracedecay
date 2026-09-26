//! `tracedecay_health` over the production MCP `tools/call` path.
//!
//! Two isolated modules have no dependency edges, matching complexity, no dead
//! functions, and no `skip-test-coverage` annotations. Modularity is then
//! `1 - 1/components` = 1/2 and the other four dimensions are 1, so the
//! composite is `(1/2)^(1/5) * 10000` = 8706. One file is a single component
//! (modularity 0, composite 0). A path that matches no file is the empty
//! graph, whose composite is 10000.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_mcp::ToolResult;

use crate::support::{
    ProductionCompositionFixture, extract_json, extract_text,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

async fn call_health(fixture: &ProductionCompositionFixture, arguments: Value) -> ToolResult {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_health", arguments)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_health production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "tracedecay_health returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    ToolResult::new(
        response
            .result
            .unwrap_or_else(|| panic!("tracedecay_health returned no production MCP result")),
        Vec::new(),
    )
}

fn write_isolated_modules(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/left.rs"),
        "pub fn alpha() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/right.rs"),
        "pub fn bravo() -> i32 {\n    1\n}\n",
    )
    .unwrap();
}

fn dimension_without_formula(payload: &Value, name: &str) -> Value {
    let mut dimension = payload["dimensions"][name].clone();
    let object = dimension
        .as_object_mut()
        .unwrap_or_else(|| panic!("{name} dimension missing from {payload}"));
    object.remove("source");
    dimension
}

#[tokio::test]
async fn health_scores_two_isolated_modules_and_distinguishes_scope() {
    let fixture = production_composition_fixture_with_sources(write_isolated_modules).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production health server");
    wait_for_current_graph(&server).await;

    let summary = call_health(&fixture, json!({})).await;
    assert_eq!(
        extract_text(&summary.value),
        "**files_analyzed:** 2\n**quality_signal:** 8706\n"
    );

    let summary_json = call_health(&fixture, json!({"format": "json"})).await;
    assert_eq!(
        extract_json(&summary_json.value),
        json!({
            "quality_signal": 8706,
            "files_analyzed": 2,
        })
    );

    let detailed = call_health(&fixture, json!({"format": "json", "details": true})).await;
    let detailed = extract_json(&detailed.value);
    assert_eq!(detailed["quality_signal"], json!(8706));
    assert_eq!(detailed["files_analyzed"], json!(2));
    assert_eq!(
        dimension_without_formula(&detailed, "acyclicity"),
        json!({"score": 1.0, "edges_in_cycles": 0})
    );
    assert_eq!(
        dimension_without_formula(&detailed, "depth"),
        json!({"score": 1.0, "max_chain": 0, "ideal_chain": 1})
    );
    assert_eq!(
        dimension_without_formula(&detailed, "equality"),
        json!({
            "score": 1.0,
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "incomplete_complexity_symbols": 0,
        })
    );
    assert_eq!(
        dimension_without_formula(&detailed, "redundancy"),
        json!({"score": 1.0, "dead_count": 0, "total_fns": 2})
    );
    assert_eq!(
        dimension_without_formula(&detailed, "modularity"),
        json!({
            "score": 0.5,
            "interpretation": "moderate",
            "components_after_hub_removal": 2,
        })
    );
    assert_eq!(
        dimension_without_formula(&detailed, "coverage_discipline"),
        json!({
            "score": 1.0,
            "skip_test_coverage_count": 0,
            "total_fns": 2,
        })
    );

    let one_file = call_health(
        &fixture,
        json!({"format": "json", "details": true, "path": "src/left.rs"}),
    )
    .await;
    let one_file = extract_json(&one_file.value);
    assert_eq!(one_file["quality_signal"], json!(0));
    assert_eq!(one_file["files_analyzed"], json!(1));
    assert_eq!(
        dimension_without_formula(&one_file, "modularity"),
        json!({
            "score": 0.0,
            "interpretation": "low",
            "components_after_hub_removal": 1,
        })
    );
    assert_eq!(
        dimension_without_formula(&one_file, "depth"),
        json!({"score": 1.0, "max_chain": 0, "ideal_chain": 0})
    );
    assert_eq!(
        dimension_without_formula(&one_file, "redundancy"),
        json!({"score": 1.0, "dead_count": 0, "total_fns": 1})
    );

    let missing = call_health(
        &fixture,
        json!({
            "format": "json",
            "details": true,
            "path": "src/missing",
        }),
    )
    .await;
    let missing = extract_json(&missing.value);
    assert_eq!(missing["quality_signal"], json!(10000));
    assert_eq!(missing["files_analyzed"], json!(0));
    assert_eq!(
        dimension_without_formula(&missing, "modularity"),
        json!({
            "score": 1.0,
            "interpretation": "high",
            "components_after_hub_removal": 0,
        })
    );
    assert_eq!(
        dimension_without_formula(&missing, "acyclicity"),
        json!({"score": 1.0, "edges_in_cycles": 0})
    );
    assert_eq!(
        dimension_without_formula(&missing, "redundancy"),
        json!({"score": 1.0, "dead_count": 0, "total_fns": 0})
    );
}

async fn health_report_error(
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
async fn health_reports_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture_with_sources(write_isolated_modules).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production health server");
    wait_for_current_graph(&server).await;

    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_gini",
            json!({"metric": "lines", "format": "json"}),
        )
        .await
        .expect("tracedecay_gini production invocation");
    let gini = extract_json(&response.result.expect("gini result"));
    assert_eq!(
        (
            &gini["metric"],
            &gini["scope"],
            &gini["total_items"],
            &gini["gini"]
        ),
        (&json!("lines"), &json!("file"), &json!(2), &json!(0.0))
    );

    assert_eq!(
        health_report_error(&fixture, "tracedecay_gini", json!({"metric": "cyclomatic"})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_gini: unknown variant `cyclomatic`, expected one of `complexity`, `lines`, `fan_in`, `fan_out`, `members`"
    );
    assert_eq!(
        health_report_error(&fixture, "tracedecay_dsm", json!({"max_files": "30"})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_dsm: invalid type: string \"30\", expected u32"
    );
    assert_eq!(
        health_report_error(&fixture, "tracedecay_health", json!({"detail": true})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_health: unknown field `detail`, expected `path` or `details`"
    );

    fixture.harness.shutdown().await;
}
