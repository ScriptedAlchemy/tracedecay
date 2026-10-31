use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn repository_root() -> PathBuf {
    std::path::absolute("../..").expect("absolute repository root")
}

fn search_eval_bin(name: &str) -> PathBuf {
    let override_env = match name {
        "tracedecay-search-eval-direct" => "TRACEDECAY_SEARCH_EVAL_DIRECT_TEST_BIN",
        _ => "TRACEDECAY_SEARCH_EVAL_TEST_BIN",
    };
    let binary = std::env::var_os(override_env).map_or_else(
        || {
            let test_executable =
                std::env::current_exe().expect("test executable path should resolve");
            let profile_dir = test_executable
                .parent()
                .and_then(Path::parent)
                .expect("integration test should run from a Cargo profile directory");
            profile_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
        },
        PathBuf::from,
    );
    assert!(
        binary.is_file(),
        "search-eval binary `{name}` is missing at {}; set {override_env} or build it beside the test profile",
        binary.display()
    );
    binary
}

fn run(args: &[&str]) -> Output {
    Command::new(search_eval_bin("tracedecay-search-eval"))
        .current_dir(repository_root())
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

#[test]
fn packaged_evaluator_binary_validates_without_a_source_checkout() {
    let project = tempfile::tempdir().expect("unrelated temporary project");
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\n",
    )
    .expect("unrelated project content");
    assert!(!project.path().join(".git").exists());
    assert!(
        !project
            .path()
            .join("tests/fixtures/search_quality")
            .exists()
    );

    let output = Command::new(search_eval_bin("tracedecay-search-eval-direct"))
        .current_dir(project.path())
        .arg("validate")
        .arg("--repo-root")
        .arg(project.path())
        .output()
        .expect("run packaged evaluator binary");
    assert!(
        output.status.success(),
        "packaged evaluator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("typed evaluator JSON");
    assert_eq!(response["status"], "pass");
    // Without a checkout the binary can only have read its packaged workload,
    // so its envelope must equal the library's receipt for those same assets.
    let receipt = tracedecay_search_eval::validate_default_workload()
        .expect("validate the packaged workload through the library");
    assert_eq!(
        response,
        serde_json::to_value(&receipt).expect("serialize packaged workload receipt"),
        "the packaged evaluator drifted from the library's packaged-workload receipt"
    );
}
