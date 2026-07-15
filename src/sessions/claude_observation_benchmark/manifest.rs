use serde::Deserialize;
use serde_json::{Value, json};

use super::baseline::ProviderBaseline;
use super::{
    BENCHMARK_COMMAND, CONCURRENCY, EVIDENCE_RUNNER, MEASURED_REPETITIONS, RECORDS_PER_REPETITION,
    WARMUP_REPETITIONS, WORKLOAD_ID, WORKLOAD_IMPLEMENTATION, WORKLOAD_MANIFEST,
    WORKLOAD_SCHEMA_VERSION,
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
            "provider": "claude",
            "scope": "profile",
            "format": "jsonl",
            "record_type": "user",
            "unique_ids": true,
            "secret_shaped_field_per_record": true
        }),
        provider_baselines: super::baseline::expected(),
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
            }
        }),
        command: BENCHMARK_COMMAND.to_string(),
        evidence_runner: EVIDENCE_RUNNER.to_string(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
