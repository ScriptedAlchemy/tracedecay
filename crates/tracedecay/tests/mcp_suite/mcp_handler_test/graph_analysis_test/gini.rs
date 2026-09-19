//! Literal `tracedecay_gini` results for a fixture whose metric values are
//! fixed by source shape, not by reading the coefficient back out of the tool.
//!
//! The handler rounds `2*Σ(i*x_i)/(n*Σx) - (n+1)/n` (1-indexed `i` on values
//! sorted ascending) to four decimals. One file, or a missing path, is the
//! empty-or-singleton case and the coefficient is exactly 0.

use std::path::Path;

use serde_json::{Value, json};

use crate::support::{extract_json, production_composition_fixture_with_sources};

use super::{AnalysisToolHost, close_test_graph, handle_tool_call, setup_empty_analysis_project};

fn write_gini_distribution_sources(project: &Path) {
    std::fs::create_dir_all(project.join("src/spans")).unwrap();
    // Body is one block and no branch: complexity 1, line span 1.
    std::fs::write(
        project.join("src/spans/short.rs"),
        "pub fn short() -> i32 { 1 }\n",
    )
    .unwrap();
    // Same complexity 1, line span 3 (declaration, body, closing brace).
    std::fs::write(
        project.join("src/spans/tall.rs"),
        "pub fn tall() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    // Tiny has one field, Big has three. `plain` is a single block (complexity
    // 1). `branched` is if + else (2 branches) inside a body block, so the
    // inner blocks reach nesting 2 and the symbol score is 4.
    std::fs::write(
        project.join("src/kinds.rs"),
        "\
pub struct Tiny {\n    \
    pub only: i32,\n\
}\n\
\n\
pub struct Big {\n    \
    pub a: i32,\n    \
    pub b: i32,\n    \
    pub c: i32,\n\
}\n\
\n\
pub fn plain() -> i32 { 1 }\n\
\n\
pub fn branched(n: i32) -> i32 {\n    \
    if n > 0 {\n        \
        n\n    \
    } else {\n        \
        0\n    \
    }\n\
}\n",
    )
    .unwrap();
}

async fn gini_json(host: &impl AnalysisToolHost, args: Value) -> Value {
    let result = handle_tool_call(host, "tracedecay_gini", args, None, None)
        .await
        .expect("tracedecay_gini over production MCP");
    extract_json(&result.value)
}

/// Equal values do not define an outlier rank. Sort by name so the assertion
/// stays on the reported rows rather than `HashMap` iteration order.
fn outliers_sorted_by_name(mut payload: Value) -> Value {
    if let Some(outliers) = payload.get_mut("outliers").and_then(Value::as_array_mut) {
        outliers.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    }
    payload
}

#[tokio::test]
async fn gini_reports_literal_coefficients_for_known_distributions() {
    let host = production_composition_fixture_with_sources(write_gini_distribution_sources).await;

    let lines = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "scope": "file",
            "path": "src/spans",
        }),
    )
    .await;
    assert_eq!(
        lines,
        json!({
            "gini": 0.25,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/tall.rs", "value": 3.0, "pct_of_max": 100.0},
                {"name": "src/spans/short.rs", "value": 1.0, "pct_of_max": 33.0},
            ],
        }),
        "line spans 1 and 3 must produce Gini 0.25: {lines}"
    );

    let truncated = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "scope": "file",
            "path": "src/spans",
            "limit": 1,
        }),
    )
    .await;
    assert_eq!(
        truncated,
        json!({
            "gini": 0.25,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/tall.rs", "value": 3.0, "pct_of_max": 100.0},
            ],
        }),
        "limit truncates the ranking and keeps the census: {truncated}"
    );

    let one_file = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "path": "src/spans/tall.rs",
        }),
    )
    .await;
    assert_eq!(
        one_file,
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 1,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/tall.rs", "value": 3.0, "pct_of_max": 100.0},
            ],
        }),
        "a single file is perfect equality: {one_file}"
    );

    let missing = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "lines",
            "path": "src/nowhere",
        }),
    )
    .await;
    assert_eq!(
        missing,
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 0,
            "metric": "lines",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [],
        }),
        "a path with no symbols is an empty census, not the unfiltered one: {missing}"
    );

    // Defaults are complexity + file. Both span functions score 1, so this
    // coefficient is 0. The lines call above is 0.25 for the same path.
    let defaults = gini_json(
        &host,
        json!({
            "format": "json",
            "path": "src/spans",
        }),
    )
    .await;
    assert_eq!(
        outliers_sorted_by_name(defaults.clone()),
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 2,
            "metric": "complexity",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/spans/short.rs", "value": 1.0, "pct_of_max": 100.0},
                {"name": "src/spans/tall.rs", "value": 1.0, "pct_of_max": 100.0},
            ],
        }),
        "default metric is complexity, not lines: {defaults}"
    );

    let members = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "members",
            "path": "src/kinds.rs",
        }),
    )
    .await;
    assert_eq!(
        members,
        json!({
            "gini": 0.25,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "members",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "Big", "value": 3.0, "pct_of_max": 100.0},
                {"name": "Tiny", "value": 1.0, "pct_of_max": 33.0},
            ],
        }),
        "struct member counts 1 and 3 must produce Gini 0.25: {members}"
    );

    let symbols = gini_json(
        &host,
        json!({
            "format": "json",
            "metric": "complexity",
            "scope": "symbol",
            "path": "src/kinds.rs",
        }),
    )
    .await;
    assert_eq!(
        symbols,
        json!({
            "gini": 0.3,
            "interpretation": "moderate inequality",
            "total_items": 2,
            "metric": "complexity",
            "scope": "symbol",
            "incomplete_complexity_symbols": 0,
            "outliers": [
                {"name": "src/kinds.rs:branched", "value": 4.0, "pct_of_max": 100.0},
                {"name": "src/kinds.rs:plain", "value": 1.0, "pct_of_max": 25.0},
            ],
        }),
        "symbol scores 1 and 4 must produce Gini 0.3: {symbols}"
    );

    close_test_graph(host).await;
}

#[tokio::test]
async fn gini_empty_index_reports_perfect_equality() {
    let (host, _, _) = setup_empty_analysis_project().await;
    let payload = gini_json(&host, json!({"format": "json"})).await;
    assert_eq!(
        payload,
        json!({
            "gini": 0.0,
            "interpretation": "low inequality (healthy)",
            "total_items": 0,
            "metric": "complexity",
            "scope": "file",
            "incomplete_complexity_symbols": 0,
            "outliers": [],
        }),
        "an empty index is not a missing field: {payload}"
    );
    close_test_graph(host).await;
}
