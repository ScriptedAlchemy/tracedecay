use std::collections::BTreeMap;

use serde::Deserialize;

use super::*;

const READINESS_AGGREGATION_SCHEMA_VERSION: u32 = 1;
pub(super) const MAX_READINESS_INPUT_ROWS: usize = 10_000;
pub(super) const READINESS_HOST_BUCKETS: usize = 6;
const READINESS_DISPOSITION_CLASSES: usize = 5;
const READINESS_DISPOSITION_STATUSES: usize = 8;
const READINESS_RETRYABLE_STATES: usize = 3;
pub(super) const MAX_DISPOSITION_SERIES: usize = READINESS_HOST_BUCKETS
    * READINESS_DISPOSITION_CLASSES
    * READINESS_DISPOSITION_STATUSES
    * READINESS_RETRYABLE_STATES;
pub(super) const LATENCY_BUCKET_UPPER_US: &[u64] = &[
    1_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    u64::MAX,
];
const BYTES_BUCKET_UPPER: &[u64] = &[
    256,
    1_024,
    4_096,
    16_384,
    65_536,
    262_144,
    1_048_576,
    u64::MAX,
];

/// Availability for a numeric metric series. Never collapses unavailable into zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricAvailability {
    Measured,
    NoSamples,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadinessHost {
    Claude,
    Codex,
    Cursor,
    Hermes,
    Kiro,
    Other,
}

impl ReadinessHost {
    fn from_event(value: &str) -> Self {
        match tracedecay_domain::HostIntegrationIdV1::from_wire(value) {
            Some(tracedecay_domain::HostIntegrationIdV1::Claude) => Self::Claude,
            Some(tracedecay_domain::HostIntegrationIdV1::Codex) => Self::Codex,
            Some(tracedecay_domain::HostIntegrationIdV1::Cursor) => Self::Cursor,
            Some(tracedecay_domain::HostIntegrationIdV1::Hermes) => Self::Hermes,
            Some(tracedecay_domain::HostIntegrationIdV1::Kiro) => Self::Kiro,
            Some(_) | None => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundedBucketCount {
    /// Inclusive upper bound of this bucket (`u64::MAX` means open-ended).
    pub(crate) upper_bound: u64,
    pub(crate) count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundedNumericSummary {
    pub(crate) availability: MetricAvailability,
    /// Events where the source field was present and numeric (including true zero).
    pub(crate) present_count: u64,
    /// Events where the source field was null, missing, or non-numeric.
    pub(crate) absent_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sum: Option<u64>,
    pub(crate) buckets: Vec<BoundedBucketCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostMetricSeries {
    pub(crate) host: ReadinessHost,
    pub(crate) summary: BoundedNumericSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PayloadBytesSeries {
    pub(crate) host: ReadinessHost,
    pub(crate) host_event_payload_bytes: BoundedNumericSummary,
    pub(crate) daemon_ipc_payload_bytes: BoundedNumericSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TimeoutOutcomesByHost {
    pub(crate) host: ReadinessHost,
    pub(crate) timed_out_true: u64,
    pub(crate) timed_out_false: u64,
    /// `timeout.timed_out` null/missing — distinct from measured false.
    pub(crate) timed_out_unavailable: u64,
    pub(crate) budget_ms_present: u64,
    pub(crate) budget_ms_absent: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispositionCount {
    pub(crate) host: ReadinessHost,
    pub(crate) class: HookDispositionClass,
    pub(crate) status: HostAdmissionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retryable: Option<bool>,
    pub(crate) count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnavailableMetricReport {
    pub(crate) metric: String,
    pub(crate) status: MetricAvailability,
    pub(crate) blocker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadinessAggregationBounds {
    pub(crate) max_input_rows: u64,
    pub(crate) host_buckets: u64,
    pub(crate) max_disposition_series: u64,
    pub(crate) latency_buckets: u64,
    pub(crate) bytes_buckets: u64,
}

/// Bounded, privacy-safe readiness distributions over real `hook_completed` rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookCompletedReadinessDistributions {
    pub(crate) schema_version: u32,
    pub(crate) source_event: String,
    pub(crate) collection_status: MetricAvailability,
    pub(crate) input_rows_received: u64,
    pub(crate) input_rows_processed: u64,
    pub(crate) input_rows_dropped_at_cap: u64,
    pub(crate) events_considered: u64,
    pub(crate) events_skipped_non_completed: u64,
    pub(crate) hook_wall_time_distribution: Vec<HostMetricSeries>,
    /// TRUE host IPC RTT from `daemon_rtt_us` (not daemon-internal processing).
    pub(crate) host_ipc_rtt_distribution: Vec<HostMetricSeries>,
    pub(crate) payload_bytes_distribution: Vec<PayloadBytesSeries>,
    pub(crate) timeout_outcomes_by_host: Vec<TimeoutOutcomesByHost>,
    pub(crate) disposition_counts_by_host: Vec<DispositionCount>,
    /// Daemon-internal processing is not on the `hook_completed` contract.
    pub(crate) unavailable_metrics: Vec<UnavailableMetricReport>,
    pub(crate) bounds: ReadinessAggregationBounds,
    pub(crate) rows_folded_to_other_host: u64,
    pub(crate) disposition_values_folded_to_unknown: u64,
}

#[derive(Default)]
struct MutableNumericSummary {
    present_count: u64,
    absent_count: u64,
    min: Option<u64>,
    max: Option<u64>,
    sum: u64,
    buckets: Vec<u64>,
}

impl MutableNumericSummary {
    fn with_buckets(bucket_count: usize) -> Self {
        Self {
            buckets: vec![0; bucket_count],
            ..Self::default()
        }
    }

    fn observe_optional(&mut self, value: Option<u64>, uppers: &[u64]) {
        match value {
            Some(sample) => {
                self.present_count = self.present_count.saturating_add(1);
                self.sum = self.sum.saturating_add(sample);
                self.min = Some(self.min.map_or(sample, |min| min.min(sample)));
                self.max = Some(self.max.map_or(sample, |max| max.max(sample)));
                if let Some(index) = uppers.iter().position(|upper| sample <= *upper) {
                    self.buckets[index] = self.buckets[index].saturating_add(1);
                }
            }
            None => {
                self.absent_count = self.absent_count.saturating_add(1);
            }
        }
    }

    fn finish(self, uppers: &[u64]) -> BoundedNumericSummary {
        let availability = if self.present_count > 0 {
            MetricAvailability::Measured
        } else {
            MetricAvailability::NoSamples
        };
        let (min, max, sum) = if self.present_count > 0 {
            (self.min, self.max, Some(self.sum))
        } else {
            (None, None, None)
        };
        BoundedNumericSummary {
            availability,
            present_count: self.present_count,
            absent_count: self.absent_count,
            min,
            max,
            sum,
            buckets: uppers
                .iter()
                .zip(self.buckets.iter())
                .map(|(upper, count)| BoundedBucketCount {
                    upper_bound: *upper,
                    count: *count,
                })
                .collect(),
        }
    }
}

struct MutableHostSeries {
    wall_time_us: MutableNumericSummary,
    host_ipc_rtt_us: MutableNumericSummary,
}

impl Default for MutableHostSeries {
    fn default() -> Self {
        Self {
            wall_time_us: MutableNumericSummary::with_buckets(LATENCY_BUCKET_UPPER_US.len()),
            host_ipc_rtt_us: MutableNumericSummary::with_buckets(LATENCY_BUCKET_UPPER_US.len()),
        }
    }
}

struct MutablePayloadSeries {
    host_event_payload_bytes: MutableNumericSummary,
    daemon_ipc_payload_bytes: MutableNumericSummary,
}

impl Default for MutablePayloadSeries {
    fn default() -> Self {
        Self {
            host_event_payload_bytes: MutableNumericSummary::with_buckets(BYTES_BUCKET_UPPER.len()),
            daemon_ipc_payload_bytes: MutableNumericSummary::with_buckets(BYTES_BUCKET_UPPER.len()),
        }
    }
}

#[derive(Default)]
struct MutableTimeoutOutcomes {
    timed_out_true: u64,
    timed_out_false: u64,
    timed_out_unavailable: u64,
    budget_ms_present: u64,
    budget_ms_absent: u64,
}

type DispositionSeriesKey = (ReadinessHost, HookDispositionClass, u8, Option<bool>);
type DispositionSeriesValue = (HostAdmissionStatus, u64);
type MutableDispositionCounts = BTreeMap<DispositionSeriesKey, DispositionSeriesValue>;

/// Aggregate real `hook_completed` telemetry into bounded readiness distributions.
///
/// Null/missing numeric fields increment `absent_count` and never enter buckets as zero.
/// Missing or invalid dispositions fold into closed typed `unknown` values — never
/// default-success. Hook names and reason codes are not emitted. Daemon processing
/// duration is reported unavailable (upstream blocker).
pub(crate) fn aggregate_hook_completed_readiness(
    rows: &[Value],
) -> HookCompletedReadinessDistributions {
    let input_rows_received = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    let input_rows_processed =
        u64::try_from(rows.len().min(MAX_READINESS_INPUT_ROWS)).unwrap_or(u64::MAX);
    let input_rows_dropped_at_cap = input_rows_received.saturating_sub(input_rows_processed);
    let mut events_considered = 0_u64;
    let mut events_skipped_non_completed = 0_u64;
    let mut rows_folded_to_other_host = 0_u64;
    let mut disposition_values_folded_to_unknown = 0_u64;

    let mut latency_by_host: BTreeMap<ReadinessHost, MutableHostSeries> = BTreeMap::new();
    let mut payload_by_host: BTreeMap<ReadinessHost, MutablePayloadSeries> = BTreeMap::new();
    let mut timeout_by_host: BTreeMap<ReadinessHost, MutableTimeoutOutcomes> = BTreeMap::new();
    let mut disposition_counts = MutableDispositionCounts::new();

    // Callers pass ascending chronological order (see read_hook_analytics_rows_at:
    // ts_unix_ms, session_id, hook_name, agent). Cap keeps the newest suffix so
    // readiness metrics advance under heavy load instead of freezing on oldest rows.
    let start = rows.len().saturating_sub(MAX_READINESS_INPUT_ROWS);
    for row in &rows[start..] {
        let Some((telemetry, disposition_folded_to_unknown)) =
            HookCompletedTelemetry::from_row(row)
        else {
            events_skipped_non_completed = events_skipped_non_completed.saturating_add(1);
            continue;
        };
        events_considered = events_considered.saturating_add(1);

        let host = ReadinessHost::from_event(&telemetry.agent);
        if host == ReadinessHost::Other {
            rows_folded_to_other_host = rows_folded_to_other_host.saturating_add(1);
        }

        // TRUE host IPC RTT. Null means unavailable — never treat as 0 RTT.
        let latency = latency_by_host.entry(host).or_default();
        latency
            .wall_time_us
            .observe_optional(telemetry.hook_wall_time_us, LATENCY_BUCKET_UPPER_US);
        latency
            .host_ipc_rtt_us
            .observe_optional(telemetry.daemon_rtt_us, LATENCY_BUCKET_UPPER_US);

        let payload = payload_by_host.entry(host).or_default();
        payload
            .host_event_payload_bytes
            .observe_optional(telemetry.payload_bytes, BYTES_BUCKET_UPPER);
        payload
            .daemon_ipc_payload_bytes
            .observe_optional(telemetry.daemon_ipc_payload_bytes, BYTES_BUCKET_UPPER);

        let timeout = timeout_by_host.entry(host).or_default();
        match telemetry.timeout.timed_out {
            Some(true) => timeout.timed_out_true = timeout.timed_out_true.saturating_add(1),
            Some(false) => timeout.timed_out_false = timeout.timed_out_false.saturating_add(1),
            None => timeout.timed_out_unavailable = timeout.timed_out_unavailable.saturating_add(1),
        }
        if telemetry.timeout.budget_ms.is_some() {
            timeout.budget_ms_present = timeout.budget_ms_present.saturating_add(1);
        } else {
            timeout.budget_ms_absent = timeout.budget_ms_absent.saturating_add(1);
        }

        if disposition_folded_to_unknown {
            disposition_values_folded_to_unknown =
                disposition_values_folded_to_unknown.saturating_add(1);
        }
        let disposition = telemetry.disposition;
        let (_, count) = disposition_counts
            .entry((
                host,
                disposition.class,
                host_admission_status_rank(disposition.status),
                disposition.retryable,
            ))
            .or_insert((disposition.status, 0));
        *count = count.saturating_add(1);
    }

    let mut wall_distribution = Vec::with_capacity(latency_by_host.len());
    let mut rtt_distribution = Vec::with_capacity(latency_by_host.len());
    for (host, series) in latency_by_host {
        wall_distribution.push(HostMetricSeries {
            host,
            summary: series.wall_time_us.finish(LATENCY_BUCKET_UPPER_US),
        });
        rtt_distribution.push(HostMetricSeries {
            host,
            summary: series.host_ipc_rtt_us.finish(LATENCY_BUCKET_UPPER_US),
        });
    }

    HookCompletedReadinessDistributions {
        schema_version: READINESS_AGGREGATION_SCHEMA_VERSION,
        source_event: "hook_completed".to_string(),
        collection_status: if events_considered == 0 {
            MetricAvailability::NoSamples
        } else {
            MetricAvailability::Measured
        },
        input_rows_received,
        input_rows_processed,
        input_rows_dropped_at_cap,
        events_considered,
        events_skipped_non_completed,
        hook_wall_time_distribution: wall_distribution,
        host_ipc_rtt_distribution: rtt_distribution,
        payload_bytes_distribution: finish_payload_series(payload_by_host),
        timeout_outcomes_by_host: finish_timeout_series(timeout_by_host),
        disposition_counts_by_host: finish_disposition_counts(disposition_counts),
        unavailable_metrics: vec![UnavailableMetricReport {
            metric: "daemon_processing_duration_distribution".to_string(),
            status: MetricAvailability::Unavailable,
            blocker: "hook_completed_does_not_emit_daemon_processing_duration".to_string(),
        }],
        bounds: ReadinessAggregationBounds {
            max_input_rows: MAX_READINESS_INPUT_ROWS as u64,
            host_buckets: READINESS_HOST_BUCKETS as u64,
            max_disposition_series: MAX_DISPOSITION_SERIES as u64,
            latency_buckets: LATENCY_BUCKET_UPPER_US.len() as u64,
            bytes_buckets: BYTES_BUCKET_UPPER.len() as u64,
        },
        rows_folded_to_other_host,
        disposition_values_folded_to_unknown,
    }
}

fn finish_payload_series(
    payload_by_host: BTreeMap<ReadinessHost, MutablePayloadSeries>,
) -> Vec<PayloadBytesSeries> {
    payload_by_host
        .into_iter()
        .map(|(host, series)| PayloadBytesSeries {
            host,
            host_event_payload_bytes: series.host_event_payload_bytes.finish(BYTES_BUCKET_UPPER),
            daemon_ipc_payload_bytes: series.daemon_ipc_payload_bytes.finish(BYTES_BUCKET_UPPER),
        })
        .collect()
}

fn finish_timeout_series(
    timeout_by_host: BTreeMap<ReadinessHost, MutableTimeoutOutcomes>,
) -> Vec<TimeoutOutcomesByHost> {
    timeout_by_host
        .into_iter()
        .map(|(host, outcomes)| TimeoutOutcomesByHost {
            host,
            timed_out_true: outcomes.timed_out_true,
            timed_out_false: outcomes.timed_out_false,
            timed_out_unavailable: outcomes.timed_out_unavailable,
            budget_ms_present: outcomes.budget_ms_present,
            budget_ms_absent: outcomes.budget_ms_absent,
        })
        .collect()
}

fn finish_disposition_counts(
    disposition_counts: MutableDispositionCounts,
) -> Vec<DispositionCount> {
    disposition_counts
        .into_iter()
        .map(
            |((host, class, _, retryable), (status, count))| DispositionCount {
                host,
                class,
                status,
                retryable,
                count,
            },
        )
        .collect()
}

fn host_admission_status_rank(status: HostAdmissionStatus) -> u8 {
    match status {
        HostAdmissionStatus::Supported => 0,
        HostAdmissionStatus::Degraded => 1,
        HostAdmissionStatus::Unavailable => 2,
        HostAdmissionStatus::Unknown => 3,
        HostAdmissionStatus::Backpressured => 4,
        HostAdmissionStatus::AcceptedForReplay => 5,
        HostAdmissionStatus::Committed => 6,
        HostAdmissionStatus::ExactDuplicate => 7,
    }
}

/// Deterministic empty distributions for readiness catalog identity (no live rows).
#[cfg(test)]
pub(crate) fn empty_hook_completed_readiness_distributions() -> HookCompletedReadinessDistributions
{
    aggregate_hook_completed_readiness(&[])
}
