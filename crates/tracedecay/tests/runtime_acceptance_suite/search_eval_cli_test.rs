use crate::common;

use std::process::{Command, Output};

use serde_json::Value;

fn run(args: &[&str]) -> Output {
    Command::new(common::search_eval_bin("tracedecay-search-eval"))
        .current_dir(common::repository_root())
        .args(args)
        .output()
        .unwrap()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}):\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn validate_reports_the_direct_checked_in_workload() {
    let output = run(&["validate"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = stdout_json(&output);
    // `validate` without `--workload` validates the packaged workload, so the
    // shipped binary must report exactly the receipt the evaluator library
    // derives for it: digests, cardinalities, and source binding come from
    // that authority, never from literals in this test.
    let receipt = tracedecay_search_eval::validate_default_workload()
        .expect("validate the packaged workload through the library");
    assert_eq!(payload["status"], "pass");
    assert_eq!(
        payload,
        serde_json::to_value(&receipt).expect("serialize workload receipt"),
        "the CLI validate envelope drifted from the library receipt"
    );
    let partition_total: u64 = payload["partition_counts"]
        .as_object()
        .expect("partition counts")
        .values()
        .map(|count| count.as_u64().expect("partition count"))
        .sum();
    assert_eq!(payload["query_count"], partition_total);
}

#[test]
fn compare_reports_the_lexical_baselines_conceptual_misses() {
    let output = run(&["compare", "--profiles", "query-fallback"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = stdout_json(&output);
    assert_eq!(payload["command"], "compare");
    assert_eq!(
        payload["status"], "fail",
        "comparison did not retain the measured conceptual misses: {payload}"
    );
    let profiles = payload["profiles"].as_array().expect("profiles array");
    assert!(!profiles.is_empty(), "no partition was compared: {payload}");
    for profile in profiles {
        // The workload pins the baseline's per-partition fallback output by
        // digest; that receipt, not a miss count, is what identifies which
        // needs the lexical baseline answers.
        assert_eq!(
            profile["fallback_matches_expected"], true,
            "the lexical baseline drifted from the workload's pinned fallback receipt: {profile}"
        );
        let queries = profile["queries"].as_array().expect("per-query results");
        let missed = queries
            .iter()
            .filter(|query| query["status"] == "fail")
            .collect::<Vec<_>>();
        assert_eq!(
            profile["failed_queries"],
            missed.len(),
            "failed_queries disagrees with the per-query statuses: {profile}"
        );
        assert!(
            !missed.is_empty(),
            "the lexical baseline no longer misses any conceptual need; re-pin the workload receipt deliberately: {profile}"
        );
        for query in &missed {
            let strata = query["strata"].as_array().expect("query strata");
            assert!(
                strata.contains(&Value::from("natural_language")),
                "the lexical baseline missed a non-conceptual need: {query}"
            );
        }
        assert_eq!(profile["status"], "fail");
        assert!(matches!(
            profile["resource_status"].as_str(),
            Some("pass" | "pending")
        ));
        assert_eq!(
            profile["quality"]["protected_recall_at_10"]["numerator"],
            profile["quality"]["protected_recall_at_10"]["denominator"]
        );
    }
}

#[test]
fn invalid_fixture_is_reported_as_fail_not_pending() {
    let output = run(&[
        "validate",
        "--workload",
        "tests/fixtures/search_quality/missing-workload.json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = stdout_json(&output);
    assert_eq!(payload["command"], "validate");
    assert_eq!(payload["status"], "fail");
    assert!(
        payload["rationale"]
            .as_str()
            .is_some_and(|rationale| rationale.contains("missing-workload.json"))
    );
}
