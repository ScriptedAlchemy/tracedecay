#![cfg(feature = "test-transport")]

//! Production MCP behavior for `tracedecay_run_affected_tests`.
//!
//! The call goes through the mounted server's `tools/call` path, the same
//! dispatch a host uses. Assertions name the JSON an agent reads, not the
//! runner the handler happens to call.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_mcp::ToolResult;

use crate::support::{
    ProductionCompositionFixture, extract_text, extract_real_server_text,
    handle_real_server_tool_call, production_composition_fixture_with_sources, wait_for_current_graph,
};

const TOOL: &str = "tracedecay_run_affected_tests";
const MATH_FAILING_TEST: &str = "math::tests::one_plus_one_is_three";
const MATH_PASSING_TEST: &str = "math::tests::one_plus_one_is_two";
const GREETING_TEST: &str = "tests::greeting_is_hello_world";
const GREETING_FAILING_TEST: &str = "greeting_is_goodbye";
const KEPT_TEST: &str = "alpha_kept";

fn write_math_sample_crate(project: &Path) {
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

fn write_greeting_fixture(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"affected_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "mod orphan;\n\
         \n\
         pub fn greeting() -> &'static str {\n\
             \"hello-world\"\n\
         }\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n\
             use super::greeting;\n\
             \n\
             #[test]\n\
             fn greeting_is_hello_world() {\n\
                 assert_eq!(greeting(), \"hello-world\");\n\
             }\n\
         }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/orphan.rs"),
        "pub fn unused_symbol() -> u8 {\n    0\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/failing_greeting.rs"),
        "#[test]\nfn greeting_is_goodbye() {\n    panic!(\"affected-test-failed: goodbye\");\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/ordered.rs"),
        "#[test]\nfn alpha_kept() {}\n\n#[test]\nfn zeta_omitted() {\n    panic!(\"zeta must not run\");\n}\n",
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

async fn call_tool_direct(server: &tracedecay::mcp::McpServer, name: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, name, arguments).await;
    parse_tool_json(extract_real_server_text(&result))
}

async fn call_tool_fixture(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> ToolResult {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    ToolResult::new(
        response
            .result
            .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result")),
        Vec::new(),
    )
}

fn parse_tool_result_json(tool_name: &str, result: &ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool_name} did not return JSON ({error}): {text}"))
}

async fn run_affected_fixture(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let mut arguments = arguments;
    if let Some(object) = arguments.as_object_mut() {
        object
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    parse_tool_result_json(
        "tracedecay_run_affected_tests",
        &call_tool_fixture(fixture, "tracedecay_run_affected_tests", arguments).await,
    )
}

async fn occurrence_id_direct(server: &tracedecay::mcp::McpServer, name: &str, qualified_name: &str) -> String {
    let payload = call_tool_direct(
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

async fn symbol_id_fixture(fixture: &ProductionCompositionFixture, name: &str, file: &str) -> String {
    let result = call_tool_fixture(
        fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    let payload = parse_tool_result_json("tracedecay_find_exact_symbol", &result);
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find(|item| {
                item["name"] == name
                    && item["qualified_name"]
                        .as_str()
                        .is_some_and(|qualified| qualified.starts_with(file))
            })
        })
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("{file} symbol `{name}` missing from exact lookup: {payload}"))
        .to_owned()
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

fn assert_rejection(output: &Value, operation: &str, message: &str) {
    assert_eq!(
        output,
        &json!({
            "passed": 0,
            "failed": 0,
            "results": [],
            "error": {
                "kind": "invalid_request",
                "operation": operation,
                "message": message,
            }
        }),
        "rejection payload: {output}"
    );
}

fn assert_stdout_line(output: &Value, line: &str) {
    let stdout = output["stdout_tail"].as_str().unwrap_or("");
    assert!(
        stdout.lines().any(|candidate| candidate == line),
        "missing `{line}` in stdout_tail:\n{stdout}"
    );
}

fn assert_empty_note(output: &Value, note: &str) {
    assert_eq!(
        output,
        &json!({
            "passed": 0,
            "failed": 0,
            "results": [],
            "note": note,
        }),
        "empty-run payload: {output}"
    );
}

#[tokio::test]
async fn run_affected_tests_reports_the_cargo_result_for_the_changed_file() {
    let fixture = production_composition_fixture_with_sources(write_math_sample_crate).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted production MCP server");
    wait_for_current_graph(&server).await;

    let missing_manifest = call_tool_direct(&server, TOOL, json!({ "timeout_secs": 1 })).await;
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

    let rejected_profile = call_tool_direct(&server, TOOL, json!({ "profile": "bench" })).await;
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

    let rejected_paths = call_tool_direct(
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

    let marker = call_tool_direct(
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
    let uncovered = call_tool_direct(
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

    let add = occurrence_id_direct(&server, "add", "src/math.rs::add").await;
    let failing = occurrence_id_direct(
        &server,
        "one_plus_one_is_three",
        "src/math.rs::tests::one_plus_one_is_three",
    )
    .await;
    let passing = occurrence_id_direct(
        &server,
        "one_plus_one_is_two",
        "src/math.rs::tests::one_plus_one_is_two",
    )
    .await;

    let observed = call_tool_direct(
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
        json!([MATH_FAILING_TEST, MATH_PASSING_TEST])
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
            json!({ "test": MATH_FAILING_TEST, "passed": false }),
            json!({ "test": MATH_PASSING_TEST, "passed": true }),
        ]
    );
    assert_eq!(
        covered_source_ids(&observed, MATH_FAILING_TEST),
        vec![failing.clone(), add.clone()]
    );
    assert_eq!(
        covered_source_ids(&observed, MATH_PASSING_TEST),
        vec![passing, add.clone()]
    );
    let stdout = observed["stdout_tail"].as_str().unwrap_or("");
    assert!(
        stdout.contains(&format!("test {MATH_FAILING_TEST} ... FAILED")),
        "libtest must have executed the failing test, stdout:\n{stdout}\npayload: {observed}"
    );
    assert!(
        stdout.contains(&format!("test {MATH_PASSING_TEST} ... ok")),
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

    let truncated = call_tool_direct(
        &server,
        TOOL,
        json!({
            "changed_paths": ["src/math.rs"],
            "timeout_secs": 60,
            "max_tests": 1
        }),
    )
    .await;
    assert_eq!(truncated["dispatched_tests"], json!([MATH_FAILING_TEST]));
    assert_eq!(truncated["passed"], 0);
    assert_eq!(truncated["failed"], 1);
    assert_eq!(truncated["truncated"], true);
    assert_eq!(truncated["exit_code"], 101);
    assert_eq!(truncated["results"][0]["test"], MATH_FAILING_TEST);
    assert_eq!(truncated["results"][0]["passed"], false);
    assert_eq!(
        covered_source_ids(&truncated, MATH_FAILING_TEST),
        vec![failing, add]
    );
    assert_eq!(truncated["terminal"]["receipt"]["termination"], "completed");

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn run_affected_tests_reports_the_cargo_result_for_the_changed_manifest() {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let fixture = production_composition_fixture_with_sources(write_greeting_fixture).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production affected-tests server");
    wait_for_current_graph(&server).await;

    assert_rejection(
        &run_affected_fixture(&fixture, json!({"timeout_secs": 1})).await,
        "changed_paths",
        "`changed_paths` is required and must explicitly scope the affected-test run",
    );
    assert_rejection(
        &run_affected_fixture(
            &fixture,
            json!({"changed_paths": "src/lib.rs", "timeout_secs": 1}),
        )
        .await,
        "changed_paths",
        "`changed_paths` must be an array of project-relative string paths",
    );
    assert_rejection(
        &run_affected_fixture(
            &fixture,
            json!({"changed_paths": ["src/lib.rs", 7], "timeout_secs": 1}),
        )
        .await,
        "changed_paths",
        "`changed_paths` must contain only project-relative string paths",
    );
    assert_rejection(
        &run_affected_fixture(
            &fixture,
            json!({"changed_paths": ["src/lib.rs"], "profile": "bench", "timeout_secs": 1}),
        )
        .await,
        "profile",
        "`profile` must be `debug` or `release`",
    );
    assert_rejection(
        &run_affected_fixture(
            &fixture,
            json!({"changed_paths": ["src/lib.rs"], "max_tests": 0, "timeout_secs": 1}),
        )
        .await,
        "max_tests",
        "`max_tests` must be an integer from 1 through 500",
    );
    assert_empty_note(
        &run_affected_fixture(&fixture, json!({"changed_paths": [], "timeout_secs": 1})).await,
        "no changed files detected",
    );
    assert_empty_note(
        &run_affected_fixture(
            &fixture,
            json!({"changed_paths": ["src/orphan.rs"], "timeout_secs": 1}),
        )
        .await,
        "no tests cover the changed paths (1 file(s))",
    );

    let greeting_id = symbol_id_fixture(&fixture, "greeting", "src/lib.rs").await;
    let greeting_test_id = symbol_id_fixture(&fixture, "greeting_is_hello_world", "src/lib.rs").await;
    let failing_id = symbol_id_fixture(&fixture, "greeting_is_goodbye", "tests/failing_greeting.rs").await;
    let kept_id = symbol_id_fixture(&fixture, "alpha_kept", "tests/ordered.rs").await;

    let covered = run_affected_fixture(
        &fixture,
        json!({
            "changed_paths": ["src/lib.rs"],
            "profile": "debug",
            "timeout_secs": 120,
            "max_tests": 5,
        }),
    )
    .await;
    assert_eq!(covered["exit_code"], json!(0), "covered payload: {covered}");
    assert_eq!(covered["passed"], json!(1), "covered payload: {covered}");
    assert_eq!(covered["failed"], json!(0), "covered payload: {covered}");
    assert_eq!(
        covered["total_observed"],
        json!(1),
        "covered payload: {covered}"
    );
    assert_eq!(
        covered["truncated"],
        json!(false),
        "covered payload: {covered}"
    );
    assert_eq!(
        covered["dispatched_tests"],
        json!([GREETING_TEST]),
        "covered payload: {covered}"
    );
    assert_eq!(
        covered["results"],
        json!([{
            "test": GREETING_TEST,
            "passed": true,
            "covers_source_ids": [greeting_test_id, greeting_id],
        }]),
        "covered payload: {covered}"
    );
    assert_stdout_line(&covered, &format!("test {GREETING_TEST} ... ok"));
    assert_eq!(
        covered["terminal"]["result_tool"],
        json!("tracedecay_test_results"),
        "covered payload: {covered}"
    );
    assert_eq!(
        covered["terminal"]["receipt"]["termination"],
        json!("completed"),
        "covered payload: {covered}"
    );

    let failed = run_affected_fixture(
        &fixture,
        json!({
            "changed_paths": ["tests/failing_greeting.rs"],
            "profile": "debug",
            "timeout_secs": 120,
            "max_tests": 5,
        }),
    )
    .await;
    assert_eq!(failed["exit_code"], json!(101), "failed payload: {failed}");
    assert_eq!(failed["passed"], json!(0), "failed payload: {failed}");
    assert_eq!(failed["failed"], json!(1), "failed payload: {failed}");
    assert_eq!(
        failed["total_observed"],
        json!(1),
        "failed payload: {failed}"
    );
    assert_eq!(
        failed["truncated"],
        json!(false),
        "failed payload: {failed}"
    );
    assert_eq!(
        failed["dispatched_tests"],
        json!([GREETING_FAILING_TEST]),
        "failed payload: {failed}"
    );
    assert_eq!(
        failed["results"],
        json!([{
            "test": GREETING_FAILING_TEST,
            "passed": false,
            "covers_source_ids": [failing_id],
        }]),
        "failed payload: {failed}"
    );
    assert_stdout_line(&failed, "affected-test-failed: goodbye");
    assert_stdout_line(&failed, &format!("test {GREETING_FAILING_TEST} ... FAILED"));
    assert_eq!(
        failed["terminal"]["receipt"]["termination"],
        json!("completed"),
        "a reported test failure is still a completed run: {failed}"
    );

    let truncated = run_affected_fixture(
        &fixture,
        json!({
            "changed_paths": ["tests/ordered.rs"],
            "profile": "debug",
            "timeout_secs": 120,
            "max_tests": 1,
        }),
    )
    .await;
    assert_eq!(
        truncated["exit_code"],
        json!(0),
        "truncated payload: {truncated}"
    );
    assert_eq!(
        truncated["passed"],
        json!(1),
        "truncated payload: {truncated}"
    );
    assert_eq!(
        truncated["failed"],
        json!(0),
        "truncated payload: {truncated}"
    );
    assert_eq!(
        truncated["truncated"],
        json!(true),
        "truncated payload: {truncated}"
    );
    assert_eq!(
        truncated["dispatched_tests"],
        json!([KEPT_TEST]),
        "truncated payload: {truncated}"
    );
    assert_eq!(
        truncated["results"],
        json!([{
            "test": KEPT_TEST,
            "passed": true,
            "covers_source_ids": [kept_id],
        }]),
        "truncated payload: {truncated}"
    );
    assert_stdout_line(&truncated, &format!("test {KEPT_TEST} ... ok"));
    let truncated_stdout = truncated["stdout_tail"].as_str().unwrap_or("");
    assert!(
        !truncated_stdout.contains("zeta must not run"),
        "max_tests must not execute the omitted test:\n{truncated_stdout}"
    );

    fixture.harness.shutdown().await;
}
