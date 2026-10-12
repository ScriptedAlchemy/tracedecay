//! `tracedecay_test_risk`, `tracedecay_dead_code`, and `tracedecay_health`
//! share one test classification over the indexed generation.
//!
//! Production code whose name or directory merely starts with `test` stays
//! visible: `test_connection` and `testimony` in `src/lib.rs`, `connect` in
//! `src/testnet/client.rs`, and `same_line`, the sibling sharing a line with
//! `#[cfg(test)] fn cfg_only`. Test code is what a path marker, a test
//! annotation, or `#[cfg(test)]` proves: the `tests/` directory, `#[test]`
//! and `#[tokio::test]`, `cfg_only`, the inline `#[cfg(test)] mod tests`,
//! and the out-of-line `#[cfg(test)] mod gated;` in `src/gated.rs`.
//!
//! Each report keeps its own handling of that classification. Test risk leaves
//! all test code out of its census. Dead code and health treat only actual
//! tests as entrypoints, so an uncalled test helper is still dead code —
//! including `test_helper`, a `test*` name in a Rust test file, which the
//! Rust runner never invokes without `#[test]`. The other runners' file and
//! owner rules are pinned by unit tests in `test_classification`.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

const LIB_RS: &str = r#"pub mod testnet;

pub fn test_connection() -> bool {
    true
}

fn testimony() -> i32 {
    1
}

/// skip-test-coverage
pub fn skipped_on_purpose() {}

#[cfg(test)]
fn cfg_only() {} fn same_line() {}

#[cfg(test)]
mod gated;

#[cfg(test)]
mod tests {
    fn helper() -> bool {
        super::test_connection()
    }

    #[test]
    fn checks_connection() {
        assert!(helper());
    }

    #[tokio::test]
    async fn checks_async() {}
}
"#;

fn write_sources(project: &Path) {
    fs::create_dir_all(project.join("src/testnet")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"classification_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/testnet/mod.rs"), "pub mod client;\n").unwrap();
    fs::write(
        project.join("src/testnet/client.rs"),
        "fn connect() -> bool {\n    true\n}\n",
    )
    .unwrap();
    fs::write(project.join("src/gated.rs"), "fn gated_helper() {}\n").unwrap();
    fs::write(
        project.join("tests/integration.rs"),
        "fn integration_helper() {}\n\nfn test_helper() {}\n\n#[test]\nfn runs_integration() {}\n",
    )
    .unwrap();
}

async fn call_json(fixture: &ProductionCompositionFixture, tool: &str, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool} returned a production MCP error: {:?}",
        response.error
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool} returned no production MCP result"));
    let text = extract_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{tool} JSON ({error}): {text}"))
}

/// The census a test-risk report attributes, without ranking-only fields.
fn test_risk_census(report: &Value) -> Value {
    let mut rows = report["risks"]
        .as_array()
        .unwrap_or_else(|| panic!("test risk must return risks: {report}"))
        .iter()
        .map(|risk| {
            json!({
                "name": risk["name"],
                "file": risk["file"],
                "attribution_method": risk["attribution_method"],
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row["name"].as_str().unwrap_or_default().to_owned());
    let summary = &report["summary"];
    json!({
        "rows": rows,
        "total_functions": summary["total_functions"],
        "tested": summary["tested"],
        "skipped": summary["skipped"],
        "excluded": summary["buckets"]["excluded"],
    })
}

fn dead_code_census(report: &Value) -> Value {
    let mut rows = report["symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("dead code must return symbols: {report}"))
        .iter()
        .map(|symbol| json!({"name": symbol["name"], "file": symbol["file"]}))
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row["name"].as_str().unwrap_or_default().to_owned());
    Value::Array(rows)
}

fn redundancy(report: &Value) -> Value {
    json!({
        "dead_count": report["dimensions"]["redundancy"]["dead_count"],
        "total_fns": report["dimensions"]["redundancy"]["total_fns"],
    })
}

#[tokio::test]
async fn reports_share_one_classification_and_keep_production_test_names_visible() {
    let fixture = production_composition_fixture_with_sources(write_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production classification server");
    wait_for_current_graph(&server).await;

    let risk = call_json(
        &fixture,
        "tracedecay_test_risk",
        json!({"format": "json", "include_tested": true}),
    )
    .await;
    assert_eq!(
        test_risk_census(&risk),
        json!({
            "rows": [
                {"name": "connect", "file": "src/testnet/client.rs", "attribution_method": "none"},
                {"name": "same_line", "file": "src/lib.rs", "attribution_method": "none"},
                {"name": "test_connection", "file": "src/lib.rs", "attribution_method": "closure"},
                {"name": "testimony", "file": "src/lib.rs", "attribution_method": "none"},
            ],
            "total_functions": 4,
            "tested": 1,
            "skipped": 1,
            "excluded": 0,
        }),
        "test risk must count every production function and no test code: {risk}"
    );

    let testnet_risk = call_json(
        &fixture,
        "tracedecay_test_risk",
        json!({"format": "json", "include_tested": true, "path": "src/testnet"}),
    )
    .await;
    assert_eq!(
        test_risk_census(&testnet_risk),
        json!({
            "rows": [
                {"name": "connect", "file": "src/testnet/client.rs", "attribution_method": "none"},
            ],
            "total_functions": 1,
            "tested": 0,
            "skipped": 0,
            "excluded": 0,
        }),
        "a directory named testnet is production code under a scoped call: {testnet_risk}"
    );

    let gated_risk = call_json(
        &fixture,
        "tracedecay_test_risk",
        json!({"format": "json", "include_tested": true, "path": "src/gated.rs"}),
    )
    .await;
    assert_eq!(
        test_risk_census(&gated_risk),
        json!({
            "rows": [],
            "total_functions": 0,
            "tested": 0,
            "skipped": 0,
            "excluded": 0,
        }),
        "a #[cfg(test)] module file stays test code when the scope excludes its declaration: {gated_risk}"
    );

    let dead = call_json(&fixture, "tracedecay_dead_code", json!({"format": "json"})).await;
    let expected_private_dead = json!([
        {"name": "cfg_only", "file": "src/lib.rs"},
        {"name": "connect", "file": "src/testnet/client.rs"},
        {"name": "gated_helper", "file": "src/gated.rs"},
        {"name": "integration_helper", "file": "tests/integration.rs"},
        {"name": "same_line", "file": "src/lib.rs"},
        {"name": "test_helper", "file": "tests/integration.rs"},
        {"name": "testimony", "file": "src/lib.rs"},
    ]);
    assert_eq!(
        dead_code_census(&dead),
        expected_private_dead,
        "dead code must report uncalled production and helper functions but no actual test: {dead}"
    );
    let dead_with_public = call_json(
        &fixture,
        "tracedecay_dead_code",
        json!({"format": "json", "include_public": true}),
    )
    .await;
    assert_eq!(
        dead_code_census(&dead_with_public),
        json!([
            {"name": "cfg_only", "file": "src/lib.rs"},
            {"name": "connect", "file": "src/testnet/client.rs"},
            {"name": "gated_helper", "file": "src/gated.rs"},
            {"name": "integration_helper", "file": "tests/integration.rs"},
            {"name": "same_line", "file": "src/lib.rs"},
            {"name": "skipped_on_purpose", "file": "src/lib.rs"},
            {"name": "test_helper", "file": "tests/integration.rs"},
            {"name": "testimony", "file": "src/lib.rs"},
        ]),
        "include_public adds the uncalled public function and still no actual test: {dead_with_public}"
    );
    let scoped_dead = call_json(
        &fixture,
        "tracedecay_dead_code",
        json!({"format": "json", "path": "src/gated.rs"}),
    )
    .await;
    assert_eq!(
        dead_code_census(&scoped_dead),
        json!([{"name": "gated_helper", "file": "src/gated.rs"}]),
        "{scoped_dead}"
    );

    let health = call_json(
        &fixture,
        "tracedecay_health",
        json!({"format": "json", "details": true}),
    )
    .await;
    assert_eq!(
        redundancy(&health),
        json!({"dead_count": 7, "total_fns": 13}),
        "health counts every function and the same seven dead functions: {health}"
    );
    let gated_health = call_json(
        &fixture,
        "tracedecay_health",
        json!({"format": "json", "details": true, "path": "src/gated.rs"}),
    )
    .await;
    assert_eq!(
        redundancy(&gated_health),
        json!({"dead_count": 1, "total_fns": 1}),
        "{gated_health}"
    );

    fixture.harness.shutdown().await;
}
