//! Git-context tools decode their arguments against a typed request over the
//! production MCP `tools/call` path: an argument outside the request contract
//! is refused instead of being silently defaulted or ignored.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::CodeGraphReadinessRequirement;

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

    let head = git_stdout(&fixture.project_root, &["rev-parse", "HEAD^{commit}"]);
    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_commit_context",
            json!({"staged_only": true, "format": "json"}),
        )
        .await,
        format!(
            r#"{{"changed_files":[],"comparison":{{"head":"{head}","kind":"staged"}},"recent_commits":["production composition fixture"],"source_coverage":{{"complete":true,"consistency":"immutable_blobs","gaps":[],"git":{{"degradations":[]}}}},"suggested_category":null,"summary":"No staged changes.","symbols_by_role":{{}}}}"#
        )
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
async fn pr_context_default_brief_orients_from_exact_commits_through_mcp() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "pub fn changed(value: u32) -> u32 { value + 1 }\n\
             pub fn unchanged() -> u32 { 7 }\n",
        )
        .unwrap();
        crate::support::commit_worktree(project, "base API");
        git_stdout(project, &["branch", "pr-base"]);
        git_stdout(project, &["checkout", "-b", "pr-feature"]);
        fs::write(
            project.join("src/lib.rs"),
            "pub fn changed(value: u64) -> u64 { value + 2 }\n\
             pub fn unchanged() -> u32 { 7 }\n",
        )
        .unwrap();
    })
    .await;
    let base_oid = git_stdout(&fixture.project_root, &["rev-parse", "pr-base"]);
    let head_oid = git_stdout(&fixture.project_root, &["rev-parse", "HEAD"]);
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;
    // Brief enrichment only reads resident data. Retain the serving owner's
    // actual catalog across this journey so an idle park cannot race the brief.
    let graph = fixture
        .harness
        .published_code_graph_store_for_test(&fixture.project_root)
        .await
        .expect("published production graph");
    let graph_for_rewarm = Arc::clone(&graph);
    let graph_admission = tokio::task::spawn_blocking(move || {
        graph_for_rewarm.await_rewarm_and_retain_for(
            Duration::from_secs(20),
            CodeGraphReadinessRequirement::Catalog,
        )
    })
    .await
    .expect("graph admission task")
    .expect("published graph catalog rewarms");
    let output = call_json(
        &fixture,
        "tracedecay_pr_context",
        json!({"base_ref": base_oid, "head_ref": head_oid, "budget_tokens": 4000, "format": "json"}),
    )
    .await;
    let brief: Value = serde_json::from_str(&output).expect("PR brief JSON");
    let _: tracedecay_contracts::retrieval::PrContextBriefV1 =
        serde_json::from_value(brief.clone()).expect("canonical PR briefing contract");
    assert_eq!(brief["view"], "brief", "{brief}");
    assert_eq!(brief["base_oid"], base_oid, "{brief}");
    assert_eq!(brief["head_oid"], head_oid, "{brief}");
    assert_eq!(brief["files_changed"], 1, "{brief}");
    assert_eq!(brief["source_analysis"]["state"], "ready", "{brief}");
    assert_eq!(brief["graph_analysis"]["state"], "partial", "{brief}");
    assert!(
        brief["graph_analysis"]["detail"]
            .as_str()
            .unwrap()
            .starts_with("Exact head graph;"),
        "{brief}"
    );
    let entries = brief["entry_points"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| {
            entry["symbol"]
                .as_str()
                .is_some_and(|symbol| symbol.ends_with("changed"))
                && entry["change"] == "signature_changed"
                && entry["before_signature"].is_string()
                && entry["after_signature"].is_string()
                && entry["revision"] == head_oid
        }),
        "{brief}"
    );
    assert!(
        entries.iter().all(|entry| {
            !entry["symbol"]
                .as_str()
                .is_some_and(|symbol| symbol.ends_with("unchanged"))
        }),
        "{brief}"
    );
    let node_id = entries[0]["node_id"]
        .as_str()
        .expect("exact-head source handle");
    let body = call_json(
        &fixture,
        "tracedecay_source_body",
        json!({"node_id":node_id}),
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert!(
        body["outcome"]["value"]["payload"]["body"]
            .as_str()
            .unwrap()
            .contains("fn changed(value: u64)"),
        "{body}"
    );
    assert_eq!(brief["budget"]["over_budget"], false, "{brief}");
    assert!(brief["budget"]["estimated_tokens"].as_u64().unwrap() <= 4000);
    assert_eq!(brief["hosted"]["required_checks_known"], false, "{brief}");
    let compact = call_json(
        &fixture,
        "tracedecay_pr_context",
        json!({"base_ref": base_oid, "head_ref": head_oid, "budget_tokens": 4000}),
    )
    .await;
    for heading in [
        "# PR context",
        "## Discussion and checks",
        "## Start reading here",
        "## Changed areas",
        "## Quality evidence and limits",
    ] {
        assert!(compact.contains(heading), "missing {heading}: {compact}");
    }
    assert!(compact.contains("signature_changed"), "{compact}");
    assert!(compact.contains("Budget:"), "{compact}");

    drop(graph_admission);
    drop(graph);

    // Follow the briefing's exact revisions after the worktree has moved on.
    fs::write(
        fixture.project_root.join("src/lib.rs"),
        "pub fn local_only() -> u8 { 99 }\n",
    )
    .unwrap();
    let changed = entries
        .iter()
        .find(|entry| entry["change"] == "signature_changed")
        .unwrap();
    for (side, expected) in [
        (
            "before_excerpt",
            "pub fn changed(value: u32) -> u32 { value + 1 }\n",
        ),
        (
            "after_excerpt",
            "pub fn changed(value: u64) -> u64 { value + 2 }\n",
        ),
    ] {
        let excerpt = &changed[side];
        let result = call_json(
            &fixture,
            "tracedecay_source_lines",
            json!({
                "revision": excerpt["revision"],
                "file": excerpt["location"]["file"],
                "start_line": excerpt["location"]["line"],
                "end_line": excerpt["location"]["end_line"],
                "format": "json",
            }),
        )
        .await;
        let result: Value = serde_json::from_str(&result).unwrap();
        let source = &result["outcome"]["value"]["payload"];
        assert_eq!(source["body"], expected, "{result}");
        assert_eq!(source["revision"], excerpt["revision"], "{result}");
        let object = format!("{}:src/lib.rs", excerpt["revision"].as_str().unwrap());
        assert_eq!(
            source["blob_oid"],
            git_stdout(&fixture.project_root, &["rev-parse", &object])
        );
        assert_eq!(source["span"]["start_byte"], 0, "{result}");
        assert_eq!(source["span"]["end_byte"], expected.len(), "{result}");
        assert!(source.get("next_line").is_none(), "{result}");
    }
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
                    "view": "symbols",
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
            let co_change_budget = page["token_budget"]["sections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["section"] == "missing_co_change_partners")
                .expect("PR context budgets co-change partners");
            assert_eq!(
                co_change_budget["shown"],
                json!(
                    page["missing_co_change_partners"]
                        .as_array()
                        .map_or(0, Vec::len)
                ),
                "{page}"
            );
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

#[tokio::test]
async fn pr_context_symbol_pages_do_not_spend_the_limit_on_unchanged_code() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        let unchanged = (0..40)
            .map(|index| format!("pub fn unchanged_{index}() -> u32 {{ {index} }}\n"))
            .collect::<String>();
        fs::write(
            project.join("src/lib.rs"),
            format!(
                "{unchanged}pub fn changed_alpha() -> u32 {{ 1 }}\n\
                 pub fn changed_beta() -> u32 {{ 2 }}\n"
            ),
        )
        .unwrap();
        crate::support::commit_worktree(project, "base symbols");
        git_stdout(project, &["branch", "pr-base"]);
        git_stdout(project, &["checkout", "-b", "pr-feature"]);
        fs::write(
            project.join("src/lib.rs"),
            format!(
                "{unchanged}pub fn changed_alpha() -> u32 {{ 11 }}\n\
                 pub fn changed_beta() -> u32 {{ 12 }}\n"
            ),
        )
        .unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;

    let mut cursor = None;
    let mut names = std::collections::BTreeSet::new();
    for page_index in 0..2 {
        let output = call_json(
            &fixture,
            "tracedecay_pr_context",
            json!({
                "base_ref": "pr-base", "head_ref": "pr-feature", "format": "json",
                "view": "symbols", "maximum_symbols": 1, "cursor": cursor,
            }),
        )
        .await;
        let page: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(page["status"], "complete", "{page}");
        assert_eq!(page["symbol_page"]["returned"], 1, "{page}");
        let modified = page["modified"].as_array().unwrap();
        assert_eq!(modified.len(), 1, "{page}");
        let expected_line = match modified[0]["name"].as_str().unwrap() {
            "changed_alpha" => 41,
            "changed_beta" => 42,
            other => panic!("unexpected changed symbol {other}"),
        };
        assert_eq!(
            modified[0]["line"], expected_line,
            "public coordinates are one-based: {page}"
        );
        assert!(names.insert(modified[0]["name"].as_str().unwrap().to_owned()));
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        assert_eq!(cursor.is_some(), page_index == 0, "{page}");
    }
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        ["changed_alpha", "changed_beta"],
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn pr_context_deleted_symbols_do_not_claim_complete_head_graph_impact() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "pub fn survivor() {}\npub fn removed_api() {}\n",
        )
        .unwrap();
        fs::write(
            project.join("src/consumer.rs"),
            "pub fn consumer() { crate::removed_api(); }\n",
        )
        .unwrap();
        crate::support::commit_worktree(project, "base API and consumer");
        git_stdout(project, &["branch", "pr-base"]);
        git_stdout(project, &["checkout", "-b", "pr-feature"]);
        fs::write(project.join("src/lib.rs"), "pub fn survivor() {}\n").unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;

    let output = call_json(
        &fixture,
        "tracedecay_pr_context",
        json!({
            "base_ref": "pr-base", "head_ref": "pr-feature", "format": "json",
            "view": "symbols",
        }),
    )
    .await;
    let page: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(page["status"], "complete", "{page}");
    assert_eq!(page["removed"][0]["name"], "removed_api", "{page}");
    assert_eq!(page["symbol_page"]["complete"], true, "{page}");
    assert_eq!(
        page["analysis_coverage"]["seed_symbols_analyzed"], 0,
        "{page}"
    );
    assert_eq!(page["analysis_coverage"]["impact_partial"], true, "{page}");
    assert_eq!(page["analysis_coverage"]["complete"], false, "{page}");
    assert_eq!(page["affected_tests_coverage"]["complete"], false, "{page}");
    assert_eq!(
        page["impacted_modules_coverage"]["complete"], false,
        "{page}"
    );
    fixture.harness.shutdown().await;
}
