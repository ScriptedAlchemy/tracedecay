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
    assert_eq!(payload["command"], "validate");
    assert_eq!(payload["status"], "pass");
    assert_eq!(payload["query_count"], 32);
    assert_eq!(payload["partition_counts"]["train"], 16);
    assert_eq!(payload["partition_counts"]["validation"], 16);
    assert_eq!(payload["profile_count"], 3);
    assert!(
        payload["workload_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
}

#[test]
fn compare_reports_conceptual_misses_before_pending_optional_stages() {
    let output = run(&["compare", "--profiles", "hybrid-reranked"]);
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
    assert_eq!(profiles[0]["failed_queries"], 2, "{payload}");
    assert_eq!(profiles[1]["failed_queries"], 1, "{payload}");
    for profile in profiles {
        assert_eq!(profile["status"], "fail");
        assert!(matches!(
            profile["resource_status"].as_str(),
            Some("pass" | "pending")
        ));
        assert_eq!(profile["optional_stages"]["semantic"], "pending");
        assert_eq!(profile["optional_stages"]["rerank"], "pending");
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
