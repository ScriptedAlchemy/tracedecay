//! Reproducible PR5 observation-pipeline baseline.
//!
//! This ignored release-mode test deliberately exercises the production Claude
//! scanner, sanitizer, authoritative store adapter, projector, V1 fold, and
//! bounded replay API. Linux `/proc` counters keep CPU, peak RSS, and write I/O
//! measurements inside the workload process instead of including Cargo/build
//! overhead.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;
use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::ObservationScopeV1;
use tracedecay_store::{ObservationReplayRequest, ObservationStore};

use super::claude::ClaudeSource;
use super::claude_observation::{ClaudeObservationIngestStats, ingest_source_with_observations};
use crate::application::observation::ObservationCancellation;
use crate::store::GlobalDbObservationStore;

const WORKLOAD_ID: &str = "pr5-observation-pipeline-v1";
const WARMUP_REPETITIONS: usize = 3;
const MEASURED_REPETITIONS: usize = 30;
const RECORDS_PER_REPETITION: usize = 64;

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    profile: PathBuf,
    transcript: PathBuf,
    db_path: PathBuf,
    db: crate::global_db::GlobalDb,
}

impl Fixture {
    async fn new(repetition: usize) -> Self {
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

    fn source(&self) -> ClaudeSource {
        let session_id = self
            .transcript
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("benchmark session id");
        ClaudeSource::with_home(&self.home).for_user_scope(Some(session_id.to_string()), Vec::new())
    }

    async fn ingest(&self, source: &ClaudeSource) -> ClaudeObservationIngestStats {
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

    async fn replay_count(&self) -> usize {
        GlobalDbObservationStore::new(&self.db)
            .replay_observations(
                ObservationReplayRequest::new(0, RECORDS_PER_REPETITION)
                    .expect("bounded replay request"),
            )
            .await
            .expect("replay committed benchmark observations")
            .len()
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
                "secret_key": format!("benchmark-secret-{index}"),
            }
        });
        body.push_str(&record.to_string());
        body.push('\n');
    }
    fs::write(path, body).expect("write benchmark transcript");
}

#[derive(Debug, Serialize)]
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
            .map(|&value| {
                let difference = value as f64 - mean;
                difference * difference
            })
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

#[derive(Default, Serialize)]
struct NoOpTotals {
    sessions_upserted: u64,
    messages_upserted: u64,
    observations_committed: u64,
    observation_duplicates: u64,
    cursor_advances: u64,
    cursor_duplicates: u64,
    records_rejected: u64,
    records_quarantined: u64,
    projections_completed: u64,
    projections_skipped: u64,
    projection_duplicates: u64,
    deferred_sources: u64,
}

#[derive(Serialize)]
struct RawPhaseSample {
    repetition: usize,
    latency_ns: u64,
    cpu_ticks: u64,
    process_write_bytes: u64,
    database_storage_growth_bytes: u64,
    peak_rss_kib: u64,
    replayed_observations: usize,
}

impl NoOpTotals {
    fn add(&mut self, stats: ClaudeObservationIngestStats) {
        self.sessions_upserted += stats.transcript.sessions_upserted;
        self.messages_upserted += stats.transcript.messages_upserted;
        self.observations_committed += stats.observations_committed;
        self.observation_duplicates += stats.observation_duplicates;
        self.cursor_advances += stats.cursor_advances;
        self.cursor_duplicates += stats.cursor_duplicates;
        self.records_rejected += stats.records_rejected;
        self.records_quarantined += stats.records_quarantined;
        self.projections_completed += stats.projections_completed;
        self.projections_skipped += stats.projections_skipped;
        self.projection_duplicates += stats.projection_duplicates;
        self.deferred_sources += stats.deferred_sources;
    }

    fn is_zero(&self) -> bool {
        self.sessions_upserted == 0
            && self.messages_upserted == 0
            && self.observations_committed == 0
            && self.observation_duplicates == 0
            && self.cursor_advances == 0
            && self.cursor_duplicates == 0
            && self.records_rejected == 0
            && self.records_quarantined == 0
            && self.projections_completed == 0
            && self.projections_skipped == 0
            && self.projection_duplicates == 0
            && self.deferred_sources == 0
    }
}

#[derive(Serialize)]
struct BenchmarkResult {
    schema_version: u32,
    workload_id: &'static str,
    benchmark_commit: String,
    benchmark_commit_dirty: bool,
    command: &'static str,
    rustc: String,
    cargo: String,
    kernel: String,
    cpu_model: String,
    logical_cpu_count: usize,
    memory_total_kib: u64,
    clock_ticks_per_second: u64,
    warmup_repetitions: usize,
    measured_repetitions: usize,
    records_per_repetition: usize,
    measured_records: usize,
    pipeline_raw_samples: Vec<RawPhaseSample>,
    pipeline_batch_latency: Distribution,
    pipeline_records_per_second: f64,
    pipeline_cpu_ticks: u64,
    pipeline_cpu_ms: f64,
    pipeline_process_write_bytes: u64,
    database_storage_growth_bytes: u64,
    peak_rss_kib: u64,
    no_op_replay_raw_samples: Vec<RawPhaseSample>,
    no_op_replay_latency: Distribution,
    no_op_replay_cpu_ticks: u64,
    no_op_replay_cpu_ms: f64,
    no_op_replay_process_write_bytes: u64,
    no_op_replay_database_storage_growth_bytes: u64,
    no_op_replay_observation_count_delta: i64,
    no_op_replay_totals: NoOpTotals,
}

#[tokio::test]
#[ignore = "release-mode PR5 performance baseline; run the documented exact command"]
async fn production_observation_pipeline_baseline() {
    for repetition in 0..WARMUP_REPETITIONS {
        let fixture = Fixture::new(repetition).await;
        let source = fixture.source();
        let stats = fixture.ingest(&source).await;
        assert_eq!(
            stats.observations_committed as usize,
            RECORDS_PER_REPETITION
        );
        assert_eq!(stats.projections_completed as usize, RECORDS_PER_REPETITION);
        assert_eq!(
            stats.transcript.messages_upserted as usize,
            RECORDS_PER_REPETITION
        );
        assert_eq!(fixture.replay_count().await, RECORDS_PER_REPETITION);
    }

    let clock_ticks_per_second = command_output("getconf", &["CLK_TCK"])
        .parse::<u64>()
        .expect("parse CLK_TCK");
    let mut pipeline_raw_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    let mut no_op_raw_samples = Vec::with_capacity(MEASURED_REPETITIONS);
    let mut pipeline_cpu_ticks = 0_u64;
    let mut no_op_cpu_ticks = 0_u64;
    let mut pipeline_process_write_bytes = 0_u64;
    let mut no_op_process_write_bytes = 0_u64;
    let mut database_storage_growth_bytes = 0_u64;
    let mut no_op_database_storage_growth_bytes = 0_u64;
    let mut no_op_observation_count_delta = 0_i64;
    let mut no_op_totals = NoOpTotals::default();
    let mut peak_rss_kib = 0_u64;

    for repetition in 0..MEASURED_REPETITIONS {
        let fixture = Fixture::new(WARMUP_REPETITIONS + repetition).await;
        let source = fixture.source();
        reset_peak_rss();
        let storage_before = database_storage_bytes(&fixture.db_path);
        let cpu_before = process_cpu_ticks();
        let write_before = process_write_bytes();
        let started = Instant::now();
        let stats = fixture.ingest(&source).await;
        let replayed = fixture.replay_count().await;
        let latency_ns = elapsed_ns(started);
        let cpu_ticks = process_cpu_ticks().saturating_sub(cpu_before);
        let sample_write_bytes = process_write_bytes().saturating_sub(write_before);
        let storage_growth_bytes =
            database_storage_bytes(&fixture.db_path).saturating_sub(storage_before);
        let sample_peak_rss_kib = process_peak_rss_kib();
        pipeline_cpu_ticks += cpu_ticks;
        pipeline_process_write_bytes += sample_write_bytes;
        database_storage_growth_bytes += storage_growth_bytes;
        peak_rss_kib = peak_rss_kib.max(sample_peak_rss_kib);
        pipeline_raw_samples.push(RawPhaseSample {
            repetition,
            latency_ns,
            cpu_ticks,
            process_write_bytes: sample_write_bytes,
            database_storage_growth_bytes: storage_growth_bytes,
            peak_rss_kib: sample_peak_rss_kib,
            replayed_observations: replayed,
        });
        assert_eq!(
            stats.observations_committed as usize,
            RECORDS_PER_REPETITION
        );
        assert_eq!(stats.projections_completed as usize, RECORDS_PER_REPETITION);
        assert_eq!(
            stats.transcript.messages_upserted as usize,
            RECORDS_PER_REPETITION
        );
        assert_eq!(replayed, RECORDS_PER_REPETITION);

        let no_op_storage_before = database_storage_bytes(&fixture.db_path);
        let no_op_cpu_before = process_cpu_ticks();
        let no_op_write_before = process_write_bytes();
        let no_op_started = Instant::now();
        let no_op_stats = fixture.ingest(&source).await;
        let replayed_after_no_op = fixture.replay_count().await;
        let no_op_latency_ns = elapsed_ns(no_op_started);
        let no_op_sample_cpu_ticks = process_cpu_ticks().saturating_sub(no_op_cpu_before);
        let no_op_sample_write_bytes = process_write_bytes().saturating_sub(no_op_write_before);
        let no_op_sample_storage_growth =
            database_storage_bytes(&fixture.db_path).saturating_sub(no_op_storage_before);
        let no_op_peak_rss_kib = process_peak_rss_kib();
        no_op_cpu_ticks += no_op_sample_cpu_ticks;
        no_op_process_write_bytes += no_op_sample_write_bytes;
        no_op_database_storage_growth_bytes += no_op_sample_storage_growth;
        no_op_observation_count_delta += replayed_after_no_op as i64 - replayed as i64;
        no_op_totals.add(no_op_stats);
        peak_rss_kib = peak_rss_kib.max(no_op_peak_rss_kib);
        no_op_raw_samples.push(RawPhaseSample {
            repetition,
            latency_ns: no_op_latency_ns,
            cpu_ticks: no_op_sample_cpu_ticks,
            process_write_bytes: no_op_sample_write_bytes,
            database_storage_growth_bytes: no_op_sample_storage_growth,
            peak_rss_kib: no_op_peak_rss_kib,
            replayed_observations: replayed_after_no_op,
        });
    }

    assert_eq!(no_op_observation_count_delta, 0);
    assert!(
        no_op_totals.is_zero(),
        "exact replay performed durable work"
    );
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
    let result = BenchmarkResult {
        schema_version: 1,
        workload_id: WORKLOAD_ID,
        benchmark_commit: command_output("git", &["rev-parse", "HEAD"]),
        benchmark_commit_dirty: !Command::new("git")
            .args(["diff", "--quiet", "--ignore-submodules", "HEAD"])
            .status()
            .expect("inspect benchmark worktree")
            .success(),
        command: "cargo test --quiet --release --lib sessions::claude_observation_benchmark::production_observation_pipeline_baseline -- --ignored --exact --nocapture --test-threads=1",
        rustc: command_output("rustc", &["-Vv"]),
        cargo: command_output("cargo", &["-V"]),
        kernel: command_output("uname", &["-srmo"]),
        cpu_model: cpu_model(),
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
        pipeline_cpu_ticks,
        pipeline_cpu_ms: ticks_to_ms(pipeline_cpu_ticks, clock_ticks_per_second),
        pipeline_process_write_bytes,
        database_storage_growth_bytes,
        peak_rss_kib,
        no_op_replay_raw_samples: no_op_raw_samples,
        no_op_replay_latency: Distribution::from_samples(&no_op_samples),
        no_op_replay_cpu_ticks: no_op_cpu_ticks,
        no_op_replay_cpu_ms: ticks_to_ms(no_op_cpu_ticks, clock_ticks_per_second),
        no_op_replay_process_write_bytes: no_op_process_write_bytes,
        no_op_replay_database_storage_growth_bytes: no_op_database_storage_growth_bytes,
        no_op_replay_observation_count_delta: no_op_observation_count_delta,
        no_op_replay_totals: no_op_totals,
    };
    println!(
        "TRACEDECAY_PR5_BENCHMARK_RESULT={} ",
        serde_json::to_string(&result).expect("serialize benchmark result")
    );
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}

fn ticks_to_ms(ticks: u64, ticks_per_second: u64) -> f64 {
    ticks as f64 * 1_000.0 / ticks_per_second as f64
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

fn process_cpu_ticks() -> u64 {
    let stat = fs::read_to_string("/proc/self/stat").expect("read process CPU counters");
    let after_name = stat
        .rfind(')')
        .and_then(|index| stat.get(index + 2..))
        .expect("parse process name in /proc/self/stat");
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let user = fields[11].parse::<u64>().expect("parse process user ticks");
    let system = fields[12]
        .parse::<u64>()
        .expect("parse process system ticks");
    user + system
}

fn process_write_bytes() -> u64 {
    proc_value("/proc/self/io", "write_bytes:")
}

fn reset_peak_rss() {
    fs::write("/proc/self/clear_refs", b"5\n").expect("reset process peak RSS");
}

fn process_peak_rss_kib() -> u64 {
    proc_value("/proc/self/status", "VmHWM:")
}

fn memory_total_kib() -> u64 {
    proc_value("/proc/meminfo", "MemTotal:")
}

fn proc_value(path: &str, key: &str) -> u64 {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
        .lines()
        .find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or_else(|| panic!("missing {key} in {path}"))
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .expect("read CPU model")
        .lines()
        .find_map(|line| line.strip_prefix("model name\t: "))
        .expect("CPU model name")
        .to_string()
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
