//! `tracedecay_diff_context` as an agent host observes it: one production
//! MCP `tools/call`, then the JSON text the caller reads.
//!
//! The fixture is one crate. `tier_b` calls `tier_c`, and `tier_a` calls
//! `tier_b`. `#[test]` on `checks_tier_c` is itself a modified symbol
//! (`annotation_usage` named `test`). Lines are the extractor's 0-based
//! tree-sitter rows.

use super::{close_test_graph, handle_tool_call, init_test_project};
use crate::support::{extract_json, test_temp_dir};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const LIB_RS: &str = "mod tier_a;\nmod tier_b;\nmod tier_c;\n";
const TIER_A_RS: &str = "use crate::tier_b::tier_b;\n\npub fn tier_a() -> u8 {\n    tier_b()\n}\n";
const TIER_B_RS: &str = "use crate::tier_c::tier_c;\n\npub fn tier_b() -> u8 {\n    tier_c()\n}\n";
const TIER_C_RS: &str = "\
pub fn tier_c() -> u8 {\n\
    1\n\
}\n\
\n\
#[test]\n\
fn checks_tier_c() {\n\
    let _ = tier_c();\n\
}\n";

fn symbol_facts(symbols: &Value) -> Vec<Value> {
    let Some(symbols) = symbols.as_array() else {
        panic!("diff_context symbol list is not an array: {symbols}");
    };
    let mut facts = symbols
        .iter()
        .map(|symbol| {
            json!({
                "name": symbol["name"],
                "kind": symbol["kind"],
                "file": symbol["file"],
                "line": symbol["line"],
            })
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| {
        (
            left["file"].as_str(),
            left["name"].as_str(),
            left["line"].as_u64(),
        )
            .cmp(&(
                right["file"].as_str(),
                right["name"].as_str(),
                right["line"].as_u64(),
            ))
    });
    facts
}

fn write_call_chain(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/tier_a.rs"), TIER_A_RS).unwrap();
    fs::write(project.join("src/tier_b.rs"), TIER_B_RS).unwrap();
    fs::write(project.join("src/tier_c.rs"), TIER_C_RS).unwrap();
}

#[tokio::test]
async fn diff_context_reports_changed_symbols_callers_and_refuses_invalid_input() {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    write_call_chain(&project);
    let (host, _) = init_test_project(&project).await;

    let changed = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/tier_c.rs"], "depth": 1, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("depth-1 diff_context");
    let changed = extract_json(&changed.value);
    assert_eq!(changed["changed_files"], json!(["src/tier_c.rs"]));
    // `tier_a` calls `tier_b`, so a depth-1 walk stops with callers still
    // unexplored. The tool must say so instead of pretending the radius is
    // complete.
    assert_eq!(changed["impact_complete"], json!(false), "{changed}");
    assert_eq!(
        symbol_facts(&changed["modified_symbols"]),
        vec![
            json!({"name": "checks_tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 5}),
            json!({"name": "test", "kind": "annotation_usage", "file": "src/tier_c.rs", "line": 4}),
            json!({"name": "tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 0}),
        ],
        "modified symbols: {changed}"
    );
    assert_eq!(changed["impacted_symbols_count"], json!(1));
    assert_eq!(
        symbol_facts(&changed["impacted_symbols"]),
        vec![json!({"name": "tier_b", "kind": "function", "file": "src/tier_b.rs", "line": 2}),],
        "direct callers of tier_c: {changed}"
    );
    assert_eq!(
        changed["affected_tests"],
        json!(["src/tier_c.rs"]),
        "{changed}"
    );

    let wider = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/tier_c.rs"], "depth": 2, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("depth-2 diff_context");
    let wider = extract_json(&wider.value);
    assert_eq!(wider["impact_complete"], json!(true), "{wider}");
    assert_eq!(wider["impacted_symbols_count"], json!(2));
    assert_eq!(
        symbol_facts(&wider["impacted_symbols"]),
        vec![
            json!({"name": "tier_a", "kind": "function", "file": "src/tier_a.rs", "line": 2}),
            json!({"name": "tier_b", "kind": "function", "file": "src/tier_b.rs", "line": 2}),
        ],
        "depth 2 also reaches tier_a, which only calls tier_b: {wider}"
    );

    let duplicated = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({
            "files": ["src/tier_c.rs", "src/tier_c.rs"],
            "depth": 1,
            "format": "json"
        }),
        None,
        None,
    )
    .await
    .expect("duplicate-path diff_context");
    let duplicated = extract_json(&duplicated.value);
    assert_eq!(duplicated["changed_files"], json!(["src/tier_c.rs"]));
    assert_eq!(
        symbol_facts(&duplicated["modified_symbols"]),
        vec![
            json!({"name": "checks_tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 5}),
            json!({"name": "test", "kind": "annotation_usage", "file": "src/tier_c.rs", "line": 4}),
            json!({"name": "tier_c", "kind": "function", "file": "src/tier_c.rs", "line": 0}),
        ]
    );
    assert_eq!(
        symbol_facts(&duplicated["impacted_symbols"]),
        vec![json!({"name": "tier_b", "kind": "function", "file": "src/tier_b.rs", "line": 2}),]
    );

    // A path this generation never published carries no symbols, so the
    // affected-test walk has no seeds and answers empty and complete rather
    // than turning "no such file here" into an invalid request.
    let absent = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/not_in_repo.rs"], "format": "json"}),
        None,
        None,
    )
    .await
    .expect("unpublished-path diff_context");
    assert_eq!(
        extract_json(&absent.value),
        json!({
            "changed_files": ["src/not_in_repo.rs"],
            "modified_symbols": [],
            "impacted_symbols_count": 0,
            "impacted_symbols": [],
            "impact_complete": true,
            "affected_tests": []
        })
    );

    let empty_files = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": [], "format": "json"}),
        None,
        None,
    )
    .await
    .expect("empty-files diff_context");
    assert_eq!(
        extract_json(&empty_files.value),
        json!({
            "changed_files": [],
            "modified_symbols": [],
            "impacted_symbols_count": 0,
            "impacted_symbols": [],
            "impact_complete": true,
            "affected_tests": []
        })
    );

    let missing_files = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"format": "json"}),
        None,
        None,
    )
    .await;
    assert_eq!(
        missing_files
            .expect_err("missing files must be refused")
            .to_string(),
        "config error: tracedecay_diff_context failed over production MCP: missing required parameter: files (array of strings)"
    );

    let files_not_array = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": "src/tier_c.rs", "format": "json"}),
        None,
        None,
    )
    .await;
    assert_eq!(
        files_not_array
            .expect_err("a string files argument must be refused")
            .to_string(),
        "config error: tracedecay_diff_context failed over production MCP: missing required parameter: files (array of strings)"
    );

    let not_object = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!(["src/tier_c.rs"]),
        None,
        None,
    )
    .await;
    assert_eq!(
        not_object
            .expect_err("non-object arguments must be refused")
            .to_string(),
        "config error: tracedecay_diff_context failed over production MCP: tool execution failed: config error: invalid arguments: tracedecay_diff_context expects a JSON object"
    );

    let zero_depth = handle_tool_call(
        &host,
        "tracedecay_diff_context",
        json!({"files": ["src/tier_c.rs"], "depth": 0, "format": "json"}),
        None,
        None,
    )
    .await;
    assert_eq!(
        zero_depth.expect_err("depth 0 must be refused").to_string(),
        "config error: tracedecay_diff_context failed over production MCP: tool project route failed: reason_code=code-graph-invalid-request retryable=false: the code-graph read request is invalid: code graph impact depth must be positive"
    );

    close_test_graph(host).await;
}
