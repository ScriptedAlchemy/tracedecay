//! Event ingest and the per-family projection entry point.
//!
//! The `project_*` methods live in two sibling modules purely to keep each
//! file under the AGENTS.md size limit: `capacity_families` holds the
//! concurrency, useful-work, fan-out width, duplicate-effort and conflict
//! families, and `lifecycle_families` holds integration, merge, drift,
//! blocked, rerun, leak and delivery. Both are `impl` blocks on the same
//! [`ExecutionTopologyEvidenceV1`] declared here.
mod capacity_families;
mod lifecycle_families;

use std::collections::BTreeMap;

use tracedecay_domain::{
    BlockedCauseV1, ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1, CoverageStateV1,
    DeliverySurfaceFamilyV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
    IntegrationOperationKindV1, IntegrationPhaseV1, IntegrationResultV1, IntervalStateV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, RerunCauseV1, RerunSourceV1, StackDriftKindV1,
    WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1,
};

use crate::clock::now_micros;
use crate::observability::{
    MetricCoverageV1, ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
};
use crate::work::work_authority;
use crate::{ApplicationProblem, RequestContext};

use super::support::{bounded_interval, count, family_state, invalid_problem, unavailable_model};
use super::{
    EXECUTION_TOPOLOGY_EVENT_KINDS_V1, ExecutionDurationBucketV1, ExecutionMetricUnavailableV1,
    ExecutionTopologyMeasurementV1, ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    MAX_EXECUTION_TOPOLOGY_EVENTS_V1,
};

/// Reads one execution-topology metrics model for the authorized scope.
///
/// Authorization runs first and uses the same authority the Work surfaces do,
/// so a concealed scope is refused before any observation is read. A store
/// that cannot answer yields a fully typed-unavailable model rather than an
/// error, because an unreadable horizon is a coverage fact about the metrics,
/// not a caller mistake.
///
/// # Errors
///
/// Returns [`ApplicationProblem::InvalidRequest`] for an inverted horizon or
/// an out-of-range event budget, and the authority's own refusal when the
/// request context does not carry a valid Work authority for the scope.
pub async fn execution_topology_metrics<Q>(
    observations: &Q,
    context: &RequestContext,
    request: &ExecutionTopologyMetricsRequestV1,
) -> Result<ExecutionTopologyMetricsV1, ApplicationProblem>
where
    Q: ObservabilityQueryPort,
{
    if request.horizon.until_micros <= request.horizon.since_micros {
        return Err(invalid_problem(
            "application.execution-topology-metrics.invalid-horizon",
            "The execution topology metrics horizon must end after it starts.",
        ));
    }
    if request.max_events == 0 || request.max_events > MAX_EXECUTION_TOPOLOGY_EVENTS_V1 {
        return Err(invalid_problem(
            "application.execution-topology-metrics.invalid-event-budget",
            "The execution topology metrics event budget must be between 1 and 20000.",
        ));
    }
    // Proves the grant covers this scope before any observation is read; the
    // authority value itself is never copied into a metric label.
    let _authority = work_authority(context)?;
    let authorized_scope_ref = context.scope().scope_digest.as_str().to_owned();
    let observed_at_micros = now_micros().0;

    let page = observations
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: authorized_scope_ref.clone(),
            event_kinds: EXECUTION_TOPOLOGY_EVENT_KINDS_V1
                .iter()
                .map(|kind| (*kind).to_owned())
                .collect(),
            horizon: request.horizon.clone(),
            after_watermark: None,
            limit: request.max_events,
        })
        .await;

    let page = match page {
        Ok(page) => page,
        Err(_) => {
            // The refusal text belongs to the port and may name storage
            // detail; the read model carries only the typed coverage fact.
            return Ok(unavailable_model(
                authorized_scope_ref,
                request.horizon.clone(),
                observed_at_micros,
                ExecutionMetricUnavailableV1::StoreUnavailable,
            ));
        }
    };

    if page.next_watermark.is_some() {
        return Ok(unavailable_model(
            authorized_scope_ref,
            request.horizon.clone(),
            observed_at_micros,
            ExecutionMetricUnavailableV1::EventBudgetExceeded,
        ));
    }

    let mut evidence = ExecutionTopologyEvidenceV1::default();
    for envelope in &page.events {
        if envelope.validate().is_err() {
            evidence.invalid_events = evidence.invalid_events.saturating_add(1);
            continue;
        }
        evidence.absorb(envelope);
    }

    let observed = evidence.family_observed();
    let state = family_state(page.coverage, &page.events, evidence.invalid_events);
    let complete = state == CoverageStateV1::Known;
    let family_coverage = MetricCoverageV1 {
        eligible: complete.then_some(observed),
        observed,
        completed: observed,
        censored: 0,
        unknown: evidence.invalid_events,
        excluded: 0,
        state,
    };

    let projection = ProjectionContext {
        horizon: request.horizon.clone(),
        watermark: page.watermark.clone(),
        complete,
    };
    let measurements = evidence.project(&projection);

    Ok(ExecutionTopologyMetricsV1 {
        authorized_scope_ref,
        horizon: request.horizon.clone(),
        watermark: page.watermark,
        observed_at_micros,
        current: complete,
        coverage: family_coverage,
        measurements,
    })
}

pub(super) struct ProjectionContext {
    pub(super) horizon: ObservabilityHorizonV1,
    pub(super) watermark: String,
    /// The whole family read with `Known` coverage. Every ratio, rate, and
    /// distribution below refuses without it, because a partial event
    /// population silently understates every denominator.
    pub(super) complete: bool,
}

#[derive(Clone, Copy)]
pub(super) struct TopologySampleV1 {
    pub(super) widths: [u16; 5],
    pub(super) interval_micros: Option<u64>,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone)]
pub(super) struct ConflictPredictionRowV1 {
    pub(super) kind: ConflictKindV1,
    pub(super) prediction: ConflictPredictionV1,
}

#[derive(Clone, Copy)]
pub(super) struct ConflictOutcomeRowV1 {
    pub(super) kind: ConflictKindV1,
    pub(super) outcome: ConflictOutcomeV1,
    /// Late correction revision. Only the highest revision for a prediction
    /// reference is evidence, so a corrected outcome never double counts.
    pub(super) correction_revision: u32,
}

#[derive(Clone, Copy)]
pub(super) struct IntegrationRowV1 {
    pub(super) phase: IntegrationPhaseV1,
    pub(super) result: IntegrationResultV1,
    pub(super) operation: IntegrationOperationKindV1,
    pub(super) coverage: CoverageStateV1,
    pub(super) valid_from_micros: Option<i64>,
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Copy)]
pub(super) struct DriftRowV1 {
    pub(super) kind: StackDriftKindV1,
    pub(super) state: IntervalStateV1,
    pub(super) age_bucket: ExecutionDurationBucketV1,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Copy)]
pub(super) struct DuplicateRowV1 {
    pub(super) kind: DuplicateEffortKindV1,
    pub(super) quantities: [Option<u64>; 5],
    pub(super) effect_outcome: DuplicateEffectOutcomeV1,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Copy)]
pub(super) struct BlockedRowV1 {
    pub(super) cause: BlockedCauseV1,
    pub(super) revision: u32,
    pub(super) valid_from_micros: i64,
    pub(super) valid_until_micros: Option<i64>,
}

#[derive(Clone, Copy)]
pub(super) struct RerunRowV1 {
    pub(super) source: RerunSourceV1,
    pub(super) cause: RerunCauseV1,
    pub(super) eligible: u64,
    pub(super) linked: u64,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Copy)]
pub(super) struct LeakRowV1 {
    pub(super) kind: WorkExecutionLeakKindV1,
    pub(super) recovery: WorkExecutionLeakRecoveryV1,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Copy)]
pub(super) struct FanoutRowV1 {
    pub(super) surface: DeliverySurfaceFamilyV1,
    pub(super) attempted: u64,
    pub(super) delivered: u64,
    pub(super) deduplicated: u64,
    pub(super) dropped: u64,
    pub(super) unknown: u64,
}

#[derive(Default)]
pub(super) struct ExecutionTopologyEvidenceV1 {
    pub(super) topology: Vec<TopologySampleV1>,
    pub(super) predictions: BTreeMap<String, ConflictPredictionRowV1>,
    pub(super) outcomes: BTreeMap<String, ConflictOutcomeRowV1>,
    pub(super) integrations: Vec<IntegrationRowV1>,
    pub(super) integration_traces: BTreeMap<String, Vec<IntegrationRowV1>>,
    pub(super) drifts: Vec<DriftRowV1>,
    pub(super) duplicates: Vec<DuplicateRowV1>,
    pub(super) blocked: Vec<BlockedRowV1>,
    pub(super) reruns: Vec<RerunRowV1>,
    pub(super) leaks: Vec<LeakRowV1>,
    pub(super) fanout: Vec<FanoutRowV1>,
    pub(super) invalid_events: u64,
}

impl ExecutionTopologyEvidenceV1 {
    fn absorb(&mut self, envelope: &ObservabilityEnvelopeV1) {
        let trace_id = envelope.trace_id.as_str();
        let event_time_micros = envelope.event_time_micros;
        let valid_from_micros = envelope.valid_from_micros;
        let valid_until_micros = envelope.valid_until_micros;
        match &envelope.payload {
            ObservabilityPayloadV1::ExecutionTopology(sample) => {
                self.topology.push(TopologySampleV1 {
                    widths: [
                        sample.requested_width,
                        sample.accepted_width,
                        sample.admitted_width,
                        sample.active_width,
                        sample.useful_width,
                    ],
                    interval_micros: bounded_interval(valid_from_micros, valid_until_micros),
                    // A sample's own coverage travels on the envelope: a
                    // sample read under anything but `Known` cannot anchor a
                    // duration-weighted denominator.
                    coverage: envelope.coverage,
                });
            }
            ObservabilityPayloadV1::WorkConflictPrediction(prediction) => {
                self.predictions.insert(
                    prediction.prediction_ref.clone(),
                    ConflictPredictionRowV1 {
                        kind: prediction.kind,
                        prediction: prediction.prediction,
                    },
                );
            }
            ObservabilityPayloadV1::WorkConflictOutcome(outcome) => {
                let row = ConflictOutcomeRowV1 {
                    kind: outcome.kind,
                    outcome: outcome.outcome,
                    correction_revision: outcome.correction_revision,
                };
                let superseded = match self.outcomes.get(&outcome.prediction_ref) {
                    Some(existing) => existing.correction_revision < row.correction_revision,
                    None => true,
                };
                if superseded {
                    self.outcomes.insert(outcome.prediction_ref.clone(), row);
                }
            }
            ObservabilityPayloadV1::WorkIntegrationTransition(transition) => {
                let row = IntegrationRowV1 {
                    phase: transition.phase,
                    result: transition.result,
                    operation: transition.operation,
                    coverage: transition.coverage,
                    valid_from_micros,
                    event_time_micros,
                };
                self.integrations.push(row);
                self.integration_traces
                    .entry(trace_id.to_owned())
                    .or_default()
                    .push(row);
            }
            ObservabilityPayloadV1::WorkStackDrift(drift) => {
                self.drifts.push(DriftRowV1 {
                    kind: drift.kind,
                    state: drift.state,
                    age_bucket: drift.age_bucket.into(),
                    coverage: drift.coverage,
                });
            }
            ObservabilityPayloadV1::WorkDuplicateEffort(duplicate) => {
                self.duplicates.push(DuplicateRowV1 {
                    kind: duplicate.kind,
                    quantities: [
                        duplicate.wall_micros,
                        duplicate.token_count,
                        duplicate.cost_micros,
                        duplicate.test_count,
                        duplicate.effect_count,
                    ],
                    effect_outcome: duplicate.effect_outcome,
                    coverage: duplicate.coverage,
                });
            }
            ObservabilityPayloadV1::WorkBlockedInterval(interval) => {
                self.blocked.push(BlockedRowV1 {
                    cause: interval.cause,
                    revision: interval.interval_revision,
                    valid_from_micros: interval.valid_from_micros,
                    valid_until_micros: interval.valid_until_micros,
                });
            }
            ObservabilityPayloadV1::WorkRerun(rerun) => {
                self.reruns.push(RerunRowV1 {
                    source: rerun.source,
                    cause: rerun.cause,
                    eligible: u64::from(rerun.eligible_original_count),
                    linked: u64::from(rerun.linked_rerun_count),
                    coverage: rerun.coverage,
                });
            }
            ObservabilityPayloadV1::WorkExecutionLeak(leak) => {
                self.leaks.push(LeakRowV1 {
                    kind: leak.kind,
                    recovery: leak.recovery,
                    coverage: leak.coverage,
                });
            }
            ObservabilityPayloadV1::WorkDeliveryFanout(fanout) => {
                self.fanout.push(FanoutRowV1 {
                    surface: fanout.surface,
                    attempted: u64::from(fanout.attempted),
                    delivered: u64::from(fanout.delivered),
                    deduplicated: u64::from(fanout.deduplicated),
                    dropped: u64::from(fanout.dropped),
                    unknown: u64::from(fanout.unknown),
                });
            }
            // `GitHubStackCapabilityObservedV1` is a capability observation
            // with no Plan 26 descriptor. It is read and deliberately not
            // turned into a metric rather than invented as one.
            _ => {}
        }
    }

    fn family_observed(&self) -> u64 {
        let lengths = [
            self.topology.len(),
            self.predictions.len(),
            self.outcomes.len(),
            self.integrations.len(),
            self.drifts.len(),
            self.duplicates.len(),
            self.blocked.len(),
            self.reruns.len(),
            self.leaks.len(),
            self.fanout.len(),
        ];
        let mut total = 0u64;
        for length in lengths {
            total = total.saturating_add(count(length));
        }
        total
    }

    fn project(&self, context: &ProjectionContext) -> Vec<ExecutionTopologyMeasurementV1> {
        let mut measurements = Vec::new();
        self.project_concurrency_width(context, &mut measurements);
        self.project_useful_ratio(context, &mut measurements);
        self.project_fanout_width(context, &mut measurements);
        self.project_duplicate_effort(context, &mut measurements);
        self.project_duplicate_effects(context, &mut measurements);
        self.project_conflict(context, &mut measurements);
        self.project_ready_to_integrated(context, &mut measurements);
        self.project_merge(context, &mut measurements);
        self.project_stale_stack(context, &mut measurements);
        self.project_blocked(context, &mut measurements);
        self.project_reruns(context, &mut measurements);
        self.project_leaks(context, &mut measurements);
        self.project_delivery(context, &mut measurements);
        measurements
    }
}
