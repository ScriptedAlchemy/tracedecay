//! Reproducible PR5 observation-pipeline baseline.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod artifact;
mod baseline;
mod manifest;
mod metrics;
mod model;
mod runner;
#[cfg(test)]
mod tests;

const RESULT_SCHEMA_VERSION: u32 = 2;
const WORKLOAD_SCHEMA_VERSION: u32 = 3;
const WORKLOAD_ID: &str = "pr5-observation-pipeline-v1";
const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const RECORDS_PER_REPETITION: usize = 64;
const CONCURRENCY: usize = 1;
const BENCHMARK_COMMAND: &str = "cargo test --quiet --locked --release --lib sessions::claude_observation_benchmark::production_observation_pipeline_baseline -- --ignored --exact --nocapture --test-threads=1";
const EVIDENCE_RUNNER: &str = "scripts/run-pr5-observation-benchmark.sh";
const WORKLOAD_IMPLEMENTATION: &str = "src/sessions/claude_observation_benchmark.rs";
const WORKLOAD_MANIFEST_PATH: &str = "benchmarks/pr5-observation/workload-v1.json";
const BENCHMARK_SECRET_PREFIX: &str = "benchmark-secret-";
const REDACTION_MARKER: &str = "[TraceDecay redacted:";
const WORKLOAD_MANIFEST: &str = include_str!("../../benchmarks/pr5-observation/workload-v1.json");
const HARNESS_SOURCES: &[(&str, &str)] = &[
    (
        "src/sessions/claude_observation_benchmark.rs",
        include_str!("claude_observation_benchmark.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/artifact.rs",
        include_str!("claude_observation_benchmark/artifact.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/baseline.rs",
        include_str!("claude_observation_benchmark/baseline.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/manifest.rs",
        include_str!("claude_observation_benchmark/manifest.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/metrics.rs",
        include_str!("claude_observation_benchmark/metrics.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/model.rs",
        include_str!("claude_observation_benchmark/model.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/runner.rs",
        include_str!("claude_observation_benchmark/runner.rs"),
    ),
    (
        "src/sessions/claude_observation_benchmark/tests.rs",
        include_str!("claude_observation_benchmark/tests.rs"),
    ),
];
const BUILD_COMMIT: Option<&str> = option_env!("TRACEDECAY_BENCHMARK_BUILD_COMMIT");
const BUILD_TREE: Option<&str> = option_env!("TRACEDECAY_BENCHMARK_BUILD_TREE");
const BUILD_PROFILE: Option<&str> = option_env!("TRACEDECAY_BENCHMARK_BUILD_PROFILE");
const BUILD_TARGET_DIR: Option<&str> = option_env!("TRACEDECAY_BENCHMARK_BUILD_TARGET_DIR");

#[tokio::test]
#[ignore = "release-mode PR5 performance baseline; run the documented exact command"]
async fn production_observation_pipeline_baseline() {
    runner::run().await;
}

#[test]
fn evidence_directory_matches_index_contract() {
    artifact::assert_repository_evidence();
}
