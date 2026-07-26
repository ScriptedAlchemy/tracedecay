#![allow(clippy::option_env_unwrap)]

use std::process::{Command, Output};

use serde_json::Value;

fn evaluator_bin() -> &'static str {
    option_env!("CARGO_BIN_EXE_tracedecay-search-eval")
        .expect("Cargo must build tracedecay-search-eval")
}

fn run(args: &[&str]) -> Output {
    Command::new(evaluator_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
    assert_eq!(payload["query_count"], 28);
    assert_eq!(payload["partition_counts"]["train"], 14);
    assert_eq!(payload["partition_counts"]["validation"], 14);
    assert_eq!(payload["profile_count"], 3);
    assert!(
        payload["workload_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
}

#[test]
fn compare_reports_unmeasured_semantic_and_rerank_stages_as_pending() {
    let output = run(&["compare", "--profiles", "hybrid-reranked"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = stdout_json(&output);
    assert_eq!(payload["command"], "compare");
    assert_eq!(payload["status"], "pending");
    for profile in payload["profiles"].as_array().expect("profiles array") {
        assert_eq!(profile["status"], "pending");
        assert!(matches!(
            profile["resource_status"].as_str(),
            Some("pass" | "pending")
        ));
        assert_eq!(profile["optional_stages"]["semantic"], "pending");
        assert_eq!(profile["optional_stages"]["rerank"], "pending");
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
