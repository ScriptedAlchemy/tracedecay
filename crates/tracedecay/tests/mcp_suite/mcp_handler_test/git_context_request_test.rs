//! Git-context tools decode their arguments against a typed request over the
//! production MCP `tools/call` path: an argument outside the request contract
//! is refused instead of being silently defaulted or ignored.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    refusal_problem, wait_for_current_graph,
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
) -> Value {
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
        .unwrap_or_else(|| panic!("{tool_name} must refuse the request"));
    refusal_problem(&result).clone()
}

fn assert_invalid_request(problem: &Value, message: &str) {
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "application.surface.invalid_request");
    assert_eq!(problem["message"], message);
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
        r#"{"changed_files":[],"freshness":{"state":"fresh"},"recent_commits":["production composition fixture"],"suggested_category":null,"summary":"No changes detected.","symbols_by_role":{}}"#
    );
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_commit_context",
            json!({"staged_only": "yes"}),
        )
        .await,
        "invalid arguments for tracedecay_commit_context: invalid type: string \"yes\", expected a boolean",
    );

    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_affected",
            json!({"files": ["tests/probe_test.rs"], "depth": 3, "format": "json"}),
        )
        .await,
        r#"{"affected_tests":["tests/probe_test.rs"],"changed_files":["tests/probe_test.rs"],"count":1,"freshness":{"state":"fresh"},"ranked_tests":[{"distance":0,"path":"tests/probe_test.rs","proximity":"changed","rank":1}],"ranking_metadata":{"compatibility_field":"affected_tests","distance":"minimum file-dependency hops from the changed files","recommended_proximity":["changed","direct","near"],"strategy":"dependency_distance_then_path"},"recommended_tests":["tests/probe_test.rs"]}"#
    );
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_affected",
            json!({"files": ["tests/probe_test.rs"], "depth": "3"}),
        )
        .await,
        "invalid arguments for tracedecay_affected: invalid type: string \"3\", expected u32",
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
            r#"{{"examined":1,"limit":5,"next_cursor":null,"reason":null,"snapshot_count":1,"snapshots":[{{"branch":"{branch}","source_revision":"{commit}","source_tree":"{tree}"}}],"status":"complete"}}"#
        )
    );
    assert_invalid_request(
        &refusal(&fixture, "tracedecay_branch_list", json!({"limt": 5})).await,
        "invalid arguments for tracedecay_branch_list: unknown field `limt`, expected `limit` or `cursor`",
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn pr_context_budgeted_cursor_returns_every_changed_symbol_once() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "pub fn changed_alpha() -> u32 { 1 }\n\
             pub fn changed_beta() -> u32 { 2 }\n\
             pub fn changed_gamma() -> u32 { 3 }\n\
             pub fn changed_delta() -> u32 { 4 }\n\
             pub fn removed_alpha() {}\n\
             pub fn removed_beta() {}\n\
             pub fn removed_gamma() {}\n\
             pub fn removed_delta() {}\n",
        )
        .unwrap();
        crate::support::commit_worktree(project, "base symbols");
        git_stdout(project, &["branch", "pr-base"]);
        git_stdout(project, &["checkout", "-b", "pr-feature"]);
        fs::write(
            project.join("src/lib.rs"),
            "pub fn changed_alpha() -> u32 { 11 }\n\
             pub fn changed_beta() -> u32 { 12 }\n\
             pub fn changed_gamma() -> u32 { 13 }\n\
             pub fn changed_delta() -> u32 { 14 }\n\
             pub fn added_alpha() {}\n\
             pub fn added_beta() {}\n\
             pub fn added_gamma() {}\n\
             pub fn added_delta() {}\n",
        )
        .unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;

    for maximum_symbols in [200, 2] {
        let mut cursor = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_cursors = std::collections::HashSet::new();
        let mut pages = 0;
        loop {
            let output = call_json(
                &fixture,
                "tracedecay_pr_context",
                json!({
                    "base_ref": "pr-base", "head_ref": "pr-feature", "format": "json",
                    "maximum_symbols": maximum_symbols, "budget_tokens": 1, "cursor": cursor,
                }),
            )
            .await;
            let page: Value = serde_json::from_str(&output).expect("PR page JSON");
            assert_eq!(page["status"], "complete", "{page}");
            let mut returned = 0;
            for section in ["added", "removed", "modified"] {
                let rows = page[section].as_array().unwrap();
                assert_eq!(
                    page[format!("symbols_{section}")],
                    json!(rows.len()),
                    "{page}"
                );
                for row in rows {
                    assert!(
                        seen_ids.insert(row["id"].as_str().unwrap().to_owned()),
                        "repeated occurrence: {row}"
                    );
                    assert!(
                        seen.insert((section.to_owned(), row["name"].as_str().unwrap().to_owned())),
                        "repeated changed symbol: {row}"
                    );
                }
                let budget_section = page["token_budget"]["sections"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["section"] == section)
                    .unwrap();
                assert_eq!(budget_section["shown"], json!(rows.len()), "{page}");
                assert!(budget_section["total"].as_u64().unwrap() >= rows.len() as u64);
                returned += rows.len();
            }
            assert!(returned > 0 && returned <= maximum_symbols, "{page}");
            assert_eq!(page["symbol_page"]["returned"], json!(returned), "{page}");
            assert_eq!(
                page["analysis_coverage"]["symbols_returned"],
                json!(returned),
                "{page}"
            );
            assert_eq!(page["token_budget"]["over_ceiling"], true, "{page}");
            pages += 1;
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            let has_more = cursor.is_some();
            assert_eq!(page["symbol_page"]["has_more"], has_more, "{page}");
            assert_eq!(page["symbol_page"]["complete"], !has_more, "{page}");
            assert_eq!(
                page["symbol_page"]["continuation_available"], has_more,
                "{page}"
            );
            assert_eq!(
                page["analysis_coverage"]["symbols_complete"], !has_more,
                "{page}"
            );
            if let Some(cursor) = cursor.as_ref() {
                assert!(seen_cursors.insert(cursor.clone()), "cursor must advance");
                assert!(
                    pages < 12,
                    "cursor walk must finish after twelve changed symbols"
                );
            } else {
                break;
            }
        }
        assert!(pages > 1, "tiny budget must require continuation");
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            vec![
                ("added".to_owned(), "added_alpha".to_owned()),
                ("added".to_owned(), "added_beta".to_owned()),
                ("added".to_owned(), "added_delta".to_owned()),
                ("added".to_owned(), "added_gamma".to_owned()),
                ("modified".to_owned(), "changed_alpha".to_owned()),
                ("modified".to_owned(), "changed_beta".to_owned()),
                ("modified".to_owned(), "changed_delta".to_owned()),
                ("modified".to_owned(), "changed_gamma".to_owned()),
                ("removed".to_owned(), "removed_alpha".to_owned()),
                ("removed".to_owned(), "removed_beta".to_owned()),
                ("removed".to_owned(), "removed_delta".to_owned()),
                ("removed".to_owned(), "removed_gamma".to_owned()),
            ]
        );
    }
    fixture.harness.shutdown().await;
}
