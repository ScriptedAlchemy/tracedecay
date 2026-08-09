//! Canonical Plan 26 Observatory panel projections.
//!
//! Only validated envelopes from the registered observability authority reach
//! this projector. Every required numeric dimension is emitted even when its
//! source has not been observed, which lets transports distinguish unknown
//! evidence from an unmounted contract.

mod product_views;
mod workflow_views;

use tracedecay_application::{
    AnalyticsModeReadModelV1, ComparisonDispositionV1, MetricCoverageV1, MetricSourceV1,
    MetricValueV1, ObservabilityHorizonV1, PerformanceComparisonReadModelV1,
};
use tracedecay_domain::{
    CoverageStateV1, LatencyStageV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
};

use super::{MeasurementDescriptor, MeasurementProvenance, MeasurementSpec, measurement};
use product_views::product_view_metrics;
use workflow_views::workflow_metrics;

const ADOPTION_DESCRIPTOR: &str = "adoption-outcomes.v1";
const RETRIEVAL_DESCRIPTOR: &str = "retrieval-quality.v1";
const PERFORMANCE_DESCRIPTOR: &str = "performance-budgets.v1";
const CONTROLS_DESCRIPTOR: &str = "analytics-controls.v1";
const COMPARISON_DESCRIPTOR: &str = "performance-comparisons.v1";
const PROJECTOR_REVISION: &str = "observatory-plan26-projector.v1";

struct MetricSpec<'a> {
    descriptor: &'a str,
    name: &'a str,
    unit: &'a str,
    denominator: &'a str,
    value: Option<f64>,
    eligible: Option<u64>,
    observed: u64,
    censored: u64,
    unknown: u64,
    state: CoverageStateV1,
    reason: Option<&'a str>,
}

fn metric(
    spec: MetricSpec<'_>,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
) -> MetricValueV1 {
    measurement(MeasurementSpec {
        descriptor: MeasurementDescriptor::new(
            spec.descriptor,
            spec.name,
            spec.unit,
            spec.denominator,
        ),
        provenance: MeasurementProvenance::new(
            MetricSourceV1::ObservabilityEnvelope,
            "observability-envelope.v1",
            PROJECTOR_REVISION,
            watermark,
        ),
        horizon,
        coverage: MetricCoverageV1 {
            eligible: spec.eligible,
            observed: spec.observed,
            completed: spec.observed,
            censored: spec.censored,
            unknown: spec.unknown,
            excluded: 0,
            state: spec.state,
        },
        value: spec.value,
        unavailable_reason: spec.reason,
    })
}

fn unknown_metric(
    descriptor: &'static str,
    name: &'static str,
    unit: &'static str,
    denominator: &'static str,
    reason: &'static str,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
) -> MetricValueV1 {
    metric(
        MetricSpec {
            descriptor,
            name,
            unit,
            denominator,
            value: None,
            eligible: None,
            observed: 0,
            censored: 0,
            unknown: 1,
            state: CoverageStateV1::Unknown,
            reason: Some(reason),
        },
        horizon,
        watermark,
    )
}

fn unknown_mode(reason: &str) -> AnalyticsModeReadModelV1 {
    AnalyticsModeReadModelV1 {
        current: None,
        transition_watermark: None,
        coverage: unknown_coverage(),
        unavailable_reason: Some(reason.to_owned()),
    }
}

fn unknown_comparison(reason: &str) -> PerformanceComparisonReadModelV1 {
    PerformanceComparisonReadModelV1 {
        baseline_build: None,
        candidate_build: None,
        workload: None,
        corpus: None,
        environment: None,
        oracle: None,
        configuration: None,
        platform: None,
        rollback_profile: None,
        eligible_outcomes: None,
        paired_outcomes: None,
        regression_observed: None,
        disposition: ComparisonDispositionV1::InsufficientEvidence,
        coverage: unknown_coverage(),
        unavailable_reason: Some(reason.to_owned()),
    }
}

const fn unknown_coverage() -> MetricCoverageV1 {
    MetricCoverageV1 {
        eligible: None,
        observed: 0,
        completed: 0,
        censored: 0,
        unknown: 1,
        excluded: 0,
        state: CoverageStateV1::Unknown,
    }
}

pub(super) fn unavailable_plan26_read_models(
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    reason: &str,
) -> (
    AnalyticsModeReadModelV1,
    PerformanceComparisonReadModelV1,
    Vec<MetricValueV1>,
) {
    let mut metrics = required_metrics(&[], horizon, watermark, true, 1);
    for value in &mut metrics {
        value.unavailable_reason = Some(reason.to_owned());
        value.uncertainty.reason = Some(reason.to_owned());
    }
    (unknown_mode(reason), unknown_comparison(reason), metrics)
}

pub(super) fn project_plan26_read_models(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_state: CoverageStateV1,
    source_unknown: u64,
) -> (
    AnalyticsModeReadModelV1,
    PerformanceComparisonReadModelV1,
    Vec<MetricValueV1>,
) {
    let source_complete = source_state == CoverageStateV1::Known;
    let mut metrics = required_metrics(events, horizon, watermark, source_complete, source_unknown);
    if !source_complete {
        for metric in &mut metrics {
            metric.coverage.state = source_state;
            metric.unavailable_reason = Some("incomplete_observability_coverage".to_owned());
            metric.uncertainty.reason = Some("incomplete_observability_coverage".to_owned());
        }
    }
    let mut analytics_mode = analytics_mode(events, source_complete);
    if !source_complete {
        analytics_mode.coverage.state = source_state;
    }
    (
        analytics_mode,
        unknown_comparison("comparison_evidence_not_recorded"),
        metrics,
    )
}

fn required_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let mut metrics = adoption_metrics(events, horizon, watermark, source_complete, source_unknown);
    metrics.extend(retrieval_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(performance_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(control_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(comparison_metrics(horizon, watermark));
    metrics.extend(product_view_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(workflow_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics
}

fn adoption_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let mut stages = [0_u64; 9];
    let mut eligibility_records = 0_u64;
    let mut outcome_records = 0_u64;
    for event in events {
        match &event.payload {
            ObservabilityPayloadV1::AdoptionEligibility(value) => {
                eligibility_records = eligibility_records.saturating_add(1);
                stages[0] = stages[0].saturating_add(value.eligible);
                stages[1] = stages[1].saturating_add(value.enabled);
                stages[2] = stages[2].saturating_add(value.available);
            }
            ObservabilityPayloadV1::AdoptionOutcome(value) => {
                outcome_records = outcome_records.saturating_add(1);
                stages[3] = stages[3].saturating_add(value.invoked);
                stages[4] = stages[4].saturating_add(value.terminal);
                stages[5] = stages[5].saturating_add(value.independently_useful);
                stages[6] = stages[6].saturating_add(value.repeat_useful);
                stages[7] = stages[7].saturating_add(value.censored);
                stages[8] = stages[8].saturating_add(value.unknown);
            }
            _ => {}
        }
    }
    let eligibility_complete = source_complete && eligibility_records > 0;
    let outcome_complete = source_complete && eligibility_records > 0 && outcome_records > 0;
    let names = [
        "adoption_eligible",
        "adoption_enabled",
        "adoption_available",
        "adoption_invoked",
        "adoption_terminal",
        "adoption_independently_useful",
        "adoption_repeat_useful",
        "adoption_censored_outcomes",
        "adoption_unknown_outcomes",
    ];
    let mut metrics = names
        .into_iter()
        .zip(stages)
        .enumerate()
        .map(|(index, (name, value))| {
            let complete = if index < 3 {
                eligibility_complete
            } else {
                outcome_complete
            };
            let reason = (!complete).then_some(if !source_complete {
                "incomplete_observability_coverage"
            } else if index < 3 {
                "adoption_eligibility_not_recorded"
            } else if eligibility_records == 0 {
                "adoption_eligibility_not_recorded"
            } else {
                "adoption_outcomes_not_recorded"
            });
            metric(
                MetricSpec {
                    descriptor: ADOPTION_DESCRIPTOR,
                    name,
                    unit: "events",
                    denominator: "eligible_adoption_units",
                    value: complete.then_some(value as f64),
                    eligible: complete.then_some(stages[0]),
                    observed: if complete { value } else { 0 },
                    censored: if index < 3 { 0 } else { stages[7] },
                    unknown: if complete {
                        if index < 3 { 0 } else { stages[8] }
                    } else {
                        source_unknown.max(1)
                    },
                    state: missing_state(source_complete, complete),
                    reason,
                },
                horizon,
                watermark,
            )
        })
        .collect::<Vec<_>>();
    metrics.push(unknown_metric(
        ADOPTION_DESCRIPTOR,
        "adoption_correct_abstention",
        "events",
        "eligible_abstentions",
        "correct_abstention_not_recorded",
        horizon,
        watermark,
    ));
    metrics
}

fn retrieval_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let mut counts = [0_u64; 5];
    let mut retrievers = 0_u64;
    let mut outcomes = 0_u64;
    let mut linked = 0_u64;
    let mut censored = 0_u64;
    let mut ablations = Vec::new();
    for event in events {
        match &event.payload {
            ObservabilityPayloadV1::Retriever(value) => {
                retrievers = retrievers.saturating_add(1);
                counts[0] = counts[0].saturating_add(value.requested_candidates);
                counts[1] = counts[1].saturating_add(value.consumed_candidates);
                counts[2] = counts[2].saturating_add(value.eligible_candidates);
                counts[3] = counts[3].saturating_add(value.returned_candidates);
                counts[4] = counts[4].saturating_add(value.unique_contributions);
            }
            ObservabilityPayloadV1::ContextOutcome(value) => {
                outcomes = outcomes.saturating_add(1);
                linked = linked.saturating_add(u64::from(value.independently_observed));
                censored = censored.saturating_add(u64::from(value.censored));
            }
            ObservabilityPayloadV1::RetrievalAblation(value) => ablations.push(value),
            _ => {}
        }
    }
    let retriever_complete = source_complete && retrievers > 0;
    let retriever = |name, value, denominator, eligible| {
        metric(
            MetricSpec {
                descriptor: RETRIEVAL_DESCRIPTOR,
                name,
                unit: "candidates",
                denominator,
                value: retriever_complete.then_some(value as f64),
                eligible: retriever_complete.then_some(eligible),
                observed: if retriever_complete { value } else { 0 },
                censored: 0,
                unknown: if retriever_complete {
                    0
                } else {
                    source_unknown.max(1)
                },
                state: missing_state(source_complete, retriever_complete),
                reason: (!retriever_complete).then_some(if source_complete {
                    "retriever_observations_not_recorded"
                } else {
                    "incomplete_observability_coverage"
                }),
            },
            horizon,
            watermark,
        )
    };
    let mut metrics = vec![
        retriever(
            "retriever_consumed_candidates",
            counts[1],
            "requested_candidates",
            counts[0],
        ),
        retriever(
            "retriever_returned_candidates",
            counts[3],
            "eligible_candidates",
            counts[2],
        ),
        retriever(
            "retriever_unique_contributions",
            counts[4],
            "returned_candidates",
            counts[3],
        ),
        unknown_metric(
            RETRIEVAL_DESCRIPTOR,
            "retriever_candidate_rank",
            "rank",
            "returned_candidates",
            "candidate_rank_not_recorded",
            horizon,
            watermark,
        ),
    ];
    for (name, unit, reason) in [
        (
            "retrieval_planner_span_p95",
            "microseconds",
            "planner_duration_not_recorded",
        ),
        (
            "retrieval_fanout_span_p95",
            "microseconds",
            "fanout_duration_not_recorded",
        ),
        (
            "retrieval_synthesis_span_p95",
            "microseconds",
            "synthesis_duration_not_recorded",
        ),
        (
            "retrieval_context_precision",
            "ratio",
            "context_contribution_not_recorded",
        ),
    ] {
        metrics.push(unknown_metric(
            RETRIEVAL_DESCRIPTOR,
            name,
            unit,
            "eligible_retrieval_observations",
            reason,
            horizon,
            watermark,
        ));
    }
    let linkage_complete = source_complete && outcomes > 0;
    metrics.push(metric(
        MetricSpec {
            descriptor: RETRIEVAL_DESCRIPTOR,
            name: "retrieval_task_outcome_linkage",
            unit: "ratio",
            denominator: "context_outcome_observations",
            value: linkage_complete.then_some(linked as f64 / outcomes as f64),
            eligible: linkage_complete.then_some(outcomes),
            observed: if linkage_complete { linked } else { 0 },
            censored,
            unknown: if linkage_complete {
                0
            } else {
                source_unknown.max(1)
            },
            state: missing_state(source_complete, linkage_complete),
            reason: (!linkage_complete).then_some("context_outcomes_not_recorded"),
        },
        horizon,
        watermark,
    ));
    let ablation_complete = source_complete
        && !ablations.is_empty()
        && ablations.iter().all(|value| {
            value.coverage == CoverageStateV1::Known
                && value.unit == ablations[0].unit
                && value.descriptor_revision == ablations[0].descriptor_revision
        });
    let ablation_value = ablation_complete.then(|| {
        ablations
            .iter()
            .map(|value| value.candidate_value - value.baseline_value)
            .sum::<f64>()
            / ablations.len() as f64
    });
    let mut ablation = metric(
        MetricSpec {
            descriptor: RETRIEVAL_DESCRIPTOR,
            name: "retrieval_equal_budget_ablation",
            unit: ablations
                .first()
                .map_or("ratio", |value| value.unit.as_str()),
            denominator: "equal_budget_ablation_observations",
            value: ablation_value,
            eligible: ablation_complete.then_some(ablations.len() as u64),
            observed: if ablation_complete {
                ablations.len() as u64
            } else {
                0
            },
            censored: 0,
            unknown: if ablation_complete {
                0
            } else {
                source_unknown.max(1)
            },
            state: missing_state(source_complete, ablation_complete),
            reason: (!ablation_complete).then_some(if ablations.is_empty() {
                "ablation_observations_not_recorded"
            } else {
                "ablation_evidence_incompatible"
            }),
        },
        horizon,
        watermark,
    );
    if ablation_complete {
        ablation.descriptor_revision = ablations[0].descriptor_revision.clone();
    }
    metrics.push(ablation);
    metrics
}

fn performance_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let resources = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::OperationResource(value) => Some(value.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let latencies = resources
        .iter()
        .map(|value| value.service_latency_micros)
        .collect::<Vec<_>>();
    let mut metrics = Vec::new();
    for (name, percentile_rank) in [
        ("operation_latency_p50", 50_u64),
        ("operation_latency_p95", 95_u64),
        ("operation_latency_p99", 99_u64),
    ] {
        metrics.push(sample_metric(
            name,
            "microseconds",
            "operation_resource_observations",
            percentile(&latencies, percentile_rank).map(|value| value as f64),
            latencies.len() as u64,
            source_complete,
            source_unknown,
            "operation_resource_observations_not_recorded",
            horizon,
            watermark,
        ));
    }
    for (stage, name) in [
        (LatencyStageV1::Queue, "queue_span_p95"),
        (LatencyStageV1::StoreLock, "store_lock_span_p95"),
        (LatencyStageV1::IndexLock, "index_lock_span_p95"),
        (
            LatencyStageV1::ProviderNegotiation,
            "provider_negotiation_span_p95",
        ),
    ] {
        let samples = events
            .iter()
            .filter_map(|event| match &event.payload {
                ObservabilityPayloadV1::Latency(value) if value.stage == stage => {
                    Some(value.service_micros)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        metrics.push(sample_metric(
            name,
            "microseconds",
            "latency_stage_observations",
            percentile(&samples, 95).map(|value| value as f64),
            samples.len() as u64,
            source_complete,
            source_unknown,
            "latency_stage_observations_not_recorded",
            horizon,
            watermark,
        ));
    }
    let rss = resources
        .iter()
        .filter_map(|value| value.process_rss_bytes)
        .collect::<Vec<_>>();
    metrics.push(sample_metric(
        "process_rss_peak",
        "bytes",
        "operation_resource_observations",
        rss.iter().copied().max().map(|value| value as f64),
        rss.len() as u64,
        source_complete,
        source_unknown,
        "process_rss_not_recorded",
        horizon,
        watermark,
    ));
    let cpu = resources.iter().try_fold(0_u64, |total, value| {
        total.checked_add(
            value
                .cpu_user_micros?
                .checked_add(value.cpu_system_micros?)?,
        )
    });
    metrics.push(sample_metric(
        "cpu_time_total",
        "microseconds",
        "operation_resource_observations",
        cpu.filter(|_| !resources.is_empty())
            .map(|value| value as f64),
        resources.len() as u64,
        source_complete,
        source_unknown,
        "cpu_time_not_recorded",
        horizon,
        watermark,
    ));
    let io = resources
        .iter()
        .try_fold((0_u64, 0_u64), |(physical, logical), value| {
            Some((
                physical.checked_add(value.read_bytes?.checked_add(value.write_bytes?)?)?,
                logical.checked_add(value.input_bytes?.checked_add(value.output_bytes?)?)?,
            ))
        });
    let io_value = io
        .and_then(|(physical, logical)| (logical > 0).then_some(physical as f64 / logical as f64));
    metrics.push(sample_metric(
        "io_amplification",
        "ratio",
        "logical_io_bytes",
        io_value,
        resources.len() as u64,
        source_complete,
        source_unknown,
        "io_amplification_not_recorded",
        horizon,
        watermark,
    ));
    let no_progress = events
        .iter()
        .filter(|event| matches!(&event.payload, ObservabilityPayloadV1::NoProgress(_)))
        .count() as u64;
    metrics.push(sample_metric(
        "no_progress_outcomes",
        "events",
        "no_progress_observations",
        (no_progress > 0).then_some(no_progress as f64),
        no_progress,
        source_complete,
        source_unknown,
        "no_progress_observations_not_recorded",
        horizon,
        watermark,
    ));
    metrics.push(unknown_metric(
        PERFORMANCE_DESCRIPTOR,
        "accepted_budget_revision",
        "revision",
        "accepted_performance_budgets",
        "accepted_budget_revision_not_recorded",
        horizon,
        watermark,
    ));
    metrics
}

#[allow(clippy::too_many_arguments)]
fn sample_metric(
    name: &'static str,
    unit: &'static str,
    denominator: &'static str,
    value: Option<f64>,
    support: u64,
    source_complete: bool,
    source_unknown: u64,
    missing_reason: &'static str,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
) -> MetricValueV1 {
    let complete = source_complete && value.is_some();
    metric(
        MetricSpec {
            descriptor: PERFORMANCE_DESCRIPTOR,
            name,
            unit,
            denominator,
            value: complete.then_some(value).flatten(),
            eligible: complete.then_some(support),
            observed: if complete { support } else { 0 },
            censored: 0,
            unknown: if complete { 0 } else { source_unknown.max(1) },
            state: missing_state(source_complete, complete),
            reason: (!complete).then_some(if value.is_some() {
                "incomplete_observability_coverage"
            } else {
                missing_reason
            }),
        },
        horizon,
        watermark,
    )
}

fn analytics_mode(
    events: &[&ObservabilityEnvelopeV1],
    source_complete: bool,
) -> AnalyticsModeReadModelV1 {
    let latest = latest_consent(events);
    match latest {
        Some((event, value)) if source_complete => AnalyticsModeReadModelV1 {
            current: Some(value.current),
            transition_watermark: Some(event.watermark.clone()),
            coverage: MetricCoverageV1 {
                eligible: Some(1),
                observed: 1,
                completed: 1,
                censored: 0,
                unknown: 0,
                excluded: 0,
                state: CoverageStateV1::Known,
            },
            unavailable_reason: None,
        },
        Some(_) => unknown_mode("incomplete_observability_coverage"),
        None => unknown_mode("analytics_consent_not_observed"),
    }
}

fn control_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let latest = latest_consent(events);
    let staging = latest.and_then(|(_, value)| value.share_staging_age_seconds);
    let complete = source_complete && staging.is_some();
    vec![
        metric(
            MetricSpec {
                descriptor: CONTROLS_DESCRIPTOR,
                name: "analytics_share_staging_age_seconds",
                unit: "seconds",
                denominator: "latest_analytics_consent",
                value: complete
                    .then_some(staging)
                    .flatten()
                    .map(|value| value as f64),
                eligible: complete.then_some(1),
                observed: u64::from(complete),
                censored: 0,
                unknown: u64::from(!complete).max(source_unknown),
                state: missing_state(source_complete, complete),
                reason: (!complete).then_some(if latest.is_some() {
                    "share_staging_age_not_observed"
                } else {
                    "analytics_consent_not_observed"
                }),
            },
            horizon,
            watermark,
        ),
        unknown_metric(
            CONTROLS_DESCRIPTOR,
            "analytics_egress_failures",
            "events",
            "analytics_egress_attempts",
            "egress_attempts_not_recorded",
            horizon,
            watermark,
        ),
    ]
}

fn latest_consent<'a>(
    events: &'a [&'a ObservabilityEnvelopeV1],
) -> Option<(
    &'a ObservabilityEnvelopeV1,
    &'a tracedecay_domain::AnalyticsConsentChangedV1,
)> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::AnalyticsConsent(value) => Some((*event, value)),
            _ => None,
        })
        .max_by_key(|(event, _)| {
            (
                event.event_time_micros,
                event.observation_time_micros,
                event.producer_sequence,
            )
        })
}

fn comparison_metrics(horizon: &ObservabilityHorizonV1, watermark: &str) -> Vec<MetricValueV1> {
    [
        "comparison_baseline_build",
        "comparison_candidate_build",
        "comparison_workload_corpus",
        "comparison_environment_platform",
        "comparison_oracle",
        "comparison_rollback_profile",
        "comparison_outcome_counts",
        "comparison_stratum_support",
        "comparison_intervals",
        "comparison_calibration",
        "comparison_risk_coverage",
        "comparison_flaky_indeterminate",
        "comparison_deviations",
        "comparison_paired_outcomes",
    ]
    .into_iter()
    .map(|name| {
        unknown_metric(
            COMPARISON_DESCRIPTOR,
            name,
            "events",
            "eligible_comparison_outcomes",
            "comparison_evidence_not_recorded",
            horizon,
            watermark,
        )
    })
    .collect()
}

fn percentile(samples: &[u64], percentile: u64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = percentile
        .saturating_mul(sorted.len() as u64)
        .saturating_add(99)
        .div_euclid(100)
        .max(1);
    sorted.get(rank.saturating_sub(1) as usize).copied()
}

const fn missing_state(source_complete: bool, metric_complete: bool) -> CoverageStateV1 {
    if metric_complete {
        CoverageStateV1::Known
    } else if source_complete {
        CoverageStateV1::Unknown
    } else {
        CoverageStateV1::Partial
    }
}
