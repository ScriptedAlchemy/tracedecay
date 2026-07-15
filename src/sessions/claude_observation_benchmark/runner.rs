use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::ObservationScopeV1;
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ObservationReplayRequest, ObservationStore,
    StoredObservation,
};

use crate::application::observation::ObservationCancellation;
use crate::sessions::claude::ClaudeSource;
use crate::sessions::claude_observation::{
    ClaudeObservationIngestStats, ingest_source_with_observations,
};
use crate::store::GlobalDbObservationStore;

use super::artifact::{
    attest_build, command_output, git_snapshot, validate_git_snapshots, workload_identity,
};
use super::manifest;
use super::metrics::{
    aggregate_samples, cpu_identity, database_storage_bytes, elapsed_ns, memory_total_kib,
    preflight_platform, process_cpu_ticks, process_peak_rss_kib, process_write_bytes,
    reset_peak_rss, ticks_to_ms, validate_no_op_invariants,
};
use super::model::{BenchmarkResult, Distribution, NoOpTotals, RawPhaseSample};
use super::{
    BENCHMARK_COMMAND, BENCHMARK_SECRET_PREFIX, MEASURED_REPETITIONS, RECORDS_PER_REPETITION,
    REDACTION_MARKER, RESULT_SCHEMA_VERSION, WARMUP_REPETITIONS, WORKLOAD_ID,
};

pub(super) struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    profile: PathBuf,
    pub(super) transcript: PathBuf,
    db_path: PathBuf,
    db: crate::global_db::GlobalDb,
}

impl Fixture {
    pub(super) async fn new(repetition: usize) -> Self {
        let temp = TempDir::new().expect("temporary benchmark fixture");
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
        let db_path = profile.join("sessions.db");
        let daemon_scope = crate::db::enter_daemon_database_scope(
            &profile,
            u64::try_from(repetition).expect("benchmark repetition fits u64") + 1,
            &session_id,
        )
        .expect("enter benchmark daemon database scope");
        let db = crate::global_db::GlobalDb::open_at(&db_path)
            .await
            .expect("open authoritative benchmark database");
        drop(daemon_scope);
        Self {
            _temp: temp,
            home,
            profile,
            transcript,
            db_path,
            db,
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
        ingest_source_with_observations(
            &self.db,
            source,
            &self.profile,
            ObservationScopeV1::Profile,
            None,
            ObservationCancellation::default(),
        )
        .await
        .expect("run production observation pipeline")
    }

    pub(super) async fn replay(&self) -> Vec<StoredObservation> {
        self.replay_after(0, RECORDS_PER_REPETITION + 1).await
    }

    async fn replay_after(&self, after_sequence: u64, limit: usize) -> Vec<StoredObservation> {
        GlobalDbObservationStore::new(&self.db)
            .replay_observations(
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
                payload.contains(REDACTION_MARKER),
                "authoritative observation payload lacks a redaction receipt marker"
            );

            let message_id = format!("benchmark-message-{index}");
            let message = self
                .db
                .get_session_message("claude", &message_id)
                .await
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
        self.verify_projector_only_v1_writes().await;
    }

    async fn verify_projector_only_v1_writes(&self) {
        let mut rows = self
            .db
            .conn()
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM session_messages WHERE provider = 'claude'),
                    COUNT(*),
                    COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND output_provider = 'claude'",
                libsql::params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .expect("count benchmark V1 projection ownership");
        let row = rows
            .next()
            .await
            .expect("read benchmark V1 projection ownership")
            .expect("benchmark V1 projection ownership row");
        let counts = (
            row.get::<i64>(0).expect("benchmark V1 message count"),
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
            "every benchmark V1 row must be created exactly once by the observation projector"
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

    fn finish(
        self,
        db_path: &Path,
        repetition: usize,
        replayed_observations: usize,
    ) -> RawPhaseSample {
        let latency_ns = elapsed_ns(self.started);
        RawPhaseSample {
            repetition,
            latency_ns,
            cpu_ticks: process_cpu_ticks().saturating_sub(self.cpu_ticks),
            process_write_bytes: process_write_bytes().saturating_sub(self.process_write_bytes),
            database_storage_growth_bytes: database_storage_bytes(db_path)
                .saturating_sub(self.database_storage_bytes),
            peak_rss_kib: process_peak_rss_kib(),
            replayed_observations,
        }
    }
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
        let pipeline_phase = PhaseSnapshot::start(&fixture.db_path);
        let stats = fixture.ingest(&source).await;
        let observations = fixture.replay().await;
        let replayed = observations.len();
        let pipeline_sample = pipeline_phase.finish(&fixture.db_path, repetition, replayed);
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

        let no_op_phase = PhaseSnapshot::start(&fixture.db_path);
        let no_op_stats = fixture.ingest(&source).await;
        let observations_after_end = fixture.replay_after(durable_end_sequence, 1).await;
        let no_op_sample =
            no_op_phase.finish(&fixture.db_path, repetition, observations_after_end.len());
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
    };
    println!(
        "TRACEDECAY_PR5_BENCHMARK_RESULT={} ",
        serde_json::to_string(&result).expect("serialize benchmark result")
    );
}
