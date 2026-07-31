use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::metrics::{
    aggregate_samples, ticks_to_ms, validate_no_op_invariants, validate_no_op_samples,
};
use super::model::{
    BenchmarkResult, BuildIdentity, Distribution, EvidenceStatus, GitSnapshot,
    PROVIDER_COMMIT_SCOPE, PROVIDER_PARSE_SCOPE, PROVIDER_REPLAY_SCOPE,
    ProviderBenchmarkSuiteResult, ProviderPhaseResult, RawPhaseSample, WorkloadIdentity,
};
use super::{
    BENCHMARK_COMMAND, BUILD_CARGO_CONFIG_IDENTITY, BUILD_CARGO_VERSION, BUILD_COMMIT,
    BUILD_PROFILE, BUILD_RUSTC_VERSION, BUILD_RUSTC_WORKSPACE_WRAPPER, BUILD_RUSTC_WRAPPER,
    BUILD_RUSTFLAGS, BUILD_SOURCE_MANIFEST_SHA256, BUILD_SOURCE_MODE, BUILD_TARGET_TRIPLE,
    BUILD_TREE, HARNESS_SOURCES, MEASURED_REPETITIONS, NATIVE_PROVIDER_FIXTURES,
    PROVIDER_PIPELINE_SCOPE, RECORDS_PER_REPETITION, RESULT_SCHEMA_VERSION, WARMUP_REPETITIONS,
    WORKLOAD_ID, WORKLOAD_MANIFEST, WORKLOAD_MANIFEST_PATH, WORKLOAD_SCHEMA_VERSION,
};

pub(super) struct AttestedBuild {
    pub(super) evidence_status: EvidenceStatus,
    pub(super) build_identity: BuildIdentity,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceIndex {
    pub(super) schema_version: u32,
    pub(super) current_acceptance: Option<String>,
    pub(super) historical_stale: Vec<String>,
}

#[derive(Deserialize)]
struct ArtifactEnvelope {
    schema_version: u32,
    evidence_status: String,
    workload_id: String,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

pub(super) fn assert_repository_evidence() {
    let strict = std::env::var_os("TRACEDECAY_BENCHMARK_REQUIRE_ACCEPTANCE")
        .is_some_and(|value| value == "1");
    let directory = std::env::var_os("TRACEDECAY_BENCHMARK_EVIDENCE_DIR").map_or_else(
        || repository_root().join("benchmarks/pr5-observation"),
        PathBuf::from,
    );
    validate_evidence_directory(&directory, strict).expect("benchmark evidence directory contract");
}

pub(super) fn validate_evidence_directory(
    directory: &Path,
    require_acceptance: bool,
) -> Result<Option<String>, String> {
    let index_path = directory.join("evidence-index.json");
    let index = serde_json::from_slice::<EvidenceIndex>(
        &fs::read(&index_path)
            .map_err(|error| format!("read {}: {error}", index_path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", index_path.display()))?;
    if index.schema_version != 1 {
        return Err(format!(
            "unsupported evidence index schema {}",
            index.schema_version
        ));
    }
    let historical_index = index
        .historical_stale
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if historical_index.len() != index.historical_stale.len() {
        return Err("evidence index contains duplicate historical artifacts".to_string());
    }
    if index
        .current_acceptance
        .as_ref()
        .is_some_and(|name| historical_index.contains(name))
    {
        return Err("current acceptance is also indexed as historical".to_string());
    }

    let mut files = fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.starts_with("result-")
                && Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .collect::<Vec<_>>();
    files.sort();

    let mut acceptance = Vec::new();
    let mut historical = BTreeSet::new();
    for name in files {
        let path = directory.join(&name);
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        let envelope = serde_json::from_slice::<ArtifactEnvelope>(&bytes)
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        match envelope.evidence_status.as_str() {
            "acceptance" => {
                let result = serde_json::from_slice::<BenchmarkResult>(&bytes)
                    .map_err(|error| format!("parse schema-2 {}: {error}", path.display()))?;
                validate_acceptance_result(&result)?;
                acceptance.push(name);
            }
            "historical_stale" => {
                validate_historical_result(&envelope)?;
                historical.insert(name);
            }
            status => {
                return Err(format!(
                    "{} has unsupported evidence_status {status}",
                    path.display()
                ));
            }
        }
    }

    if acceptance.len() > 1 {
        return Err(format!(
            "expected at most one acceptance artifact, found {}",
            acceptance.len()
        ));
    }
    if historical != historical_index {
        return Err(format!(
            "historical evidence index mismatch: indexed={historical_index:?}, files={historical:?}"
        ));
    }
    match (&index.current_acceptance, acceptance.as_slice()) {
        (Some(expected), [actual]) if expected == actual => Ok(Some(actual.clone())),
        (None, []) if !require_acceptance => Ok(None),
        (None, []) => Err("evidence finalization requires one acceptance artifact".to_string()),
        (Some(expected), actual) => Err(format!(
            "evidence index names {expected}, current artifacts are {actual:?}"
        )),
        (None, _) => Err("unindexed current acceptance artifact".to_string()),
    }
}

fn validate_historical_result(result: &ArtifactEnvelope) -> Result<(), String> {
    if result.schema_version == RESULT_SCHEMA_VERSION {
        return validate_retired_acceptance_result(result);
    }
    if result.schema_version != 1
        || result.workload_id != WORKLOAD_ID
        || string(&result.rest, "stale_reason")?.is_empty()
        || unsigned(&result.rest, "superseded_by_result_schema_version")?
            != u64::from(RESULT_SCHEMA_VERSION)
        || !is_lower_hex(string(&result.rest, "benchmark_commit")?, 40)
        || boolean(&result.rest, "benchmark_commit_dirty")?
    {
        return Err("historical result provenance is invalid".to_string());
    }
    if unsigned(&result.rest, "warmup_repetitions")?
        != u64::try_from(WARMUP_REPETITIONS).expect("warmup count fits u64")
        || unsigned(&result.rest, "measured_repetitions")?
            != u64::try_from(MEASURED_REPETITIONS).expect("sample count fits u64")
        || unsigned(&result.rest, "records_per_repetition")?
            != u64::try_from(RECORDS_PER_REPETITION).expect("record count fits u64")
        || unsigned(&result.rest, "measured_records")?
            != u64::try_from(MEASURED_REPETITIONS * RECORDS_PER_REPETITION)
                .expect("measured record count fits u64")
    {
        return Err("historical result repetition contract mismatch".to_string());
    }
    let pipeline = samples(&result.rest, "pipeline_raw_samples")?;
    let no_op = samples(&result.rest, "no_op_replay_raw_samples")?;
    validate_sample_sequence(&pipeline, RECORDS_PER_REPETITION)?;
    validate_no_op_samples(&no_op, 0, RECORDS_PER_REPETITION)?;
    validate_distribution(&result.rest, "pipeline_batch_latency", &pipeline)?;
    validate_distribution(&result.rest, "no_op_replay_latency", &no_op)?;
    let no_op_totals = result
        .rest
        .get("no_op_replay_totals")
        .and_then(Value::as_object)
        .ok_or_else(|| "historical result lacks no-op totals".to_string())?;
    if no_op_totals.is_empty() || no_op_totals.values().any(|value| value.as_u64() != Some(0)) {
        return Err("historical no-op coordinator reported work".to_string());
    }
    validate_aggregates(&result.rest, &pipeline, &no_op)
}

fn validate_retired_acceptance_result(result: &ArtifactEnvelope) -> Result<(), String> {
    let git_before = result
        .rest
        .get("git_before")
        .and_then(Value::as_object)
        .ok_or_else(|| "retired acceptance result lacks git_before".to_string())?;
    if result.workload_id != WORKLOAD_ID
        || string(&result.rest, "stale_reason")?.is_empty()
        || unsigned(&result.rest, "superseded_by_workload_schema_version")?
            != u64::from(WORKLOAD_SCHEMA_VERSION)
        || !is_lower_hex(string(git_before, "commit")?, 40)
        || boolean(git_before, "dirty")?
    {
        return Err("retired acceptance result provenance is invalid".to_string());
    }
    let pipeline = samples(&result.rest, "pipeline_raw_samples")?;
    let no_op = samples(&result.rest, "no_op_replay_raw_samples")?;
    validate_sample_sequence(&pipeline, RECORDS_PER_REPETITION)?;
    validate_no_op_samples(&no_op, 0, 0)?;
    validate_distribution(&result.rest, "pipeline_batch_latency", &pipeline)?;
    validate_distribution(&result.rest, "no_op_replay_latency", &no_op)?;
    validate_aggregates(&result.rest, &pipeline, &no_op)
}

fn validate_acceptance_result(result: &BenchmarkResult) -> Result<(), String> {
    if result.schema_version != RESULT_SCHEMA_VERSION
        || result.evidence_status != EvidenceStatus::Acceptance
        || result.workload_id != WORKLOAD_ID
        || result.command != BENCHMARK_COMMAND
        || result.workload_identity != workload_identity()
    {
        return Err("acceptance result identity is invalid".to_string());
    }
    validate_git_snapshots(&result.git_before, &result.git_after)?;
    if result.build_identity.commit != result.git_before.commit
        || result.build_identity.tree != result.git_before.tree
        || !is_lower_hex(&result.build_identity.commit, 40)
        || !is_lower_hex(&result.build_identity.tree, 40)
        || result.build_identity.profile != "release"
        || result.build_identity.source_mode != "git_archive_read_only_v1"
        || !is_lower_hex(&result.build_identity.source_manifest_sha256, 64)
        || result.build_identity.source_file_count == 0
        || result.build_identity.target_triple.is_empty()
        || result.build_identity.rustc_version != result.rustc
        || result.build_identity.cargo_version != result.cargo
        || result.build_identity.rustflags != "normalized-empty"
        || result.build_identity.rustc_wrapper.is_empty()
        || result.build_identity.rustc_workspace_wrapper.is_empty()
        || !is_lower_hex(&result.build_identity.cargo_config_identity, 40)
        || result.build_identity.data_root_basis != "current_executable_parent"
        || !is_lower_hex(&result.build_identity.executable_sha256, 64)
        || result.build_identity.executable_size_bytes == 0
    {
        return Err("acceptance result build attestation is invalid".to_string());
    }
    if result.rustc.is_empty()
        || result.cargo.is_empty()
        || result.kernel.is_empty()
        || result.cpu_identity.is_empty()
        || result.logical_cpu_count == 0
        || result.memory_total_kib == 0
        || result.clock_ticks_per_second == 0
        || result.warmup_repetitions != WARMUP_REPETITIONS
        || result.measured_repetitions != MEASURED_REPETITIONS
        || result.records_per_repetition != RECORDS_PER_REPETITION
        || result.measured_records != MEASURED_REPETITIONS * RECORDS_PER_REPETITION
    {
        return Err("acceptance result environment or repetition contract is invalid".to_string());
    }
    validate_sample_sequence(&result.pipeline_raw_samples, RECORDS_PER_REPETITION)?;
    validate_no_op_invariants(
        &result.no_op_replay_raw_samples,
        result.no_op_replay_observation_count_delta,
        &result.no_op_replay_totals,
    )?;
    if !distribution_matches(&result.pipeline_batch_latency, &result.pipeline_raw_samples)
        || !distribution_matches(
            &result.no_op_replay_latency,
            &result.no_op_replay_raw_samples,
        )
    {
        return Err("acceptance result distribution mismatch".to_string());
    }
    let pipeline = aggregate_samples(&result.pipeline_raw_samples);
    let no_op = aggregate_samples(&result.no_op_replay_raw_samples);
    if pipeline.cpu_ticks != result.pipeline_cpu_ticks
        || pipeline.process_write_bytes != result.pipeline_process_write_bytes
        || pipeline.database_storage_growth_bytes != result.database_storage_growth_bytes
        || no_op.cpu_ticks != result.no_op_replay_cpu_ticks
        || no_op.process_write_bytes != result.no_op_replay_process_write_bytes
        || no_op.database_storage_growth_bytes != result.no_op_replay_database_storage_growth_bytes
        || pipeline.peak_rss_kib.max(no_op.peak_rss_kib) != result.peak_rss_kib
    {
        return Err("acceptance result aggregate mismatch".to_string());
    }
    let total_ns = result
        .pipeline_raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .sum::<u64>();
    if total_ns == 0
        || !float_close(
            result.pipeline_records_per_second,
            result.measured_records as f64 * 1_000_000_000.0 / total_ns as f64,
        )
        || !float_close(
            result.pipeline_cpu_ms,
            ticks_to_ms(result.pipeline_cpu_ticks, result.clock_ticks_per_second),
        )
        || !float_close(
            result.no_op_replay_cpu_ms,
            ticks_to_ms(result.no_op_replay_cpu_ticks, result.clock_ticks_per_second),
        )
    {
        return Err("acceptance result derived metrics mismatch".to_string());
    }
    validate_provider_performance(
        result
            .provider_observation_performance
            .as_ref()
            .ok_or_else(|| "acceptance result has no provider performance artifact".to_string())?,
        result.clock_ticks_per_second,
    )?;
    if result.hook_telemetry_readiness.as_ref()
        != Some(&super::baseline::hook_telemetry_readiness())
    {
        return Err("acceptance result has invalid hook telemetry readiness evidence".to_string());
    }
    Ok(())
}

fn validate_provider_performance(
    suite: &ProviderBenchmarkSuiteResult,
    clock_ticks_per_second: u64,
) -> Result<(), String> {
    if suite.schema_version != 1
        || suite.workload_id != WORKLOAD_ID
        || suite.providers.len() != super::baseline::PROVIDERS.len()
    {
        return Err("provider performance artifact identity is invalid".to_string());
    }
    validate_provider_fairness(suite)?;
    for (result, expected_provider) in suite
        .providers
        .iter()
        .zip(super::baseline::PROVIDERS.iter())
    {
        if result.provider != *expected_provider
            || result.production_path
                != format!("{expected_provider}_production_observation_pipeline_v1")
            || result.pipeline_scope != PROVIDER_PIPELINE_SCOPE
            || result.measured_repetitions != MEASURED_REPETITIONS
            || result.observations_per_repetition
                != super::baseline::PROVIDER_RECORDS_PER_REPETITION
            || result.replay_limit != super::baseline::PROVIDER_RECORDS_PER_REPETITION + 1
            || result.max_backlog_records != result.observations_per_repetition
            || result.no_op_observation_count_delta != 0
        {
            return Err(format!(
                "provider performance contract mismatch for {expected_provider}"
            ));
        }
        validate_sample_sequence(
            &result.pipeline_raw_samples,
            result.observations_per_repetition,
        )?;
        validate_provider_phase(
            &result.parse,
            PROVIDER_PARSE_SCOPE,
            result.observations_per_repetition,
            clock_ticks_per_second,
        )?;
        validate_provider_phase(
            &result.commit,
            PROVIDER_COMMIT_SCOPE,
            result.observations_per_repetition,
            clock_ticks_per_second,
        )?;
        validate_provider_phase(
            &result.replay,
            PROVIDER_REPLAY_SCOPE,
            result.observations_per_repetition,
            clock_ticks_per_second,
        )?;
        if result.no_op_raw_samples.len() != MEASURED_REPETITIONS
            || result
                .no_op_raw_samples
                .iter()
                .enumerate()
                .any(|(repetition, sample)| {
                    sample.repetition != repetition
                        || sample.replayed_observations != 0
                        || sample.process_write_bytes != 0
                        || sample.database_storage_growth_bytes != 0
                })
        {
            return Err(format!(
                "provider no-op samples are invalid for {expected_provider}"
            ));
        }
        if !distribution_matches(&result.pipeline_latency, &result.pipeline_raw_samples)
            || !distribution_matches(&result.no_op_latency, &result.no_op_raw_samples)
        {
            return Err(format!(
                "provider distribution mismatch for {expected_provider}"
            ));
        }
        let pipeline = aggregate_samples(&result.pipeline_raw_samples);
        let no_op = aggregate_samples(&result.no_op_raw_samples);
        if pipeline.cpu_ticks != result.pipeline_cpu_ticks
            || pipeline.process_write_bytes != result.pipeline_process_write_bytes
            || pipeline.database_storage_growth_bytes
                != result.pipeline_database_storage_growth_bytes
            || no_op.cpu_ticks != result.no_op_cpu_ticks
            || no_op.process_write_bytes != result.no_op_process_write_bytes
            || no_op.database_storage_growth_bytes != result.no_op_database_storage_growth_bytes
            || pipeline.peak_rss_kib.max(no_op.peak_rss_kib) != result.peak_rss_kib
        {
            return Err(format!(
                "provider aggregate mismatch for {expected_provider}"
            ));
        }
        let total_ns = result
            .pipeline_raw_samples
            .iter()
            .map(|sample| sample.latency_ns)
            .sum::<u64>();
        let total_records = result.observations_per_repetition * result.measured_repetitions;
        if total_ns == 0
            || !float_close(
                result.pipeline_records_per_second,
                total_records as f64 * 1_000_000_000.0 / total_ns as f64,
            )
            || !float_close(
                result.pipeline_cpu_ms,
                ticks_to_ms(result.pipeline_cpu_ticks, clock_ticks_per_second),
            )
            || !float_close(
                result.no_op_cpu_ms,
                ticks_to_ms(result.no_op_cpu_ticks, clock_ticks_per_second),
            )
        {
            return Err(format!(
                "provider derived metrics mismatch for {expected_provider}"
            ));
        }
    }
    Ok(())
}

fn validate_provider_fairness(suite: &ProviderBenchmarkSuiteResult) -> Result<(), String> {
    let fairness = &suite.fairness;
    let providers = super::baseline::PROVIDERS;
    if fairness.policy != "round_robin_v1"
        || fairness.rounds != MEASURED_REPETITIONS
        || fairness.providers_per_round != providers.len()
        || fairness.max_provider_turn_distance != providers.len()
        || fairness.turns.len() != fairness.rounds * fairness.providers_per_round
    {
        return Err("provider fairness contract mismatch".to_string());
    }
    for (index, turn) in fairness.turns.iter().enumerate() {
        let round = index / providers.len();
        let position = index % providers.len();
        if turn.round != round || turn.position != position || turn.provider != providers[position]
        {
            return Err("provider fairness turn sequence mismatch".to_string());
        }
    }
    Ok(())
}

fn validate_provider_phase(
    phase: &ProviderPhaseResult,
    scope: &str,
    records_per_repetition: usize,
    clock_ticks_per_second: u64,
) -> Result<(), String> {
    if phase.scope != scope
        || phase.raw_samples.len() != MEASURED_REPETITIONS
        || phase
            .raw_samples
            .iter()
            .enumerate()
            .any(|(repetition, sample)| {
                sample.repetition != repetition
                    || sample.record_count != records_per_repetition
                    || sample.latency_ns == 0
            })
    {
        return Err(format!("provider phase sample mismatch for {scope}"));
    }
    let latencies = phase
        .raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let expected = Distribution::from_samples(&latencies);
    let cpu_ticks: u64 = phase
        .raw_samples
        .iter()
        .map(|sample| sample.cpu_ticks)
        .sum();
    let process_write_bytes: u64 = phase
        .raw_samples
        .iter()
        .map(|sample| sample.process_write_bytes)
        .sum();
    let database_storage_growth_bytes: u64 = phase
        .raw_samples
        .iter()
        .map(|sample| sample.database_storage_growth_bytes)
        .sum();
    let peak_rss_kib = phase
        .raw_samples
        .iter()
        .map(|sample| sample.peak_rss_kib)
        .max()
        .unwrap_or_default();
    if expected.repetitions != phase.latency.repetitions
        || expected.min_ns != phase.latency.min_ns
        || expected.p50_ns != phase.latency.p50_ns
        || expected.p95_ns != phase.latency.p95_ns
        || expected.p99_ns != phase.latency.p99_ns
        || expected.max_ns != phase.latency.max_ns
        || !float_close(expected.mean_ns, phase.latency.mean_ns)
        || !float_close(expected.sample_stddev_ns, phase.latency.sample_stddev_ns)
        || phase.cpu_ticks != cpu_ticks
        || !float_close(phase.cpu_ms, ticks_to_ms(cpu_ticks, clock_ticks_per_second))
        || phase.process_write_bytes != process_write_bytes
        || phase.database_storage_growth_bytes != database_storage_growth_bytes
        || phase.peak_rss_kib != peak_rss_kib
    {
        return Err(format!("provider phase aggregate mismatch for {scope}"));
    }
    Ok(())
}

fn validate_aggregates(
    fields: &Map<String, Value>,
    pipeline_samples: &[RawPhaseSample],
    no_op_samples: &[RawPhaseSample],
) -> Result<(), String> {
    let pipeline = aggregate_samples(pipeline_samples);
    let no_op = aggregate_samples(no_op_samples);
    let expected = [
        ("pipeline_cpu_ticks", pipeline.cpu_ticks),
        ("pipeline_process_write_bytes", pipeline.process_write_bytes),
        (
            "database_storage_growth_bytes",
            pipeline.database_storage_growth_bytes,
        ),
        ("no_op_replay_cpu_ticks", no_op.cpu_ticks),
        (
            "no_op_replay_process_write_bytes",
            no_op.process_write_bytes,
        ),
        (
            "no_op_replay_database_storage_growth_bytes",
            no_op.database_storage_growth_bytes,
        ),
        (
            "peak_rss_kib",
            pipeline.peak_rss_kib.max(no_op.peak_rss_kib),
        ),
    ];
    if expected
        .iter()
        .any(|(field, value)| unsigned(fields, field) != Ok(*value))
    {
        return Err("historical result aggregate mismatch".to_string());
    }
    Ok(())
}

fn validate_sample_sequence(
    samples: &[RawPhaseSample],
    replayed_observations: usize,
) -> Result<(), String> {
    if samples.len() != MEASURED_REPETITIONS {
        return Err(format!(
            "expected {MEASURED_REPETITIONS} samples, found {}",
            samples.len()
        ));
    }
    if samples.iter().enumerate().any(|(repetition, sample)| {
        sample.repetition != repetition || sample.replayed_observations != replayed_observations
    }) {
        return Err("invalid sample sequence".to_string());
    }
    Ok(())
}

fn validate_distribution(
    fields: &Map<String, Value>,
    name: &str,
    samples: &[RawPhaseSample],
) -> Result<(), String> {
    let distribution = serde_json::from_value::<Distribution>(
        fields
            .get(name)
            .cloned()
            .ok_or_else(|| format!("historical result lacks {name}"))?,
    )
    .map_err(|error| format!("parse historical {name}: {error}"))?;
    distribution_matches(&distribution, samples)
        .then_some(())
        .ok_or_else(|| format!("historical {name} mismatch"))
}

fn distribution_matches(expected: &Distribution, samples: &[RawPhaseSample]) -> bool {
    let actual = Distribution::from_samples(
        &samples
            .iter()
            .map(|sample| sample.latency_ns)
            .collect::<Vec<_>>(),
    );
    actual.repetitions == expected.repetitions
        && actual.min_ns == expected.min_ns
        && actual.p50_ns == expected.p50_ns
        && actual.p95_ns == expected.p95_ns
        && actual.p99_ns == expected.p99_ns
        && actual.max_ns == expected.max_ns
        && float_close(actual.mean_ns, expected.mean_ns)
        && float_close(actual.sample_stddev_ns, expected.sample_stddev_ns)
}

fn samples(fields: &Map<String, Value>, name: &str) -> Result<Vec<RawPhaseSample>, String> {
    serde_json::from_value(
        fields
            .get(name)
            .cloned()
            .ok_or_else(|| format!("historical result lacks {name}"))?,
    )
    .map_err(|error| format!("parse historical {name}: {error}"))
}

fn string<'a>(fields: &'a Map<String, Value>, name: &str) -> Result<&'a str, String> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("artifact lacks string {name}"))
}

fn unsigned(fields: &Map<String, Value>, name: &str) -> Result<u64, String> {
    fields
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("artifact lacks unsigned {name}"))
}

fn boolean(fields: &Map<String, Value>, name: &str) -> Result<bool, String> {
    fields
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("artifact lacks boolean {name}"))
}

fn float_close(left: f64, right: f64) -> bool {
    (left - right).abs() <= left.abs().max(right.abs()).max(1.0) * 1e-12
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(super) fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size = size
            .checked_add(u64::try_from(read).expect("read size fits u64"))
            .expect("executable size fits u64");
    }
    Ok((hex::encode(digest.finalize()), size))
}

pub(super) fn validate_release_profile(
    debug_assertions: bool,
    profile: Option<&str>,
) -> Result<(), String> {
    if debug_assertions {
        return Err("benchmark evidence cannot run with debug assertions".to_string());
    }
    if profile != Some("release") {
        return Err("benchmark evidence requires release build attestation".to_string());
    }
    Ok(())
}

pub(super) fn attest_build(git: &GitSnapshot) -> AttestedBuild {
    validate_release_profile(cfg!(debug_assertions), BUILD_PROFILE)
        .expect("benchmark must use the release evidence runner");
    let commit = BUILD_COMMIT.expect("missing build-time Git commit attestation");
    let tree = BUILD_TREE.expect("missing build-time Git tree attestation");
    assert_eq!(commit, git.commit, "build commit differs from runtime HEAD");
    assert_eq!(tree, git.tree, "build tree differs from runtime HEAD tree");
    let executable = fs::canonicalize(std::env::current_exe().expect("resolve executable"))
        .expect("canonicalize executable");
    let (executable_sha256, executable_size_bytes) =
        sha256_file(&executable).expect("hash benchmark executable");
    let rustc_version = BUILD_RUSTC_VERSION.expect("missing build-time rustc identity");
    let cargo_version = BUILD_CARGO_VERSION.expect("missing build-time Cargo identity");
    assert_eq!(rustc_version, command_output("rustc", &["-Vv"]));
    assert_eq!(cargo_version, command_output("cargo", &["-V"]));
    let (_, source_file_count) = validate_source_archive();
    AttestedBuild {
        evidence_status: EvidenceStatus::Acceptance,
        build_identity: BuildIdentity {
            commit: commit.to_string(),
            tree: tree.to_string(),
            profile: "release".to_string(),
            source_mode: BUILD_SOURCE_MODE
                .expect("missing immutable source mode")
                .to_string(),
            source_manifest_sha256: BUILD_SOURCE_MANIFEST_SHA256
                .expect("missing source manifest identity")
                .to_string(),
            source_file_count,
            target_triple: BUILD_TARGET_TRIPLE
                .expect("missing build target triple")
                .to_string(),
            rustc_version: rustc_version.to_string(),
            cargo_version: cargo_version.to_string(),
            rustflags: BUILD_RUSTFLAGS
                .expect("missing normalized Rust flags")
                .to_string(),
            rustc_wrapper: BUILD_RUSTC_WRAPPER
                .expect("missing rustc wrapper identity")
                .to_string(),
            rustc_workspace_wrapper: BUILD_RUSTC_WORKSPACE_WRAPPER
                .expect("missing workspace wrapper identity")
                .to_string(),
            cargo_config_identity: BUILD_CARGO_CONFIG_IDENTITY
                .expect("missing Cargo configuration identity")
                .to_string(),
            data_root_basis: "current_executable_parent".to_string(),
            executable_sha256,
            executable_size_bytes,
        },
    }
}

pub(super) fn validate_git_snapshots(
    before: &GitSnapshot,
    after: &GitSnapshot,
) -> Result<(), String> {
    if before.dirty {
        return Err("worktree was dirty before benchmark execution".to_string());
    }
    if before.commit != after.commit || before.tree != after.tree {
        return Err("Git identity changed during benchmark execution".to_string());
    }
    if after.dirty {
        return Err("worktree became dirty during benchmark execution".to_string());
    }
    Ok(())
}

pub(super) fn workload_identity() -> WorkloadIdentity {
    let manifest_sha256 = sha256_hex(WORKLOAD_MANIFEST.as_bytes());
    let harness_sha256 = harness_sources_sha256(
        HARNESS_SOURCES
            .iter()
            .map(|(path, source)| (*path, source.as_bytes())),
    );
    let native_fixtures_sha256 = harness_sources_sha256(
        NATIVE_PROVIDER_FIXTURES
            .iter()
            .map(|(path, source)| (*path, source.as_bytes())),
    );
    assert_eq!(
        sha256_hex(
            &fs::read(repository_root().join(WORKLOAD_MANIFEST_PATH))
                .expect("read workload manifest")
        ),
        manifest_sha256,
        "compiled workload manifest differs from checkout"
    );
    let checkout = HARNESS_SOURCES
        .iter()
        .map(|(path, _)| {
            (
                *path,
                fs::read(repository_root().join(path))
                    .unwrap_or_else(|error| panic!("read benchmark harness {path}: {error}")),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        harness_sources_sha256(
            checkout
                .iter()
                .map(|(path, source)| (*path, source.as_slice()))
        ),
        harness_sha256,
        "compiled benchmark harness differs from checkout"
    );
    let native_checkout = NATIVE_PROVIDER_FIXTURES
        .iter()
        .map(|(path, _)| {
            (
                *path,
                fs::read(repository_root().join(path))
                    .unwrap_or_else(|error| panic!("read native provider fixture {path}: {error}")),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        harness_sources_sha256(
            native_checkout
                .iter()
                .map(|(path, source)| (*path, source.as_slice()))
        ),
        native_fixtures_sha256,
        "compiled native provider fixtures differ from checkout"
    );
    WorkloadIdentity {
        manifest_path: WORKLOAD_MANIFEST_PATH.to_string(),
        manifest_sha256,
        harness_paths: HARNESS_SOURCES
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect(),
        harness_sha256,
        native_fixture_paths: NATIVE_PROVIDER_FIXTURES
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect(),
        native_fixtures_sha256,
    }
}

fn harness_sources_sha256<'a>(sources: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut digest = Sha256::new();
    for (path, source) in sources {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(source);
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

pub(super) fn command_output(command: &str, args: &[&str]) -> String {
    let output = Command::new(command)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run {command}: {error}"));
    assert!(output.status.success(), "{command} failed");
    String::from_utf8(output.stdout)
        .expect("command output is UTF-8")
        .trim()
        .to_string()
}

pub(super) fn git_snapshot() -> GitSnapshot {
    if BUILD_SOURCE_MODE == Some("git_archive_read_only_v1") {
        return validate_source_archive().0;
    }
    verify_git_toplevel();
    GitSnapshot {
        commit: git_output(&["rev-parse", "HEAD"]),
        tree: git_output(&["rev-parse", "HEAD^{tree}"]),
        dirty: worktree_is_dirty(),
    }
}

fn validate_source_archive() -> (GitSnapshot, usize) {
    assert_eq!(BUILD_SOURCE_MODE, Some("git_archive_read_only_v1"));
    let manifest_path = repository_root().join(".tracedecay-benchmark-source-manifest");
    let manifest = fs::read(&manifest_path).expect("read immutable source manifest");
    assert_eq!(
        sha256_hex(&manifest),
        BUILD_SOURCE_MANIFEST_SHA256.expect("missing source manifest identity"),
        "source manifest differs from build attestation"
    );
    let manifest = String::from_utf8(manifest).expect("source manifest is UTF-8");
    let mut source_file_count = 0;
    for line in manifest.lines() {
        let mut fields = line.splitn(3, '\t');
        let mode = fields.next().expect("source manifest mode");
        let digest = fields.next().expect("source manifest digest");
        let relative = fields.next().expect("source manifest path");
        if mode == "160000" {
            continue;
        }
        let path = repository_root().join(relative);
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("read immutable compiler input {relative}: {error}"));
        assert_eq!(
            sha256_hex(&bytes),
            digest,
            "compiler input changed: {relative}"
        );
        assert!(
            fs::metadata(&path)
                .expect("compiler input metadata")
                .permissions()
                .readonly(),
            "compiler input is writable: {relative}"
        );
        source_file_count += 1;
    }
    assert!(source_file_count > 0, "source manifest contains no files");
    (
        GitSnapshot {
            commit: BUILD_COMMIT
                .expect("missing source commit identity")
                .to_string(),
            tree: BUILD_TREE
                .expect("missing source tree identity")
                .to_string(),
            dirty: false,
        },
        source_file_count,
    )
}

fn worktree_is_dirty() -> bool {
    let output = Command::new("git")
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ])
        .current_dir(repository_root())
        .output()
        .expect("inspect benchmark worktree");
    assert!(output.status.success(), "git status failed");
    status_output_is_dirty(&output.stdout)
}

pub(super) fn status_output_is_dirty(output: &[u8]) -> bool {
    !output.is_empty()
}

pub(super) fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository_root())
        .output()
        .unwrap_or_else(|error| panic!("run git {}: {error}", args.join(" ")));
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

pub(super) fn verify_git_toplevel() {
    let expected = fs::canonicalize(repository_root()).expect("canonicalize manifest directory");
    let actual = fs::canonicalize(git_output(&["rev-parse", "--show-toplevel"]))
        .expect("canonicalize Git toplevel");
    assert_eq!(
        actual, expected,
        "Git toplevel differs from manifest directory"
    );
}
