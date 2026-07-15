use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::metrics::{
    aggregate_samples, ticks_to_ms, validate_no_op_invariants, validate_no_op_samples,
};
use super::model::{
    BenchmarkResult, BuildIdentity, Distribution, EvidenceStatus, GitSnapshot, RawPhaseSample,
    WorkloadIdentity,
};
use super::{
    BENCHMARK_COMMAND, BUILD_COMMIT, BUILD_PROFILE, BUILD_TARGET_DIR, BUILD_TREE, HARNESS_SOURCES,
    MEASURED_REPETITIONS, RECORDS_PER_REPETITION, RESULT_SCHEMA_VERSION, WARMUP_REPETITIONS,
    WORKLOAD_ID, WORKLOAD_MANIFEST, WORKLOAD_MANIFEST_PATH,
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
    validate_evidence_directory(
        &repository_root().join("benchmarks/pr5-observation"),
        strict,
    )
    .expect("benchmark evidence directory contract");
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
        || !result.build_identity.commit_keyed_target
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
    let target = fs::canonicalize(
        BUILD_TARGET_DIR.expect("missing build-time target directory attestation"),
    )
    .expect("canonicalize attested target directory");
    assert_eq!(
        target.file_name().and_then(|name| name.to_str()),
        Some(commit),
        "benchmark target directory is not commit-keyed"
    );
    let executable = fs::canonicalize(std::env::current_exe().expect("resolve executable"))
        .expect("canonicalize executable");
    assert!(executable.starts_with(&target));
    let (executable_sha256, executable_size_bytes) =
        sha256_file(&executable).expect("hash benchmark executable");
    AttestedBuild {
        evidence_status: EvidenceStatus::Acceptance,
        build_identity: BuildIdentity {
            commit: commit.to_string(),
            tree: tree.to_string(),
            profile: "release".to_string(),
            commit_keyed_target: true,
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
    let manifest_sha256 = portable_text_sha256(WORKLOAD_MANIFEST.as_bytes());
    let harness_sha256 = harness_sources_sha256(
        HARNESS_SOURCES
            .iter()
            .map(|(path, source)| (*path, source.as_bytes())),
    );
    assert_eq!(
        portable_text_sha256(
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
    WorkloadIdentity {
        manifest_path: WORKLOAD_MANIFEST_PATH.to_string(),
        manifest_sha256,
        harness_paths: HARNESS_SOURCES
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect(),
        harness_sha256,
    }
}

fn harness_sources_sha256<'a>(sources: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut digest = Sha256::new();
    for (path, source) in sources {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(portable_text(source));
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

pub(super) fn portable_text_sha256(bytes: &[u8]) -> String {
    sha256_hex(&portable_text(bytes))
}

fn portable_text(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == b'\r' && bytes.get(offset + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            offset += 2;
        } else {
            normalized.push(bytes[offset]);
            offset += 1;
        }
    }
    normalized
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
    verify_git_toplevel();
    GitSnapshot {
        commit: git_output(&["rev-parse", "HEAD"]),
        tree: git_output(&["rev-parse", "HEAD^{tree}"]),
        dirty: worktree_is_dirty(),
    }
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
