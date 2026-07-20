//! Reproducible PR7 memory, fact, anchor, and migration baseline.
//!
//! This harness mirrors the PR5 observation-pipeline evidence contract for the
//! PR7 slice: a versioned workload manifest machine-asserted by normal tests,
//! a normal measurement test that executes bounded distributions for
//! fact-write, anchor-create, anchor resolution, anchor replay, and the
//! v19-to-v22 migration, and a checked evidence directory under
//! `benchmarks/pr7-memory/`.
//!
//! PR7 evidence is provisional: the working tree is dirty, so the
//! commit-attested PR5 artifact workflow cannot run. The measurement test
//! emits `result-provisional.json` with `evidence_status: provisional`, no
//! build attestation, and an honest Git snapshot. A clean-commit attested
//! artifact replaces it when PR7 evidence is finalized.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, Confidence, CoverageReportV1,
    DurableClaudeObservationV1, EntityId, EntityKind, EntityRef, EvidenceClass,
    FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, ObservationScopeV1,
    PayloadAccessState, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
    ProjectionGenerationId, ProvenanceId, ResolutionAuthorizationV1, RetentionClass,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    ScopeResolutionId, SensitivityV1, SessionId, UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    AnchoredObservationWrite, FactCommitOutcome, FactCurrentQuery, FactWriteBatch,
    ObservationPersistOutcome, ObservationReplayRequest, ObservationStore, ObservationWrite,
    SESSION_MESSAGE_PROJECTOR_VERSION, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use crate::application::memory::MemoryApplication;
use crate::db::{Database, DatabaseAuthority};
use crate::global_db::GlobalDb;
use crate::store::memory::DatabaseFactStore;
use crate::store::observation::GlobalDbObservationStore;

const RESULT_SCHEMA_VERSION: u32 = 1;
const WORKLOAD_SCHEMA_VERSION: u32 = 1;
const WORKLOAD_ID: &str = "pr7-memory-baseline-v1";
const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const RECORDS_PER_REPETITION: usize = 8;
const CONCURRENCY: usize = 1;
const MIGRATION_FROM_USER_VERSION: u32 = 18;
const MIGRATION_MINIMUM_FINAL_USER_VERSION: u32 = 22;
const BENCHMARK_COMMAND: &str = "cargo test --lib store::memory_benchmark::pr7_memory_baseline -- --exact --nocapture --test-threads=1";
const WORKLOAD_IMPLEMENTATION: &str = "src/store/memory_benchmark.rs";
const WORKLOAD_MANIFEST_PATH: &str = "benchmarks/pr7-memory/workload-v1.json";
const EVIDENCE_DIRECTORY_PATH: &str = "benchmarks/pr7-memory";
const EVIDENCE_INDEX_NAME: &str = "evidence-index.json";
const README_NAME: &str = "README.md";
const PROVISIONAL_ARTIFACT_NAME: &str = "result-provisional.json";
const PROVISIONAL_REASON: &str = "dirty_worktree_no_commit_attestation";
const WORKLOAD_MANIFEST: &str = include_str!("../../benchmarks/pr7-memory/workload-v1.json");
const HARNESS_SOURCES: &[(&str, &str)] = &[(
    "src/store/memory_benchmark.rs",
    include_str!("memory_benchmark.rs"),
)];

const FACT_WRITE_SCOPE: &str = "memory_application_commit_fact_v1";
const ANCHOR_CREATE_SCOPE: &str = "observation_store_persist_anchored_observation_v1";
const ANCHOR_RESOLUTION_SCOPE: &str = "global_db_resolve_observation_evidence_anchor_v1";
const ANCHOR_REPLAY_SCOPE: &str = "observation_store_repeat_persist_exact_duplicate_v1";
const MIGRATION_SCOPE: &str = "db_migrate_user_version_18_to_latest";

const FACT_WRITE_RECORD: &str = "one owner-bound fact committed through MemoryApplication::commit_fact: derived fact identity, one sanitized assertion with one evidence reference, one new retrieval anchor materialized in the fact shard, and one assertion-recorded lineage event in a single daemon authority transaction";
const ANCHOR_CREATE_RECORD: &str = "one sanitized observation and its stable V2 retrieval anchor committed through GlobalDbObservationStore::persist_observation in a single authoritative transaction";
const ANCHOR_RESOLUTION_RECORD: &str = "one owner-bound evidence anchor resolution through GlobalDb::resolve_observation_evidence_anchor against the retained observation store";
const ANCHOR_REPLAY_RECORD: &str = "one exact repeat persist of an already-committed anchored observation returning the existing anchor as an exact duplicate without advancing durable state";
const MIGRATION_RECORD: &str = "one production db::migrations::migrate run over a pre-PR7 database at user_version 18, applying every PR7 migration from v19 onward (v19 through v22 at workload authoring, v23 once it landed in the same chain); the recorded record count is the number of applied migration steps";

const ANCHOR_AUTHORIZATION_DIGEST_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ANCHOR_AUTHORIZATION_DIGEST_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

// ---------------------------------------------------------------------------
// Metrics helpers (mirrors the PR5 /proc measurement contract).
// ---------------------------------------------------------------------------

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}

fn ticks_to_ms(ticks: u64, ticks_per_second: u64) -> f64 {
    ticks as f64 * 1_000.0 / ticks_per_second as f64
}

#[derive(Debug, PartialEq, Eq)]
struct PhaseAggregate {
    cpu_ticks: u64,
    process_write_bytes: u64,
    database_storage_growth_bytes: u64,
    peak_rss_kib: u64,
}

fn aggregate_samples(samples: &[RawPhaseSample]) -> PhaseAggregate {
    samples.iter().fold(
        PhaseAggregate {
            cpu_ticks: 0,
            process_write_bytes: 0,
            database_storage_growth_bytes: 0,
            peak_rss_kib: 0,
        },
        |mut aggregate, sample| {
            aggregate.cpu_ticks += sample.cpu_ticks;
            aggregate.process_write_bytes += sample.process_write_bytes;
            aggregate.database_storage_growth_bytes += sample.database_storage_growth_bytes;
            aggregate.peak_rss_kib = aggregate.peak_rss_kib.max(sample.peak_rss_kib);
            aggregate
        },
    )
}

fn process_cpu_ticks() -> u64 {
    let stat = fs::read_to_string("/proc/self/stat").expect("read process CPU counters");
    parse_proc_stat_cpu_ticks(&stat).expect("parse process CPU counters")
}

fn parse_proc_stat_cpu_ticks(stat: &str) -> Result<u64, String> {
    let after_name = stat
        .rfind(')')
        .and_then(|index| stat.get(index + 2..))
        .ok_or_else(|| "missing process-name terminator in /proc/self/stat".to_string())?;
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let user = fields
        .get(11)
        .ok_or_else(|| "missing utime in /proc/self/stat".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("parse process user ticks: {error}"))?;
    let system = fields
        .get(12)
        .ok_or_else(|| "missing stime in /proc/self/stat".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("parse process system ticks: {error}"))?;
    user.checked_add(system)
        .ok_or_else(|| "process CPU tick total overflowed u64".to_string())
}

fn process_write_bytes() -> u64 {
    proc_value("/proc/self/io", "write_bytes:")
}

fn reset_peak_rss() {
    write_clear_refs().expect("reset process peak RSS");
}

fn process_peak_rss_kib() -> u64 {
    proc_value("/proc/self/status", "VmHWM:")
}

fn memory_total_kib() -> u64 {
    proc_value("/proc/meminfo", "MemTotal:")
}

fn proc_value(path: &str, key: &str) -> u64 {
    let contents = fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    parse_proc_value(&contents, key).unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

fn parse_proc_value(contents: &str, key: &str) -> Result<u64, String> {
    contents
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(':')?;
            if candidate.trim() != key.trim_end_matches(':') {
                return None;
            }
            value.split_whitespace().next()?.parse::<u64>().ok()
        })
        .ok_or_else(|| format!("missing or invalid {key}"))
}

fn cpu_identity() -> String {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").expect("read CPU identity");
    parse_cpu_identity(&cpuinfo)
        .unwrap_or_else(|| format!("unknown Linux CPU ({})", std::env::consts::ARCH))
}

fn parse_cpu_identity(cpuinfo: &str) -> Option<String> {
    const KEYS: &[&str] = &[
        "model name",
        "hardware",
        "cpu",
        "uarch",
        "processor",
        "cpu model",
        "machine",
    ];
    KEYS.iter().find_map(|wanted| {
        cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim().eq_ignore_ascii_case(wanted) && !value.trim().is_empty())
                .then(|| value.trim().to_string())
        })
    })
}

fn write_clear_refs() -> std::io::Result<()> {
    let mut clear_refs = OpenOptions::new()
        .write(true)
        .open("/proc/self/clear_refs")?;
    clear_refs.write_all(b"5\n")
}

fn parse_clock_ticks_per_second(output: &str) -> Result<u64, String> {
    let ticks = output
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("parse getconf CLK_TCK: {error}"))?;
    if ticks == 0 {
        return Err("getconf CLK_TCK returned zero".to_string());
    }
    Ok(ticks)
}

fn preflight_platform() -> u64 {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "PR7 benchmark contract requires Linux"
    );
    for path in [
        "/proc/self/stat",
        "/proc/self/io",
        "/proc/self/status",
        "/proc/meminfo",
        "/proc/cpuinfo",
    ] {
        fs::File::open(path).unwrap_or_else(|error| {
            panic!("PR7 benchmark contract requires readable {path}: {error}")
        });
    }
    write_clear_refs().unwrap_or_else(|error| {
        panic!(
            "PR7 benchmark contract requires writable /proc/self/clear_refs with value 5: {error}"
        )
    });
    parse_clock_ticks_per_second(&command_output("getconf", &["CLK_TCK"]))
        .expect("PR7 benchmark contract requires nonzero getconf CLK_TCK")
}

fn database_storage_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .iter()
    .filter_map(|candidate| fs::metadata(candidate).ok())
    .map(|metadata| metadata.len())
    .sum()
}

fn command_output(command: &str, args: &[&str]) -> String {
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

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn evidence_directory() -> PathBuf {
    repository_root().join(EVIDENCE_DIRECTORY_PATH)
}

fn evidence_lock() -> std::fs::File {
    let path = std::env::temp_dir().join("tracedecay-pr7-benchmark-evidence.lock");
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .expect("open PR7 benchmark evidence lock")
}

// ---------------------------------------------------------------------------
// Result model.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Distribution {
    repetitions: usize,
    min_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    mean_ns: f64,
    sample_stddev_ns: f64,
}

impl Distribution {
    fn from_samples(samples: &[u64]) -> Self {
        assert!(!samples.is_empty(), "benchmark requires samples");
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let mean = sorted.iter().map(|&value| value as f64).sum::<f64>() / sorted.len() as f64;
        let variance = sorted
            .iter()
            .map(|&value| (value as f64 - mean).powi(2))
            .sum::<f64>()
            / (sorted.len() - 1).max(1) as f64;
        Self {
            repetitions: sorted.len(),
            min_ns: sorted[0],
            p50_ns: percentile(&sorted, 50),
            p95_ns: percentile(&sorted, 95),
            p99_ns: percentile(&sorted, 99),
            max_ns: *sorted.last().expect("last benchmark sample"),
            mean_ns: mean,
            sample_stddev_ns: variance.sqrt(),
        }
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RawPhaseSample {
    repetition: usize,
    latency_ns: u64,
    cpu_ticks: u64,
    process_write_bytes: u64,
    database_storage_growth_bytes: u64,
    peak_rss_kib: u64,
    record_count: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PhaseMeasurement {
    scope: String,
    measured_records: usize,
    raw_samples: Vec<RawPhaseSample>,
    latency: Distribution,
    records_per_second: f64,
    cpu_ticks: u64,
    cpu_ms: f64,
    process_write_bytes: u64,
    database_storage_growth_bytes: u64,
    peak_rss_kib: u64,
}

fn assemble_phase(
    scope: &str,
    samples: Vec<RawPhaseSample>,
    ticks_per_second: u64,
) -> PhaseMeasurement {
    assert_eq!(
        samples.len(),
        MEASURED_REPETITIONS,
        "phase {scope} produced {} samples instead of {MEASURED_REPETITIONS}",
        samples.len()
    );
    let latencies = samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let total_ns = latencies.iter().sum::<u64>();
    assert!(total_ns > 0, "phase {scope} measured zero total latency");
    let measured_records = samples
        .iter()
        .map(|sample| sample.record_count)
        .sum::<usize>();
    let aggregate = aggregate_samples(&samples);
    PhaseMeasurement {
        scope: scope.to_string(),
        measured_records,
        latency: Distribution::from_samples(&latencies),
        records_per_second: measured_records as f64 * 1_000_000_000.0 / total_ns as f64,
        cpu_ticks: aggregate.cpu_ticks,
        cpu_ms: ticks_to_ms(aggregate.cpu_ticks, ticks_per_second),
        process_write_bytes: aggregate.process_write_bytes,
        database_storage_growth_bytes: aggregate.database_storage_growth_bytes,
        peak_rss_kib: aggregate.peak_rss_kib,
        raw_samples: samples,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GitSnapshot {
    commit: String,
    dirty: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WorkloadIdentity {
    manifest_path: String,
    manifest_sha256: String,
    harness_paths: Vec<String>,
    harness_sha256: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EvidenceStatus {
    Provisional,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PlatformInfo {
    rustc: String,
    cargo: String,
    kernel: String,
    cpu_identity: String,
    logical_cpu_count: usize,
    memory_total_kib: u64,
    clock_ticks_per_second: u64,
    debug_assertions: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkResult {
    schema_version: u32,
    workload_id: String,
    evidence_status: EvidenceStatus,
    provisional_reason: String,
    captured_at_unix_micros: i64,
    command: String,
    workload_identity: WorkloadIdentity,
    git: GitSnapshot,
    platform: PlatformInfo,
    warmup_repetitions: usize,
    measured_repetitions: usize,
    records_per_repetition: usize,
    concurrency: usize,
    fact_write: PhaseMeasurement,
    anchor_create: PhaseMeasurement,
    anchor_resolution: PhaseMeasurement,
    anchor_replay: PhaseMeasurement,
    migration_v19_to_v22: PhaseMeasurement,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EvidenceIndex {
    schema_version: u32,
    current_acceptance: Option<String>,
    provisional: Option<String>,
    historical_stale: Vec<String>,
}

#[derive(Deserialize)]
struct ArtifactEnvelope {
    schema_version: u32,
    evidence_status: String,
    workload_id: String,
    #[serde(flatten)]
    #[allow(dead_code)]
    rest: Map<String, Value>,
}

// ---------------------------------------------------------------------------
// Workload manifest contract.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkloadPhase {
    phase: String,
    scope: String,
    #[serde(default)]
    records_per_repetition: Option<usize>,
    #[serde(default)]
    records_from_user_version: Option<u32>,
    #[serde(default)]
    minimum_final_user_version: Option<u32>,
    record: String,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkloadManifest {
    schema_version: u32,
    workload_id: String,
    implementation: String,
    platform: Value,
    profile: String,
    repetitions: Value,
    phases: Vec<WorkloadPhase>,
    setup_excluded: Vec<String>,
    verification_excluded: Vec<String>,
    metrics: Value,
    evidence: Value,
    command: String,
}

fn record_phase(phase: &str, scope: &str, record: &str) -> WorkloadPhase {
    WorkloadPhase {
        phase: phase.to_string(),
        scope: scope.to_string(),
        records_per_repetition: Some(RECORDS_PER_REPETITION),
        records_from_user_version: None,
        minimum_final_user_version: None,
        record: record.to_string(),
    }
}

fn expected_manifest() -> WorkloadManifest {
    let mut migration = record_phase("migration_v19_to_v22", MIGRATION_SCOPE, MIGRATION_RECORD);
    migration.records_per_repetition = None;
    migration.records_from_user_version = Some(MIGRATION_FROM_USER_VERSION);
    migration.minimum_final_user_version = Some(MIGRATION_MINIMUM_FINAL_USER_VERSION);
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
            "clear_refs_value": 5,
            "unsupported_platform_behavior": "measurement_test_skips_without_emitting"
        }),
        profile: "cargo test".to_string(),
        repetitions: json!({
            "warmup": WARMUP_REPETITIONS,
            "measured": MEASURED_REPETITIONS,
            "records_per_repetition": RECORDS_PER_REPETITION,
            "concurrency": CONCURRENCY
        }),
        phases: vec![
            record_phase("fact_write", FACT_WRITE_SCOPE, FACT_WRITE_RECORD),
            record_phase("anchor_create", ANCHOR_CREATE_SCOPE, ANCHOR_CREATE_RECORD),
            record_phase(
                "anchor_resolution",
                ANCHOR_RESOLUTION_SCOPE,
                ANCHOR_RESOLUTION_RECORD,
            ),
            record_phase("anchor_replay", ANCHOR_REPLAY_SCOPE, ANCHOR_REPLAY_RECORD),
            migration,
        ],
        setup_excluded: strings(&[
            "temporary_directory_creation",
            "database_open_and_schema_initialization",
            "record_and_batch_construction",
            "daemon_authority_scope_acquisition",
            "migration_fixture_database_creation_and_version_pinning",
        ]),
        verification_excluded: strings(&[
            "committed_fact_point_reads",
            "committed_observation_point_reads",
            "resolved_anchor_identity_assertions",
            "exact_duplicate_replay_assertions",
            "replay_cardinality_assertions",
            "migration_final_version_and_schema_assertions",
        ]),
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
                "numerator": "committed_or_processed_phase_records",
                "denominator": "summed_phase_latency"
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
                "fields": [
                    "repetition", "latency_ns", "cpu_ticks", "process_write_bytes",
                    "database_storage_growth_bytes", "peak_rss_kib", "record_count"
                ]
            }
        }),
        evidence: json!({
            "artifact": PROVISIONAL_ARTIFACT_NAME,
            "evidence_status": "provisional",
            "attestation": "unavailable_dirty_worktree_no_clean_commit",
            "index": EVIDENCE_INDEX_NAME,
            "write_method": "locked_atomic_rename_from_measurement_test"
        }),
        command: BENCHMARK_COMMAND.to_string(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn validate_manifest() {
    let manifest = serde_json::from_str::<WorkloadManifest>(WORKLOAD_MANIFEST)
        .expect("deserialize PR7 benchmark workload manifest");
    assert_eq!(manifest, expected_manifest());
}

// ---------------------------------------------------------------------------
// Workload and git identity.
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
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

fn workload_identity() -> WorkloadIdentity {
    let manifest_sha256 = sha256_hex(WORKLOAD_MANIFEST.as_bytes());
    let harness_sha256 = harness_sources_sha256(
        HARNESS_SOURCES
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

fn git_snapshot() -> GitSnapshot {
    GitSnapshot {
        commit: git_output(&["rev-parse", "HEAD"]),
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
    !output.stdout.is_empty()
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

// ---------------------------------------------------------------------------
// Fixtures and record builders.
// ---------------------------------------------------------------------------

fn benchmark_tempdir(prefix: &str) -> TempDir {
    let executable = fs::canonicalize(std::env::current_exe().expect("resolve test executable"))
        .expect("canonicalize test executable");
    let root = executable
        .parent()
        .expect("test executable parent")
        .join("tracedecay-benchmark-data");
    fs::create_dir_all(&root).expect("create target-relative benchmark data root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create target-relative benchmark fixture")
}

struct ObservationFixture {
    db: GlobalDb,
    db_path: PathBuf,
    _daemon_scope: crate::db::DaemonDatabaseScope,
    _temp: TempDir,
}

async fn observation_fixture() -> ObservationFixture {
    let temp = benchmark_tempdir("pr7-anchor-");
    let profile = temp.path().join("profile");
    fs::create_dir_all(&profile).expect("create benchmark profile root");
    let db_path = profile.join("sessions.db");
    let daemon_scope = crate::db::enter_daemon_database_scope(&profile, 1, "pr7-memory-benchmark")
        .expect("enter benchmark daemon database scope");
    let db = GlobalDb::open_at(&db_path)
        .await
        .expect("open authoritative benchmark database");
    ObservationFixture {
        db,
        db_path,
        _daemon_scope: daemon_scope,
        _temp: temp,
    }
}

struct MemoryFixture {
    db: Database,
    db_path: PathBuf,
    _temp: TempDir,
}

async fn memory_fixture() -> MemoryFixture {
    let temp = tempfile::tempdir().expect("create PR7 fact benchmark fixture");
    let db_path = temp.path().join("pr7-facts.db");
    let authority = DatabaseAuthority::acquire_test(&db_path, "pr7 memory benchmark")
        .expect("acquire benchmark fact authority");
    let (db, _) = Database::initialize(&db_path, &authority)
        .await
        .expect("initialize benchmark fact database");
    MemoryFixture {
        db,
        db_path,
        _temp: temp,
    }
}

struct MigrationFixture {
    conn: libsql::Connection,
    db_path: PathBuf,
    _db: libsql::Database,
    _temp: TempDir,
}

async fn migration_fixture() -> MigrationFixture {
    let temp = benchmark_tempdir("pr7-migration-");
    let db_path = temp.path().join("pre-pr7.db");
    let db = libsql::Builder::new_local(&db_path)
        .build()
        .await
        .expect("build migration fixture database");
    let conn = db.connect().expect("connect migration fixture database");
    conn.execute_batch(
        "PRAGMA auto_vacuum = INCREMENTAL;
         PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )
    .await
    .expect("apply migration fixture pragmas");
    conn.execute(
        &format!("PRAGMA user_version = {MIGRATION_FROM_USER_VERSION}"),
        (),
    )
    .await
    .expect("pin pre-PR7 schema version");
    MigrationFixture {
        conn,
        db_path,
        _db: db,
        _temp: temp,
    }
}

fn observation_source() -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new("session.pr7-memory-benchmark").unwrap()).unwrap()
}

fn observation(ordinal: usize) -> DurableClaudeObservationV1 {
    let start = u64::try_from(ordinal).expect("benchmark ordinal fits u64") * 128;
    let end = start + 128;
    let payload = json!({
        "kind": "assistant_message",
        "body": format!("bounded PR7 benchmark observation {ordinal}"),
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.pr7.observation.{ordinal}")).unwrap(),
            ComponentVersion::new("sanitizer.pr7-benchmark.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        observation_source(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).unwrap(),
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();
    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.pr7-benchmark").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = ClaudeSourceCursorV1::new(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap();
    let authorization = build_observation_resolution_authorization_v1(
        write.observation(),
        "pr7-memory-benchmark.v1",
    )
    .unwrap();
    let retrieval_anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, retrieval_anchor, projection_generation).unwrap()
}

fn fact_anchor(repetition: usize, index: usize) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new(format!("entity.pr7.fact.{repetition}.{index}")).unwrap(),
            kind: EntityKind::Document,
        }),
        owner: ObservationScopeV1::Profile,
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.pr7-benchmark.v1").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.pr7-benchmark").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.pr7-benchmark").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(ANCHOR_AUTHORIZATION_DIGEST_A).unwrap(),
            capability_id: CapabilityId::new("capability.pr7-benchmark").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(
                ANCHOR_AUTHORIZATION_DIGEST_B,
            )
            .unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.pr7-benchmark").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

fn fact_payload(repetition: usize, index: usize) -> FactPayloadV1 {
    let content = format!("bounded PR7 benchmark fact {repetition}.{index}");
    let tags = vec!["benchmark".to_string(), "pr7".to_string()];
    let entities = vec!["TraceDecay".to_string()];
    let metadata = json!({});
    let material = json!({
        "content": content,
        "category": "project",
        "tags": tags,
        "entities": entities,
        "metadata": metadata,
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.pr7.fact.{repetition}.{index}")).unwrap(),
            ComponentVersion::new("sanitizer.pr7-benchmark.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&material).unwrap()),
    )
    .unwrap();
    FactPayloadV1::new(
        content,
        FactCategoryV1::Project,
        tags,
        entities,
        metadata,
        receipt,
        RetentionClass::new("retention.pr7-benchmark").unwrap(),
    )
    .unwrap()
}

fn fact_batch(owner: &FactOwnerV1, repetition: usize, index: usize) -> FactWriteBatch {
    let operation_id =
        ProvenanceId::try_from(format!("benchmark.pr7.fact-write.{repetition}.{index}")).unwrap();
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application { operation_id },
    )
    .unwrap();
    let fact_id = FactId::derive(&identity).unwrap();
    let anchor = fact_anchor(repetition, index);
    let evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        anchor.anchor_id().clone(),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        fact_payload(repetition, index),
        vec![evidence],
        UtcMicros(1),
        None,
    )
    .unwrap();
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(1),
        None,
    )
    .unwrap();
    FactWriteBatch::new(
        fact_id,
        owner.clone(),
        Some(assertion),
        vec![event],
        vec![anchor],
        vec![],
        None,
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap()
}

// ---------------------------------------------------------------------------
// Phase measurement.
// ---------------------------------------------------------------------------

struct PhaseSnapshot {
    started: Instant,
    cpu_ticks: u64,
    process_write_bytes: u64,
    database_storage_bytes: u64,
}

impl PhaseSnapshot {
    fn start(db_path: &Path) -> Self {
        reset_peak_rss();
        let database_storage_bytes = database_storage_bytes(db_path);
        let cpu_ticks = process_cpu_ticks();
        let process_write_bytes = process_write_bytes();
        Self {
            started: Instant::now(),
            cpu_ticks,
            process_write_bytes,
            database_storage_bytes,
        }
    }

    fn finish(self, db_path: &Path, repetition: usize, record_count: usize) -> RawPhaseSample {
        let latency_ns = elapsed_ns(self.started);
        RawPhaseSample {
            repetition,
            latency_ns,
            cpu_ticks: process_cpu_ticks().saturating_sub(self.cpu_ticks),
            process_write_bytes: process_write_bytes().saturating_sub(self.process_write_bytes),
            database_storage_growth_bytes: database_storage_bytes(db_path)
                .saturating_sub(self.database_storage_bytes),
            peak_rss_kib: process_peak_rss_kib(),
            record_count,
        }
    }
}

async fn measure_fact_write(ticks_per_second: u64) -> PhaseMeasurement {
    let fixture = memory_fixture().await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&fixture.db))
        .expect("create benchmark memory application");
    let mut committed = Vec::with_capacity(MEASURED_REPETITIONS * RECORDS_PER_REPETITION);
    let mut samples = Vec::with_capacity(MEASURED_REPETITIONS);
    for repetition in 0..(WARMUP_REPETITIONS + MEASURED_REPETITIONS) {
        let measured = repetition >= WARMUP_REPETITIONS;
        let mut batches = Vec::with_capacity(RECORDS_PER_REPETITION);
        for index in 0..RECORDS_PER_REPETITION {
            batches.push(fact_batch(&owner, repetition, index));
        }
        let snapshot = PhaseSnapshot::start(&fixture.db_path);
        for batch in batches {
            let fact_id = batch.fact_id().clone();
            let outcome = memory
                .commit_fact(batch)
                .await
                .expect("commit benchmark fact");
            assert!(
                matches!(outcome, FactCommitOutcome::Committed(_)),
                "benchmark fact commit must be a fresh commit: {outcome:?}"
            );
            if measured {
                committed.push(fact_id);
            }
        }
        if measured {
            samples.push(snapshot.finish(
                &fixture.db_path,
                repetition - WARMUP_REPETITIONS,
                RECORDS_PER_REPETITION,
            ));
        }
    }
    // Correctness verification is deliberately outside the measured samples.
    for fact_id in &committed {
        let fact = memory
            .query_fact_current(FactCurrentQuery::new(owner.clone(), fact_id.clone()).unwrap())
            .await
            .expect("read back committed benchmark fact")
            .expect("committed benchmark fact must resolve");
        let payload = fact.payload().expect("committed benchmark fact payload");
        assert!(
            payload.content().starts_with("bounded PR7 benchmark fact "),
            "committed fact payload mismatch: {}",
            payload.content()
        );
    }
    assemble_phase(FACT_WRITE_SCOPE, samples, ticks_per_second)
}

async fn measure_anchor_phases(
    ticks_per_second: u64,
) -> (PhaseMeasurement, PhaseMeasurement, PhaseMeasurement) {
    let fixture = observation_fixture().await;
    let store = GlobalDbObservationStore::new(&fixture.db);
    let mut ordinal = 0_usize;
    let mut expected_cursor = None;
    let mut create = |writes: &mut Vec<AnchoredObservationWrite>, count: usize| {
        let mut expected = expected_cursor.take();
        for _ in 0..count {
            let write = anchored_write(observation(ordinal), expected);
            ordinal += 1;
            expected = Some(write.next_cursor().clone());
            writes.push(write);
        }
        expected_cursor = expected;
    };

    // Warmup exercises the same create, resolution, and replay paths.
    let mut warmup_writes = Vec::new();
    create(
        &mut warmup_writes,
        WARMUP_REPETITIONS * RECORDS_PER_REPETITION,
    );
    for write in &warmup_writes {
        let outcome = store
            .persist_observation(write.clone())
            .await
            .expect("warmup anchor create");
        assert!(matches!(outcome, ObservationPersistOutcome::Committed(_)));
        let resolved = fixture
            .db
            .resolve_observation_evidence_anchor(
                &ObservationScopeV1::Profile,
                write.retrieval_anchor_id(),
            )
            .await
            .expect("warmup anchor resolution")
            .expect("warmup anchor must resolve");
        assert_eq!(resolved.anchor_id(), write.retrieval_anchor_id());
        let replayed = store
            .persist_observation(write.clone())
            .await
            .expect("warmup anchor replay");
        assert!(matches!(
            replayed,
            ObservationPersistOutcome::ExactDuplicate(_)
        ));
    }

    let mut create_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    let mut measured_writes = Vec::with_capacity(MEASURED_REPETITIONS * RECORDS_PER_REPETITION);
    for repetition in 0..MEASURED_REPETITIONS {
        let mut writes = Vec::new();
        create(&mut writes, RECORDS_PER_REPETITION);
        let snapshot = PhaseSnapshot::start(&fixture.db_path);
        for write in &writes {
            let outcome = store
                .persist_observation(write.clone())
                .await
                .expect("measured anchor create");
            assert!(
                matches!(outcome, ObservationPersistOutcome::Committed(_)),
                "measured anchor create must commit: {outcome:?}"
            );
        }
        create_samples.push(snapshot.finish(&fixture.db_path, repetition, RECORDS_PER_REPETITION));
        measured_writes.extend(writes);
    }

    let mut resolution_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    let mut resolved_anchor_ids = BTreeSet::new();
    for (repetition, writes) in measured_writes.chunks(RECORDS_PER_REPETITION).enumerate() {
        let snapshot = PhaseSnapshot::start(&fixture.db_path);
        for write in writes {
            let resolved = fixture
                .db
                .resolve_observation_evidence_anchor(
                    &ObservationScopeV1::Profile,
                    write.retrieval_anchor_id(),
                )
                .await
                .expect("measured anchor resolution")
                .expect("measured anchor must resolve");
            resolved_anchor_ids.insert(resolved.anchor_id().clone());
        }
        resolution_samples.push(snapshot.finish(
            &fixture.db_path,
            repetition,
            RECORDS_PER_REPETITION,
        ));
    }

    let mut replay_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    for (repetition, writes) in measured_writes.chunks(RECORDS_PER_REPETITION).enumerate() {
        let snapshot = PhaseSnapshot::start(&fixture.db_path);
        for write in writes {
            let replayed = store
                .persist_observation(write.clone())
                .await
                .expect("measured anchor replay");
            let ObservationPersistOutcome::ExactDuplicate(receipt) = replayed else {
                panic!("measured anchor replay must return the existing anchor: {replayed:?}");
            };
            assert_eq!(
                receipt.retrieval_anchor_id(),
                write.retrieval_anchor_id(),
                "anchor replay must return the originally created anchor"
            );
        }
        replay_samples.push(snapshot.finish(&fixture.db_path, repetition, RECORDS_PER_REPETITION));
    }

    // Correctness verification is deliberately outside the measured samples.
    let expected_anchor_ids = measured_writes
        .iter()
        .map(|write| write.retrieval_anchor_id().clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        resolved_anchor_ids, expected_anchor_ids,
        "anchor resolution must resolve exactly the created anchors"
    );
    let cardinality = store
        .replay_observations(ObservationReplayRequest::new(0, 1_000).unwrap())
        .await
        .expect("bounded replay after anchor phases")
        .len();
    assert_eq!(
        cardinality,
        (WARMUP_REPETITIONS + MEASURED_REPETITIONS) * RECORDS_PER_REPETITION,
        "exact replay must not advance durable observation cardinality"
    );
    for write in &measured_writes {
        let stored = store
            .get_observation(write.observation().observation_id())
            .await
            .expect("read back committed benchmark observation")
            .expect("committed benchmark observation must resolve");
        assert_eq!(
            stored.retrieval_anchor().anchor_id(),
            write.retrieval_anchor_id(),
            "committed observation must retain its created anchor"
        );
    }
    (
        assemble_phase(ANCHOR_CREATE_SCOPE, create_samples, ticks_per_second),
        assemble_phase(
            ANCHOR_RESOLUTION_SCOPE,
            resolution_samples,
            ticks_per_second,
        ),
        assemble_phase(ANCHOR_REPLAY_SCOPE, replay_samples, ticks_per_second),
    )
}

async fn measure_migration(ticks_per_second: u64) -> PhaseMeasurement {
    for _ in 0..WARMUP_REPETITIONS {
        let fixture = migration_fixture().await;
        let migrated = crate::db::migrations::migrate(&fixture.conn)
            .await
            .expect("warmup migration");
        assert!(migrated, "warmup migration must apply pending migrations");
    }
    let mut samples = Vec::with_capacity(MEASURED_REPETITIONS);
    for repetition in 0..MEASURED_REPETITIONS {
        let fixture = migration_fixture().await;
        let snapshot = PhaseSnapshot::start(&fixture.db_path);
        let migrated = crate::db::migrations::migrate(&fixture.conn)
            .await
            .expect("measured v19-to-v22 migration");
        assert!(migrated, "measured migration must apply pending migrations");
        let final_version = user_version(&fixture.conn).await;
        assert!(
            final_version >= MIGRATION_MINIMUM_FINAL_USER_VERSION,
            "migration stopped at user_version {final_version}, below v{MIGRATION_MINIMUM_FINAL_USER_VERSION}"
        );
        let applied = usize::try_from(final_version - MIGRATION_FROM_USER_VERSION)
            .expect("applied migration count fits usize");
        samples.push(snapshot.finish(&fixture.db_path, repetition, applied));
        // Correctness verification is deliberately outside the measured samples.
        for table in ["memory_v2_facts", "retrieval_anchors"] {
            assert!(
                sqlite_master_entry_exists(&fixture.conn, "table", table).await,
                "migration must install {table}"
            );
        }
        assert!(
            sqlite_master_entry_exists(
                &fixture.conn,
                "trigger",
                "retrieval_anchors_immutable_update"
            )
            .await,
            "migration must install retrieval anchor immutability triggers"
        );
    }
    assemble_phase(MIGRATION_SCOPE, samples, ticks_per_second)
}

async fn user_version(conn: &libsql::Connection) -> u32 {
    let mut rows = conn
        .query("PRAGMA user_version", ())
        .await
        .expect("query migration user_version");
    let row = rows
        .next()
        .await
        .expect("read migration user_version row")
        .expect("user_version should return a row");
    let version: i64 = row.get(0).expect("read user_version value");
    u32::try_from(version).expect("user_version fits u32")
}

async fn sqlite_master_entry_exists(
    conn: &libsql::Connection,
    entry_kind: &str,
    name: &str,
) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type = ?1 AND name = ?2",
            libsql::params![entry_kind, name],
        )
        .await
        .expect("query sqlite_master");
    rows.next().await.expect("read sqlite_master row").is_some()
}

// ---------------------------------------------------------------------------
// Evidence emission and validation.
// ---------------------------------------------------------------------------

fn platform_info(clock_ticks_per_second: u64) -> PlatformInfo {
    PlatformInfo {
        rustc: command_output("rustc", &["-Vv"]),
        cargo: command_output("cargo", &["-V"]),
        kernel: command_output("uname", &["-srmo"]),
        cpu_identity: cpu_identity(),
        logical_cpu_count: std::thread::available_parallelism()
            .expect("available logical CPUs")
            .get(),
        memory_total_kib: memory_total_kib(),
        clock_ticks_per_second,
        debug_assertions: cfg!(debug_assertions),
    }
}

fn captured_at_unix_micros() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch");
    i64::try_from(elapsed.as_micros()).expect("capture time fits i64")
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn float_close(left: f64, right: f64) -> bool {
    (left - right).abs() <= left.abs().max(right.abs()).max(1.0) * 1e-12
}

fn validate_phase(
    phase: &PhaseMeasurement,
    scope: &str,
    record_count: impl Fn(usize) -> bool,
    clock_ticks_per_second: u64,
) -> Result<(), String> {
    if phase.scope != scope {
        return Err(format!(
            "phase scope mismatch: expected {scope}, found {}",
            phase.scope
        ));
    }
    if phase.raw_samples.len() != MEASURED_REPETITIONS {
        return Err(format!(
            "phase {scope} has {} samples instead of {MEASURED_REPETITIONS}",
            phase.raw_samples.len()
        ));
    }
    let mut total_records = 0_usize;
    for (repetition, sample) in phase.raw_samples.iter().enumerate() {
        if sample.repetition != repetition {
            return Err(format!("phase {scope} sample order mismatch"));
        }
        if !record_count(sample.record_count) {
            return Err(format!(
                "phase {scope} sample {repetition} has invalid record count {}",
                sample.record_count
            ));
        }
        if sample.latency_ns == 0 {
            return Err(format!(
                "phase {scope} sample {repetition} has zero latency"
            ));
        }
        total_records += sample.record_count;
    }
    if phase.measured_records != total_records {
        return Err(format!("phase {scope} record total mismatch"));
    }
    let latencies = phase
        .raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let expected = Distribution::from_samples(&latencies);
    if expected.repetitions != phase.latency.repetitions
        || expected.min_ns != phase.latency.min_ns
        || expected.p50_ns != phase.latency.p50_ns
        || expected.p95_ns != phase.latency.p95_ns
        || expected.p99_ns != phase.latency.p99_ns
        || expected.max_ns != phase.latency.max_ns
        || !float_close(expected.mean_ns, phase.latency.mean_ns)
        || !float_close(expected.sample_stddev_ns, phase.latency.sample_stddev_ns)
    {
        return Err(format!(
            "phase {scope} distribution mismatch: recomputed {expected:?}, stored {:?}",
            phase.latency
        ));
    }
    let aggregate = aggregate_samples(&phase.raw_samples);
    if phase.cpu_ticks != aggregate.cpu_ticks
        || phase.process_write_bytes != aggregate.process_write_bytes
        || phase.database_storage_growth_bytes != aggregate.database_storage_growth_bytes
        || phase.peak_rss_kib != aggregate.peak_rss_kib
    {
        return Err(format!("phase {scope} aggregate mismatch"));
    }
    let total_ns = latencies.iter().sum::<u64>();
    if total_ns == 0
        || !float_close(
            phase.cpu_ms,
            ticks_to_ms(aggregate.cpu_ticks, clock_ticks_per_second),
        )
        || !float_close(
            phase.records_per_second,
            total_records as f64 * 1_000_000_000.0 / total_ns as f64,
        )
    {
        return Err(format!("phase {scope} derived metrics mismatch"));
    }
    Ok(())
}

fn validate_result(result: &BenchmarkResult) -> Result<(), String> {
    if result.schema_version != RESULT_SCHEMA_VERSION
        || result.workload_id != WORKLOAD_ID
        || result.evidence_status != EvidenceStatus::Provisional
        || result.provisional_reason != PROVISIONAL_REASON
        || result.command != BENCHMARK_COMMAND
    {
        return Err("provisional result identity is invalid".to_string());
    }
    if result.workload_identity != workload_identity() {
        return Err("provisional result workload identity mismatch".to_string());
    }
    if !is_lower_hex(&result.git.commit, 40) {
        return Err("provisional result git commit is invalid".to_string());
    }
    if result.captured_at_unix_micros <= 0
        || result.platform.rustc.is_empty()
        || result.platform.cargo.is_empty()
        || result.platform.kernel.is_empty()
        || result.platform.cpu_identity.is_empty()
        || result.platform.logical_cpu_count == 0
        || result.platform.memory_total_kib == 0
        || result.platform.clock_ticks_per_second == 0
    {
        return Err("provisional result environment is invalid".to_string());
    }
    if result.warmup_repetitions != WARMUP_REPETITIONS
        || result.measured_repetitions != MEASURED_REPETITIONS
        || result.records_per_repetition != RECORDS_PER_REPETITION
        || result.concurrency != CONCURRENCY
    {
        return Err("provisional result repetition contract mismatch".to_string());
    }
    let ticks = result.platform.clock_ticks_per_second;
    let record_phase_records = |count: usize| count == RECORDS_PER_REPETITION;
    validate_phase(
        &result.fact_write,
        FACT_WRITE_SCOPE,
        record_phase_records,
        ticks,
    )?;
    validate_phase(
        &result.anchor_create,
        ANCHOR_CREATE_SCOPE,
        record_phase_records,
        ticks,
    )?;
    validate_phase(
        &result.anchor_resolution,
        ANCHOR_RESOLUTION_SCOPE,
        record_phase_records,
        ticks,
    )?;
    validate_phase(
        &result.anchor_replay,
        ANCHOR_REPLAY_SCOPE,
        record_phase_records,
        ticks,
    )?;
    let migration_minimum =
        usize::try_from(MIGRATION_MINIMUM_FINAL_USER_VERSION - MIGRATION_FROM_USER_VERSION)
            .expect("minimum migration count fits usize");
    validate_phase(
        &result.migration_v19_to_v22,
        MIGRATION_SCOPE,
        |count| count >= migration_minimum,
        ticks,
    )?;
    Ok(())
}

fn validate_evidence_directory(directory: &Path) -> Result<(), String> {
    let lock = evidence_lock();
    lock.lock_shared()
        .map_err(|error| format!("lock evidence directory: {error}"))?;
    let result = validate_evidence_directory_locked(directory);
    lock.unlock()
        .map_err(|error| format!("unlock evidence directory: {error}"))?;
    result
}

fn validate_evidence_directory_locked(directory: &Path) -> Result<(), String> {
    if !directory.join(README_NAME).is_file() {
        return Err("evidence directory lacks README.md".to_string());
    }
    let manifest_path = directory.join(
        WORKLOAD_MANIFEST_PATH
            .rsplit('/')
            .next()
            .expect("manifest name"),
    );
    if fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?
        != WORKLOAD_MANIFEST.as_bytes()
    {
        return Err("checked-in workload manifest differs from the compiled contract".to_string());
    }
    let index_path = directory.join(EVIDENCE_INDEX_NAME);
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
    if index.current_acceptance.is_some() {
        return Err(
            "commit-attested acceptance evidence is impossible from the dirty PR7 worktree"
                .to_string(),
        );
    }
    let historical_index = index
        .historical_stale
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if historical_index.len() != index.historical_stale.len() {
        return Err("evidence index contains duplicate historical artifacts".to_string());
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

    let mut provisional = Vec::new();
    let mut historical = BTreeSet::new();
    for name in files {
        let path = directory.join(&name);
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        let envelope = serde_json::from_slice::<ArtifactEnvelope>(&bytes)
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        match envelope.evidence_status.as_str() {
            "provisional" => {
                let result = serde_json::from_slice::<BenchmarkResult>(&bytes)
                    .map_err(|error| format!("parse provisional {}: {error}", path.display()))?;
                validate_result(&result)?;
                provisional.push(name);
            }
            "historical_stale" => {
                if envelope.schema_version != RESULT_SCHEMA_VERSION
                    || envelope.workload_id != WORKLOAD_ID
                {
                    return Err(format!(
                        "{} has invalid historical identity",
                        path.display()
                    ));
                }
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
    if historical != historical_index {
        return Err(format!(
            "historical evidence index mismatch: indexed={historical_index:?}, files={historical:?}"
        ));
    }
    match (&index.provisional, provisional.as_slice()) {
        (Some(expected), [actual]) if expected == actual => Ok(()),
        (None, []) => Err("evidence index has no provisional artifact".to_string()),
        (Some(expected), actual) => Err(format!(
            "evidence index names {expected}, provisional artifacts are {actual:?}"
        )),
        (None, _) => Err("unindexed provisional artifact".to_string()),
    }
}

fn write_provisional_artifact(result: &BenchmarkResult) {
    let directory = evidence_directory();
    let lock = evidence_lock();
    lock.lock_exclusive().expect("lock PR7 benchmark evidence");
    let temporary = directory.join(format!(
        ".{}-{}.tmp",
        PROVISIONAL_ARTIFACT_NAME,
        std::process::id()
    ));
    fs::write(
        &temporary,
        serde_json::to_string(result).expect("serialize PR7 benchmark result"),
    )
    .expect("write provisional PR7 benchmark artifact");
    fs::rename(&temporary, directory.join(PROVISIONAL_ARTIFACT_NAME))
        .expect("publish provisional PR7 benchmark artifact");
    lock.unlock().expect("unlock PR7 benchmark evidence");
}

async fn run() {
    validate_manifest();
    let clock_ticks_per_second = preflight_platform();
    let identity_before = workload_identity();
    let git = git_snapshot();
    let fact_write = measure_fact_write(clock_ticks_per_second).await;
    let (anchor_create, anchor_resolution, anchor_replay) =
        Box::pin(measure_anchor_phases(clock_ticks_per_second)).await;
    let migration_v19_to_v22 = measure_migration(clock_ticks_per_second).await;
    assert_eq!(
        workload_identity(),
        identity_before,
        "manifest or harness source changed during benchmark execution"
    );
    let result = BenchmarkResult {
        schema_version: RESULT_SCHEMA_VERSION,
        workload_id: WORKLOAD_ID.to_string(),
        evidence_status: EvidenceStatus::Provisional,
        provisional_reason: PROVISIONAL_REASON.to_string(),
        captured_at_unix_micros: captured_at_unix_micros(),
        command: BENCHMARK_COMMAND.to_string(),
        workload_identity: identity_before,
        git,
        platform: platform_info(clock_ticks_per_second),
        warmup_repetitions: WARMUP_REPETITIONS,
        measured_repetitions: MEASURED_REPETITIONS,
        records_per_repetition: RECORDS_PER_REPETITION,
        concurrency: CONCURRENCY,
        fact_write,
        anchor_create,
        anchor_resolution,
        anchor_replay,
        migration_v19_to_v22,
    };
    validate_result(&result)
        .expect("fresh PR7 benchmark result must satisfy the evidence contract");
    write_provisional_artifact(&result);
    validate_evidence_directory(&evidence_directory())
        .expect("emitted PR7 benchmark evidence must validate");
    println!(
        "TRACEDECAY_PR7_BENCHMARK_RESULT={}",
        serde_json::to_string(&result).expect("serialize PR7 benchmark result")
    );
}

#[test]
fn workload_manifest_matches_code_contract() {
    validate_manifest();
}

#[test]
fn evidence_directory_matches_index_contract() {
    validate_evidence_directory(&evidence_directory())
        .expect("PR7 benchmark evidence directory contract");
}

#[tokio::test]
async fn pr7_memory_baseline() {
    if std::env::consts::OS != "linux" {
        eprintln!("[pr7-benchmark] skipping measurement: the evidence platform contract is Linux");
        return;
    }
    let _env_lock = crate::config::lock_user_data_dir_test_env();
    Box::pin(run()).await;
}
