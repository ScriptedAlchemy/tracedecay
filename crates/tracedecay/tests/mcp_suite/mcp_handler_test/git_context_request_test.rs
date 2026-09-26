//! Git-context tools decode their arguments against a typed request over the
//! production MCP `tools/call` path: an argument outside the request contract
//! is refused instead of being silently defaulted or ignored.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

fn write_probe_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn probe() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/probe_test.rs"),
        "#[test]\nfn probe_runs() {\n    assert_eq!(1, 1);\n}\n",
    )
    .unwrap();
}

fn git_stdout(project: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

async fn call_json(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> String {
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
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    extract_text(&result).to_owned()
}

async fn refusal(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.result.is_none(),
        "{tool_name} must refuse the request, answered {:?}",
        response.result
    );
    response
        .error
        .unwrap_or_else(|| panic!("{tool_name} must refuse the request"))
        .message
}

#[tokio::test]
async fn git_context_tools_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture_with_sources(write_probe_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production git-context server");
    wait_for_current_graph(&server).await;

    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_commit_context",
            json!({"staged_only": true, "format": "json"}),
        )
        .await,
        r#"{"changed_files":[],"recent_commits":["production composition fixture"],"suggested_category":null,"summary":"No changes detected.","symbols_by_role":{}}"#
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_commit_context",
            json!({"staged_only": "yes"})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_commit_context: invalid type: string \"yes\", expected a boolean"
    );

    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_affected",
            json!({"files": ["tests/probe_test.rs"], "depth": 3, "format": "json"}),
        )
        .await,
        r#"{"affected_tests":["tests/probe_test.rs"],"changed_files":["tests/probe_test.rs"],"count":1,"ranked_tests":[{"distance":0,"path":"tests/probe_test.rs","proximity":"changed","rank":1}],"ranking_metadata":{"compatibility_field":"affected_tests","distance":"minimum file-dependency hops from the changed files","recommended_proximity":["changed","direct","near"],"strategy":"dependency_distance_then_path"},"recommended_tests":["tests/probe_test.rs"]}"#
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_affected",
            json!({"files": ["tests/probe_test.rs"], "depth": "3"}),
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_affected: invalid type: string \"3\", expected u32"
    );

    let project = fixture.project_root.as_path();
    let branch = git_stdout(project, &["branch", "--show-current"]);
    let commit = git_stdout(project, &["rev-parse", "HEAD^{commit}"]);
    let tree = git_stdout(project, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_branch_list",
            json!({"limit": 5, "format": "json"}),
        )
        .await,
        format!(
            r#"{{"examined":1,"limit":5,"next_after":null,"reason":null,"snapshot_count":1,"snapshots":[{{"branch":"{branch}","source_revision":"{commit}","source_tree":"{tree}"}}],"status":"complete"}}"#
        )
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_branch_list", json!({"limt": 5})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_branch_list: unknown field `limt`, expected `limit` or `after`"
    );

    fixture.harness.shutdown().await;
}
