//! Behavior of `tracedecay_complexity` through the production MCP `tools/call` path.
//!
//! Expected spans, branch/loop/nesting counts, and call fan-in/fan-out are
//! counted from the sources below. They are not read back from the tool.
//! Score is `lines + (fan_out × 3) + fan_in`. Cyclomatic complexity is
//! `branches + 1`. `Contains` edges (struct→field, impl→method) count toward
//! fan-in and fan-out the same way call edges do.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

/// Free functions whose call graph is entirely inside this file.
///
/// ```text
///  1 pub fn leaf() -> u32 {
///  2     1
///  3 }
///  4
///  5 pub fn branched(n: u32) -> u32 {
///  6     if n == 0 {
///  7         return 0;
///  8     }
///  9     n + 1
/// 10 }
/// 11
/// 12 pub fn caller(n: u32) -> u32 {
/// 13     branched(n) + leaf()
/// 14 }
/// 15
/// 16 pub fn sum_items(items: &[u32]) -> u32 {
/// 17     let mut total = 0;
/// 18     for item in items {
/// 19         if *item > 10 {
/// 20             total += leaf();
/// 21         }
/// 22     }
/// 23     total
/// 24 }
/// ```
const SCORES_RS: &str = concat!(
    "pub fn leaf() -> u32 {\n",
    "    1\n",
    "}\n",
    "\n",
    "pub fn branched(n: u32) -> u32 {\n",
    "    if n == 0 {\n",
    "        return 0;\n",
    "    }\n",
    "    n + 1\n",
    "}\n",
    "\n",
    "pub fn caller(n: u32) -> u32 {\n",
    "    branched(n) + leaf()\n",
    "}\n",
    "\n",
    "pub fn sum_items(items: &[u32]) -> u32 {\n",
    "    let mut total = 0;\n",
    "    for item in items {\n",
    "        if *item > 10 {\n",
    "            total += leaf();\n",
    "        }\n",
    "    }\n",
    "    total\n",
    "}\n",
);

/// Eight source lines, no calls. Longer than `caller`, shorter in score.
const ELSEWHERE_RS: &str = concat!(
    "pub fn elsewhere_plain() -> u32 {\n",
    "    let a = 1;\n",
    "    let b = 2;\n",
    "    let c = 3;\n",
    "    let d = 4;\n",
    "    let e = 5;\n",
    "    a + b + c + d + e\n",
    "}\n",
);

/// A struct, its field, an inherent impl, and one method.
///
/// ```text
///  1 pub struct Meter {
///  2     value: u32,
///  3 }
///  4
///  5 impl Meter {
///  6     pub fn read(&self, n: u32) -> u32 {
///  7         if n > 2 {
///  8             self.value + n
///  9         } else {
/// 10             self.value
/// 11         }
/// 12     }
/// 13 }
/// ```
const KINDS_RS: &str = concat!(
    "pub struct Meter {\n",
    "    value: u32,\n",
    "}\n",
    "\n",
    "impl Meter {\n",
    "    pub fn read(&self, n: u32) -> u32 {\n",
    "        if n > 2 {\n",
    "            self.value + n\n",
    "        } else {\n",
    "            self.value\n",
    "        }\n",
    "    }\n",
    "}\n",
);

const FORMULA: &str = "lines + (fan_out × 3) + fan_in";
const NOTE: &str = "cyclomatic_complexity = branches + 1 (computed from AST during extraction); counters are null when complexity_analysis is not complete";

fn visible_ranking(payload: &Value) -> Vec<Value> {
    payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking array missing: {payload}"))
        .iter()
        .map(|item| {
            json!({
                "name": item["name"],
                "kind": item["kind"],
                "file": item["file"],
                "line": item["line"],
                "lines": item["lines"],
                "cyclomatic_complexity": item["cyclomatic_complexity"],
                "branches": item["branches"],
                "loops": item["loops"],
                "max_nesting": item["max_nesting"],
                "complexity_analysis": item["complexity_analysis"],
                "fan_out": item["fan_out"],
                "fan_in": item["fan_in"],
                "score": item["score"],
            })
        })
        .collect()
}

fn assert_published_formula(payload: &Value) {
    assert_eq!(payload["formula"], FORMULA, "{payload}");
    assert_eq!(payload["note"], NOTE, "{payload}");
    let ranking = payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking array missing: {payload}"));
    assert_eq!(payload["result_count"], ranking.len(), "{payload}");
    for item in ranking {
        let lines = item["lines"]
            .as_u64()
            .unwrap_or_else(|| panic!("lines missing: {item}"));
        let fan_out = item["fan_out"]
            .as_u64()
            .unwrap_or_else(|| panic!("fan_out missing: {item}"));
        let fan_in = item["fan_in"]
            .as_u64()
            .unwrap_or_else(|| panic!("fan_in missing: {item}"));
        let branches = item["branches"]
            .as_u64()
            .unwrap_or_else(|| panic!("branches missing: {item}"));
        assert_eq!(
            item["score"].as_u64(),
            Some(lines + fan_out.saturating_mul(3) + fan_in),
            "score must follow the published formula: {item}"
        );
        assert_eq!(
            item["cyclomatic_complexity"].as_u64(),
            Some(branches + 1),
            "cyclomatic complexity must be branches + 1: {item}"
        );
        assert_eq!(item["complexity_analysis"], "complete", "{item}");
    }
}

fn score_ranking() -> Vec<Value> {
    vec![
        json!({
            "name": "sum_items",
            "kind": "function",
            "file": "src/scores.rs",
            "line": 16,
            "lines": 9,
            "cyclomatic_complexity": 2,
            "branches": 1,
            "loops": 1,
            "max_nesting": 3,
            "complexity_analysis": "complete",
            "fan_out": 1,
            "fan_in": 0,
            "score": 12
        }),
        json!({
            "name": "caller",
            "kind": "function",
            "file": "src/scores.rs",
            "line": 12,
            "lines": 3,
            "cyclomatic_complexity": 1,
            "branches": 0,
            "loops": 0,
            "max_nesting": 1,
            "complexity_analysis": "complete",
            "fan_out": 2,
            "fan_in": 0,
            "score": 9
        }),
        json!({
            "name": "branched",
            "kind": "function",
            "file": "src/scores.rs",
            "line": 5,
            "lines": 6,
            "cyclomatic_complexity": 2,
            "branches": 1,
            "loops": 0,
            "max_nesting": 2,
            "complexity_analysis": "complete",
            "fan_out": 0,
            "fan_in": 1,
            "score": 7
        }),
        json!({
            "name": "leaf",
            "kind": "function",
            "file": "src/scores.rs",
            "line": 1,
            "lines": 3,
            "cyclomatic_complexity": 1,
            "branches": 0,
            "loops": 0,
            "max_nesting": 1,
            "complexity_analysis": "complete",
            "fan_out": 0,
            "fan_in": 2,
            "score": 5
        }),
    ]
}

async fn call_complexity(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_complexity", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.error.is_none(),
        "tracedecay_complexity failed: {:?}",
        response.error
    );
    let result = response.result.expect("tracedecay_complexity result");
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "{result}"
    );
    let text = extract_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("complexity JSON ({error}): {text}"))
}

async fn call_complexity_text(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_complexity", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.error.is_none(),
        "tracedecay_complexity failed: {:?}",
        response.error
    );
    let result = response.result.expect("tracedecay_complexity result");
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "{result}"
    );
    extract_text(&result).to_owned()
}

#[tokio::test]
async fn complexity_ranks_by_lines_fanout_and_fanin() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/scores.rs"), SCORES_RS).unwrap();
        fs::write(project.join("src/elsewhere.rs"), ELSEWHERE_RS).unwrap();
        fs::write(project.join("src/kinds.rs"), KINDS_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let functions = call_complexity(
        &fixture,
        json!({"node_kind": "function", "path": "src/scores.rs", "format": "json"}),
    )
    .await;
    assert_published_formula(&functions);
    assert_eq!(functions["result_count"], 4, "{functions}");
    assert_eq!(
        visible_ranking(&functions),
        score_ranking(),
        "fan-out must outrank a longer function with no calls: {functions}"
    );

    let limited = call_complexity(
        &fixture,
        json!({
            "node_kind": "function",
            "path": "src/scores.rs",
            "limit": 2,
            "format": "json"
        }),
    )
    .await;
    assert_published_formula(&limited);
    assert_eq!(limited["result_count"], 2, "{limited}");
    assert_eq!(visible_ranking(&limited), score_ranking()[..2], "{limited}");

    let unscoped = call_complexity(
        &fixture,
        json!({"node_kind": "function", "limit": 3, "format": "json"}),
    )
    .await;
    assert_published_formula(&unscoped);
    assert_eq!(unscoped["result_count"], 3, "{unscoped}");
    assert_eq!(
        visible_ranking(&unscoped),
        vec![
            score_ranking()[0].clone(),
            score_ranking()[1].clone(),
            json!({
                "name": "elsewhere_plain",
                "kind": "function",
                "file": "src/elsewhere.rs",
                "line": 1,
                "lines": 8,
                "cyclomatic_complexity": 1,
                "branches": 0,
                "loops": 0,
                "max_nesting": 1,
                "complexity_analysis": "complete",
                "fan_out": 0,
                "fan_in": 0,
                "score": 8
            }),
        ],
        "the 3-line caller (fan-out 2, score 9) must outrank the 8-line function (score 8): {unscoped}"
    );

    let under_src = call_complexity(
        &fixture,
        json!({"node_kind": "function", "path": "src", "limit": 1, "format": "json"}),
    )
    .await;
    assert_eq!(under_src["result_count"], 1, "{under_src}");
    assert_eq!(
        visible_ranking(&under_src),
        score_ranking()[..1],
        "{under_src}"
    );

    let elsewhere = call_complexity(
        &fixture,
        json!({"path": "src/elsewhere.rs", "format": "json"}),
    )
    .await;
    assert_published_formula(&elsewhere);
    assert_eq!(elsewhere["result_count"], 1, "{elsewhere}");
    assert_eq!(
        visible_ranking(&elsewhere),
        vec![json!({
            "name": "elsewhere_plain",
            "kind": "function",
            "file": "src/elsewhere.rs",
            "line": 1,
            "lines": 8,
            "cyclomatic_complexity": 1,
            "branches": 0,
            "loops": 0,
            "max_nesting": 1,
            "complexity_analysis": "complete",
            "fan_out": 0,
            "fan_in": 0,
            "score": 8
        })],
        "{elsewhere}"
    );

    let methods = call_complexity(
        &fixture,
        json!({"node_kind": "method", "path": "src/kinds.rs", "format": "json"}),
    )
    .await;
    assert_published_formula(&methods);
    assert_eq!(methods["result_count"], 1, "{methods}");
    assert_eq!(
        visible_ranking(&methods),
        vec![json!({
            "name": "read",
            "kind": "method",
            "file": "src/kinds.rs",
            "line": 6,
            "lines": 7,
            "cyclomatic_complexity": 3,
            "branches": 2,
            "loops": 0,
            "max_nesting": 2,
            "complexity_analysis": "complete",
            "fan_out": 0,
            "fan_in": 1,
            "score": 8
        })],
        "if/else is two branches, and the impl Contains edge is fan-in: {methods}"
    );

    let kinds_default =
        call_complexity(&fixture, json!({"path": "src/kinds.rs", "format": "json"})).await;
    assert_published_formula(&kinds_default);
    assert_eq!(kinds_default["result_count"], 4, "{kinds_default}");
    assert_eq!(
        visible_ranking(&kinds_default),
        vec![
            json!({
                "name": "Meter",
                "kind": "impl",
                "file": "src/kinds.rs",
                "line": 5,
                "lines": 9,
                "cyclomatic_complexity": 1,
                "branches": 0,
                "loops": 0,
                "max_nesting": 0,
                "complexity_analysis": "complete",
                "fan_out": 1,
                "fan_in": 0,
                "score": 12
            }),
            json!({
                "name": "read",
                "kind": "method",
                "file": "src/kinds.rs",
                "line": 6,
                "lines": 7,
                "cyclomatic_complexity": 3,
                "branches": 2,
                "loops": 0,
                "max_nesting": 2,
                "complexity_analysis": "complete",
                "fan_out": 0,
                "fan_in": 1,
                "score": 8
            }),
            json!({
                "name": "Meter",
                "kind": "struct",
                "file": "src/kinds.rs",
                "line": 1,
                "lines": 3,
                "cyclomatic_complexity": 1,
                "branches": 0,
                "loops": 0,
                "max_nesting": 0,
                "complexity_analysis": "complete",
                "fan_out": 1,
                "fan_in": 0,
                "score": 6
            }),
            json!({
                "name": "value",
                "kind": "field",
                "file": "src/kinds.rs",
                "line": 2,
                "lines": 1,
                "cyclomatic_complexity": 1,
                "branches": 0,
                "loops": 0,
                "max_nesting": 0,
                "complexity_analysis": "complete",
                "fan_out": 0,
                "fan_in": 1,
                "score": 2
            }),
        ],
        "the default ranking includes impls, structs, and fields, not only functions: {kinds_default}"
    );

    let functions_in_kinds = call_complexity(
        &fixture,
        json!({"node_kind": "function", "path": "src/kinds.rs", "format": "json"}),
    )
    .await;
    assert_eq!(
        functions_in_kinds,
        json!({
            "formula": FORMULA,
            "note": NOTE,
            "result_count": 0,
            "ranking": []
        }),
        "a method is not a function: {functions_in_kinds}"
    );
    let methods_in_scores = call_complexity(
        &fixture,
        json!({"node_kind": "method", "path": "src/scores.rs", "format": "json"}),
    )
    .await;
    assert_eq!(methods_in_scores["result_count"], 0, "{methods_in_scores}");
    assert_eq!(
        methods_in_scores["ranking"],
        json!([]),
        "{methods_in_scores}"
    );

    let absent = call_complexity(
        &fixture,
        json!({"path": "src/does-not-exist.rs", "format": "json"}),
    )
    .await;
    assert_eq!(
        absent,
        json!({
            "formula": FORMULA,
            "note": NOTE,
            "result_count": 0,
            "ranking": []
        }),
        "a path with no symbols is an empty ranking, not a hidden hit: {absent}"
    );

    let markdown = call_complexity_text(
        &fixture,
        json!({"node_kind": "function", "path": "src/scores.rs", "limit": 1}),
    )
    .await;
    assert!(
        markdown.contains(&format!("**formula:** {FORMULA}\n")),
        "{markdown}"
    );
    assert!(markdown.contains("**result_count:** 1\n"), "{markdown}");
    assert!(
        markdown.contains(
            "- **sum_items**\n  **kind:** function\n  **file:** src/scores.rs\n  **line:** 16\n"
        ),
        "{markdown}"
    );
    assert!(
        markdown.contains(
            "  **branches:** 1\n  **complexity_analysis:** complete\n  **cyclomatic_complexity:** 2\n  **fan_in:** 0\n  **fan_out:** 1\n  **lines:** 9\n  **loops:** 1\n  **max_nesting:** 3\n  **score:** 12\n"
        ),
        "{markdown}"
    );
    for excluded in [
        "- **caller**",
        "- **branched**",
        "- **leaf**",
        "- **elsewhere_plain**",
        "- **read**",
    ] {
        assert!(!markdown.contains(excluded), "{markdown}");
    }

    let empty_markdown =
        call_complexity_text(&fixture, json!({"path": "src/does-not-exist.rs"})).await;
    assert!(
        empty_markdown.contains("**result_count:** 0\n"),
        "{empty_markdown}"
    );
    assert!(
        empty_markdown.contains("ranking: none\n"),
        "{empty_markdown}"
    );
    assert!(!empty_markdown.contains("sum_items"), "{empty_markdown}");

    fixture.harness.shutdown().await;
}
