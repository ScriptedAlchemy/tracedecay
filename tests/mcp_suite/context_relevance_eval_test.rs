//! Frozen-corpus relevance eval for `tracedecay_context`, the flagship
//! exploration tool. `tracedecay_context` has zero relevance coverage
//! elsewhere in the suite: every other MCP test on it asserts shape
//! (headings, truncation, memory lanes), never "did it actually return the
//! symbols a human would expect for this task".
//!
//! Indexes the hand-built fixture project under
//! `tests/fixtures/context_eval_project/` (a small purpose-built crate, not
//! a copy of the real tracedecay repo, so the corpus can't drift as the
//! codebase changes) and drives real `tracedecay_context` calls through
//! `handle_tool_call` — the same dispatch path an agent's MCP client uses —
//! against `tests/fixtures/context_eval_labeled.json`. Metrics
//! (recall@5, required-anchor hit rate) are recomputed from the live
//! `entry_points` ranking by the test itself, mirroring the redundancy
//! eval's recomputed-metrics pattern
//! (`redundancy_eval_fixture_scores_real_cases` in `src/redundancy.rs`): a
//! fixture whose `expected_metrics` disagree with its own cases fails
//! loudly instead of silently drifting.
//!
//! Cases believed to reflect a real ranking gap (rather than a mislabeled
//! fixture) are marked `expected_current_failure` and scored as a miss
//! without failing the test, same convention as the redundancy eval's
//! reject cases.

#![cfg(feature = "test-transport")]

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, warm_code_index_search,
};

const TOP_K: usize = 5;
const ANCHOR_K: usize = 3;

#[tokio::test]
async fn context_eval_fixture_scores_real_queries() {
    let production = production_composition_fixture_with_sources(copy_fixture_project).await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "authenticate").await;

    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/context_eval_labeled.json"))
            .expect("valid context eval fixture");
    let cases = fixture["cases"].as_array().expect("cases");
    assert!(!cases.is_empty(), "fixture must define at least one case");

    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut recalls: Vec<f64> = Vec::new();
    let mut anchors_total = 0usize;
    let mut anchors_hit = 0usize;

    for case in cases {
        let id = case["id"].as_str().expect("case id");
        assert!(seen_ids.insert(id), "duplicate fixture case id {id}");
        let task = case["task"].as_str().expect("task");
        let relevant: HashSet<&str> = case["relevant"]
            .as_array()
            .expect("relevant")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            !relevant.is_empty(),
            "case {id} must declare at least one relevant qualified name"
        );
        let required_top3: Vec<&str> = case["required_top3"]
            .as_array()
            .expect("required_top3")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            required_top3.iter().all(|name| relevant.contains(name)),
            "case {id}: every required_top3 name must also be in relevant"
        );
        let expected_current_failure = case["expected_current_failure"].as_bool().unwrap_or(false);

        let result = handle_real_server_tool_call(
            &server,
            "tracedecay_context",
            json!({"task": task, "format": "json"}),
        )
        .await;
        let payload: Value = serde_json::from_str(extract_real_server_text(&result))
            .unwrap_or_else(|err| {
                panic!("case {id}: tracedecay_context returned invalid JSON: {err}")
            });
        let entry_points = payload["entry_points"]
            .as_array()
            .unwrap_or_else(|| panic!("case {id}: missing entry_points array in {payload}"));
        let ranked: Vec<&str> = entry_points
            .iter()
            .filter_map(|n| n["qualified_name"].as_str())
            .collect();

        let recall = recall_at_k(&ranked, &relevant, TOP_K);
        let anchors_hit_here = required_top3
            .iter()
            .filter(|name| ranked.iter().take(ANCHOR_K).any(|r| *r == **name))
            .count();
        anchors_total += required_top3.len();
        anchors_hit += anchors_hit_here;

        if expected_current_failure {
            // Recorded as a known gap: still scored (so the aggregate
            // metrics reflect real current behavior), but does not fail the
            // test outright.
            recalls.push(recall);
            continue;
        }

        assert!(
            anchors_hit_here == required_top3.len(),
            "case {id} ({task}): required_top3 {required_top3:?} not all within top {ANCHOR_K} of ranked entry_points {ranked:?}"
        );
        recalls.push(recall);
    }

    let mean_recall = round2(recalls.iter().sum::<f64>() / recalls.len() as f64);
    let anchor_hit_rate = round2(if anchors_total == 0 {
        0.0
    } else {
        anchors_hit as f64 / anchors_total as f64
    });

    let expected = &fixture["expected_metrics"];
    let expected_recall = expected["mean_recall_at_5"]
        .as_f64()
        .expect("mean_recall_at_5");
    let expected_anchor_rate = expected["anchor_hit_rate"]
        .as_f64()
        .expect("anchor_hit_rate");
    assert!(
        (mean_recall - expected_recall).abs() < 1e-9,
        "mean_recall_at_5: computed {mean_recall}, fixture expects {expected_recall}"
    );
    assert!(
        (anchor_hit_rate - expected_anchor_rate).abs() < 1e-9,
        "anchor_hit_rate: computed {anchor_hit_rate}, fixture expects {expected_anchor_rate}"
    );
    production.harness.shutdown().await;
}

/// `recall@k` for a single query: the fraction of the labeled `relevant`
/// set that appears within the top `k` ranked results.
fn recall_at_k(ranked: &[&str], relevant: &HashSet<&str>, k: usize) -> f64 {
    let top_k: HashSet<&str> = ranked.iter().take(k).copied().collect();
    let hits = relevant.iter().filter(|name| top_k.contains(*name)).count();
    hits as f64 / relevant.len() as f64
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn copy_fixture_project(dest: &Path) {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("tests/fixtures/context_eval_project");
    copy_dir_all(&src, dest);
}

fn copy_dir_all(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}
