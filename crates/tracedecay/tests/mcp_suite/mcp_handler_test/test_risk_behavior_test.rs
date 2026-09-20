//! Production `tools/call` proof for `tracedecay_test_risk`.
//!
//! The fixture is two commits on `src/lib.rs`, so file churn is 2 and the
//! risk multiplier is `log2(3)`. `covered` is called from `tests/`; `wide`
//! and `narrow` are not. Ranking, the default untested filter, `limit`, and
//! a path that matches nothing are what a caller observes.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::support::{TestTempDir, test_temp_dir};

const LIB_RS: &str = r#"pub fn wide(flag: bool) -> i32 {
    if flag { 1 } else { 2 }
}

pub fn narrow() -> i32 {
    1
}

pub fn covered() -> i32 {
    1
}
"#;

const CONFIDENCE_NOTE: &str = "coverage_pct is a depth-3 static attribution lower bound over the admitted generation; complexity uses extraction-attested branches, loops, and maximum nesting, and is null (with risk weighing lower-bound counters) when complexity_analysis reports an incomplete walk; direct_unit is strongest, while closure retains higher residual risk.";

struct RankedProject {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: PathBuf,
    _isolation: TestTempDir,
}

#[tokio::test]
async fn tracedecay_test_risk_ranks_the_next_untested_symbol() {
    let project = open_ranked_project().await;

    let default_report = call_test_risk(&project, json!({"format": "json"})).await;
    let same_file = call_test_risk(&project, json!({"format": "json", "path": "src/lib.rs"})).await;
    let missing_file = call_test_risk(
        &project,
        json!({"format": "json", "path": "src/missing.rs"}),
    )
    .await;
    let limited = call_test_risk(&project, json!({"format": "json", "limit": 1})).await;
    let with_tested =
        call_test_risk(&project, json!({"format": "json", "include_tested": true})).await;

    let summary = indexed_summary();
    assert_eq!(
        observable(&default_report),
        json!({
            "risks": [
                risk_item("wide", 1, 4, 0, false, "none", None, 7.92),
                risk_item("narrow", 5, 1, 0, false, "none", None, 3.17),
            ],
            "summary": summary,
        }),
        "default tracedecay_test_risk report: {default_report}"
    );
    assert_eq!(
        observable(&same_file),
        observable(&default_report),
        "src/lib.rs must return the same ranked report as an unscoped call: {same_file}"
    );
    assert_eq!(
        observable(&missing_file),
        json!({
            "risks": [],
            "summary": {
                "total_functions": 0,
                "tested": 0,
                "skipped": 0,
                "coverage_pct": 0.0,
                "top_risk_untested": "",
                "top_risk_unattributed": "",
                "attribution": {
                    "depth": 3,
                    "direct_unit_attributed": 0,
                    "closure_attributed": 0,
                    "trait_resolved_attributed": 0,
                    "public_api_attributed": 0,
                    "cli_entry_attributed": 0,
                    "total_attributed": 0
                },
                "buckets": {
                    "attributed": 0,
                    "reachable_unattributed": 0,
                    "orphan_entry": 0,
                    "excluded": 0
                },
                "confidence": "static_lower_bound",
                "confidence_note": CONFIDENCE_NOTE
            }
        }),
        "a path with no source symbols must not repeat the ranked report: {missing_file}"
    );
    assert_eq!(
        observable(&limited),
        json!({
            "risks": [
                risk_item("wide", 1, 4, 0, false, "none", None, 7.92),
            ],
            "summary": indexed_summary(),
        }),
        "limit 1 must keep the census and return only the highest untested risk: {limited}"
    );
    assert_eq!(
        observable(&with_tested),
        json!({
            "risks": [
                risk_item("wide", 1, 4, 0, false, "none", None, 7.92),
                risk_item("narrow", 5, 1, 0, false, "none", None, 3.17),
                risk_item("covered", 9, 1, 1, true, "direct_unit", Some(1), 0.63),
            ],
            "summary": indexed_summary(),
        }),
        "include_tested must append the covered symbol without changing the census: {with_tested}"
    );

    project.harness.shutdown().await;
}

fn indexed_summary() -> Value {
    json!({
        "total_functions": 3,
        "tested": 1,
        "skipped": 0,
        "coverage_pct": 33.0,
        "top_risk_untested": "wide",
        "top_risk_unattributed": "wide",
        "attribution": {
            "depth": 3,
            "direct_unit_attributed": 1,
            "closure_attributed": 0,
            "trait_resolved_attributed": 0,
            "public_api_attributed": 0,
            "cli_entry_attributed": 0,
            "total_attributed": 1
        },
        "buckets": {
            "attributed": 1,
            "reachable_unattributed": 0,
            "orphan_entry": 2,
            "excluded": 0
        },
        "confidence": "static_lower_bound",
        "confidence_note": CONFIDENCE_NOTE
    })
}

fn risk_item(
    name: &str,
    line: u32,
    complexity: u32,
    fan_in: usize,
    has_test: bool,
    attribution_method: &str,
    attribution_depth: Option<u32>,
    risk: f64,
) -> Value {
    json!({
        "name": name,
        "file": "src/lib.rs",
        "line": line,
        "complexity": complexity,
        "complexity_analysis": "complete",
        "fan_in": fan_in,
        "has_test": has_test,
        "attribution_method": attribution_method,
        "attribution_depth": attribution_depth,
        "risk": risk,
        "churn": 2
    })
}

fn observable(report: &Value) -> Value {
    let mut report = report.clone();
    let risks = report["risks"]
        .as_array()
        .expect("tracedecay_test_risk risks should be an array");
    let mut ids = Vec::new();
    for risk in risks {
        let id = risk["id"]
            .as_str()
            .expect("each risk row should carry a symbol id");
        assert!(
            id.starts_with("symbol.v1.") && id.len() > "symbol.v1.".len(),
            "risk id should be a symbol occurrence, got {id}"
        );
        ids.push(id.to_owned());
    }
    let before = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), before, "risk ids must be unique: {ids:?}");
    if let Some(risks) = report.get_mut("risks").and_then(Value::as_array_mut) {
        for risk in risks {
            risk.as_object_mut()
                .expect("risk row should be an object")
                .remove("id");
        }
    }
    report
}

async fn call_test_risk(project: &RankedProject, arguments: Value) -> Value {
    let response = project
        .harness
        .call_tool(&project.project_root, "tracedecay_test_risk", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.error.is_none(),
        "tracedecay_test_risk failed: {:?}",
        response.error
    );
    let result = response
        .result
        .expect("tracedecay_test_risk should return a JSON-RPC result");
    assert_eq!(result["content"][0]["type"], json!("text"));
    let text = result["content"][0]["text"].as_str().expect("tool text");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("tool JSON ({error}): {text}"))
}

async fn open_ranked_project() -> RankedProject {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    fs::create_dir_all(project_root.join("src")).expect("src");
    fs::create_dir_all(project_root.join("tests")).expect("tests");
    fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"risk_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("Cargo.toml");
    let lib = project_root.join("src/lib.rs");
    fs::write(&lib, LIB_RS).expect("lib.rs");
    fs::write(
        project_root.join("tests/covered.rs"),
        "use risk_fixture::covered;\n#[test]\nfn covers_covered() {\n    assert_eq!(covered(), 1);\n}\n",
    )
    .expect("integration test");
    git(&project_root, &["init", "-q"]);
    git(&project_root, &["add", "."]);
    commit(&project_root, "initial symbols");
    let mut updated = fs::read_to_string(&lib).expect("read lib");
    updated.push_str("// second commit raises churn without moving symbols\n");
    fs::write(&lib, updated).expect("append churn commit");
    git(&project_root, &["add", "src/lib.rs"]);
    commit(&project_root, "touch src/lib.rs");

    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        vec![project_root.clone()],
    ))
    .await
    .expect("production composition harness");
    RankedProject {
        harness,
        project_root,
        _isolation: isolation,
    }
}

fn git(project: &Path, args: &[&str]) {
    let status = Command::new(crate::common::git_program())
        .args(args)
        .current_dir(project)
        .status()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(status.success(), "git {args:?} exited {status}");
}

fn commit(project: &Path, message: &str) {
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}
