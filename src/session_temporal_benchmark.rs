//! Session-temporal benchmark harness.
//!
//! Drives production Codex admission, `CanonicalSessionTemporalProjector`
//! materialization through the registered session database,
//! [`SessionRefreshService`], and [`SessionRetrievalService`]. Output is a
//! Linux/macOS diagnostic measurement capture with descriptive sample quantiles only.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, ObservationScopeV1, ProjectId, RepositoryId, RetrievalGrainV1, SessionId,
    TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_store::SessionRefreshCompletionRequestV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_runtime_core::storage::{
    EnrollmentMarker, StorageMode, read_repository_identity_marker, write_enrollment_marker,
    write_repository_identity_marker,
};
use tracedecay_runtime_core::timeutil::nearest_rank;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::codex;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ports::ExecutionControl;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use tracedecay_usecases::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_usecases::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRefreshSchedulerError, SessionRefreshSchedulerPort, SessionRequestBinding,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalService,
    SessionScopeAuthorizationRequest, SessionScopeAuthorizer, SessionTemporalQuery,
};

mod root_relation_fixture;

const SCHEMA_VERSION: u64 = 2;
const WORKLOAD_ID: &str = "session-temporal-v1";
const WORKLOAD_PATH: &str = "benchmarks/session-temporal/workload-v1.json";
const EVIDENCE_INDEX_PATH: &str = "benchmarks/session-temporal/evidence-index.json";
const RESULT_PATH: &str = "benchmarks/session-temporal/result-current.json";
const RESULT_FILE_NAME: &str = "result-current.json";
const HISTORICAL_RESULT_PATH: &str = "benchmarks/session-temporal/result-provisional.json";
const HISTORICAL_RESULT_FILE_NAME: &str = "result-provisional.json";
const HISTORICAL_HARNESS_PATH: &str = "src/sessions/session_temporal_benchmark.rs";
const RUNNER_PATH: &str = "scripts/run-session-temporal-benchmark.sh";
const HARNESS_PATH: &str = "src/session_temporal_benchmark.rs";
const SANITIZATION_RECEIPT_PATH: &str =
    "benchmarks/session-temporal/fixtures/codex-sanitization-receipt.json";
const P95_LABEL: &str = "descriptive nearest-rank sample p95";
const P99_LABEL: &str = "descriptive nearest-rank sample p99 (sample maximum when n=30)";
const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const BENCHMARK_REQUEST_WORK_UNIT_LIMIT: u64 = 100_000;
const PROJECTOR_VERSION: &str = "session-temporal-projector.v1";
const CONFIG_VERSION: &str = "session-refresh-config.v1";
const BENCHMARK_PROJECT_ID: &str = "proj_session_temporal_benchmark";
const DIGEST: [u8; 32] = [0x8b; 32];

const NATIVE_CODEX_FIXTURES: &[(&str, &str)] = &[
    (
        "tests/fixtures/provider_normalization/codex/session_meta.input.json",
        include_str!("../tests/fixtures/provider_normalization/codex/session_meta.input.json"),
    ),
    (
        "tests/fixtures/provider_normalization/codex/agent_message.input.json",
        include_str!("../tests/fixtures/provider_normalization/codex/agent_message.input.json"),
    ),
];

type BenchResult<T> = Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BenchmarkHostPolicy {
    Linux,
    Macos,
    Unsupported,
}

impl BenchmarkHostPolicy {
    fn for_target_os(target_os: &str) -> Self {
        match target_os {
            "linux" => Self::Linux,
            "macos" => Self::Macos,
            _ => Self::Unsupported,
        }
    }

    const fn allows_diagnostic_measurement(self) -> bool {
        matches!(self, Self::Linux | Self::Macos)
    }

    const fn allows_contract_refresh(self) -> bool {
        matches!(self, Self::Linux)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    RebuildActivate,
    ExactReplay,
    CompactRank,
    LateHydrate,
}

impl Phase {
    pub const ALL: [Phase; 4] = [
        Phase::RebuildActivate,
        Phase::ExactReplay,
        Phase::CompactRank,
        Phase::LateHydrate,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Phase::RebuildActivate => "rebuild_activate",
            Phase::ExactReplay => "exact_replay",
            Phase::CompactRank => "compact_rank",
            Phase::LateHydrate => "late_hydrate",
        }
    }
}

/// RAII isolation for `HOME` and `TRACEDECAY_DATA_DIR`.
pub struct IsolatedBenchmarkEnv {
    _env_lock: std::sync::MutexGuard<'static, ()>,
    temp: TempDir,
    home: PathBuf,
    data_dir: PathBuf,
    previous_home: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

impl IsolatedBenchmarkEnv {
    pub fn enter(prefix: &str) -> BenchResult<Self> {
        let env_lock = crate::config::lock_user_data_dir_test_env();
        let temp = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| format!("create tempdir: {error}"))?;
        let home = temp.path().join("home");
        let data_dir = temp.path().join("tracedecay-data");
        fs::create_dir_all(&home).map_err(|error| format!("create HOME: {error}"))?;
        fs::create_dir_all(&data_dir)
            .map_err(|error| format!("create TRACEDECAY_DATA_DIR: {error}"))?;

        let previous_home = env::var_os("HOME");
        let previous_data_dir = env::var_os("TRACEDECAY_DATA_DIR");
        // SAFETY: the crate-wide environment lock is held for the RAII lifetime.
        unsafe {
            env::set_var("HOME", &home);
            env::set_var("TRACEDECAY_DATA_DIR", &data_dir);
        }

        Ok(Self {
            _env_lock: env_lock,
            temp,
            home,
            data_dir,
            previous_home,
            previous_data_dir,
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn path(&self) -> &Path {
        self.temp.path()
    }
}

impl Drop for IsolatedBenchmarkEnv {
    fn drop(&mut self) {
        restore_env("HOME", self.previous_home.take());
        restore_env("TRACEDECAY_DATA_DIR", self.previous_data_dir.take());
    }
}

fn restore_env(key: &str, previous: Option<OsString>) {
    // SAFETY: restores process environment captured by IsolatedBenchmarkEnv.
    unsafe {
        match previous {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }
}

#[derive(Clone, Copy)]
struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.session-temporal-benchmark").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

#[derive(Clone, Copy, Default)]
struct NoopWake;

impl SessionRefreshSchedulerPort for NoopWake {
    fn wake(&self) -> Result<(), SessionRefreshSchedulerError> {
        Ok(())
    }
}

struct Words(&'static str);

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &str {
        self.0
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

struct PreparedRepetition {
    registered: Arc<RegisteredGlobalDb>,
    session: SessionId,
    root_sessions: Vec<SessionId>,
    context: RequestContext,
    binding: SessionRequestBinding,
    complete_request: SessionRefreshCompletionRequestV1,
    rebuild_activate_ns: u64,
    root_record_count: usize,
    _daemon_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _env: IsolatedBenchmarkEnv,
}

struct RepetitionMeasurement {
    phase_latencies: Vec<(Phase, u64)>,
    record_count: usize,
}

struct MeasurementCapture {
    measurement: Value,
    records_per_repetition: usize,
}

/// Validate checked-in artifacts without Cargo mutation (also used by the bench).
pub fn validate_contract() -> BenchResult<()> {
    let root = repository_root();
    let workload_path = root.join(WORKLOAD_PATH);
    let workload = read_json(&workload_path)?;
    require_json_value(
        &workload["schema_version"],
        json!(SCHEMA_VERSION),
        "workload schema",
    )?;
    require_json_value(&workload["workload_id"], json!(WORKLOAD_ID), "workload id")?;
    require_json_value(
        &workload["status"],
        json!("harness_ready"),
        "workload status",
    )?;
    require_json_value(
        &workload["fixture_evidence"]["independently_sourced"],
        json!(true),
        "fixture source status",
    )?;
    require_json_value(
        &workload["fixture_evidence"]["sanitization_receipt"],
        json!(SANITIZATION_RECEIPT_PATH),
        "fixture sanitization receipt",
    )?;
    if workload["measurement_contract"].is_null() {
        return Err("measurement_contract must be defined once fixtures are authentic".to_owned());
    }
    let phases = workload["measurement_contract"]["phases"]
        .as_array()
        .ok_or_else(|| "measurement phases must be an array".to_owned())?;
    let expected: Vec<&str> = Phase::ALL.iter().map(|phase| phase.as_str()).collect();
    let actual: Vec<&str> = phases
        .iter()
        .filter_map(|phase| phase["phase"].as_str())
        .collect();
    if actual != expected {
        return Err(format!(
            "measurement phases mismatch: expected {expected:?}, got {actual:?}"
        ));
    }
    require_json_value(
        &workload["statistics"]["p95_label"],
        json!(P95_LABEL),
        "p95 label",
    )?;
    require_json_value(
        &workload["statistics"]["p99_label"],
        json!(P99_LABEL),
        "p99 label",
    )?;
    require_json_value(
        &workload["production_path"]["available_to_benchmark_target"],
        json!(true),
        "production path availability",
    )?;
    require_json_value(
        &workload["implementation"]["path"],
        json!(HARNESS_PATH),
        "implementation path",
    )?;
    require_json_value(
        &workload["implementation"]["sha256"],
        json!(sha256_file(&root.join(HARNESS_PATH))?),
        "implementation hash",
    )?;
    validate_file_inventory(&root, &workload["file_inventory"])?;
    validate_bench_profile(&root)?;
    validate_sanitization_receipt(&root)?;

    let index = read_json(&root.join(EVIDENCE_INDEX_PATH))?;
    require_json_value(
        &index["schema_version"],
        json!(SCHEMA_VERSION),
        "index schema",
    )?;
    require_json_value(
        &index["current_acceptance"],
        Value::Null,
        "current acceptance",
    )?;
    require_json_value(&index["blocked"], Value::Null, "blocked result pointer")?;
    require_json_value(
        &index["historical_stale"],
        json!([HISTORICAL_RESULT_FILE_NAME]),
        "historical result index",
    )?;
    validate_historical_result(&root)?;

    match index["provisional"].as_str() {
        Some(RESULT_FILE_NAME) => validate_current_result(&root, &workload_path, &workload)?,
        None => {
            if workload.get("refresh_provenance").is_some() {
                return Err(
                    "harness without current measurement must not retain refresh provenance"
                        .to_owned(),
                );
            }
            match fs::metadata(root.join(RESULT_PATH)) {
                Ok(_) => {
                    return Err(
                        "current measurement exists without an evidence-index pointer".to_owned(),
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("inspect current measurement artifact: {error}"));
                }
            }
        }
        Some(value) => return Err(format!("unexpected provisional result pointer: {value}")),
    }

    Ok(())
}

fn validate_historical_result(root: &Path) -> BenchResult<()> {
    let result = read_json(&root.join(HISTORICAL_RESULT_PATH))?;
    require_json_value(
        &result["schema_version"],
        json!(SCHEMA_VERSION),
        "historical result schema",
    )?;
    require_json_value(
        &result["workload_id"],
        json!(WORKLOAD_ID),
        "historical result workload id",
    )?;
    require_json_value(
        &result["capture_status"],
        json!("provisional"),
        "historical capture status",
    )?;
    require_json_value(
        &result["evidence_status"],
        json!("historical_stale"),
        "historical evidence status",
    )?;
    require_json_value(
        &result["acceptance_eligible"],
        json!(false),
        "historical acceptance eligibility",
    )?;
    require_json_value(
        &result["source_identity"]["harness"],
        json!(HISTORICAL_HARNESS_PATH),
        "historical harness path",
    )?;
    if !matches!(result["stale_reason"].as_str(), Some(reason) if !reason.is_empty()) {
        return Err("historical result must explain why it is stale".to_owned());
    }
    Ok(())
}

fn validate_current_result(root: &Path, workload_path: &Path, workload: &Value) -> BenchResult<()> {
    let result = read_json(&root.join(RESULT_PATH))?;
    require_json_value(
        &result["schema_version"],
        json!(SCHEMA_VERSION),
        "result schema",
    )?;
    require_json_value(
        &result["workload_id"],
        json!(WORKLOAD_ID),
        "result workload id",
    )?;
    require_json_value(
        &result["capture_status"],
        json!("provisional"),
        "capture status",
    )?;
    require_json_value(
        &result["acceptance_eligible"],
        json!(false),
        "acceptance eligibility",
    )?;
    require_json_value(
        &result["workload_manifest_sha256"],
        json!(sha256_file(&workload_path)?),
        "workload manifest hash",
    )?;
    require_json_value(
        &result["source_identity"]["harness"],
        json!(HARNESS_PATH),
        "result harness path",
    )?;
    require_json_value(
        &result["source_identity"]["harness_sha256"],
        json!(sha256_file(&root.join(HARNESS_PATH))?),
        "result harness hash",
    )?;
    let provenance = &workload["refresh_provenance"];
    require_json_value(
        &provenance["source_mode"],
        json!("clean_git_worktree_v1"),
        "refresh source mode",
    )?;
    require_json_value(
        &provenance["warmup_repetitions"],
        json!(WARMUP_REPETITIONS),
        "refresh warmup repetitions",
    )?;
    require_json_value(
        &provenance["measured_repetitions"],
        json!(MEASURED_REPETITIONS),
        "refresh measured repetitions",
    )?;
    let records_per_repetition = provenance["records_per_repetition"]
        .as_u64()
        .filter(|count| *count > 0)
        .ok_or_else(|| "refresh records_per_repetition must be positive".to_owned())?;
    require_json_value(
        &provenance["record_count"],
        json!(records_per_repetition),
        "refresh record count",
    )?;
    require_json_value(
        &provenance["measured_record_count"],
        json!(records_per_repetition * MEASURED_REPETITIONS as u64),
        "refresh measured record count",
    )?;
    require_json_value(
        &result["source_identity"]["commit"],
        provenance["source_commit"].clone(),
        "result source commit",
    )?;
    require_json_value(
        &result["measurement"]["records_per_repetition"],
        json!(records_per_repetition),
        "result records per repetition",
    )?;
    require_json_value(
        &result["measurement"]["measured_record_count"],
        json!(records_per_repetition * MEASURED_REPETITIONS as u64),
        "result measured record count",
    )?;
    Ok(())
}

fn validate_refresh_inputs(root: &Path, workload: &Value) -> BenchResult<()> {
    require_json_value(
        &workload["schema_version"],
        json!(SCHEMA_VERSION),
        "workload schema",
    )?;
    require_json_value(&workload["workload_id"], json!(WORKLOAD_ID), "workload id")?;
    require_json_value(
        &workload["status"],
        json!("harness_ready"),
        "workload status",
    )?;
    require_json_value(
        &workload["fixture_evidence"]["independently_sourced"],
        json!(true),
        "fixture source status",
    )?;
    require_json_value(
        &workload["fixture_evidence"]["sanitization_receipt"],
        json!(SANITIZATION_RECEIPT_PATH),
        "fixture sanitization receipt",
    )?;
    let phases = workload["measurement_contract"]["phases"]
        .as_array()
        .ok_or_else(|| "measurement phases must be an array".to_owned())?;
    let actual = phases
        .iter()
        .filter_map(|phase| phase["phase"].as_str())
        .collect::<Vec<_>>();
    let expected = Phase::ALL.map(Phase::as_str);
    if actual != expected {
        return Err(format!(
            "measurement phases mismatch: expected {expected:?}, got {actual:?}"
        ));
    }
    require_json_value(
        &workload["statistics"]["p95_label"],
        json!(P95_LABEL),
        "p95 label",
    )?;
    require_json_value(
        &workload["statistics"]["p99_label"],
        json!(P99_LABEL),
        "p99 label",
    )?;
    require_json_value(
        &workload["production_path"]["available_to_benchmark_target"],
        json!(true),
        "production path availability",
    )?;
    validate_bench_profile(root)?;
    validate_sanitization_receipt(root)
}

/// Run the production measurement loop without publishing evidence.
///
/// A current result requires the clean-source provenance guaranteed by
/// [`refresh_contract`]; this mode prints diagnostic samples only.
pub async fn run_measurement() -> BenchResult<Value> {
    let host_policy = BenchmarkHostPolicy::for_target_os(std::env::consts::OS);
    if !host_policy.allows_diagnostic_measurement() {
        return Err(
            "Session-temporal --run diagnostic measurement is supported on Linux/macOS".to_owned(),
        );
    }
    validate_contract()?;
    validate_bench_profile_enforced()?;

    let root = repository_root();
    let capture = capture_measurement().await?;
    let workload_sha256 = sha256_file(&root.join(WORKLOAD_PATH))?;
    let source_identity = current_state_identity(&root, &workload_sha256)?;
    Ok(measurement_result(
        source_identity,
        workload_sha256,
        capture.measurement,
    ))
}

/// Refresh the manifest and result from one measured run over a clean commit.
///
/// No caller-provided measurements or hashes are accepted. Both artifacts are
/// derived after the run and published as one consistency-checked pair.
pub async fn refresh_contract() -> BenchResult<Value> {
    let host_policy = BenchmarkHostPolicy::for_target_os(std::env::consts::OS);
    if !host_policy.allows_contract_refresh() {
        return Err("Session-temporal contract refresh is Linux-hosted".to_owned());
    }
    validate_bench_profile_enforced()?;
    let root = repository_root();
    let workload = read_json(&root.join(WORKLOAD_PATH))?;
    validate_refresh_inputs(&root, &workload)?;
    let source_commit = clean_source_commit(&root)?;
    let capture = capture_measurement().await?;
    let commit_after = clean_source_commit(&root)?;
    if commit_after != source_commit {
        return Err("source commit changed during benchmark contract refresh".to_owned());
    }
    let refreshed_workload = refresh_workload_manifest(
        &root,
        workload,
        &source_commit,
        capture.records_per_repetition,
    )?;
    let workload_sha256 = hex::encode(Sha256::digest(encode_json(&refreshed_workload)?));
    let source_identity = current_state_identity(&root, &workload_sha256)?;
    let result = measurement_result(source_identity, workload_sha256, capture.measurement);
    write_contract_pair_atomic(
        &root.join(WORKLOAD_PATH),
        &refreshed_workload,
        &root.join(RESULT_PATH),
        &result,
    )?;
    // Publish the current-evidence pointer only after the workload/result pair
    // is consistent, so an interrupted refresh never claims current evidence.
    write_json_atomic(&root.join(EVIDENCE_INDEX_PATH), &current_evidence_index())?;
    validate_contract()?;
    Ok(result)
}

async fn capture_measurement() -> BenchResult<MeasurementCapture> {
    let mut phase_latencies: Vec<(Phase, Vec<u64>)> = Phase::ALL
        .iter()
        .copied()
        .map(|phase| (phase, Vec::new()))
        .collect();
    let mut records_per_repetition = None;

    for repetition in 0..(WARMUP_REPETITIONS + MEASURED_REPETITIONS) {
        let repetition_measurement = run_one_repetition(repetition).await?;
        match records_per_repetition {
            Some(expected) if expected != repetition_measurement.record_count => {
                return Err(format!(
                    "record count changed across repetitions: expected {expected}, got {}",
                    repetition_measurement.record_count
                ));
            }
            None => records_per_repetition = Some(repetition_measurement.record_count),
            Some(_) => {}
        }
        if repetition < WARMUP_REPETITIONS {
            continue;
        }
        for (phase, latency_ns) in repetition_measurement.phase_latencies {
            let slot = phase_latencies
                .iter_mut()
                .find(|(candidate, _)| *candidate == phase)
                .unwrap();
            slot.1.push(latency_ns);
        }
    }

    let mut phases = Vec::new();
    for (phase, mut samples) in phase_latencies {
        if samples.is_empty() {
            return Err(format!("phase {} produced no samples", phase.as_str()));
        }
        samples.sort_unstable();
        phases.push(json!({
            "phase": phase.as_str(),
            "sample_count": samples.len(),
            "p50_ns": nearest_rank(&samples, 50).unwrap_or_default(),
            "p95_ns": nearest_rank(&samples, 95).unwrap_or_default(),
            "p99_ns": nearest_rank(&samples, 99).unwrap_or_default(),
            "maximum_ns": *samples.last().unwrap(),
            "p95_label": P95_LABEL,
            "p99_label": P99_LABEL,
            "raw_latency_ns": samples,
        }));
    }

    let measurement = json!({
        "warmup_repetitions": WARMUP_REPETITIONS,
        "measured_repetitions": MEASURED_REPETITIONS,
        "records_per_repetition": records_per_repetition,
        "measured_record_count": records_per_repetition
            .unwrap_or_default()
            .saturating_mul(MEASURED_REPETITIONS),
        "inferential_claim": false,
        "phases": phases,
    });
    Ok(MeasurementCapture {
        measurement,
        records_per_repetition: records_per_repetition
            .ok_or_else(|| "measurement produced no record count".to_owned())?,
    })
}

fn measurement_result(
    source_identity: Value,
    workload_manifest_sha256: String,
    measurement: Value,
) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "workload_id": WORKLOAD_ID,
        "capture_status": "provisional",
        "acceptance_eligible": false,
        "provisional_reason": "diagnostic_measurement_capture",
        "workload_manifest": WORKLOAD_PATH,
        "workload_manifest_sha256": workload_manifest_sha256,
        "source_identity": source_identity,
        "runtime": {
            "operating_system": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "measurement": measurement,
        "claims": {
            "performance_acceptance": {
                "status": "provisional",
                "reason": "samples recorded; descriptive latency only, not an acceptance decision"
            },
            "database_query_latency": {
                "status": "provisional",
                "reason": "descriptive nearest-rank sample quantiles only; not inferential"
            }
        }
    })
}

async fn prepare_repetition(repetition: usize) -> BenchResult<PreparedRepetition> {
    let env = IsolatedBenchmarkEnv::enter("session-temporal-")?;
    let project = env.path().join("project");
    fs::create_dir_all(&project).map_err(|error| format!("create project: {error}"))?;
    let project_id = enroll_benchmark_project(&project)?;

    let profile = env.data_dir().join("profile");
    let profile_identity = crate::daemon::profile_identity::load_or_create(&profile)
        .map_err(|error| format!("create benchmark profile identity: {error}"))?;
    let brain_id = profile_identity.brain_id().clone();
    let profile_id = profile_identity.profile_id().clone();
    let _daemon_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile,
        u64::try_from(repetition).unwrap() + 1,
        &format!("session-temporal-bench-{repetition}"),
    )
    .map_err(|error| format!("enter daemon scope: {error}"))?;
    let session_registry = DaemonSessionRuntimeRegistryV1::open(profile_identity)
        .await
        .map_err(|error| format!("open benchmark session registry: {error}"))?;
    let registered = session_registry
        .project_sessions(project_id.clone(), [project.clone()])
        .await
        .map_err(|error| format!("mount benchmark project sessions: {error}"))?;
    let root_sessions = root_relation_fixture::session_ids(repetition)?;
    let admission = HostAdmissionFacade::new(HostAdmissionAuthorities::for_project(
        brain_id,
        profile_id.clone(),
        project_id.clone(),
        registered.as_ref(),
    ));
    for session_id in &root_sessions {
        let rollout = write_codex_rollout(env.home(), &project, session_id.as_str())?;
        codex::try_admit_codex_jsonl_observations_for_project_with_admission(
            &rollout,
            &project,
            project_id.clone(),
            &admission,
            None,
        )
        .await
        .map_err(|error| format!("Codex production admit failed: {error}"))?;
    }

    let observation_count = count_observations(registered.as_ref()).await?;
    if observation_count
        < u64::try_from(root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT)
            .map_err(|error| format!("root participant count: {error}"))?
    {
        return Err(format!(
            "Codex admit produced {observation_count} observations for {} root participants",
            root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT
        ));
    }
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let projection = admission
        .drain_projection_queue(
            "codex",
            &scope,
            &ObservationCancellation::default(),
            usize::try_from(observation_count)
                .map_err(|error| format!("projection queue size: {error}"))?,
        )
        .await
        .map_err(|outcome| format!("Codex canonical projection failed: {outcome:?}"))?;
    if projection.projected_outputs
        < u64::try_from(root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT)
            .map_err(|error| format!("root participant count: {error}"))?
    {
        return Err(format!(
            "Codex canonical projector produced {} outputs for {} root participants",
            projection.projected_outputs,
            root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT
        ));
    }

    let db = registered.as_ref();
    let (context, binding) = request_context(
        &format!("request.session-temporal.{repetition}"),
        &project_id,
        profile_id.as_str(),
    )?;
    let started = Instant::now();
    let root_fixture = root_relation_fixture::refresh_sessions(
        db,
        &context,
        &binding,
        root_sessions,
        observation_count,
    )
    .await?;
    let rebuild_activate_ns = elapsed_ns(started);
    if root_fixture
        .anchor_durable_progress
        .frontier()
        .observed_through()
        != root_fixture
            .anchor_durable_progress
            .frontier()
            .committed_through()
        || root_fixture.anchor_durable_progress.committed_records() == 0
    {
        return Err("root refresh did not persist a complete durable frontier".to_owned());
    }

    Ok(PreparedRepetition {
        registered,
        session: root_fixture.anchor_session,
        root_sessions: root_fixture.sessions,
        context,
        binding,
        complete_request: root_fixture.anchor_complete_request,
        rebuild_activate_ns,
        root_record_count: root_fixture.record_count,
        _daemon_scope,
        _env: env,
    })
}

async fn run_one_repetition(repetition: usize) -> BenchResult<RepetitionMeasurement> {
    let prepared = prepare_repetition(repetition).await?;
    let record_count = prepared.root_record_count;
    if record_count < root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT {
        return Err(format!(
            "root refresh persisted {record_count} records for {} participant sessions",
            root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT
        ));
    }
    let replay_started = Instant::now();
    prepared
        .registered
        .complete_session_refresh_result(
            prepared.complete_request.clone(),
            ExecutionControl::new(None),
        )
        .await
        .map_err(|error| format!("exact replay complete: {error:?}"))?;
    let exact_replay_ns = elapsed_ns(replay_started);

    let execution = RegisteredGlobalDbSessionTemporalExecution::new(prepared.registered.as_ref());
    let retrieval = SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words("words-v1"),
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    );
    let query = |grain: RetrievalGrainV1, text: &str| {
        SessionTemporalQuery::new(
            prepared.session.clone(),
            Some("codex".to_owned()),
            text.to_owned(),
            None,
            TemporalModeV1::Current,
            grain,
            8,
            DiversityLimits::default(),
            ContextBudget {
                max_bytes: 64_000,
                max_tokens: 16_000,
                estimator_version: "words-v1".to_owned(),
            },
        )
        .unwrap()
    };

    let compact_started = Instant::now();
    require_retrieval_success(
        "compact_rank",
        retrieval
            .retrieve(
                &prepared.context,
                &prepared.binding,
                query(RetrievalGrainV1::LogicalMessage, "pipeline"),
            )
            .await,
    )?;
    let compact_rank_ns = elapsed_ns(compact_started);

    // Preserve the frozen wire shape: the existing late-hydrate sample records
    // the root-wide occurrence hydration rather than minting another phase.
    let hydrate_started = Instant::now();
    let root_hydration = retrieval
        .retrieve(
            &prepared.context,
            &prepared.binding,
            root_relation_fixture::root_relation_query(prepared.session.clone())?,
        )
        .await;
    root_relation_fixture::require_root_relation_hydration(
        root_hydration,
        &prepared.root_sessions,
    )?;
    let late_hydrate_ns = elapsed_ns(hydrate_started);

    Ok(RepetitionMeasurement {
        phase_latencies: vec![
            (Phase::RebuildActivate, prepared.rebuild_activate_ns),
            (Phase::ExactReplay, exact_replay_ns),
            (Phase::CompactRank, compact_rank_ns),
            (Phase::LateHydrate, late_hydrate_ns),
        ],
        record_count,
    })
}

fn enroll_benchmark_project(project: &Path) -> BenchResult<ProjectId> {
    let status = Command::new("git")
        .args(["-C"])
        .arg(project)
        .arg("init")
        .status()
        .map_err(|error| format!("git init: {error}"))?;
    if !status.success() {
        return Err("git init failed for benchmark project".to_owned());
    }
    if !write_repository_identity_marker(project, BENCHMARK_PROJECT_ID)
        .map_err(|error| format!("write identity marker: {error}"))?
    {
        return Err("repository identity marker was not written".to_owned());
    }
    write_enrollment_marker(
        project,
        &EnrollmentMarker {
            project_id: BENCHMARK_PROJECT_ID.to_owned(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .map_err(|error| format!("write enrollment marker: {error}"))?;
    let marker = read_repository_identity_marker(project)
        .map_err(|error| format!("read identity marker: {error}"))?
        .ok_or_else(|| "repository identity marker missing after write".to_owned())?;
    ProjectId::new(marker.project_id).map_err(|error| error.to_string())
}

fn write_codex_rollout(home: &Path, project: &Path, session_id: &str) -> BenchResult<PathBuf> {
    let directory = home.join(".codex/sessions/2026/07/15");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create Codex sessions dir: {error}"))?;
    let path = directory.join(format!("rollout-{session_id}.jsonl"));

    let mut session: Value = serde_json::from_str(NATIVE_CODEX_FIXTURES[0].1)
        .map_err(|error| format!("parse session_meta fixture: {error}"))?;
    session["payload"]["id"] = json!(session_id);
    session["payload"]["cwd"] = json!(project);

    let message: Value = serde_json::from_str(NATIVE_CODEX_FIXTURES[1].1)
        .map_err(|error| format!("parse agent_message fixture: {error}"))?;

    let mut body = String::new();
    for line in [session, message] {
        body.push_str(&serde_json::to_string(&line).unwrap());
        body.push('\n');
    }
    fs::write(&path, body).map_err(|error| format!("write Codex rollout: {error}"))?;
    Ok(path)
}

async fn count_observations(db: &RegisteredGlobalDb) -> BenchResult<u64> {
    let mut rows = db
        .read_connection()
        .query("SELECT COUNT(*) FROM observations", ())
        .await
        .map_err(|error| format!("count observations: {error}"))?;
    let value: i64 = rows
        .next()
        .await
        .map_err(|error| format!("count observations row: {error}"))?
        .ok_or_else(|| "count observations returned no row".to_owned())?
        .get(0)
        .map_err(|error| format!("count observations value: {error}"))?;
    u64::try_from(value).map_err(|error| format!("observation count: {error}"))
}

fn require_retrieval_success<T: std::fmt::Debug>(
    phase: &str,
    outcome: SessionRetrievalOutcome<T>,
) -> BenchResult<()> {
    match outcome {
        SessionRetrievalOutcome::Complete { .. }
        | SessionRetrievalOutcome::CompleteZero { .. }
        | SessionRetrievalOutcome::Partial { .. } => Ok(()),
        other => Err(format!("{phase} retrieve failed: {other:?}")),
    }
}

fn request_context(
    request: &str,
    project_id: &ProjectId,
    profile_id: &str,
) -> BenchResult<(RequestContext, SessionRequestBinding)> {
    let actor = ActorId::new("actor.session-temporal.benchmark").unwrap();
    let request_id = RequestId::new(request).unwrap();
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new(profile_id.to_owned())
            .map_err(|error| format!("bind benchmark profile identity: {error}"))?,
        project_id.clone(),
        SessionStoreId::new(format!("store.{}", project_id.as_str())).unwrap(),
        SessionRootId::new("root.session-temporal.benchmark").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.session-temporal.benchmark").unwrap(),
            WorktreeId::new("worktree.session-temporal.benchmark").unwrap(),
            BranchId::new("branch.session-temporal.benchmark").unwrap(),
        ),
    );
    let scope = identity.application_scope().unwrap();
    let capability = CapabilityDigest::new(DIGEST);
    let access_policy = tracedecay_store::observation_capture_access_policy_digest_v1()
        .map_err(|error| format!("resolve benchmark observation access policy: {error}"))?;
    let policy = PolicyDigest::from_access_policy_digest(&access_policy)
        .map_err(|error| format!("bind benchmark observation access policy: {error}"))?;
    let configuration = ConfigurationDigest::new(DIGEST);
    let cancellation = CancellationToken::for_application_request(request_id.as_str());
    let budgets =
        RequestBudgets::new(64, 64 * 1024 * 1024, BENCHMARK_REQUEST_WORK_UNIT_LIMIT).unwrap();
    let observed_at = application_observed_at();
    let expires_at = UtcMicros(observed_at.0.saturating_add(30_000_000));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.session-temporal-benchmark.application").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.session-temporal.benchmark").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id.clone(),
        Deadline::new(expires_at).unwrap(),
        CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
    )
    .unwrap();
    let binding = SessionRequestBinding::new(
        identity,
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
    );
    Ok((context, binding))
}

fn current_state_identity(root: &Path, workload_manifest_sha256: &str) -> BenchResult<Value> {
    let commit = current_commit(root)?;
    Ok(json!({
        "commit": commit,
        "workload_manifest": WORKLOAD_PATH,
        "workload_manifest_sha256": workload_manifest_sha256,
        "harness": HARNESS_PATH,
        "harness_sha256": sha256_file(&root.join(HARNESS_PATH))?,
        "runner": RUNNER_PATH,
        "runner_sha256": sha256_file(&root.join(RUNNER_PATH))?,
        "sanitization_receipt": SANITIZATION_RECEIPT_PATH,
        "sanitization_receipt_sha256": sha256_file(&root.join(SANITIZATION_RECEIPT_PATH))?,
        "binary": std::env::current_exe()
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned)),
        "native_fixtures": NATIVE_CODEX_FIXTURES
            .iter()
            .map(|(path, _)| {
                json!({
                    "path": path,
                    "sha256": sha256_file(&root.join(path)).unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>(),
        "profile": {
            "cargo_profile": "bench",
            "opt_level": 3,
            "debug": false,
            "debug_assertions": false,
            "overflow_checks": false,
            "incremental": false,
        }
    }))
}

fn current_evidence_index() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "current_acceptance": null,
        "blocked": null,
        "provisional": RESULT_FILE_NAME,
        "historical_stale": [HISTORICAL_RESULT_FILE_NAME],
    })
}

fn validate_sanitization_receipt(root: &Path) -> BenchResult<()> {
    let receipt = read_json(&root.join(SANITIZATION_RECEIPT_PATH))?;
    require_json_value(
        &receipt["schema"],
        json!("session-temporal-fixture-sanitization-receipt-v1"),
        "receipt schema",
    )?;
    require_json_value(
        &receipt["independently_sourced"],
        json!(true),
        "receipt source",
    )?;
    require_json_value(&receipt["provider"], json!("codex"), "receipt provider")?;
    let files = receipt["files"]
        .as_array()
        .ok_or_else(|| "receipt files missing".to_owned())?;
    for entry in files {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| "receipt file path missing".to_owned())?;
        let expected = entry["sha256"]
            .as_str()
            .ok_or_else(|| format!("receipt hash missing for {path}"))?;
        let actual = sha256_file(&root.join(path))?;
        if actual != expected {
            return Err(format!(
                "receipt hash mismatch for {path}: expected {expected}, got {actual}"
            ));
        }
    }
    Ok(())
}

fn validate_file_inventory(root: &Path, inventory: &Value) -> BenchResult<()> {
    let entries = inventory
        .as_array()
        .ok_or_else(|| "file_inventory must be an array".to_owned())?;
    if entries.is_empty() {
        return Err("file_inventory must not be empty".to_owned());
    }
    let mut paths = std::collections::BTreeSet::new();
    for entry in entries {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| "inventory path must be a string".to_owned())?;
        let expected = entry["sha256"]
            .as_str()
            .ok_or_else(|| format!("inventory hash missing for {path}"))?;
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(format!(
                "inventory path must remain repository-relative: {path}"
            ));
        }
        if !paths.insert(path) {
            return Err(format!("duplicate inventory path: {path}"));
        }
        let actual = sha256_file(&root.join(relative))?;
        if actual != expected {
            return Err(format!(
                "inventory hash mismatch for {path}: expected {expected}, got {actual}"
            ));
        }
    }
    Ok(())
}

fn validate_bench_profile(root: &Path) -> BenchResult<()> {
    let manifest = fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|error| format!("read Cargo.toml: {error}"))?;
    let profile = manifest
        .split_once("[profile.bench]")
        .map(|(_, profile)| profile.split("\n[").next().unwrap_or(profile))
        .ok_or_else(|| "Cargo.toml is missing [profile.bench]".to_owned())?;
    for line in [
        "opt-level = 3",
        "debug = false",
        "debug-assertions = false",
        "overflow-checks = false",
        "incremental = false",
    ] {
        if !profile.lines().any(|candidate| candidate.trim() == line) {
            return Err(format!("bench profile is missing {line:?}"));
        }
    }
    Ok(())
}

fn validate_bench_profile_enforced() -> BenchResult<()> {
    if cfg!(debug_assertions) {
        return Err(
            "optimized bench profile required: debug_assertions are enabled (use cargo bench)"
                .to_owned(),
        );
    }
    Ok(())
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}

fn read_json(path: &Path) -> BenchResult<Value> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&contents).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_json_atomic(path: &Path, value: &Value) -> BenchResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("result path has no parent: {}", path.display()))?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("result")
    ));
    let encoded =
        serde_json::to_vec_pretty(value).map_err(|error| format!("encode result: {error}"))?;
    fs::write(&temporary, encoded).map_err(|error| format!("write temp result: {error}"))?;
    fs::rename(&temporary, path).map_err(|error| format!("rename result: {error}"))?;
    Ok(())
}

fn refresh_workload_manifest(
    root: &Path,
    mut workload: Value,
    source_commit: &str,
    records_per_repetition: usize,
) -> BenchResult<Value> {
    let workload_object = workload
        .as_object_mut()
        .ok_or_else(|| "workload manifest must be an object".to_owned())?;
    workload_object["implementation"]["sha256"] = json!(sha256_file(&root.join(HARNESS_PATH))?);
    workload_object["runner"]["sha256"] = json!(sha256_file(&root.join(RUNNER_PATH))?);
    workload_object["profile"]["manifest_sha256"] = json!(sha256_file(&root.join("Cargo.toml"))?);
    let inventory = workload_object["file_inventory"]
        .as_array_mut()
        .ok_or_else(|| "workload file inventory must be an array".to_owned())?;
    for entry in inventory {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| "workload inventory entry is missing its path".to_owned())?;
        entry["sha256"] = json!(sha256_file(&root.join(path))?);
    }
    workload_object.insert(
        "refresh_provenance".to_owned(),
        json!({
            "source_commit": source_commit,
            "source_mode": "clean_git_worktree_v1",
            "warmup_repetitions": WARMUP_REPETITIONS,
            "measured_repetitions": MEASURED_REPETITIONS,
            "record_count": records_per_repetition,
            "records_per_repetition": records_per_repetition,
            "measured_record_count": records_per_repetition * MEASURED_REPETITIONS,
        }),
    );
    Ok(workload)
}

fn write_contract_pair_atomic(
    workload_path: &Path,
    workload: &Value,
    result_path: &Path,
    result: &Value,
) -> BenchResult<()> {
    if workload_path.parent() != result_path.parent() {
        return Err("workload and result must share one artifact directory".to_owned());
    }
    let workload_bytes = encode_json(workload)?;
    let workload_sha256 = hex::encode(Sha256::digest(&workload_bytes));
    require_json_value(
        &result["workload_manifest_sha256"],
        json!(workload_sha256),
        "result workload manifest hash",
    )?;

    let workload_original = fs::read(workload_path).ok();
    let result_original = fs::read(result_path).ok();
    let workload_temporary = temporary_artifact_path(workload_path);
    let result_temporary = temporary_artifact_path(result_path);
    fs::write(&workload_temporary, workload_bytes)
        .map_err(|error| format!("write temporary workload manifest: {error}"))?;
    if let Err(error) = fs::write(&result_temporary, encode_json(result)?) {
        let _ = fs::remove_file(&workload_temporary);
        return Err(format!("write temporary benchmark result: {error}"));
    }
    fs::rename(&workload_temporary, workload_path)
        .map_err(|error| format!("publish workload manifest: {error}"))?;
    if let Err(error) = fs::rename(&result_temporary, result_path) {
        restore_artifact(workload_path, workload_original.as_deref())?;
        restore_artifact(result_path, result_original.as_deref())?;
        let _ = fs::remove_file(&result_temporary);
        return Err(format!(
            "publish benchmark result after workload manifest; restored prior pair: {error}"
        ));
    }
    Ok(())
}

fn encode_json(value: &Value) -> BenchResult<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(|error| format!("encode benchmark artifact: {error}"))
}

fn temporary_artifact_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.refresh.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact")
    ))
}

fn restore_artifact(path: &Path, original: Option<&[u8]>) -> BenchResult<()> {
    match original {
        Some(bytes) => fs::write(path, bytes)
            .map_err(|error| format!("restore benchmark artifact '{}': {error}", path.display())),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove newly published benchmark artifact '{}': {error}",
                path.display()
            )),
        },
    }
}

fn require_json_value(actual: &Value, expected: Value, label: &str) -> BenchResult<()> {
    if actual != &expected {
        return Err(format!(
            "{label} mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> BenchResult<String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read {} for SHA-256: {error}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn current_commit(root: &Path) -> BenchResult<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| format!("read current commit: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn clean_source_commit(root: &Path) -> BenchResult<String> {
    let status = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain=v1", "--untracked-files=no"])
        .output()
        .map_err(|error| format!("inspect benchmark source state: {error}"))?;
    if !status.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    if !status.stdout.is_empty() {
        return Err(
            "benchmark contract refresh requires a clean source commit; tracked changes are present"
                .to_owned(),
        );
    }
    current_commit(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_matches_checked_in_artifacts() {
        validate_contract().expect("session-temporal contract");
    }

    #[test]
    fn phases_are_descriptive_and_ordered() {
        assert_eq!(
            Phase::ALL.map(Phase::as_str),
            [
                "rebuild_activate",
                "exact_replay",
                "compact_rank",
                "late_hydrate",
            ]
        );
        assert_eq!(P95_LABEL, "descriptive nearest-rank sample p95");
        assert_eq!(
            P99_LABEL,
            "descriptive nearest-rank sample p99 (sample maximum when n=30)"
        );
    }

    #[test]
    fn diagnostic_measurement_host_policy_allows_linux_and_macos() {
        assert!(BenchmarkHostPolicy::for_target_os("linux").allows_diagnostic_measurement());
        assert!(BenchmarkHostPolicy::for_target_os("macos").allows_diagnostic_measurement());
    }

    #[test]
    fn contract_refresh_host_policy_is_linux_only() {
        assert!(BenchmarkHostPolicy::for_target_os("linux").allows_contract_refresh());
        assert!(!BenchmarkHostPolicy::for_target_os("macos").allows_contract_refresh());
    }

    #[test]
    fn nearest_rank_uses_descriptive_sample_labels() {
        let samples = [10_u64, 20, 30, 40, 50];
        assert_eq!(nearest_rank(&samples, 50), Some(30));
        assert_eq!(nearest_rank(&samples, 95), Some(50));
        assert_eq!(nearest_rank(&samples, 99), Some(50));
    }

    #[test]
    fn refresh_manifest_records_one_real_run_provenance() {
        let root = repository_root();
        let workload = read_json(&root.join(WORKLOAD_PATH)).unwrap();
        let refreshed = refresh_workload_manifest(&root, workload, "0123456789abcdef", 2).unwrap();

        assert_eq!(
            refreshed["refresh_provenance"]["source_commit"],
            json!("0123456789abcdef")
        );
        assert_eq!(
            refreshed["refresh_provenance"]["source_mode"],
            json!("clean_git_worktree_v1")
        );
        assert_eq!(
            refreshed["refresh_provenance"]["warmup_repetitions"],
            json!(WARMUP_REPETITIONS)
        );
        assert_eq!(
            refreshed["refresh_provenance"]["measured_repetitions"],
            json!(MEASURED_REPETITIONS)
        );
        assert_eq!(
            refreshed["refresh_provenance"]["records_per_repetition"],
            json!(2)
        );
        assert_eq!(
            refreshed["refresh_provenance"]["measured_record_count"],
            json!(2 * MEASURED_REPETITIONS)
        );
        assert_eq!(
            refreshed["implementation"]["sha256"],
            json!(sha256_file(&root.join(HARNESS_PATH)).unwrap())
        );
        assert_eq!(
            refreshed["runner"]["sha256"],
            json!(sha256_file(&root.join(RUNNER_PATH)).unwrap())
        );
    }

    #[test]
    fn contract_pair_writer_rejects_disagreeing_result() {
        let temp = TempDir::new().unwrap();
        let workload_path = temp.path().join("workload.json");
        let result_path = temp.path().join("result.json");
        let workload = json!({"schema_version": SCHEMA_VERSION});
        let result = json!({
            "workload_manifest_sha256": "not-the-workload-hash"
        });

        let error = write_contract_pair_atomic(&workload_path, &workload, &result_path, &result)
            .unwrap_err();

        assert!(error.contains("workload manifest hash"));
        assert!(!workload_path.exists());
        assert!(!result_path.exists());
    }

    #[test]
    fn refresh_preflight_requires_a_clean_source_commit() {
        let temp = TempDir::new().unwrap();
        let run_git = |arguments: &[&str]| {
            let status = Command::new("git")
                .current_dir(temp.path())
                .args(arguments)
                .status()
                .unwrap();
            assert!(status.success(), "git {arguments:?}");
        };
        run_git(&["init", "--quiet"]);
        fs::write(temp.path().join("source.txt"), "clean\n").unwrap();
        run_git(&["add", "source.txt"]);
        run_git(&[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed source",
        ]);

        let commit = clean_source_commit(temp.path()).unwrap();
        assert_eq!(commit.len(), 40);

        fs::write(temp.path().join("source.txt"), "dirty\n").unwrap();
        let error = clean_source_commit(temp.path()).unwrap_err();
        assert!(error.contains("tracked changes"), "{error}");
    }

    #[tokio::test]
    async fn fixture_refresh_persists_progress_before_measurement() {
        let prepared = prepare_repetition(0)
            .await
            .expect("production fixture refresh must persist durable progress");
        assert_eq!(
            prepared.root_sessions.len(),
            root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT
        );
        assert!(
            prepared.root_record_count >= root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT
        );
    }

    #[tokio::test]
    async fn fresh_benchmark_db_provisions_key_for_rank_and_hydration() {
        let samples = run_one_repetition(0)
            .await
            .expect("fresh benchmark database must provision an authenticated cursor key");
        let phases = samples
            .phase_latencies
            .iter()
            .map(|(phase, _)| *phase)
            .collect::<Vec<_>>();

        assert!(phases.contains(&Phase::CompactRank));
        assert!(phases.contains(&Phase::LateHydrate));
        assert!(samples.record_count >= root_relation_fixture::ROOT_RELATION_PARTICIPANT_COUNT);
    }

    #[tokio::test]
    async fn isolated_env_sets_and_restores_home_and_data_dir() {
        let isolated = IsolatedBenchmarkEnv::enter("session-temporal-env-").unwrap();
        let prior_home = isolated.previous_home.clone();
        let prior_data = isolated.previous_data_dir.clone();
        assert_eq!(
            env::var_os("HOME").as_deref(),
            Some(isolated.home().as_os_str())
        );
        assert_eq!(
            env::var_os("TRACEDECAY_DATA_DIR").as_deref(),
            Some(isolated.data_dir().as_os_str())
        );
        drop(isolated);
        assert_eq!(env::var_os("HOME"), prior_home);
        assert_eq!(env::var_os("TRACEDECAY_DATA_DIR"), prior_data);
    }
}
