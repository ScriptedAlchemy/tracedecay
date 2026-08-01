use serde::{Deserialize, Serialize};

use super::baseline::HookTelemetryReadiness;
use crate::sessions::claude_observation::ClaudeObservationIngestStats;
use crate::sessions::shared::TranscriptIngestStats;
use tracedecay_runtime_core::timeutil::nearest_rank;

pub(super) const PROVIDER_PARSE_SCOPE: &str = "native_provider_format_decode";
pub(super) const PROVIDER_COMMIT_SCOPE: &str =
    "production_adapter_parse_normalize_sanitize_commit_and_project";
pub(super) const PROVIDER_REPLAY_SCOPE: &str = "authoritative_store_bounded_replay";

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Distribution {
    pub(super) repetitions: usize,
    pub(super) min_ns: u64,
    pub(super) p50_ns: u64,
    pub(super) p95_ns: u64,
    pub(super) p99_ns: u64,
    pub(super) max_ns: u64,
    pub(super) mean_ns: f64,
    pub(super) sample_stddev_ns: f64,
}

impl Distribution {
    pub(super) fn from_samples(samples: &[u64]) -> Self {
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
            p50_ns: nearest_rank(&sorted, 50).expect("non-empty benchmark sample"),
            p95_ns: nearest_rank(&sorted, 95).expect("non-empty benchmark sample"),
            p99_ns: nearest_rank(&sorted, 99).expect("non-empty benchmark sample"),
            max_ns: *sorted.last().expect("last benchmark sample"),
            mean_ns: mean,
            sample_stddev_ns: variance.sqrt(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct NoOpTotals {
    pub(super) source_bytes_scanned: u64,
    pub(super) sessions_upserted: u64,
    pub(super) messages_upserted: u64,
    pub(super) observations_committed: u64,
    pub(super) observation_duplicates: u64,
    pub(super) cursor_advances: u64,
    pub(super) cursor_duplicates: u64,
    pub(super) records_rejected: u64,
    pub(super) records_quarantined: u64,
    pub(super) projections_completed: u64,
    pub(super) projections_skipped: u64,
    pub(super) projection_duplicates: u64,
    pub(super) deferred_sources: u64,
}

impl NoOpTotals {
    pub(super) fn add(&mut self, stats: ClaudeObservationIngestStats) {
        let ClaudeObservationIngestStats {
            transcript,
            source_bytes_scanned,
            observations_committed,
            observation_duplicates,
            cursor_advances,
            cursor_duplicates,
            records_rejected,
            records_quarantined,
            projections_completed,
            projection_outputs: _,
            projections_skipped,
            projection_duplicates,
            deferred_sources,
        } = stats;
        let TranscriptIngestStats {
            sessions_upserted,
            messages_upserted,
        } = transcript;
        self.source_bytes_scanned += source_bytes_scanned;
        self.sessions_upserted += sessions_upserted;
        self.messages_upserted += messages_upserted;
        self.observations_committed += observations_committed;
        self.observation_duplicates += observation_duplicates;
        self.cursor_advances += cursor_advances;
        self.cursor_duplicates += cursor_duplicates;
        self.records_rejected += records_rejected;
        self.records_quarantined += records_quarantined;
        self.projections_completed += projections_completed;
        self.projections_skipped += projections_skipped;
        self.projection_duplicates += projection_duplicates;
        self.deferred_sources += deferred_sources;
    }

    pub(super) fn is_zero(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPhaseSample {
    pub(super) repetition: usize,
    pub(super) latency_ns: u64,
    pub(super) cpu_ticks: u64,
    pub(super) process_write_bytes: u64,
    pub(super) database_storage_growth_bytes: u64,
    pub(super) peak_rss_kib: u64,
    pub(super) replayed_observations: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RawProviderPhaseSample {
    pub(super) repetition: usize,
    pub(super) latency_ns: u64,
    pub(super) cpu_ticks: u64,
    pub(super) process_write_bytes: u64,
    pub(super) database_storage_growth_bytes: u64,
    pub(super) peak_rss_kib: u64,
    pub(super) record_count: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderPhaseResult {
    pub(super) scope: String,
    pub(super) raw_samples: Vec<RawProviderPhaseSample>,
    pub(super) latency: Distribution,
    pub(super) cpu_ticks: u64,
    pub(super) cpu_ms: f64,
    pub(super) process_write_bytes: u64,
    pub(super) database_storage_growth_bytes: u64,
    pub(super) peak_rss_kib: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderBenchmarkResult {
    pub(super) provider: String,
    pub(super) production_path: String,
    pub(super) pipeline_scope: String,
    pub(super) measured_repetitions: usize,
    pub(super) observations_per_repetition: usize,
    pub(super) replay_limit: usize,
    pub(super) max_backlog_records: usize,
    pub(super) parse: ProviderPhaseResult,
    pub(super) commit: ProviderPhaseResult,
    pub(super) replay: ProviderPhaseResult,
    pub(super) pipeline_raw_samples: Vec<RawPhaseSample>,
    pub(super) pipeline_latency: Distribution,
    pub(super) pipeline_records_per_second: f64,
    pub(super) pipeline_cpu_ticks: u64,
    pub(super) pipeline_cpu_ms: f64,
    pub(super) pipeline_process_write_bytes: u64,
    pub(super) pipeline_database_storage_growth_bytes: u64,
    pub(super) peak_rss_kib: u64,
    pub(super) no_op_raw_samples: Vec<RawPhaseSample>,
    pub(super) no_op_latency: Distribution,
    pub(super) no_op_cpu_ticks: u64,
    pub(super) no_op_cpu_ms: f64,
    pub(super) no_op_process_write_bytes: u64,
    pub(super) no_op_database_storage_growth_bytes: u64,
    pub(super) no_op_observation_count_delta: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderBenchmarkSuiteResult {
    pub(super) schema_version: u32,
    pub(super) workload_id: String,
    pub(super) fairness: ProviderFairnessResult,
    pub(super) providers: Vec<ProviderBenchmarkResult>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderFairnessResult {
    pub(super) policy: String,
    pub(super) rounds: usize,
    pub(super) providers_per_round: usize,
    pub(super) max_provider_turn_distance: usize,
    pub(super) turns: Vec<ProviderScheduleTurn>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderScheduleTurn {
    pub(super) round: usize,
    pub(super) position: usize,
    pub(super) provider: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct GitSnapshot {
    pub(super) commit: String,
    pub(super) tree: String,
    pub(super) dirty: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkloadIdentity {
    pub(super) manifest_path: String,
    pub(super) manifest_sha256: String,
    pub(super) harness_paths: Vec<String>,
    pub(super) harness_sha256: String,
    pub(super) native_fixture_paths: Vec<String>,
    pub(super) native_fixtures_sha256: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum EvidenceStatus {
    Acceptance,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildIdentity {
    pub(super) commit: String,
    pub(super) tree: String,
    pub(super) profile: String,
    pub(super) source_mode: String,
    pub(super) source_manifest_sha256: String,
    pub(super) source_file_count: usize,
    pub(super) target_triple: String,
    pub(super) rustc_version: String,
    pub(super) cargo_version: String,
    pub(super) rustflags: String,
    pub(super) rustc_wrapper: String,
    pub(super) rustc_workspace_wrapper: String,
    pub(super) cargo_config_identity: String,
    pub(super) data_root_basis: String,
    pub(super) executable_sha256: String,
    pub(super) executable_size_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BenchmarkResult {
    pub(super) schema_version: u32,
    pub(super) workload_id: String,
    pub(super) evidence_status: EvidenceStatus,
    pub(super) workload_identity: WorkloadIdentity,
    pub(super) build_identity: BuildIdentity,
    pub(super) git_before: GitSnapshot,
    pub(super) git_after: GitSnapshot,
    pub(super) command: String,
    pub(super) rustc: String,
    pub(super) cargo: String,
    pub(super) kernel: String,
    pub(super) cpu_identity: String,
    pub(super) logical_cpu_count: usize,
    pub(super) memory_total_kib: u64,
    pub(super) clock_ticks_per_second: u64,
    pub(super) warmup_repetitions: usize,
    pub(super) measured_repetitions: usize,
    pub(super) records_per_repetition: usize,
    pub(super) measured_records: usize,
    pub(super) pipeline_raw_samples: Vec<RawPhaseSample>,
    pub(super) pipeline_batch_latency: Distribution,
    pub(super) pipeline_records_per_second: f64,
    pub(super) pipeline_cpu_ticks: u64,
    pub(super) pipeline_cpu_ms: f64,
    pub(super) pipeline_process_write_bytes: u64,
    pub(super) database_storage_growth_bytes: u64,
    pub(super) peak_rss_kib: u64,
    pub(super) no_op_replay_raw_samples: Vec<RawPhaseSample>,
    pub(super) no_op_replay_latency: Distribution,
    pub(super) no_op_replay_cpu_ticks: u64,
    pub(super) no_op_replay_cpu_ms: f64,
    pub(super) no_op_replay_process_write_bytes: u64,
    pub(super) no_op_replay_database_storage_growth_bytes: u64,
    pub(super) no_op_replay_observation_count_delta: i64,
    pub(super) no_op_replay_totals: NoOpTotals,
    /// Additive schema-2 field. Current acceptance validation requires it,
    /// while historical schema-2 artifacts can still deserialize.
    #[serde(default)]
    pub(super) provider_observation_performance: Option<ProviderBenchmarkSuiteResult>,
    #[serde(default)]
    pub(super) hook_telemetry_readiness: Option<HookTelemetryReadiness>,
}
