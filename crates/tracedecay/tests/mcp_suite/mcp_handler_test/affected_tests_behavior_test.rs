//! Production MCP behavior for `tracedecay_run_affected_tests`.
//!
//! The call goes through the mounted server's `tools/call` path, the same
//! dispatch a host uses. Assertions name the JSON an agent reads, not the
//! runner the handler happens to call.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

const TOOL: &str = "tracedecay_run_affected_tests";
const FAILING_TEST: &str = "math::tests::one_plus_one_is_three";
const PASSING_TEST: &str = "math::tests::one_plus_one_is_two";

fn write_sample_crate(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"affected_sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub mod math;\npub mod marker;\n",
    )
    .unwrap();
    // A callable with no test callers. Indexed, but not a reason to run cargo.
    fs::write(
        project.join("src/marker.rs"),
        "pub fn unused_marker_value() -> u32 {\n    7\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/math.rs"),
        "pub fn add(left: u32, right: u32) -> u32 {\n    left + right\n}\n\n\
         #[cfg(test)]\nmod tests {\n    use super::add;\n\n    \
         #[test]\n    fn one_plus_one_is_two() {\n        assert_eq!(add(1, 1), 2);\n    }\n\n    \
         #[test]\n    fn one_plus_one_is_three() {\n        assert_eq!(add(1, 1), 3);\n    }\n}\n",
    )
    .unwrap();
}

fn parse_tool_json(text: &str) -> Value {
    if let Ok(value) = serde_json::from_str(text) {
        return value;
    }
    let start = text.find('{').unwrap_or(0);
    let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
    values
        .next()
        .unwrap_or_else(|| panic!("tool text has no JSON value: {text}"))
        .unwrap_or_else(|error| panic!("tool text is not JSON ({error}): {text}"))
}

async fn call_tool(server: &McpServer, name: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, name, arguments).await;
    parse_tool_json(extract_real_server_text(&result))
}

async fn occurrence_id(server: &McpServer, name: &str, qualified_name: &str) -> String {
    let payload = call_tool(
        server,
        "tracedecay_find_exact_symbol",
        json!({ "name": name, "limit": 20 }),
    )
    .await;
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|item| item["qualified_name"] == qualified_name)
        })
        .and_then(|item| item["id"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("indexed symbol {qualified_name} missing from {payload}"))
}

fn covered_source_ids(payload: &Value, test_name: &str) -> Vec<String> {
    payload["results"]
        .as_array()
        .and_then(|results| results.iter().find(|item| item["test"] == test_name))
        .and_then(|item| item["covers_source_ids"].as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_else(|| panic!("{test_name} is missing from {payload}"))
}

#[tokio::test]
async fn run_affected_tests_reports_the_cargo_result_for_the_changed_file() {
    let fixture = production_composition_fixture_with_sources(write_sample_crate).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted production MCP server");
    wait_for_current_graph(&server).await;

    let missing_manifest = call_tool(&server, TOOL, json!({ "timeout_secs": 1 })).await;
    assert_eq!(
        missing_manifest,
        json!({
            "passed": 0,
            "failed": 0,
            "results": [],
            "error": {
                "kind": "invalid_request",
                "operation": "changed_paths",
                "message": "`changed_paths` is required and must explicitly scope the affected-test run"
            }
        })
    );

    let rejected_profile = call_tool(&server, TOOL, json!({ "profile": "bench" })).await;
    assert_eq!(
        rejected_profile,
        json!({
            "passed": 0,
            "failed": 0,
            "results": [],
            "error": {
                "kind": "invalid_request",
                "operation": "profile",
                "message": "`profile` must be `debug` or `release`"
            }
        })
    );

    let rejected_paths = call_tool(
        &server,
        TOOL,
        json!({ "changed_paths": ["src/math.rs", 7] }),
    )
    .await;
    assert_eq!(
        rejected_paths,
        json!({
            "passed": 0,
            "failed": 0,
            "results": [],
            "error": {
                "kind": "invalid_request",
                "operation": "changed_paths",
                "message": "`changed_paths` must contain only project-relative string paths"
            }
        })
    );

    // Presence first: the marker function is indexed. The next call must
    // still report no tests, rather than treating a missing index row as
    // an empty success.
    let marker = call_tool(
        &server,
        "tracedecay_find_exact_symbol",
        json!({ "name": "unused_marker_value", "limit": 20 }),
    )
    .await;
    assert_eq!(marker["count"], 1);
    assert_eq!(marker["matches"][0]["name"], "unused_marker_value");
    assert_eq!(
        marker["matches"][0]["qualified_name"],
        "src/marker.rs::unused_marker_value"
    );
    assert_eq!(marker["matches"][0]["file"], "src/marker.rs");
    assert_eq!(marker["matches"][0]["kind"], "function");
    let uncovered = call_tool(
        &server,
        TOOL,
        json!({ "changed_paths": ["src/marker.rs"], "timeout_secs": 30 }),
    )
    .await;
    assert_eq!(
        uncovered,
        json!({
            "passed": 0,
            "failed": 0,
            "results": [],
            "note": "no tests cover the changed paths (1 file(s))"
        })
    );

    let add = occurrence_id(&server, "add", "src/math.rs::add").await;
    let failing = occurrence_id(
        &server,
        "one_plus_one_is_three",
        "src/math.rs::tests::one_plus_one_is_three",
    )
    .await;
    let passing = occurrence_id(
        &server,
        "one_plus_one_is_two",
        "src/math.rs::tests::one_plus_one_is_two",
    )
    .await;

    let observed = call_tool(
        &server,
        TOOL,
        json!({
            "changed_paths": ["src/math.rs"],
            "profile": "debug",
            "timeout_secs": 180,
            "max_tests": 5
        }),
    )
    .await;

    assert_eq!(observed["exit_code"], 101);
    assert_eq!(observed["passed"], 1);
    assert_eq!(observed["failed"], 1);
    assert_eq!(observed["total_observed"], 2);
    assert_eq!(observed["truncated"], false);
    assert_eq!(
        observed["dispatched_tests"],
        json!([FAILING_TEST, PASSING_TEST])
    );
    assert_eq!(
        observed["results"]
            .as_array()
            .map(|results| {
                results
                    .iter()
                    .map(|item| {
                        json!({
                            "test": item["test"],
                            "passed": item["passed"],
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| panic!("results missing: {observed}")),
        vec![
            json!({ "test": FAILING_TEST, "passed": false }),
            json!({ "test": PASSING_TEST, "passed": true }),
        ]
    );
    // Direct dispatch records the test itself; the call from each test to
    // `add` records the changed source. Order is self, then the source.
    assert_eq!(
        covered_source_ids(&observed, FAILING_TEST),
        vec![failing.clone(), add.clone()]
    );
    assert_eq!(
        covered_source_ids(&observed, PASSING_TEST),
        vec![passing, add.clone()]
    );
    let stdout = observed["stdout_tail"].as_str().unwrap_or("");
    assert!(
        stdout.contains(&format!("test {FAILING_TEST} ... FAILED")),
        "libtest must have executed the failing test, stdout:\n{stdout}\npayload: {observed}"
    );
    assert!(
        stdout.contains(&format!("test {PASSING_TEST} ... ok")),
        "libtest must have executed the passing test, stdout:\n{stdout}\npayload: {observed}"
    );
    let stderr = observed["stderr_tail"].as_str().unwrap_or("");
    assert!(
        stderr.contains("assertion `left == right` failed") && stderr.contains("left: 2"),
        "the failing assertion must be the one in the fixture, stderr:\n{stderr}"
    );
    assert_eq!(observed["terminal"]["receipt"]["termination"], "completed");
    assert_eq!(
        observed["terminal"]["result_tool"],
        "tracedecay_test_results"
    );

    let truncated = call_tool(
        &server,
        TOOL,
        json!({
            "changed_paths": ["src/math.rs"],
            "timeout_secs": 60,
            "max_tests": 1
        }),
    )
    .await;
    assert_eq!(truncated["dispatched_tests"], json!([FAILING_TEST]));
    assert_eq!(truncated["passed"], 0);
    assert_eq!(truncated["failed"], 1);
    assert_eq!(truncated["truncated"], true);
    assert_eq!(truncated["exit_code"], 101);
    assert_eq!(truncated["results"][0]["test"], FAILING_TEST);
    assert_eq!(truncated["results"][0]["passed"], false);
    assert_eq!(
        covered_source_ids(&truncated, FAILING_TEST),
        vec![failing, add]
    );
    assert_eq!(truncated["terminal"]["receipt"]["termination"], "completed");

    fixture.harness.shutdown().await;
}
