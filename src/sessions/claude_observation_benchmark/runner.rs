use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_store::{
    ObservationReplayRequest, SESSION_MESSAGE_PROJECTOR_VERSION, StoredObservation,
};

use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use crate::application::observation::ObservationCancellation;
use crate::sessions::claude::ClaudeSource;
use crate::sessions::claude_observation::{
    ClaudeObservationIngestStats, ingest_source_with_observations_with_admission,
};
use crate::sessions::cline_like::{ClineLikeSource, capture_cline_like_snapshot_observations};
use crate::sessions::{codex, cursor, hermes, kiro};
use tracedecay_runtime_core::sqlite_read_snapshot::open_immutable_read_only;
use tracedecay_runtime_core::storage::{
    read_repository_identity_marker, write_repository_identity_marker,
};

use super::artifact::{
    attest_build, command_output, git_snapshot, validate_git_snapshots, workload_identity,
};
use super::metrics::{
    aggregate_samples, cpu_identity, elapsed_ns, memory_total_kib, preflight_platform,
    process_cpu_ticks, process_peak_rss_kib, process_write_bytes, reset_peak_rss, ticks_to_ms,
    validate_no_op_invariants,
};
use super::model::{
    BenchmarkResult, Distribution, NoOpTotals, PROVIDER_COMMIT_SCOPE, PROVIDER_PARSE_SCOPE,
    PROVIDER_REPLAY_SCOPE, ProviderBenchmarkResult, ProviderBenchmarkSuiteResult,
    ProviderFairnessResult, ProviderPhaseResult, ProviderScheduleTurn, RawPhaseSample,
    RawProviderPhaseSample,
};
use super::{
    BENCHMARK_COMMAND, BENCHMARK_SECRET_PREFIX, MEASURED_REPETITIONS, NATIVE_PROVIDER_FIXTURES,
    PROVIDER_PIPELINE_SCOPE, RECORDS_PER_REPETITION, RESULT_SCHEMA_VERSION, WARMUP_REPETITIONS,
    WORKLOAD_ID,
};
use super::{baseline, manifest};

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

pub(super) struct Fixture {
    home: PathBuf,
    profile: PathBuf,
    pub(super) transcript: PathBuf,
    runtime: HostAdmissionTestRuntimeV1,
    _temp: TempDir,
}

impl Fixture {
    pub(super) async fn new(repetition: usize) -> Self {
        let temp = benchmark_tempdir("pipeline-");
        let home = temp.path().join("home");
        let profile = home.join(".tracedecay");
        let session_id = format!("benchmark-session-{repetition}");
        let transcript = home
            .join(".claude/projects/project-scope")
            .join(format!("{session_id}.jsonl"));
        fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("create Claude benchmark fixture tree");
        fs::create_dir_all(&profile).expect("create benchmark profile root");
        write_records(&transcript, &session_id);
        let runtime = HostAdmissionTestRuntimeV1::profile(&profile)
            .await
            .expect("open registered benchmark runtime");
        Self {
            home,
            profile,
            transcript,
            runtime,
            _temp: temp,
        }
    }

    pub(super) fn source(&self) -> ClaudeSource {
        let session_id = self
            .transcript
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("benchmark session id");
        ClaudeSource::with_home(&self.home).for_user_scope(Some(session_id.to_string()), Vec::new())
    }

    pub(super) async fn ingest(&self, source: &ClaudeSource) -> ClaudeObservationIngestStats {
        let admission = self.runtime.facade();
        ingest_source_with_observations_with_admission(
            source,
            &self.profile,
            ObservationScopeV1::Profile,
            &admission,
            None,
            ObservationCancellation::default(),
        )
        .await
        .expect("run production observation pipeline")
    }

    pub(super) async fn replay(&self) -> Vec<StoredObservation> {
        self.replay_after(0, RECORDS_PER_REPETITION + 1).await
    }

    fn database_storage_bytes(&self) -> u64 {
        self.runtime
            .session_database_storage_bytes_for_test(HostAdmissionScope::Profile)
            .expect("measure registered profile database storage")
    }

    async fn replay_after(&self, after_sequence: u64, limit: usize) -> Vec<StoredObservation> {
        self.runtime
            .replay_observations(
                HostAdmissionScope::Profile,
                ObservationReplayRequest::new(after_sequence, limit)
                    .expect("bounded replay request"),
            )
            .await
            .expect("replay committed benchmark observations")
    }

    pub(super) async fn verify_committed_state(&self, observations: &[StoredObservation]) {
        assert_eq!(observations.len(), RECORDS_PER_REPETITION);
        let expected_session_id = self
            .transcript
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("benchmark session id");
        for (index, stored) in observations.iter().enumerate() {
            let payload = stored.observation().payload().to_string();
            assert!(
                !payload.contains(BENCHMARK_SECRET_PREFIX),
                "authoritative observation payload retained the secret canary"
            );
            assert!(
                stored.observation().receipt().payload().is_some(),
                "authoritative observation lacks a payload-bound sanitization receipt"
            );

            let message_id = format!("benchmark-message-{index}");
            let message = self
                .runtime
                .session_message_for_test(HostAdmissionScope::Profile, "claude", &message_id)
                .await
                .expect("read folded V1 message")
                .unwrap_or_else(|| panic!("missing folded V1 message {message_id}"));
            assert_eq!(message.provider, "claude");
            assert_eq!(message.message_id, message_id);
            assert_eq!(message.session_id, expected_session_id);
            assert_eq!(message.role, "user");
            assert_eq!(
                message.text,
                format!("bounded production observation {index}")
            );
            let folded_state = format!(
                "{}\n{}\n{}",
                message.text,
                message.metadata_json.as_deref().unwrap_or_default(),
                message.source_path.as_deref().unwrap_or_default()
            );
            assert!(
                !folded_state.contains(BENCHMARK_SECRET_PREFIX),
                "folded V1 projection retained the secret canary"
            );
        }
        self.verify_projector_only_current_writes().await;
    }

    async fn verify_projector_only_current_writes(&self) {
        let snapshot = self
            .runtime
            .read_snapshot(HostAdmissionScope::Profile)
            .await
            .expect("open registered benchmark projection snapshot");
        let mut rows = snapshot
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM session_messages WHERE provider = 'claude'),
                    COUNT(*),
                    COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND output_provider = 'claude'",
                (SESSION_MESSAGE_PROJECTOR_VERSION,),
            )
            .await
            .expect("count benchmark projection ownership");
        let row = rows
            .next()
            .await
            .expect("read benchmark projection ownership")
            .expect("benchmark projection ownership row");
        let counts = (
            row.get::<i64>(0).expect("benchmark message count"),
            row.get::<i64>(1)
                .expect("benchmark projection provenance count"),
            row.get::<i64>(2)
                .expect("benchmark projector-created message count"),
        );
        let expected = i64::try_from(RECORDS_PER_REPETITION)
            .expect("benchmark record count fits SQLite integer");
        assert_eq!(
            counts,
            (expected, expected, expected),
            "every benchmark row must be created exactly once by the active observation projector"
        );
    }
}

fn write_records(path: &Path, session_id: &str) {
    let mut body = String::new();
    for index in 0..RECORDS_PER_REPETITION {
        let record = json!({
            "type": "user",
            "sessionId": session_id,
            "uuid": format!("benchmark-message-{index}"),
            "timestamp": "2026-07-15T00:00:00Z",
            "cwd": path.parent(),
            "message": {
                "role": "user",
                "content": format!("bounded production observation {index}"),
                "secret_key": format!("{BENCHMARK_SECRET_PREFIX}{index}"),
            }
        });
        body.push_str(&record.to_string());
        body.push('\n');
    }
    fs::write(path, body).expect("write benchmark transcript");
}

struct PhaseSnapshot {
    started: Instant,
    cpu_ticks: u64,
    process_write_bytes: u64,
    database_storage_bytes: u64,
}

impl PhaseSnapshot {
    fn start(database_storage_bytes: u64) -> Self {
        reset_peak_rss();
        let cpu_ticks = process_cpu_ticks();
        let process_write_bytes = process_write_bytes();
        Self {
            started: Instant::now(),
            cpu_ticks,
            process_write_bytes,
            database_storage_bytes,
        }
    }

    fn finish(
        self,
        database_storage_bytes: u64,
        repetition: usize,
        replayed_observations: usize,
    ) -> RawPhaseSample {
        let latency_ns = elapsed_ns(self.started);
        RawPhaseSample {
            repetition,
            latency_ns,
            cpu_ticks: process_cpu_ticks().saturating_sub(self.cpu_ticks),
            process_write_bytes: process_write_bytes().saturating_sub(self.process_write_bytes),
            database_storage_growth_bytes: database_storage_bytes
                .saturating_sub(self.database_storage_bytes),
            peak_rss_kib: process_peak_rss_kib(),
            replayed_observations,
        }
    }

    fn finish_provider(
        self,
        database_storage_bytes: u64,
        repetition: usize,
        record_count: usize,
    ) -> RawProviderPhaseSample {
        let latency_ns = elapsed_ns(self.started);
        RawProviderPhaseSample {
            repetition,
            latency_ns,
            cpu_ticks: process_cpu_ticks().saturating_sub(self.cpu_ticks),
            process_write_bytes: process_write_bytes().saturating_sub(self.process_write_bytes),
            database_storage_growth_bytes: database_storage_bytes
                .saturating_sub(self.database_storage_bytes),
            peak_rss_kib: process_peak_rss_kib(),
            record_count,
        }
    }
}

const PROVIDER_RESULT_SCHEMA_VERSION: u32 = 1;
const PROVIDER_REPLAY_LIMIT: usize = baseline::PROVIDER_RECORDS_PER_REPETITION + 1;
/// Fixture enrollment id written into the repository identity marker once.
/// Runtime `ProjectId` always comes from that immutable marker, never from paths or labels.
const PROVIDER_BENCHMARK_PROJECT_ID: &str = "proj_benchmark_provider";

#[derive(Clone, Copy, Debug)]
enum ProviderKind {
    Claude,
    Codex,
    Cursor,
    Hermes,
    Kiro,
    Cline,
    RooCode,
    Kilo,
}

impl ProviderKind {
    const ALL: [Self; 8] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Hermes,
        Self::Kiro,
        Self::Cline,
        Self::RooCode,
        Self::Kilo,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Hermes => "hermes",
            Self::Kiro => "kiro",
            Self::Cline => "cline",
            Self::RooCode => "roo-code",
            Self::Kilo => "kilo",
        }
    }

    fn production_path(self) -> String {
        format!("{}_production_observation_pipeline_v1", self.id())
    }
}

fn enroll_provider_benchmark_project(project: &Path) -> ProjectId {
    let status = std::process::Command::new("git")
        .args(["-C"])
        .arg(project)
        .arg("init")
        .status()
        .expect("git init provider benchmark project");
    assert!(
        status.success(),
        "git init provider benchmark project failed"
    );
    assert!(
        write_repository_identity_marker(project, PROVIDER_BENCHMARK_PROJECT_ID)
            .expect("write provider benchmark repository identity marker"),
        "provider benchmark repository identity marker must be written"
    );
    let marker = read_repository_identity_marker(project)
        .expect("read provider benchmark repository identity marker")
        .expect("provider benchmark repository identity marker must exist");
    ProjectId::new(marker.project_id).expect("valid provider benchmark ProjectId")
}

struct ProviderFixture {
    kind: ProviderKind,
    home: PathBuf,
    profile: PathBuf,
    project: PathBuf,
    project_id: ProjectId,
    source_path: PathBuf,
    runtime: HostAdmissionTestRuntimeV1,
    _temp: TempDir,
}

impl ProviderFixture {
    async fn new(kind: ProviderKind, repetition: usize) -> Self {
        let temp = benchmark_tempdir("provider-");
        let home = temp.path().join("home");
        let profile = home.join(".tracedecay");
        let project = temp.path().join("project");
        fs::create_dir_all(&home).expect("create provider benchmark home");
        fs::create_dir_all(&profile).expect("create provider benchmark profile");
        fs::create_dir_all(&project).expect("create provider benchmark project");
        let project_id = enroll_provider_benchmark_project(&project);
        let source_path =
            write_provider_fixture(kind, temp.path(), &home, &project, repetition).await;
        let runtime = if matches!(kind, ProviderKind::Claude) {
            HostAdmissionTestRuntimeV1::profile(&profile)
                .await
                .expect("open registered profile benchmark runtime")
        } else {
            HostAdmissionTestRuntimeV1::project(&profile, &project, project_id.clone())
                .await
                .expect("open registered project benchmark runtime")
        };
        Self {
            kind,
            home,
            profile,
            project,
            project_id,
            source_path,
            runtime,
            _temp: temp,
        }
    }

    fn scope(&self) -> ObservationScopeV1 {
        if matches!(self.kind, ProviderKind::Claude) {
            ObservationScopeV1::Profile
        } else {
            ObservationScopeV1::Project {
                project_id: self.project_id(),
            }
        }
    }

    fn project_id(&self) -> ProjectId {
        self.project_id.clone()
    }

    fn database_storage_bytes(&self) -> u64 {
        let scope = if matches!(self.kind, ProviderKind::Claude) {
            HostAdmissionScope::Profile
        } else {
            HostAdmissionScope::Project
        };
        self.runtime
            .session_database_storage_bytes_for_test(scope)
            .expect("measure registered provider database storage")
    }

    async fn parse_native_fixture(&self) -> usize {
        match self.kind {
            ProviderKind::Claude | ProviderKind::Codex | ProviderKind::Cursor => {
                fs::read_to_string(&self.source_path)
                    .expect("read provider JSONL fixture")
                    .lines()
                    .fold(0, |count, line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .expect("decode provider JSONL record");
                        count + 1
                    })
            }
            ProviderKind::Hermes => {
                let state_db = self.source_path.join("profiles/benchmark/state.db");
                let connection = open_immutable_read_only(&state_db)
                    .expect("open immutable Hermes parse-phase fixture database");
                let count = connection
                    .query_row("SELECT COUNT(*) FROM messages", (), |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("count Hermes parse-phase records");
                usize::try_from(count).expect("Hermes parse-phase count fits usize")
            }
            ProviderKind::Kiro => serde_json::from_slice::<serde_json::Value>(
                &fs::read(&self.source_path).expect("read Kiro provider fixture"),
            )
            .expect("decode Kiro provider fixture")
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .expect("Kiro provider fixture messages")
            .len(),
            ProviderKind::Cline | ProviderKind::RooCode | ProviderKind::Kilo => {
                serde_json::from_slice::<Vec<serde_json::Value>>(
                    &fs::read(&self.source_path).expect("read Cline-family provider fixture"),
                )
                .expect("decode Cline-family provider fixture")
                .len()
            }
        }
    }

    async fn ingest(&self) -> u64 {
        let scope = self.scope();
        let cancellation = ObservationCancellation::default();
        let admission = self.runtime.facade();
        let adapter_work = match self.kind {
            ProviderKind::Claude => {
                let session_id = self
                    .source_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .expect("Claude provider benchmark session id");
                let source = ClaudeSource::with_home(&self.home)
                    .for_user_scope(Some(session_id.to_string()), Vec::new());
                let stats = ingest_source_with_observations_with_admission(
                    &source,
                    &self.profile,
                    scope.clone(),
                    &admission,
                    None,
                    cancellation.clone(),
                )
                .await
                .expect("run Claude production provider path");
                stats.observations_committed + stats.projections_completed
            }
            ProviderKind::Codex => {
                codex::try_admit_codex_jsonl_observations_for_project_with_admission(
                    &self.source_path,
                    &self.project,
                    self.project_id(),
                    &admission,
                    None,
                )
                .await
                .expect("run Codex production provider path");
                0
            }
            ProviderKind::Cursor => {
                let event = json!({
                    "session_id": "benchmark-cursor-session",
                    "conversation_id": "benchmark-cursor-conversation",
                    "transcript_path": self.source_path,
                    "cwd": self.project,
                    "workspace_roots": [self.project],
                    "model": "benchmark-model"
                });
                let stats = cursor::try_ingest_cursor_transcript_event_capped_with_admission(
                    &event.to_string(),
                    self.project_id(),
                    &admission,
                    None,
                )
                .await
                .expect("run Cursor production provider path");
                stats.messages_upserted
            }
            ProviderKind::Hermes => {
                let stats = hermes::ingest_homes_capped_with_admission(
                    std::slice::from_ref(&self.source_path),
                    &self.project,
                    self.project_id(),
                    &admission,
                    None,
                )
                .await;
                stats.stats.messages_upserted
            }
            ProviderKind::Kiro => {
                let source = kiro::KiroSource::with_home(&self.home);
                kiro::capture_kiro_snapshot_observations(
                    &admission,
                    &source,
                    &self.project,
                    scope.clone(),
                    None,
                    &cancellation,
                )
                .await
                .expect("run Kiro production provider path")
                .stats
                .messages_upserted
            }
            ProviderKind::Cline | ProviderKind::RooCode | ProviderKind::Kilo => {
                let source = match self.kind {
                    ProviderKind::Cline => ClineLikeSource::cline_with_home(&self.home),
                    ProviderKind::RooCode => ClineLikeSource::roo_code_with_home(&self.home),
                    ProviderKind::Kilo => ClineLikeSource::kilo_with_home(&self.home),
                    _ => unreachable!("Cline-family match is exhaustive"),
                };
                capture_cline_like_snapshot_observations(
                    &admission,
                    &source,
                    &self.project,
                    scope.clone(),
                    None,
                    &cancellation,
                )
                .await
                .expect("run Cline-family production provider path")
                .stats
                .messages_upserted
            }
        };
        let projection = admission
            .drain_projection_queue(self.kind.id(), &scope, &cancellation, PROVIDER_REPLAY_LIMIT)
            .await
            .expect("drain provider benchmark projection queue");
        adapter_work + projection.projected
    }

    async fn replay(&self) -> Vec<StoredObservation> {
        self.replay_after(0, PROVIDER_REPLAY_LIMIT).await
    }

    async fn replay_after(&self, sequence: u64, limit: usize) -> Vec<StoredObservation> {
        self.runtime
            .replay_observations(
                match self.scope() {
                    ObservationScopeV1::Project { .. } => HostAdmissionScope::Project,
                    ObservationScopeV1::Profile => HostAdmissionScope::Profile,
                },
                ObservationReplayRequest::new(sequence, limit)
                    .expect("bounded provider replay request"),
            )
            .await
            .expect("replay provider benchmark observations")
    }

    fn verify_redacted(&self, observations: &[StoredObservation]) {
        assert!(
            !observations.is_empty(),
            "{} production path committed no observations",
            self.kind.id()
        );
        assert!(
            observations.len() < PROVIDER_REPLAY_LIMIT,
            "{} exceeded bounded replay sentinel",
            self.kind.id()
        );
        for observation in observations {
            let payload = observation.observation().payload().to_string();
            assert!(
                !payload.contains(BENCHMARK_SECRET_PREFIX),
                "{} retained the secret canary",
                self.kind.id()
            );
        }
    }
}

pub(super) async fn exercise_provider_paths_once() -> Vec<String> {
    let mut executed = Vec::with_capacity(ProviderKind::ALL.len());
    for (index, kind) in ProviderKind::ALL.into_iter().enumerate() {
        let fixture = ProviderFixture::new(kind, 20_000 + index).await;
        assert!(
            fixture.ingest().await > 0,
            "{} production path reported no work",
            kind.id()
        );
        let observations = fixture.replay().await;
        fixture.verify_redacted(&observations);
        assert_eq!(
            observations.len(),
            baseline::PROVIDER_RECORDS_PER_REPETITION,
            "{} fixture did not produce the bounded baseline backlog",
            kind.id()
        );
        let end = observations
            .last()
            .expect("provider replay has durable end")
            .sequence();
        let no_op_work = fixture.ingest().await;
        assert_eq!(no_op_work, 0, "{} repeat ingest was not a no-op", kind.id());
        assert!(
            fixture.replay_after(end, 1).await.is_empty(),
            "{} repeat ingest created an observation",
            kind.id()
        );
        assert_eq!(
            fixture.replay().await.len(),
            observations.len(),
            "{} repeat ingest changed observation cardinality",
            kind.id()
        );
        executed.push(kind.id().to_string());
    }
    executed
}

struct ProviderSamples {
    kind: ProviderKind,
    parse: Vec<RawProviderPhaseSample>,
    commit: Vec<RawProviderPhaseSample>,
    replay: Vec<RawProviderPhaseSample>,
    pipeline: Vec<RawPhaseSample>,
    no_op: Vec<RawPhaseSample>,
}

impl ProviderSamples {
    fn new(kind: ProviderKind, repetitions: usize) -> Self {
        Self {
            kind,
            parse: Vec::with_capacity(repetitions),
            commit: Vec::with_capacity(repetitions),
            replay: Vec::with_capacity(repetitions),
            pipeline: Vec::with_capacity(repetitions),
            no_op: Vec::with_capacity(repetitions),
        }
    }

    async fn measure_turn(&mut self, repetition: usize, fixture_id: usize) {
        let fixture = ProviderFixture::new(self.kind, fixture_id).await;
        let parse = PhaseSnapshot::start(fixture.database_storage_bytes());
        let parsed_records = fixture.parse_native_fixture().await;
        self.parse.push(parse.finish_provider(
            fixture.database_storage_bytes(),
            repetition,
            parsed_records,
        ));
        assert_eq!(
            parsed_records,
            baseline::PROVIDER_RECORDS_PER_REPETITION,
            "{} native parse phase did not decode the bounded fixture",
            self.kind.id()
        );
        let pipeline = PhaseSnapshot::start(fixture.database_storage_bytes());
        let commit = PhaseSnapshot::start(fixture.database_storage_bytes());
        assert!(
            fixture.ingest().await > 0,
            "{} production path reported no measured work",
            self.kind.id()
        );
        self.commit.push(commit.finish_provider(
            fixture.database_storage_bytes(),
            repetition,
            parsed_records,
        ));
        let replay = PhaseSnapshot::start(fixture.database_storage_bytes());
        let observations = fixture.replay().await;
        let observation_count = observations.len();
        self.replay.push(replay.finish_provider(
            fixture.database_storage_bytes(),
            repetition,
            observation_count,
        ));
        self.pipeline.push(pipeline.finish(
            fixture.database_storage_bytes(),
            repetition,
            observation_count,
        ));
        fixture.verify_redacted(&observations);
        assert_eq!(
            observation_count,
            baseline::PROVIDER_RECORDS_PER_REPETITION,
            "{} fixture did not produce the bounded baseline backlog",
            self.kind.id()
        );
        let end = observations
            .last()
            .expect("provider replay has durable end")
            .sequence();
        let no_op = PhaseSnapshot::start(fixture.database_storage_bytes());
        let no_op_work = fixture.ingest().await;
        let after_end = fixture.replay_after(end, 1).await;
        self.no_op.push(no_op.finish(
            fixture.database_storage_bytes(),
            repetition,
            after_end.len(),
        ));
        assert_eq!(
            no_op_work,
            0,
            "{} measured repeat ingest was not a no-op",
            self.kind.id()
        );
        assert_eq!(
            fixture.replay().await.len(),
            observation_count,
            "{} no-op changed observation count",
            self.kind.id()
        );
    }

    fn finish(self, repetitions: usize, clock_ticks_per_second: u64) -> ProviderBenchmarkResult {
        assert!(
            self.no_op.iter().all(|sample| {
                sample.process_write_bytes == 0
                    && sample.database_storage_growth_bytes == 0
                    && sample.replayed_observations == 0
            }),
            "{} no-op performed durable work",
            self.kind.id()
        );
        let pipeline_latencies = self
            .pipeline
            .iter()
            .map(|sample| sample.latency_ns)
            .collect::<Vec<_>>();
        let no_op_latencies = self
            .no_op
            .iter()
            .map(|sample| sample.latency_ns)
            .collect::<Vec<_>>();
        let pipeline_totals = aggregate_samples(&self.pipeline);
        let no_op_totals = aggregate_samples(&self.no_op);
        let observations_per_repetition = baseline::PROVIDER_RECORDS_PER_REPETITION;
        let total_records = observations_per_repetition * repetitions;
        let total_pipeline_ns = pipeline_latencies.iter().sum::<u64>();
        ProviderBenchmarkResult {
            provider: self.kind.id().to_string(),
            production_path: self.kind.production_path(),
            pipeline_scope: PROVIDER_PIPELINE_SCOPE.to_string(),
            measured_repetitions: repetitions,
            observations_per_repetition,
            replay_limit: PROVIDER_REPLAY_LIMIT,
            max_backlog_records: observations_per_repetition,
            parse: provider_phase_result(PROVIDER_PARSE_SCOPE, self.parse, clock_ticks_per_second),
            commit: provider_phase_result(
                PROVIDER_COMMIT_SCOPE,
                self.commit,
                clock_ticks_per_second,
            ),
            replay: provider_phase_result(
                PROVIDER_REPLAY_SCOPE,
                self.replay,
                clock_ticks_per_second,
            ),
            pipeline_raw_samples: self.pipeline,
            pipeline_latency: Distribution::from_samples(&pipeline_latencies),
            pipeline_records_per_second: total_records as f64 * 1_000_000_000.0
                / total_pipeline_ns as f64,
            pipeline_cpu_ticks: pipeline_totals.cpu_ticks,
            pipeline_cpu_ms: ticks_to_ms(pipeline_totals.cpu_ticks, clock_ticks_per_second),
            pipeline_process_write_bytes: pipeline_totals.process_write_bytes,
            pipeline_database_storage_growth_bytes: pipeline_totals.database_storage_growth_bytes,
            peak_rss_kib: pipeline_totals.peak_rss_kib.max(no_op_totals.peak_rss_kib),
            no_op_raw_samples: self.no_op,
            no_op_latency: Distribution::from_samples(&no_op_latencies),
            no_op_cpu_ticks: no_op_totals.cpu_ticks,
            no_op_cpu_ms: ticks_to_ms(no_op_totals.cpu_ticks, clock_ticks_per_second),
            no_op_process_write_bytes: no_op_totals.process_write_bytes,
            no_op_database_storage_growth_bytes: no_op_totals.database_storage_growth_bytes,
            no_op_observation_count_delta: 0,
        }
    }
}

async fn run_provider_benchmark_suite(
    repetitions: usize,
    clock_ticks_per_second: u64,
) -> ProviderBenchmarkSuiteResult {
    let mut samples = ProviderKind::ALL.map(|kind| ProviderSamples::new(kind, repetitions));
    let mut turns = Vec::with_capacity(repetitions * ProviderKind::ALL.len());
    for repetition in 0..repetitions {
        for (position, provider) in samples.iter_mut().enumerate() {
            turns.push(ProviderScheduleTurn {
                round: repetition,
                position,
                provider: provider.kind.id().to_string(),
            });
            provider
                .measure_turn(
                    repetition,
                    30_000 + repetition * ProviderKind::ALL.len() + position,
                )
                .await;
        }
    }
    let providers = samples
        .into_iter()
        .map(|samples| samples.finish(repetitions, clock_ticks_per_second))
        .collect();
    ProviderBenchmarkSuiteResult {
        schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
        workload_id: WORKLOAD_ID.to_string(),
        fairness: ProviderFairnessResult {
            policy: "round_robin_v1".to_string(),
            rounds: repetitions,
            providers_per_round: ProviderKind::ALL.len(),
            max_provider_turn_distance: ProviderKind::ALL.len(),
            turns,
        },
        providers,
    }
}

fn provider_phase_result(
    scope: &str,
    raw_samples: Vec<RawProviderPhaseSample>,
    clock_ticks_per_second: u64,
) -> ProviderPhaseResult {
    let latencies = raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let cpu_ticks: u64 = raw_samples.iter().map(|sample| sample.cpu_ticks).sum();
    let process_write_bytes: u64 = raw_samples
        .iter()
        .map(|sample| sample.process_write_bytes)
        .sum();
    let database_storage_growth_bytes: u64 = raw_samples
        .iter()
        .map(|sample| sample.database_storage_growth_bytes)
        .sum();
    let peak_rss_kib = raw_samples
        .iter()
        .map(|sample| sample.peak_rss_kib)
        .max()
        .expect("provider phase samples");
    ProviderPhaseResult {
        scope: scope.to_string(),
        latency: Distribution::from_samples(&latencies),
        cpu_ticks,
        cpu_ms: ticks_to_ms(cpu_ticks, clock_ticks_per_second),
        process_write_bytes,
        database_storage_growth_bytes,
        peak_rss_kib,
        raw_samples,
    }
}

fn native_fixture(path: &str) -> serde_json::Value {
    let source = NATIVE_PROVIDER_FIXTURES
        .iter()
        .find_map(|(candidate, source)| (*candidate == path).then_some(*source))
        .unwrap_or_else(|| panic!("native provider fixture is not attested: {path}"));
    serde_json::from_str(source)
        .unwrap_or_else(|error| panic!("parse native fixture {path}: {error}"))
}

fn benchmark_canary(repetition: usize, index: usize) -> String {
    format!(
        "{BENCHMARK_SECRET_PREFIX}{:06}",
        (repetition * baseline::PROVIDER_RECORDS_PER_REPETITION + index) % 1_000_000
    )
}

fn inject_canary(content: &mut serde_json::Value, canary: &str) {
    if let Some(text) = content.as_str() {
        *content = json!(format!("{text} {canary}"));
        return;
    }
    let blocks = content
        .as_array_mut()
        .expect("native fixture content must be text or blocks");
    let text = blocks
        .iter_mut()
        .find(|block| block["type"] == "text")
        .and_then(|block| block.get_mut("text"))
        .expect("native fixture content has no text block");
    let original = text.as_str().expect("native fixture text block");
    *text = json!(format!("{original} {canary}"));
}

async fn write_provider_fixture(
    kind: ProviderKind,
    root: &Path,
    home: &Path,
    project: &Path,
    repetition: usize,
) -> PathBuf {
    match kind {
        ProviderKind::Claude => write_provider_claude(home, repetition),
        ProviderKind::Codex => write_provider_codex(home, project, repetition),
        ProviderKind::Cursor => write_provider_cursor(root, repetition),
        ProviderKind::Hermes => write_provider_hermes(home, project, repetition).await,
        ProviderKind::Kiro => write_provider_kiro(home, project, repetition),
        ProviderKind::Cline | ProviderKind::RooCode | ProviderKind::Kilo => {
            write_provider_cline_like(kind, home, project, repetition)
        }
    }
}

fn write_provider_claude(home: &Path, repetition: usize) -> PathBuf {
    let session_id = format!("benchmark-claude-session-{repetition}");
    let path = home
        .join(".claude/projects/provider-benchmark")
        .join(format!("{session_id}.jsonl"));
    fs::create_dir_all(path.parent().expect("Claude provider fixture parent"))
        .expect("create Claude provider fixture");
    let template = native_fixture(
        "tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json",
    );
    write_provider_jsonl(&path, |index| {
        let mut record = template.clone();
        record["sessionId"] = json!(session_id);
        record["uuid"] = json!(format!("benchmark-claude-message-{index}"));
        record["cwd"] = json!(path.parent());
        record["message"]["id"] = json!(format!("benchmark-claude-native-{index}"));
        inject_canary(
            &mut record["message"]["content"],
            &benchmark_canary(repetition, index),
        );
        record
    });
    path
}

fn write_provider_codex(home: &Path, project: &Path, repetition: usize) -> PathBuf {
    let session_id = format!("benchmark-codex-session-{repetition}");
    let directory = home.join(".codex/sessions/2026/07/15");
    fs::create_dir_all(&directory).expect("create Codex provider fixture");
    let path = directory.join(format!("rollout-{session_id}.jsonl"));
    let mut session =
        native_fixture("tests/fixtures/provider_normalization/codex/session_meta.input.json");
    session["payload"]["id"] = json!(session_id);
    session["payload"]["cwd"] = json!(project);
    let mut lines = vec![session];
    let message_template =
        native_fixture("tests/fixtures/provider_normalization/codex/agent_message.input.json");
    for index in 0..baseline::PROVIDER_RECORDS_PER_REPETITION - 1 {
        let mut record = message_template.clone();
        inject_canary(
            &mut record["payload"]["message"],
            &benchmark_canary(repetition, index),
        );
        lines.push(record);
    }
    write_jsonl_values(&path, &lines);
    path
}

fn write_provider_cursor(root: &Path, repetition: usize) -> PathBuf {
    let path = root.join(format!("benchmark-cursor-session-{repetition}.jsonl"));
    let template =
        native_fixture("tests/fixtures/provider_normalization/cursor/tool_use.input.json");
    write_provider_jsonl(&path, |index| {
        let mut record = template.clone();
        inject_canary(
            &mut record["message"]["content"],
            &benchmark_canary(repetition, index),
        );
        record
    });
    path
}

async fn write_provider_hermes(home: &Path, project: &Path, repetition: usize) -> PathBuf {
    let template = native_fixture(
        "tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json",
    );
    let hermes_home = home.join(".hermes");
    let profile = hermes_home.join("profiles/benchmark");
    fs::create_dir_all(&profile).expect("create Hermes provider fixture");
    let pin = serde_json::to_string(project.to_string_lossy().as_ref())
        .expect("encode Hermes project pin");
    fs::write(
        profile.join("config.yaml"),
        format!("plugins:\n  tracedecay:\n    project_root: {pin}\n"),
    )
    .expect("write Hermes provider config");
    let state_db = profile.join("state.db");
    let connection =
        rusqlite::Connection::open(&state_db).expect("open Hermes provider fixture database");
    connection
        .execute(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT,
                parent_session_id TEXT, started_at REAL NOT NULL, cwd TEXT,
                input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
                reasoning_tokens INTEGER DEFAULT 0, model_config TEXT
            )",
            (),
        )
        .expect("create Hermes sessions table");
    connection
        .execute(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                role TEXT NOT NULL, content TEXT, tool_calls TEXT, tool_name TEXT,
                timestamp REAL NOT NULL
            )",
            (),
        )
        .expect("create Hermes messages table");
    let session_id = format!("benchmark-hermes-session-{repetition}");
    connection
        .execute(
            "INSERT INTO sessions
             (id, source, model, started_at, cwd, model_config,
              input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens)
             VALUES (?1, 'benchmark', ?2, ?3, ?4, '{}', ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                session_id.clone(),
                template["session_model"]
                    .as_str()
                    .expect("Hermes fixture model"),
                template["timestamp"]
                    .as_f64()
                    .expect("Hermes fixture timestamp"),
                project.to_string_lossy().as_ref(),
                template["session_input_tokens"]
                    .as_i64()
                    .expect("Hermes input tokens"),
                template["session_output_tokens"]
                    .as_i64()
                    .expect("Hermes output tokens"),
                template["session_cache_read_tokens"]
                    .as_i64()
                    .expect("Hermes cache read"),
                template["session_cache_write_tokens"]
                    .as_i64()
                    .expect("Hermes cache write"),
                template["session_reasoning_tokens"]
                    .as_i64()
                    .expect("Hermes reasoning tokens")
            ],
        )
        .expect("insert Hermes session");
    for index in 0..baseline::PROVIDER_RECORDS_PER_REPETITION {
        let mut tool_calls = template["tool_calls"].clone();
        let arguments = tool_calls[0]["function"]["arguments"]
            .as_str()
            .expect("Hermes fixture tool arguments");
        tool_calls[0]["function"]["arguments"] = json!(format!(
            "{arguments} {}",
            benchmark_canary(repetition, index)
        ));
        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, tool_calls, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    session_id.clone(),
                    template["role"].as_str().expect("Hermes fixture role"),
                    template["content"]
                        .as_str()
                        .expect("Hermes fixture content"),
                    tool_calls.to_string(),
                    template["timestamp"]
                        .as_f64()
                        .expect("Hermes fixture timestamp")
                        + index as f64
                ],
            )
            .expect("insert Hermes message");
    }
    drop(connection);
    hermes_home
}

fn write_provider_kiro(home: &Path, project: &Path, repetition: usize) -> PathBuf {
    let data_dir = tracedecay_sessions::host_ports::kiro_data_dir(home);
    let workspace_hash = "0123456789abcdef0123456789abcdef";
    let workspace_storage = data_dir.join("User/workspaceStorage").join(workspace_hash);
    fs::create_dir_all(&workspace_storage).expect("create Kiro workspace fixture");
    fs::write(
        workspace_storage.join("workspace.json"),
        json!({"folder": format!("file://{}", project.display())}).to_string(),
    )
    .expect("write Kiro workspace metadata");
    let execution_dir = data_dir
        .join("User/globalStorage/kiro.kiroagent")
        .join(workspace_hash);
    fs::create_dir_all(&execution_dir).expect("create Kiro execution fixture");
    let path = execution_dir.join(format!("benchmark-kiro-session-{repetition}"));
    let mut template =
        native_fixture("tests/fixtures/provider_normalization/kiro/workspace_session.input.json");
    let native_messages = template["messages"]
        .as_array()
        .expect("native Kiro messages")
        .clone();
    let messages = (0..baseline::PROVIDER_RECORDS_PER_REPETITION)
        .map(|index| {
            let mut message = native_messages[index % native_messages.len()].clone();
            inject_canary(
                &mut message["content"],
                &benchmark_canary(repetition, index),
            );
            message["timestamp"] = json!(1_784_073_600_000_i64 + index as i64);
            message
        })
        .collect::<Vec<_>>();
    template["sessionId"] = json!(format!("benchmark-kiro-session-{repetition}"));
    template["messages"] = json!(messages);
    fs::write(&path, template.to_string()).expect("write Kiro provider fixture");
    path
}

fn write_provider_cline_like(
    kind: ProviderKind,
    home: &Path,
    project: &Path,
    repetition: usize,
) -> PathBuf {
    let (extension, task_id) = match kind {
        ProviderKind::Cline => ("saoudrizwan.claude-dev", "benchmark-cline-session"),
        ProviderKind::RooCode => ("rooveterinaryinc.roo-cline", "benchmark-roo-code-session"),
        ProviderKind::Kilo => ("kilocode.kilo-code", "benchmark-kilo-session"),
        _ => unreachable!("Cline-family fixture kind"),
    };
    let task_dir = tracedecay_sessions::host_ports::vscode_data_dir(home)
        .join("User/globalStorage")
        .join(extension)
        .join("tasks")
        .join(format!("{task_id}-{repetition}"));
    fs::create_dir_all(&task_dir).expect("create Cline-family provider fixture");
    let mut task_metadata =
        native_fixture("tests/fixtures/transcript_golden/cline_like/input/task_metadata.json");
    task_metadata["workspacePath"] = json!(project);
    fs::write(
        task_dir.join("task_metadata.json"),
        task_metadata.to_string(),
    )
    .expect("write Cline-family task metadata");
    let native_messages = native_fixture(
        "tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json",
    );
    let native_messages = native_messages
        .as_array()
        .expect("native Cline-family messages");
    let messages = (0..baseline::PROVIDER_RECORDS_PER_REPETITION)
        .map(|index| {
            let mut message = native_messages[index % native_messages.len()].clone();
            inject_canary(
                &mut message["content"],
                &benchmark_canary(repetition, index),
            );
            message["ts"] = json!(1_784_073_600_i64 + index as i64);
            message
        })
        .collect::<Vec<_>>();
    let path = task_dir.join("api_conversation_history.json");
    fs::write(
        &path,
        serde_json::to_vec(&messages).expect("serialize Cline-family messages"),
    )
    .expect("write Cline-family provider fixture");
    path
}

fn write_provider_jsonl(path: &Path, record: impl Fn(usize) -> serde_json::Value) {
    let values = (0..baseline::PROVIDER_RECORDS_PER_REPETITION)
        .map(record)
        .collect::<Vec<_>>();
    write_jsonl_values(path, &values);
}

fn write_jsonl_values(path: &Path, values: &[serde_json::Value]) {
    let mut body = String::new();
    for value in values {
        body.push_str(&value.to_string());
        body.push('\n');
    }
    fs::write(path, body).expect("write provider JSONL fixture");
}

pub(super) async fn run() {
    manifest::validate();
    let clock_ticks_per_second = preflight_platform();
    let git_before = git_snapshot();
    assert!(
        !git_before.dirty,
        "benchmark evidence requires a clean worktree before execution"
    );
    let identity_before = workload_identity();
    let attested_build = attest_build(&git_before);
    for repetition in 0..WARMUP_REPETITIONS {
        let fixture = Fixture::new(repetition).await;
        let source = fixture.source();
        let stats = fixture.ingest(&source).await;
        assert_eq!(
            stats.observations_committed as usize, RECORDS_PER_REPETITION,
            "unexpected warmup ingest counters: {stats:?}"
        );
        assert_eq!(stats.projections_completed as usize, RECORDS_PER_REPETITION);
        assert_eq!(stats.transcript.sessions_upserted, 1);
        assert_eq!(
            stats.transcript.messages_upserted as usize,
            RECORDS_PER_REPETITION
        );
        let observations = fixture.replay().await;
        fixture.verify_committed_state(&observations).await;
    }

    let mut pipeline_raw_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    let mut no_op_raw_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    let mut no_op_observation_count_delta = 0_i64;
    let mut no_op_totals = NoOpTotals::default();

    for repetition in 0..MEASURED_REPETITIONS {
        let fixture = Fixture::new(WARMUP_REPETITIONS + repetition).await;
        let source = fixture.source();
        let pipeline_phase = PhaseSnapshot::start(fixture.database_storage_bytes());
        let stats = fixture.ingest(&source).await;
        let observations = fixture.replay().await;
        let replayed = observations.len();
        let pipeline_sample =
            pipeline_phase.finish(fixture.database_storage_bytes(), repetition, replayed);
        fixture.verify_committed_state(&observations).await;
        pipeline_raw_samples.push(pipeline_sample);
        assert_eq!(
            stats.observations_committed as usize, RECORDS_PER_REPETITION,
            "unexpected measured ingest counters: {stats:?}"
        );
        assert_eq!(stats.projections_completed as usize, RECORDS_PER_REPETITION);
        assert_eq!(stats.transcript.sessions_upserted, 1);
        assert_eq!(
            stats.transcript.messages_upserted as usize,
            RECORDS_PER_REPETITION
        );
        assert_eq!(replayed, RECORDS_PER_REPETITION);
        let durable_end_sequence = observations
            .last()
            .expect("pipeline replay must establish a durable end cursor")
            .sequence();

        let no_op_phase = PhaseSnapshot::start(fixture.database_storage_bytes());
        let no_op_stats = fixture.ingest(&source).await;
        let observations_after_end = fixture.replay_after(durable_end_sequence, 1).await;
        let no_op_sample = no_op_phase.finish(
            fixture.database_storage_bytes(),
            repetition,
            observations_after_end.len(),
        );
        let observations_after_no_op = fixture.replay().await;
        fixture
            .verify_committed_state(&observations_after_no_op)
            .await;
        no_op_observation_count_delta += i64::try_from(observations_after_no_op.len())
            .expect("bounded replay count fits i64")
            - i64::try_from(replayed).expect("bounded replay count fits i64");
        no_op_totals.add(no_op_stats);
        no_op_raw_samples.push(no_op_sample);
    }

    validate_no_op_invariants(
        &no_op_raw_samples,
        no_op_observation_count_delta,
        &no_op_totals,
    )
    .expect("exact replay must be a durable no-op");
    let pipeline_samples = pipeline_raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let no_op_samples = no_op_raw_samples
        .iter()
        .map(|sample| sample.latency_ns)
        .collect::<Vec<_>>();
    let total_pipeline_ns = pipeline_samples.iter().sum::<u64>();
    let measured_records = MEASURED_REPETITIONS * RECORDS_PER_REPETITION;
    let pipeline_totals = aggregate_samples(&pipeline_raw_samples);
    let no_op_totals_metrics = aggregate_samples(&no_op_raw_samples);
    let provider_result =
        run_provider_benchmark_suite(MEASURED_REPETITIONS, clock_ticks_per_second).await;
    assert_eq!(
        provider_result.providers.len(),
        baseline::PROVIDERS.len(),
        "provider result omitted a supported provider"
    );
    let git_after = git_snapshot();
    validate_git_snapshots(&git_before, &git_after)
        .expect("benchmark Git snapshots must remain clean and identical");
    assert_eq!(
        workload_identity(),
        identity_before,
        "manifest or harness source changed during benchmark execution"
    );
    let result = BenchmarkResult {
        schema_version: RESULT_SCHEMA_VERSION,
        workload_id: WORKLOAD_ID.to_string(),
        evidence_status: attested_build.evidence_status,
        workload_identity: identity_before,
        build_identity: attested_build.build_identity,
        git_before,
        git_after,
        command: BENCHMARK_COMMAND.to_string(),
        rustc: command_output("rustc", &["-Vv"]),
        cargo: command_output("cargo", &["-V"]),
        kernel: command_output("uname", &["-srmo"]),
        cpu_identity: cpu_identity(),
        logical_cpu_count: std::thread::available_parallelism()
            .expect("available logical CPUs")
            .get(),
        memory_total_kib: memory_total_kib(),
        clock_ticks_per_second,
        warmup_repetitions: WARMUP_REPETITIONS,
        measured_repetitions: MEASURED_REPETITIONS,
        records_per_repetition: RECORDS_PER_REPETITION,
        measured_records,
        pipeline_raw_samples,
        pipeline_batch_latency: Distribution::from_samples(&pipeline_samples),
        pipeline_records_per_second: measured_records as f64 * 1_000_000_000.0
            / total_pipeline_ns as f64,
        pipeline_cpu_ticks: pipeline_totals.cpu_ticks,
        pipeline_cpu_ms: ticks_to_ms(pipeline_totals.cpu_ticks, clock_ticks_per_second),
        pipeline_process_write_bytes: pipeline_totals.process_write_bytes,
        database_storage_growth_bytes: pipeline_totals.database_storage_growth_bytes,
        peak_rss_kib: pipeline_totals
            .peak_rss_kib
            .max(no_op_totals_metrics.peak_rss_kib),
        no_op_replay_raw_samples: no_op_raw_samples,
        no_op_replay_latency: Distribution::from_samples(&no_op_samples),
        no_op_replay_cpu_ticks: no_op_totals_metrics.cpu_ticks,
        no_op_replay_cpu_ms: ticks_to_ms(no_op_totals_metrics.cpu_ticks, clock_ticks_per_second),
        no_op_replay_process_write_bytes: no_op_totals_metrics.process_write_bytes,
        no_op_replay_database_storage_growth_bytes: no_op_totals_metrics
            .database_storage_growth_bytes,
        no_op_replay_observation_count_delta: no_op_observation_count_delta,
        no_op_replay_totals: no_op_totals,
        provider_observation_performance: Some(provider_result),
        hook_telemetry_readiness: Some(baseline::hook_telemetry_readiness()),
    };
    println!(
        "TRACEDECAY_PR5_BENCHMARK_RESULT={} ",
        serde_json::to_string(&result).expect("serialize benchmark result")
    );
    println!(
        "TRACEDECAY_PROVIDER_OBSERVATION_BASELINES={}",
        serde_json::to_string(&baseline::catalog()).expect("serialize provider baseline catalog")
    );
}
