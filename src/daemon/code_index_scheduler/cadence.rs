//! Code-index scheduler cadence telemetry and event-to-ready receipts.
//!
//! Hints, mount wakes, and query-admission freshness checks are wake-up signals
//! only. Every receipt records the scheduled-arrival-to-terminal latency for one
//! completed reconcile (publish or no-op) so operators can prove cadence instead
//! of inferring it from sealed-generation age.
//!
//! Arrival, dequeue, and terminal instants are recorded separately so queue wait
//! and service time are distinct measurements rather than one interval reported
//! twice. A reconcile whose arrival cannot be attributed reports
//! [`CodeIndexArrivalV1::Unavailable`]: queue delay and event-to-ready latency
//! are then withheld, never rendered as a zero-latency sample.

use std::collections::VecDeque;
use std::path::PathBuf;

use tracedecay_domain::{CodeGenerationId, ContentDigest};

/// Why the scheduler was asked to reconcile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodeIndexCadenceTriggerV1 {
    /// Worktree mount scheduled an initial/verification reconcile.
    Mount,
    /// Host after-file-edit (or equivalent) hint paths arrived.
    HookHint,
    /// Hint overflow / dropped-event reconciliation.
    Overflow,
    /// Single-owner git watcher advanced its monotonic worktree frontier.
    GitWatcher,
    /// Query-admission freshness ladder required truth.
    QueryAdmission,
    /// Follow-up wake after a busy serve-prior-generation admission.
    BusyFollowUp,
}

impl CodeIndexCadenceTriggerV1 {
    /// Stable label for bounded, redacted telemetry.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Mount => "mount",
            Self::HookHint => "hook_hint",
            Self::Overflow => "overflow",
            Self::GitWatcher => "git_watcher",
            Self::QueryAdmission => "query_admission",
            Self::BusyFollowUp => "busy_follow_up",
        }
    }
}

/// When the wake that produced one reconcile was accepted.
///
/// A reconcile that runs without an attributable pending wake — a follow-up pass
/// draining work an earlier wake already claimed, or an out-of-range clock
/// reading — has no arrival instant. That is a typed absence, not an instant
/// equal to the terminal time, because substituting the terminal time would
/// publish a zero queue delay and a zero event-to-ready latency for a sample
/// whose arrival was never observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodeIndexArrivalV1 {
    /// Exact accepted wake instant (Unix micros).
    Observed { wake_micros: i64 },
    /// No arrival instant is attributable to this reconcile.
    Unavailable,
}

impl CodeIndexArrivalV1 {
    /// The observed arrival instant, or `None` when arrival is unavailable.
    pub(crate) fn wake_micros(self) -> Option<i64> {
        match self {
            Self::Observed { wake_micros } => Some(wake_micros),
            Self::Unavailable => None,
        }
    }

    /// Stable label for bounded, redacted telemetry.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Observed { .. } => "observed",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Terminal outcome of one cadence-driven reconcile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodeIndexCadenceOutcomeV1 {
    Published {
        generation_id: CodeGenerationId,
        reextracted_files: usize,
        changed_chunks: usize,
        reused_chunks: usize,
    },
    Noop {
        snapshot_content_identity: ContentDigest,
    },
}

/// One completed event-to-ready measurement for a mounted worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexEventToReadyReceiptV1 {
    pub project_root: PathBuf,
    pub trigger: CodeIndexCadenceTriggerV1,
    /// When the wake was accepted, when that is attributable.
    pub arrival: CodeIndexArrivalV1,
    /// When the scheduler dequeued the wake and began reconcile work
    /// (Unix micros). Always observed, because the scheduler stamps it itself.
    pub started_micros: i64,
    /// When the reconcile reached a terminal publish/no-op (Unix micros).
    pub ready_micros: i64,
    pub outcome: CodeIndexCadenceOutcomeV1,
    pub overflow_reconciled: bool,
}

impl CodeIndexEventToReadyReceiptV1 {
    pub(crate) fn new(
        project_root: PathBuf,
        trigger: CodeIndexCadenceTriggerV1,
        arrival: CodeIndexArrivalV1,
        started_micros: i64,
        ready_micros: i64,
        outcome: CodeIndexCadenceOutcomeV1,
        overflow_reconciled: bool,
    ) -> Self {
        Self {
            project_root,
            trigger,
            arrival,
            started_micros,
            ready_micros,
            outcome,
            overflow_reconciled,
        }
    }

    /// Queue wait: arrival through dequeue. `None` when arrival is unavailable.
    pub(crate) fn queue_delay_micros(&self) -> Option<i64> {
        self.arrival
            .wake_micros()
            .map(|wake_micros| self.started_micros.saturating_sub(wake_micros).max(0))
    }

    /// Service time: dequeue through terminal outcome. Always available.
    pub(crate) fn service_micros(&self) -> i64 {
        self.ready_micros.saturating_sub(self.started_micros).max(0)
    }

    /// Scheduled-arrival-to-terminal latency. `None` when arrival is
    /// unavailable, so an unobserved arrival never contributes a zero sample.
    pub(crate) fn event_to_ready_micros(&self) -> Option<i64> {
        self.arrival
            .wake_micros()
            .map(|wake_micros| self.ready_micros.saturating_sub(wake_micros).max(0))
    }

    pub(crate) fn is_noop(&self) -> bool {
        matches!(self.outcome, CodeIndexCadenceOutcomeV1::Noop { .. })
    }

    /// Stable label for bounded, redacted telemetry.
    pub(crate) fn outcome_label(&self) -> &'static str {
        if self.is_noop() { "noop" } else { "published" }
    }
}

/// Minimum matching samples before a percentile may be reported.
///
/// These mirror the frozen runtime measurement policy in
/// `benchmarks/runtime/policies/journey-margins-v1.json` so a percentile this
/// read model publishes is admissible under the same rule the harness applies.
pub(crate) const P50_MINIMUM_SAMPLES: usize = 2;
pub(crate) const P95_MINIMUM_SAMPLES: usize = 40;
pub(crate) const P99_MINIMUM_SAMPLES: usize = 100;

/// One percentile and the sample floor it requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexPercentileV1 {
    pub minimum_samples: usize,
    /// `None` when the retained population is below `minimum_samples`.
    pub value: Option<i64>,
}

impl CodeIndexPercentileV1 {
    fn from_sorted(sorted: &[i64], percentile: usize, minimum_samples: usize) -> Self {
        let value = (sorted.len() >= minimum_samples)
            .then(|| nearest_rank(sorted, percentile))
            .flatten();
        Self {
            minimum_samples,
            value,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_available(self) -> bool {
        self.value.is_some()
    }
}

/// p50/p95/p99 for one measured quantity, each independently eligible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexPercentilesV1 {
    pub sample_count: usize,
    pub p50: CodeIndexPercentileV1,
    pub p95: CodeIndexPercentileV1,
    pub p99: CodeIndexPercentileV1,
}

impl CodeIndexPercentilesV1 {
    fn from_values(mut values: Vec<i64>) -> Self {
        values.sort_unstable();
        Self {
            sample_count: values.len(),
            p50: CodeIndexPercentileV1::from_sorted(&values, 50, P50_MINIMUM_SAMPLES),
            p95: CodeIndexPercentileV1::from_sorted(&values, 95, P95_MINIMUM_SAMPLES),
            p99: CodeIndexPercentileV1::from_sorted(&values, 99, P99_MINIMUM_SAMPLES),
        }
    }
}

/// Nearest-rank percentile over an ascending slice.
fn nearest_rank(sorted: &[i64], percentile: usize) -> Option<i64> {
    let last = sorted.len().checked_sub(1)?;
    let index = last.saturating_mul(percentile).div_ceil(100);
    sorted.get(index).copied()
}

/// Bounded truthful cadence read model over the retained receipt ring.
///
/// Latency percentiles are computed only from receipts with an observed arrival.
/// `arrival_unavailable_count` keeps the withheld receipts visible so a caller
/// can tell a small admissible population from a large inadmissible one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexCadenceReadModelV1 {
    /// Receipts retained in the ring.
    pub retained_count: usize,
    /// Ring capacity, so a caller can see the ceiling on any percentile.
    pub capacity: usize,
    /// Receipts whose arrival was observed and therefore carry latency.
    pub latency_sample_count: usize,
    /// Receipts whose arrival was unavailable and contribute no latency.
    pub arrival_unavailable_count: usize,
    pub published_count: usize,
    pub noop_count: usize,
    pub overflow_reconciled_count: usize,
    pub event_to_ready_micros: CodeIndexPercentilesV1,
    pub queue_delay_micros: CodeIndexPercentilesV1,
    /// Service time is measured for every receipt, including those whose
    /// arrival is unavailable.
    pub service_micros: CodeIndexPercentilesV1,
}

/// Bounded ring of recent event-to-ready receipts.
///
/// Capacity is at least [`P99_MINIMUM_SAMPLES`] so a retained population can
/// actually reach p99 eligibility; a shorter ring would make p99 permanently
/// unavailable by construction.
#[derive(Debug, Default)]
pub(crate) struct CodeIndexCadenceTelemetryV1 {
    receipts: VecDeque<CodeIndexEventToReadyReceiptV1>,
}

impl CodeIndexCadenceTelemetryV1 {
    pub(crate) const CAPACITY: usize = 128;

    const _CAPACITY_SUPPORTS_P99: () = assert!(
        Self::CAPACITY >= P99_MINIMUM_SAMPLES,
        "cadence ring must be able to hold a p99-eligible population"
    );

    pub(crate) fn record(&mut self, receipt: CodeIndexEventToReadyReceiptV1) {
        while self.receipts.len() >= Self::CAPACITY {
            self.receipts.pop_front();
        }
        self.receipts.push_back(receipt);
    }

    #[cfg(test)]
    pub(crate) fn latest(&self) -> Option<&CodeIndexEventToReadyReceiptV1> {
        self.receipts.back()
    }

    #[cfg(test)]
    pub(crate) fn receipts(
        &self,
    ) -> impl ExactSizeIterator<Item = &CodeIndexEventToReadyReceiptV1> {
        self.receipts.iter()
    }

    /// Number of retained receipts carrying an observed arrival.
    pub(crate) fn latency_sample_count(&self) -> usize {
        self.receipts
            .iter()
            .filter(|receipt| receipt.arrival.wake_micros().is_some())
            .count()
    }

    /// Aggregate the retained ring into the bounded truthful read model.
    pub(crate) fn read_model(&self) -> CodeIndexCadenceReadModelV1 {
        let mut event_to_ready = Vec::with_capacity(self.receipts.len());
        let mut queue_delay = Vec::with_capacity(self.receipts.len());
        let mut service = Vec::with_capacity(self.receipts.len());
        let mut published_count = 0;
        let mut noop_count = 0;
        let mut overflow_reconciled_count = 0;
        let mut arrival_unavailable_count = 0;

        for receipt in &self.receipts {
            service.push(receipt.service_micros());
            if receipt.is_noop() {
                noop_count += 1;
            } else {
                published_count += 1;
            }
            if receipt.overflow_reconciled {
                overflow_reconciled_count += 1;
            }
            match (
                receipt.event_to_ready_micros(),
                receipt.queue_delay_micros(),
            ) {
                (Some(latency), Some(delay)) => {
                    event_to_ready.push(latency);
                    queue_delay.push(delay);
                }
                _ => arrival_unavailable_count += 1,
            }
        }

        CodeIndexCadenceReadModelV1 {
            retained_count: self.receipts.len(),
            capacity: Self::CAPACITY,
            latency_sample_count: event_to_ready.len(),
            arrival_unavailable_count,
            published_count,
            noop_count,
            overflow_reconciled_count,
            event_to_ready_micros: CodeIndexPercentilesV1::from_values(event_to_ready),
            queue_delay_micros: CodeIndexPercentilesV1::from_values(queue_delay),
            service_micros: CodeIndexPercentilesV1::from_values(service),
        }
    }
}

/// Whether adding one latency sample newly crossed a percentile floor.
///
/// The scheduler emits the aggregate read model exactly when a percentile
/// becomes eligible, so aggregate telemetry stays bounded to a few lines per
/// ring cycle instead of one line per reconcile.
pub(crate) fn newly_eligible_percentile(latency_sample_count: usize) -> Option<&'static str> {
    match latency_sample_count {
        P50_MINIMUM_SAMPLES => Some("p50"),
        P95_MINIMUM_SAMPLES => Some("p95"),
        P99_MINIMUM_SAMPLES => Some("p99"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tracedecay_domain::ContentDigest;

    fn digest(seed: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("digest")
    }

    fn receipt(
        arrival: CodeIndexArrivalV1,
        started_micros: i64,
        ready_micros: i64,
    ) -> CodeIndexEventToReadyReceiptV1 {
        CodeIndexEventToReadyReceiptV1::new(
            PathBuf::from("/tmp/project"),
            CodeIndexCadenceTriggerV1::HookHint,
            arrival,
            started_micros,
            ready_micros,
            CodeIndexCadenceOutcomeV1::Noop {
                snapshot_content_identity: digest('a'),
            },
            false,
        )
    }

    #[test]
    fn queue_delay_is_arrival_to_dequeue_and_service_is_dequeue_to_ready() {
        // Arrival at 100, dequeued at 400, terminal at 900: the wait in the
        // queue and the work done after dequeue are different measurements.
        let receipt = receipt(CodeIndexArrivalV1::Observed { wake_micros: 100 }, 400, 900);
        assert_eq!(receipt.queue_delay_micros(), Some(300));
        assert_eq!(receipt.service_micros(), 500);
        assert_eq!(receipt.event_to_ready_micros(), Some(800));
        assert!(
            receipt.queue_delay_micros() < receipt.event_to_ready_micros(),
            "queue delay must be strictly less than total latency when the \
             scheduler spent time reconciling after dequeue"
        );
    }

    #[test]
    fn unavailable_arrival_withholds_latency_instead_of_reporting_zero() {
        let receipt = receipt(CodeIndexArrivalV1::Unavailable, 400, 900);
        assert_eq!(receipt.queue_delay_micros(), None);
        assert_eq!(receipt.event_to_ready_micros(), None);
        // Service time is still authoritative: the scheduler stamped both ends.
        assert_eq!(receipt.service_micros(), 500);
    }

    #[test]
    fn unavailable_arrival_is_counted_but_never_enters_the_latency_population() {
        let mut telemetry = CodeIndexCadenceTelemetryV1::default();
        for _ in 0..4 {
            telemetry.record(receipt(CodeIndexArrivalV1::Unavailable, 400, 900));
        }
        telemetry.record(receipt(
            CodeIndexArrivalV1::Observed { wake_micros: 100 },
            400,
            900,
        ));

        let read_model = telemetry.read_model();
        assert_eq!(read_model.retained_count, 5);
        assert_eq!(read_model.arrival_unavailable_count, 4);
        assert_eq!(read_model.latency_sample_count, 1);
        assert_eq!(read_model.event_to_ready_micros.sample_count, 1);
        // One sample is below every floor, so no percentile may be published —
        // and none of the four withheld receipts contributed a zero.
        assert_eq!(read_model.event_to_ready_micros.p50.value, None);
        assert_eq!(read_model.queue_delay_micros.p50.value, None);
        // Service time is measured for all five.
        assert_eq!(read_model.service_micros.sample_count, 5);
        assert_eq!(read_model.service_micros.p50.value, Some(500));
    }

    #[test]
    fn out_of_order_clock_reading_cannot_produce_negative_latency() {
        let receipt = receipt(CodeIndexArrivalV1::Observed { wake_micros: 900 }, 400, 500);
        assert_eq!(receipt.queue_delay_micros(), Some(0));
        assert_eq!(receipt.event_to_ready_micros(), Some(0));
        assert_eq!(receipt.service_micros(), 100);
    }

    #[test]
    fn percentiles_become_available_only_at_their_declared_floors() {
        let mut telemetry = CodeIndexCadenceTelemetryV1::default();
        for index in 0..(P99_MINIMUM_SAMPLES - 1) {
            let wake = i64::try_from(index).expect("fixture index fits i64");
            telemetry.record(receipt(
                CodeIndexArrivalV1::Observed { wake_micros: wake },
                wake + 10,
                wake + 20,
            ));
        }
        let below = telemetry.read_model();
        assert_eq!(below.latency_sample_count, P99_MINIMUM_SAMPLES - 1);
        assert!(below.event_to_ready_micros.p50.is_available());
        assert!(below.event_to_ready_micros.p95.is_available());
        assert!(
            !below.event_to_ready_micros.p99.is_available(),
            "p99 must stay unavailable at 99 matching samples"
        );
        assert_eq!(
            below.event_to_ready_micros.p99.minimum_samples,
            P99_MINIMUM_SAMPLES
        );

        let wake = i64::try_from(P99_MINIMUM_SAMPLES).expect("fixture count fits i64");
        telemetry.record(receipt(
            CodeIndexArrivalV1::Observed { wake_micros: wake },
            wake + 10,
            wake + 20,
        ));
        let at_floor = telemetry.read_model();
        assert_eq!(at_floor.latency_sample_count, P99_MINIMUM_SAMPLES);
        assert!(
            at_floor.event_to_ready_micros.p99.is_available(),
            "p99 must become available at exactly 100 matching samples"
        );
    }

    #[test]
    fn ring_capacity_can_hold_a_p99_eligible_population() {
        const {
            assert!(CodeIndexCadenceTelemetryV1::CAPACITY >= P99_MINIMUM_SAMPLES);
        }
        let mut telemetry = CodeIndexCadenceTelemetryV1::default();
        for index in 0..(CodeIndexCadenceTelemetryV1::CAPACITY + 3) {
            let wake = i64::try_from(index).expect("fixture index fits i64");
            telemetry.record(receipt(
                CodeIndexArrivalV1::Observed { wake_micros: wake },
                wake + 10,
                wake + 20,
            ));
        }
        assert_eq!(
            telemetry.receipts().len(),
            CodeIndexCadenceTelemetryV1::CAPACITY
        );
        assert_eq!(
            telemetry.latency_sample_count(),
            CodeIndexCadenceTelemetryV1::CAPACITY
        );
        assert!(
            telemetry
                .read_model()
                .event_to_ready_micros
                .p99
                .is_available()
        );
        // Oldest receipts were evicted, newest retained.
        assert_eq!(
            telemetry
                .latest()
                .and_then(|receipt| receipt.arrival.wake_micros()),
            i64::try_from(CodeIndexCadenceTelemetryV1::CAPACITY + 2).ok()
        );
    }

    #[test]
    fn eligibility_milestones_fire_once_per_floor() {
        assert_eq!(newly_eligible_percentile(1), None);
        assert_eq!(newly_eligible_percentile(P50_MINIMUM_SAMPLES), Some("p50"));
        assert_eq!(newly_eligible_percentile(P50_MINIMUM_SAMPLES + 1), None);
        assert_eq!(newly_eligible_percentile(P95_MINIMUM_SAMPLES), Some("p95"));
        assert_eq!(newly_eligible_percentile(P99_MINIMUM_SAMPLES), Some("p99"));
    }
}
