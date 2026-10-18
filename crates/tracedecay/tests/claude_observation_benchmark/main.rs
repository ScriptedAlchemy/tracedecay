//! Reproducible claude-observation pipeline baseline.

#![allow(clippy::too_many_lines)]
mod artifact;
mod baseline;
mod manifest;
mod metrics;
mod model;
mod runner;
mod tests;

const RESULT_SCHEMA_VERSION: u32 = 2;
const WORKLOAD_SCHEMA_VERSION: u32 = 3;
const WORKLOAD_ID: &str = "claude-observation-pipeline-v1";
/// Workload id recorded by archival artifacts sealed before the capability
/// rename; accepted only for historical and retired evidence provenance.
const RETIRED_WORKLOAD_ID: &str = "pr5-observation-pipeline-v1";
const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const RECORDS_PER_REPETITION: usize = 64;
const CONCURRENCY: usize = 1;
const BENCHMARK_COMMAND: &str = "scripts/run-claude-observation-benchmark.sh";
const EVIDENCE_RUNNER: &str = "scripts/run-claude-observation-benchmark.sh";
const WORKLOAD_IMPLEMENTATION: &str = "tests/claude_observation_benchmark/main.rs";
const WORKLOAD_MANIFEST_PATH: &str = "benchmark_data/claude-observation/workload-v1.json";
const BENCHMARK_SECRET_PREFIX: &str = "sk-test-";
const REDACTION_MARKER: &str = "[TraceDecay redacted:";
const PROVIDER_PIPELINE_SCOPE: &str =
    "production_parse_normalize_sanitize_commit_project_and_replay";
const WORKLOAD_MANIFEST: &str =
    include_str!("../../../../benchmark_data/claude-observation/workload-v1.json");
const NATIVE_PROVIDER_FIXTURES: &[(&str, &str)] = &[
    (
        "tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json",
        include_str!(
            "../../../../tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json"
        ),
    ),
    (
        "tests/fixtures/provider_normalization/codex/session_meta.input.json",
        include_str!(
            "../../../../tests/fixtures/provider_normalization/codex/session_meta.input.json"
        ),
    ),
    (
        "tests/fixtures/provider_normalization/codex/agent_message.input.json",
        include_str!(
            "../../../../tests/fixtures/provider_normalization/codex/agent_message.input.json"
        ),
    ),
    (
        "tests/fixtures/provider_normalization/cursor/tool_use.input.json",
        include_str!(
            "../../../../tests/fixtures/provider_normalization/cursor/tool_use.input.json"
        ),
    ),
    (
        "tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json",
        include_str!(
            "../../../../tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json"
        ),
    ),
    (
        "tests/fixtures/provider_normalization/kiro/workspace_session.input.json",
        include_str!(
            "../../../../tests/fixtures/provider_normalization/kiro/workspace_session.input.json"
        ),
    ),
    (
        "tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json",
        include_str!(
            "../../../../tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json"
        ),
    ),
    (
        "tests/fixtures/transcript_golden/cline_like/input/task_metadata.json",
        include_str!(
            "../../../../tests/fixtures/transcript_golden/cline_like/input/task_metadata.json"
        ),
    ),
];
const HARNESS_SOURCES: &[(&str, &str)] = &[
    (
        "tests/claude_observation_benchmark/main.rs",
        include_str!("main.rs"),
    ),
    (
        "tests/claude_observation_benchmark/artifact.rs",
        include_str!("artifact.rs"),
    ),
    (
        "tests/claude_observation_benchmark/baseline.rs",
        include_str!("baseline.rs"),
    ),
    (
        "tests/claude_observation_benchmark/manifest.rs",
        include_str!("manifest.rs"),
    ),
    (
        "tests/claude_observation_benchmark/metrics.rs",
        include_str!("metrics.rs"),
    ),
    (
        "tests/claude_observation_benchmark/model.rs",
        include_str!("model.rs"),
    ),
    (
        "tests/claude_observation_benchmark/runner.rs",
        include_str!("runner.rs"),
    ),
    (
        "tests/claude_observation_benchmark/tests.rs",
        include_str!("tests.rs"),
    ),
];
#[tokio::test]
#[ignore = "release-mode claude-observation performance baseline; run the documented exact command"]
async fn production_observation_pipeline_baseline() {
    runner::run().await;
}

#[test]
fn evidence_directory_matches_index_contract() {
    artifact::assert_repository_evidence();
}
