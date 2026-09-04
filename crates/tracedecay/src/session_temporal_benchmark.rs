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
use std::time::Instant;

use serde_json::{Value, json};
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

use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_runtime_core::storage::{
    read_repository_identity_marker, write_repository_identity_marker,
};
use tracedecay_runtime_core::timeutil::nearest_rank;
use tracedecay_session_memory::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use tracedecay_session_memory::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRefreshSchedulerError, SessionRefreshSchedulerPort, SessionRequestBinding,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalService,
    SessionScopeAuthorizationRequest, SessionScopeAuthorizer, SessionTemporalQuery,
};
use tracedecay_session_temporal_store::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::codex;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ports::ExecutionControl;
use tracedecay_temporal_query::ranking::DiversityLimits;

mod root_relation_fixture;

const SCHEMA_VERSION: u64 = 2;
const WORKLOAD_ID: &str = "session-temporal-v1";
const WORKLOAD_PATH: &str = "benchmark_data/session-temporal/workload-v1.json";
const EVIDENCE_INDEX_PATH: &str = "benchmark_data/session-temporal/evidence-index.json";
const RESULT_PATH: &str = "benchmark_data/session-temporal/result-current.json";
const RESULT_FILE_NAME: &str = "result-current.json";
const HISTORICAL_RESULT_PATH: &str = "benchmark_data/session-temporal/result-provisional.json";
const HISTORICAL_RESULT_FILE_NAME: &str = "result-provisional.json";
const HISTORICAL_HARNESS_PATH: &str = "src/sessions/session_temporal_benchmark.rs";
const RUNNER_PATH: &str = "scripts/run-session-temporal-benchmark.sh";
const HARNESS_PATH: &str = "crates/tracedecay/src/session_temporal_benchmark.rs";
const SOURCE_MODE_CLEAN: &str = "clean_git_worktree_v1";
const SANITIZATION_RECEIPT_PATH: &str =
    "benchmark_data/session-temporal/fixtures/codex-sanitization-receipt.json";
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
        include_str!(
            "../../../tests/fixtures/provider_normalization/codex/session_meta.input.json"
        ),
    ),
    (
        "tests/fixtures/provider_normalization/codex/agent_message.input.json",
        include_str!(
            "../../../tests/fixtures/provider_normalization/codex/agent_message.input.json"
        ),
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

    #[hotpath::skip]
    const fn allows_diagnostic_measurement(self) -> bool {
        matches!(self, Self::Linux | Self::Macos)
    }

    #[hotpath::skip]
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

    #[hotpath::skip]
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
///
/// Holds the crate-wide `lock_user_data_dir_test_env` mutex for the lifetime
/// of the guard so restoration can be observed without racing other env
/// mutators.
pub struct IsolatedBenchmarkEnv {
    _env_lock: std::sync::MutexGuard<'static, ()>,
    temp: TempDir,
    home: PathBuf,
    data_dir: PathBuf,
    previous_home: Option<OsString>,
    previous_data_dir: Option<OsString>,
    restored: bool,
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
            restored: false,
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

    /// Restore `HOME` and `TRACEDECAY_DATA_DIR` while the env lock is still held.
    pub fn restore_under_lock(&mut self) {
        if self.restored {
            return;
        }
        restore_env("HOME", self.previous_home.take());
        restore_env("TRACEDECAY_DATA_DIR", self.previous_data_dir.take());
        self.restored = true;
    }
}

impl Drop for IsolatedBenchmarkEnv {
    fn drop(&mut self) {
        self.restore_under_lock();
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
    registered: RegisteredGlobalDbLeaseV1,
    session: SessionId,
    root_sessions: Vec<SessionId>,
    context: RequestContext,
    binding: SessionRequestBinding,
    complete_request: SessionRefreshCompletionRequestV1,
    rebuild_activate_ns: u64,
    root_record_count: usize,
    /// Keeps the project-memory owner mounted for the whole repetition so the
    /// sessions store retains its verified graph runtime.
    _memory: std::sync::Arc<tracedecay_runtime_core::db::Database>,
    _daemon_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _env: IsolatedBenchmarkEnv,
}

struct RepetitionMeasurement {
    phase_latencies: Vec<(Phase, u64)>,
    record_count: usize,
}

/// Validate checked-in artifacts without Cargo mutation (also used by the bench).
///
/// Content identity is the git commit: every artifact and source path here is
/// tracked, so the contract checks artifact shape and state, not file hashes.
pub fn validate_contract() -> BenchResult<()> {
    let root = repository_root();
    let workload = read_json(&root.join(WORKLOAD_PATH))?;
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
        &workload["runner"]["path"],
        json!(RUNNER_PATH),
        "runner path",
    )?;
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
        Some(RESULT_FILE_NAME) => validate_current_result(&root)?,
        None => match fs::metadata(root.join(RESULT_PATH)) {
            Ok(_) => {
                return Err(
                    "current measurement exists without an evidence-index pointer".to_owned(),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("inspect current measurement artifact: {error}"));
            }
        },
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

fn validate_current_result(root: &Path) -> BenchResult<()> {
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
        &result["workload_manifest"],
        json!(WORKLOAD_PATH),
        "result workload manifest",
    )?;
    require_json_value(
        &result["source_identity"]["harness"],
        json!(HARNESS_PATH),
        "result harness path",
    )?;
    require_json_value(
        &result["source_identity"]["runner"],
        json!(RUNNER_PATH),
        "result runner path",
    )?;
    require_json_value(
        &result["source_identity"]["source_mode"],
        json!(SOURCE_MODE_CLEAN),
        "result source mode",
    )?;
    if !matches!(
        result["source_identity"]["commit"].as_str(),
        Some(commit) if commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    ) {
        return Err("result source commit must be a full git commit id".to_owned());
    }
    require_json_value(
        &result["measurement"]["warmup_repetitions"],
        json!(WARMUP_REPETITIONS),
        "result warmup repetitions",
    )?;
    require_json_value(
        &result["measurement"]["measured_repetitions"],
        json!(MEASURED_REPETITIONS),
        "result measured repetitions",
    )?;
    let records_per_repetition = result["measurement"]["records_per_repetition"]
        .as_u64()
        .filter(|count| *count > 0)
        .ok_or_else(|| "result records_per_repetition must be positive".to_owned())?;
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
    let measurement = capture_measurement().await?;
    Ok(measurement_result(
        source_identity(&current_commit(&root)?),
        measurement,
    ))
}

/// Publish the current result from one measured run over a clean commit.
///
/// No caller-provided measurements are accepted. The workload manifest is
/// static configuration and is never rewritten by runs; the published result
/// records the clean source commit it was measured at, which is the content
/// authority for every tracked artifact and source path it names.
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
    let measurement = capture_measurement().await?;
    let commit_after = clean_source_commit(&root)?;
    if commit_after != source_commit {
        return Err("source commit changed during benchmark contract refresh".to_owned());
    }
    let mut identity = source_identity(&source_commit);
    identity["source_mode"] = json!(SOURCE_MODE_CLEAN);
    let result = measurement_result(identity, measurement);
    write_json_atomic(&root.join(RESULT_PATH), &result)?;
    // Publish the current-evidence pointer only after the result is durably
    // written, so an interrupted refresh never claims current evidence.
    write_json_atomic(&root.join(EVIDENCE_INDEX_PATH), &current_evidence_index())?;
    validate_contract()?;
    Ok(result)
}

async fn capture_measurement() -> BenchResult<Value> {
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

    let records_per_repetition =
        records_per_repetition.ok_or_else(|| "measurement produced no record count".to_owned())?;
    Ok(json!({
        "warmup_repetitions": WARMUP_REPETITIONS,
        "measured_repetitions": MEASURED_REPETITIONS,
        "records_per_repetition": records_per_repetition,
        "measured_record_count": records_per_repetition.saturating_mul(MEASURED_REPETITIONS),
        "inferential_claim": false,
        "phases": phases,
    }))
}

fn measurement_result(source_identity: Value, measurement: Value) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "workload_id": WORKLOAD_ID,
        "capture_status": "provisional",
        "acceptance_eligible": false,
        "provisional_reason": "diagnostic_measurement_capture",
        "workload_manifest": WORKLOAD_PATH,
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

/// Installs the process-wide background CPU and resident-memory authorities
/// the production Codex admission path requires.
///
/// Production installs both during daemon bootstrap, which this harness never
/// runs; without them every capture is refused with the typed
/// `BackgroundResourceUnavailable` states. The shared JSONL preparation
/// authority is process-wide and single (a second install with a different
/// memory handle fails closed), so this delegates to the same helper
/// `HostAdmissionTestRuntimeV1` uses instead of racing it with a
/// benchmark-private handle.
fn ensure_admission_resource_authorities() {
    crate::host_admission::ensure_process_background_cpu_authority()
        .expect("install process capture authorities for the benchmark");
}

async fn prepare_repetition(repetition: usize) -> BenchResult<PreparedRepetition> {
    ensure_admission_resource_authorities();
    let env = IsolatedBenchmarkEnv::enter("session-temporal-")?;
    let project = env.path().join("project");
    fs::create_dir_all(&project).map_err(|error| format!("create project: {error}"))?;
    let project_id = enroll_benchmark_project(&project)?;

    let profile = env.data_dir().join("profile");
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile)
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
    // The canonical projection drain converges Git evidence, which publishes
    // through the project sessions store's verified graph runtime. That runtime
    // is bound only once the project-memory owner is mounted too, so the
    // benchmark mounts the same pair the daemon does.
    let memory = session_registry
        .project_memory(project_id.clone(), [project.clone()])
        .await
        .map_err(|error| format!("mount benchmark project memory: {error}"))?;
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
        _memory: memory,
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

/// Identity of the source that produced a capture. The recorded commit is the
/// content authority: every path named here is git-tracked at that commit, so
/// no per-file hashing is repeated.
fn source_identity(commit: &str) -> Value {
    json!({
        "commit": commit,
        "harness": HARNESS_PATH,
        "runner": RUNNER_PATH,
        "sanitization_receipt": SANITIZATION_RECEIPT_PATH,
        "binary": std::env::current_exe()
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned)),
    })
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

/// The receipt documents sanitization provenance for the independently
/// sourced provider fixtures; the fixture bytes themselves are compiled into
/// this harness via `include_str!`, so git and the compiler own their content.
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

fn require_json_value(actual: &Value, expected: Value, label: &str) -> BenchResult<()> {
    if actual != &expected {
        return Err(format!(
            "{label} mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn repository_root() -> PathBuf {
    // The product package sits at `crates/tracedecay`; this benchmark reads git
    // metadata and fixtures that live at the workspace root above it.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root above crates/tracedecay")
        .to_owned()
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
    fn refresh_result_records_clean_run_provenance() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let mut identity = source_identity(commit);
        identity["source_mode"] = json!(SOURCE_MODE_CLEAN);
        let result = measurement_result(identity, json!({"records_per_repetition": 2}));

        assert_eq!(result["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(result["capture_status"], json!("provisional"));
        assert_eq!(result["acceptance_eligible"], json!(false));
        assert_eq!(result["workload_manifest"], json!(WORKLOAD_PATH));
        assert_eq!(result["source_identity"]["commit"], json!(commit));
        assert_eq!(
            result["source_identity"]["source_mode"],
            json!(SOURCE_MODE_CLEAN)
        );
        assert_eq!(result["source_identity"]["harness"], json!(HARNESS_PATH));
        assert_eq!(result["source_identity"]["runner"], json!(RUNNER_PATH));
        assert_eq!(result["measurement"]["records_per_repetition"], json!(2),);
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
        let mut isolated = IsolatedBenchmarkEnv::enter("session-temporal-env-").unwrap();
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
        isolated.restore_under_lock();
        assert_eq!(env::var_os("HOME"), prior_home);
        assert_eq!(env::var_os("TRACEDECAY_DATA_DIR"), prior_data);
    }
}
