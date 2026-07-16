use serde::Deserialize;
use serde_json::{Value, json};

use super::baseline::ProviderBaseline;
use super::model::{PROVIDER_COMMIT_SCOPE, PROVIDER_PARSE_SCOPE, PROVIDER_REPLAY_SCOPE};
use super::{
    BENCHMARK_COMMAND, CONCURRENCY, EVIDENCE_RUNNER, MEASURED_REPETITIONS, PROVIDER_PIPELINE_SCOPE,
    RECORDS_PER_REPETITION, WARMUP_REPETITIONS, WORKLOAD_ID, WORKLOAD_IMPLEMENTATION,
    WORKLOAD_MANIFEST, WORKLOAD_SCHEMA_VERSION,
};

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkloadManifest {
    schema_version: u32,
    workload_id: String,
    implementation: String,
    platform: Value,
    profile: String,
    repetitions: Value,
    input: Value,
    provider_baselines: Vec<ProviderBaseline>,
    #[serde(default)]
    provider_result: Option<Value>,
    #[serde(default)]
    hook_telemetry_readiness: Option<Value>,
    phases: Vec<String>,
    setup_excluded: Vec<String>,
    verification_excluded: Vec<String>,
    no_op: Value,
    metrics: Value,
    command: String,
    evidence_runner: String,
}

pub(super) fn validate() {
    let manifest = serde_json::from_str::<WorkloadManifest>(WORKLOAD_MANIFEST)
        .expect("deserialize PR5 benchmark workload manifest");
    assert_eq!(manifest, expected());
    super::baseline::validate(&manifest.provider_baselines);
}

#[cfg(test)]
pub(super) fn accepts_value(value: Value) -> bool {
    serde_json::from_value::<WorkloadManifest>(value).is_ok_and(|manifest| manifest == expected())
}

fn expected() -> WorkloadManifest {
    WorkloadManifest {
        schema_version: WORKLOAD_SCHEMA_VERSION,
        workload_id: WORKLOAD_ID.to_string(),
        implementation: WORKLOAD_IMPLEMENTATION.to_string(),
        platform: json!({
            "operating_system": "linux",
            "procfs_mount": "/proc",
            "required_interfaces": [
                "self/stat", "self/io", "self/status", "self/clear_refs", "meminfo", "cpuinfo"
            ],
            "clear_refs_value": 5
        }),
        profile: "release".to_string(),
        repetitions: json!({
            "warmup": WARMUP_REPETITIONS,
            "measured": MEASURED_REPETITIONS,
            "records_per_repetition": RECORDS_PER_REPETITION,
            "concurrency": CONCURRENCY
        }),
        input: json!({
            "providers": ["claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo"],
            "scopes": ["profile", "project"],
            "formats": ["jsonl", "json_snapshot", "sqlite"],
            "unique_ids": true,
            "secret_shaped_field_per_record": true
        }),
        provider_baselines: super::baseline::expected(),
        provider_result: Some(json!({
            "schema_version": 1,
            "artifact_id": "provider-observation-performance-result-v1",
            "result_field": "provider_observation_performance",
            "required_provider_count": 8,
            "records_per_repetition": super::baseline::PROVIDER_RECORDS_PER_REPETITION,
            "pipeline_scope": PROVIDER_PIPELINE_SCOPE,
            "required_phase_scopes": {
                "parse": PROVIDER_PARSE_SCOPE,
                "commit": PROVIDER_COMMIT_SCOPE,
                "replay": PROVIDER_REPLAY_SCOPE
            },
            "required_distributions": [
                "parse.latency", "commit.latency", "replay.latency",
                "pipeline_latency", "no_op_latency"
            ],
            "required_resources": [
                "cpu_ticks", "process_write_bytes", "database_storage_growth_bytes", "peak_rss_kib"
            ],
            "required_backlog_fields": ["replay_limit", "max_backlog_records"],
            "required_fairness_fields": [
                "policy", "rounds", "providers_per_round",
                "max_provider_turn_distance", "turns"
            ],
            "required_no_op_observation_count_delta": 0
        })),
        hook_telemetry_readiness: Some(json!({
            "artifact_id": "hook-telemetry-baseline-readiness-v1",
            "artifact_kind": "readiness_and_fixture_identity_not_runtime_contract",
            "result_field": "hook_telemetry_readiness",
            "direct_fixture_paths": [
                "tests/fixtures/host_events/claude/baseline.json",
                "tests/fixtures/host_events/codex/baseline.json",
                "tests/fixtures/host_events/cursor/baseline.json",
                "tests/fixtures/host_events/hermes/baseline.json",
                "tests/fixtures/host_events/kiro/baseline.json"
            ],
            "fixture_identity_method": "sha256_recorded_in_runtime_readiness_catalog",
            "canonical_payload_method": "crate_hooks_measure_host_event_payload_bytes",
            "canonical_telemetry_contract": "crate_hooks_host_hook_telemetry_contract"
        })),
        phases: strings(&[
            "scan_complete_transcript",
            "parse_records",
            "sanitize_records",
            "atomic_authoritative_commit",
            "drain_projection_and_v1_fold",
            "bounded_replay_with_overproduction_sentinel",
        ]),
        setup_excluded: strings(&[
            "temporary_directory_creation",
            "input_generation",
            "database_open_and_schema_initialization",
        ]),
        verification_excluded: strings(&[
            "authoritative_payload_redaction_assertions",
            "folded_v1_projection_assertions",
        ]),
        no_op: json!({
            "operation": "repeat_ingest_and_bounded_replay_at_durable_end_cursor",
            "required_observation_count_delta": 0,
            "require_zero_coordinator_work": true,
            "require_zero_process_write_bytes": true,
            "require_zero_database_storage_growth": true
        }),
        metrics: json!({
            "latency": {
                "source": "monotonic_clock",
                "unit": "nanoseconds",
                "percentiles": [50, 95, 99],
                "percentile_method": "nearest_rank",
                "dispersion": "sample_stddev"
            },
            "throughput": {
                "unit": "records_per_second",
                "numerator": "committed_and_replayed_input_records",
                "denominator": "summed_pipeline_latency"
            },
            "provider_pipeline_scope": {
                "measured": PROVIDER_PIPELINE_SCOPE,
                "separate_phase_distributions": {
                    "parse": PROVIDER_PARSE_SCOPE,
                    "commit": PROVIDER_COMMIT_SCOPE,
                    "replay": PROVIDER_REPLAY_SCOPE
                }
            },
            "cpu": {
                "source": "proc_self_stat_user_plus_system",
                "clock_ticks_per_second": "getconf_clk_tck",
                "reported_units": ["ticks", "milliseconds"]
            },
            "peak_memory": {
                "source": "proc_self_status_vmhwm",
                "reset": "proc_self_clear_refs_5",
                "unit": "kibibytes"
            },
            "bytes_written": {
                "source": "proc_self_io",
                "field": "write_bytes",
                "unit": "bytes"
            },
            "database_storage_growth": {
                "files": ["database", "wal", "shm"],
                "method": "summed_file_length_growth",
                "unit": "bytes"
            },
            "raw_samples": {
                "phases": ["pipeline", "no_op_replay"],
                "fields": [
                    "repetition", "latency_ns", "cpu_ticks", "process_write_bytes",
                    "database_storage_growth_bytes", "peak_rss_kib", "replayed_observations"
                ]
            },
            "provider_phase_raw_samples": {
                "phases": ["parse", "commit", "replay"],
                "fields": [
                    "repetition", "latency_ns", "cpu_ticks", "process_write_bytes",
                    "database_storage_growth_bytes", "peak_rss_kib", "record_count"
                ]
            }
        }),
        command: BENCHMARK_COMMAND.to_string(),
        evidence_runner: EVIDENCE_RUNNER.to_string(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
