//! What an agent observes when it calls `tracedecay_run_affected_tests`.
//!
//! The call goes through the production MCP `tools/call` path and the real
//! `cargo test` runner. Assertions are the JSON the caller reads, not which
//! helper the handler invoked.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_application::operation_stream::operation_event_authority;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{Deadline, OperationBudgetUsage, OperationReceipt};
use tracedecay_domain::UtcMicros;
use tracedecay_mcp::ToolResult;
use url::Url;

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

const GREETING_TEST: &str = "tests::greeting_is_hello_world";
const FAILING_TEST: &str = "greeting_is_goodbye";
const KEPT_TEST: &str = "alpha_kept";

fn write_affected_fixture(project: &Path) {
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

async fn call_tool(
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

fn parse_tool_json(tool_name: &str, result: &ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool_name} did not return JSON ({error}): {text}"))
}

async fn run_affected(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let mut arguments = arguments;
    if let Some(object) = arguments.as_object_mut() {
        object
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    parse_tool_json(
        "tracedecay_run_affected_tests",
        &call_tool(fixture, "tracedecay_run_affected_tests", arguments).await,
    )
}

async fn symbol_id(fixture: &ProductionCompositionFixture, name: &str, file: &str) -> String {
    let result = call_tool(
        fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    let payload = parse_tool_json("tracedecay_find_exact_symbol", &result);
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
async fn run_affected_tests_reports_the_cargo_result_for_the_changed_manifest() {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let fixture = production_composition_fixture_with_sources(write_affected_fixture).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production affected-tests server");
    wait_for_current_graph(&server).await;

    assert_rejection(
        &run_affected(&fixture, json!({"timeout_secs": 1})).await,
        "changed_paths",
        "`changed_paths` is required and must explicitly scope the affected-test run",
    );
    assert_rejection(
        &run_affected(
            &fixture,
            json!({"changed_paths": "src/lib.rs", "timeout_secs": 1}),
        )
        .await,
        "changed_paths",
        "`changed_paths` must be an array of project-relative string paths",
    );
    assert_rejection(
        &run_affected(
            &fixture,
            json!({"changed_paths": ["src/lib.rs", 7], "timeout_secs": 1}),
        )
        .await,
        "changed_paths",
        "`changed_paths` must contain only project-relative string paths",
    );
    assert_rejection(
        &run_affected(
            &fixture,
            json!({"changed_paths": ["src/lib.rs"], "profile": "bench", "timeout_secs": 1}),
        )
        .await,
        "profile",
        "`profile` must be `debug` or `release`",
    );
    assert_rejection(
        &run_affected(
            &fixture,
            json!({"changed_paths": ["src/lib.rs"], "max_tests": 0, "timeout_secs": 1}),
        )
        .await,
        "max_tests",
        "`max_tests` must be an integer from 1 through 500",
    );
    assert_empty_note(
        &run_affected(&fixture, json!({"changed_paths": [], "timeout_secs": 1})).await,
        "no changed files detected",
    );
    assert_empty_note(
        &run_affected(
            &fixture,
            json!({"changed_paths": ["src/orphan.rs"], "timeout_secs": 1}),
        )
        .await,
        "no tests cover the changed paths (1 file(s))",
    );

    let greeting_id = symbol_id(&fixture, "greeting", "src/lib.rs").await;
    let greeting_test_id = symbol_id(&fixture, "greeting_is_hello_world", "src/lib.rs").await;
    let failing_id = symbol_id(&fixture, "greeting_is_goodbye", "tests/failing_greeting.rs").await;
    let kept_id = symbol_id(&fixture, "alpha_kept", "tests/ordered.rs").await;

    let covered = run_affected(
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

    let failed = run_affected(
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
        json!([FAILING_TEST]),
        "failed payload: {failed}"
    );
    assert_eq!(
        failed["results"],
        json!([{
            "test": FAILING_TEST,
            "passed": false,
            "covers_source_ids": [failing_id],
        }]),
        "failed payload: {failed}"
    );
    assert_stdout_line(&failed, "affected-test-failed: goodbye");
    assert_stdout_line(&failed, &format!("test {FAILING_TEST} ... FAILED"));
    assert_eq!(
        failed["terminal"]["receipt"]["termination"],
        json!("completed"),
        "a reported test failure is still a completed run: {failed}"
    );

    let truncated = run_affected(
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

/// The operation-event authority is process-global, so every daemon
/// composition in this test process shares it. A peer composition shutting
/// down (the production `DaemonInvocationState::shutdown` path) between a
/// managed run's admission and its first result must leave the accepted
/// record in place: the first result lands at sequence 1 behind it instead
/// of failing with an expired history frontier.
#[tokio::test]
async fn operation_history_keeps_an_admitted_test_run_across_a_peer_composition_shutdown() {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let owner = production_composition_fixture_with_sources(write_affected_fixture).await;
    let peer = production_composition_fixture().await;

    let root_uri = Url::from_directory_path(
        fs::canonicalize(&owner.project_root).expect("canonical affected fixture root"),
    )
    .expect("affected fixture root URI")
    .to_string();
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let emitter = operation_event_authority()
        .begin_managed_test_run(
            root_uri,
            mint_global_request_id(GlobalRequestSurface::ManagedTestRun).expect("request id"),
            None,
            None,
            BTreeMap::new(),
            deadline.clone(),
        )
        .await
        .expect("managed test run admitted before the peer shuts down");

    peer.harness.shutdown().await;

    let first_result = emitter
        .test_result(GREETING_TEST.to_owned(), true)
        .await
        .expect("first result must land after a peer composition shutdown");
    assert_eq!(
        first_result.sequence, 1,
        "the accepted record at sequence 0 precedes the first result"
    );
    emitter
        .terminal(
            OperationReceipt::completed(
                UtcMicros(1),
                UtcMicros(2),
                deadline,
                OperationBudgetUsage::default(),
            )
            .expect("receipt"),
        )
        .await
        .expect("terminal receipt after the first result");

    owner.harness.shutdown().await;
}
