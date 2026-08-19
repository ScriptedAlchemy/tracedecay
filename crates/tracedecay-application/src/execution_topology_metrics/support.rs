use tracedecay_domain::CoverageStateV1;

use crate::observability::{
    MetricCohortV1, MetricCoverageV1, MetricEvidenceClassV1, MetricProvenanceV1, MetricSourceV1,
    MetricTemporalV1, MetricUncertaintyV1, MetricValueV1, ObservabilityHorizonV1,
};
use crate::{ApplicationProblem, LegalAction, RetryDirective, SafeDiagnostic};

use super::projection::ProjectionContext;
use super::{
    CONFLICT_MIN_ADJUDICATED_CASES_V1, EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1,
    EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1, ExecutionMetricUnavailableV1,
    ExecutionTopologyDimensionV1, ExecutionTopologyMeasurementV1, ExecutionTopologyMetricsV1,
    MAX_CENSORING_RATIO_V1, MAX_METRIC_DIMENSIONS_V1, MIN_COVERAGE_RATIO_V1,
    RATE_MIN_ELIGIBLE_CASES_V1,
};

const SOURCE_REVISION_V1: &str = "observability-envelope.v1";

pub(super) struct MeasurementInput<'a> {
    pub(super) metric: &'static str,
    pub(super) unit: &'static str,
    pub(super) denominator: &'static str,
    pub(super) evidence_class: MetricEvidenceClassV1,
    pub(super) dimensions: Vec<ExecutionTopologyDimensionV1>,
    pub(super) coverage: MetricCoverageV1,
    pub(super) value: Option<f64>,
    pub(super) unavailable: Option<ExecutionMetricUnavailableV1>,
    pub(super) context: &'a ProjectionContext,
}

pub(super) fn measurement(input: MeasurementInput<'_>) -> ExecutionTopologyMeasurementV1 {
    let MeasurementInput {
        metric,
        unit,
        denominator,
        evidence_class,
        mut dimensions,
        coverage,
        value,
        unavailable,
        context,
    } = input;
    dimensions.truncate(MAX_METRIC_DIMENSIONS_V1);
    // A value and a typed absence are mutually exclusive by construction, so
    // a reader can never see both, or neither.
    let (value, unavailable) = match (value, unavailable) {
        (Some(value), None) => (Some(value), None),
        (_, Some(reason)) => (None, Some(reason)),
        (None, None) => (None, Some(ExecutionMetricUnavailableV1::NoEligibleEvidence)),
    };
    let uncertainty = match value {
        Some(value) => MetricUncertaintyV1 {
            lower: Some(value),
            upper: Some(value),
            reason: None,
        },
        None => MetricUncertaintyV1 {
            lower: None,
            upper: None,
            reason: unavailable.map(|reason| reason.as_str().to_owned()),
        },
    };
    let local_support = coverage.eligible.unwrap_or(coverage.observed);
    ExecutionTopologyMeasurementV1 {
        dimensions,
        unavailable,
        value: MetricValueV1 {
            descriptor_revision: EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1.to_owned(),
            metric: metric.to_owned(),
            value,
            unit: unit.to_owned(),
            denominator: denominator.to_owned(),
            denominator_value: coverage.eligible,
            coverage,
            evidence_class,
            provenance: MetricProvenanceV1 {
                source: MetricSourceV1::ObservabilityEnvelope,
                source_revision: SOURCE_REVISION_V1.to_owned(),
                projector_revision: EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned(),
                watermark: context.watermark.clone(),
            },
            cohort: MetricCohortV1 {
                descriptor_revision: format!("{denominator}.v1"),
                eligible_population: denominator.to_owned(),
            },
            temporal: MetricTemporalV1 {
                horizon: context.horizon.clone(),
                baseline_watermark: None,
                delta: None,
            },
            uncertainty,
            calibration: None,
            unavailable_reason: unavailable.map(|reason| reason.as_str().to_owned()),
        },
        // Scalar descriptors use their own denominator as the safe default;
        // dimensional projectors override this with the exact cell support.
        local_support,
    }
}

/// Attach exact support for one dimensional entity cell without exposing it
/// through the public measurement contract.
pub(super) fn measurement_with_local_support(
    input: MeasurementInput<'_>,
    local_support: u64,
) -> ExecutionTopologyMeasurementV1 {
    measurement(input).with_local_support(local_support)
}

pub(super) fn unavailable_model(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    reason: ExecutionMetricUnavailableV1,
) -> ExecutionTopologyMetricsV1 {
    unavailable_model_at(
        authorized_scope_ref,
        horizon,
        observed_at_micros,
        "execution-topology:unavailable".to_owned(),
        reason,
    )
}

pub(super) fn unavailable_model_at(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    watermark: String,
    reason: ExecutionMetricUnavailableV1,
) -> ExecutionTopologyMetricsV1 {
    let state = match reason {
        ExecutionMetricUnavailableV1::EventBudgetExceeded
        | ExecutionMetricUnavailableV1::CellBudgetExceeded => CoverageStateV1::Capped,
        _ => CoverageStateV1::Unknown,
    };
    unavailable_model_with_state_at(
        authorized_scope_ref,
        horizon,
        observed_at_micros,
        watermark,
        reason,
        state,
    )
}

pub(super) fn unavailable_model_with_state_at(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    watermark: String,
    reason: ExecutionMetricUnavailableV1,
    state: CoverageStateV1,
) -> ExecutionTopologyMetricsV1 {
    let coverage = MetricCoverageV1 {
        eligible: None,
        observed: 0,
        completed: 0,
        censored: 0,
        unknown: 1,
        excluded: 0,
        state,
    };
    let context = ProjectionContext {
        horizon: horizon.clone(),
        watermark: watermark.clone(),
        complete: false,
        source_state: state,
    };
    let mut measurements = Vec::new();
    for (metric, unit, denominator) in EXECUTION_TOPOLOGY_METRIC_DESCRIPTORS_V1 {
        measurements.push(measurement(MeasurementInput {
            metric,
            unit,
            denominator,
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage: coverage.clone(),
            value: None,
            unavailable: Some(reason),
            context: &context,
        }));
    }
    ExecutionTopologyMetricsV1 {
        authorized_scope_ref,
        horizon,
        watermark,
        observed_at_micros,
        current: false,
        coverage,
        emission_coverage: super::ExecutionTopologyEmissionCoverageV1 {
            emitted: None,
            delayed: None,
            dropped: None,
            sampled_events: None,
        },
        github_stack_capability: super::ExecutionGitHubStackCapabilityReadingV1 {
            capability: None,
            standard_git_fallback_available: None,
            other_forge_fallback_available: None,
            coverage: MetricCoverageV1 {
                eligible: None,
                observed: 0,
                completed: 0,
                censored: 0,
                unknown: 1,
                excluded: 0,
                state,
            },
            unavailable: Some(reason),
        },
        drill_anchors: Vec::new(),
        measurements,
    }
}

/// Every Plan 26 execution-topology descriptor, with its unit and eligible
/// population. An unreadable horizon still returns one typed-absent row per
/// descriptor so a consumer never sees a shrinking descriptor set.
pub const EXECUTION_TOPOLOGY_METRIC_DESCRIPTORS_V1: [(&str, &str, &str); 19] = [
    (
        "work_execution_concurrency_width",
        "microseconds",
        "duration_weighted_topology_samples",
    ),
    (
        "work_execution_useful_concurrency_ratio",
        "ratio",
        "admitted_attempt_micros",
    ),
    ("work_execution_fanout_width", "events", "topology_samples"),
    (
        "work_duplicate_effort_total",
        "events",
        "adjudicated_duplicate_relations",
    ),
    (
        "work_duplicate_effort_ratio",
        "ratio",
        "adjudicated_effort_quantity",
    ),
    (
        "work_duplicate_effects_total",
        "events",
        "observed_duplicate_effects",
    ),
    (
        "work_conflict_prediction_total",
        "events",
        "linked_conflict_predictions",
    ),
    (
        "work_conflict_prediction_precision",
        "ratio",
        "predicted_conflicts_with_outcome",
    ),
    (
        "work_conflict_prediction_recall",
        "ratio",
        "observed_conflicts_with_prediction",
    ),
    (
        "work_merge_attempts_total",
        "events",
        "observed_native_integrations",
    ),
    (
        "work_merge_success_ratio",
        "ratio",
        "observed_native_integrations",
    ),
    (
        "work_stale_stack_age_seconds",
        "events",
        "observed_stack_drifts",
    ),
    (
        "work_blocked_wall_seconds",
        "seconds",
        "closed_blocked_intervals",
    ),
    (
        "work_blocked_cause_seconds",
        "seconds",
        "closed_blocked_intervals",
    ),
    ("work_reruns_total", "events", "eligible_original_attempts"),
    ("work_rerun_rate", "ratio", "eligible_original_attempts"),
    (
        "work_execution_leaks_total",
        "events",
        "observed_leak_detections",
    ),
    (
        "work_delivery_fanout_total",
        "events",
        "attempted_deliveries",
    ),
    (
        "work_delivery_duplicate_ratio",
        "ratio",
        "attempted_deliveries",
    ),
];

/// Coverage ladder: the least trustworthy observation in a population decides
/// the population's state.
pub(super) const fn worse_state(left: CoverageStateV1, right: CoverageStateV1) -> CoverageStateV1 {
    if state_rank(right) > state_rank(left) {
        right
    } else {
        left
    }
}

const fn state_rank(state: CoverageStateV1) -> u8 {
    match state {
        CoverageStateV1::Known => 0,
        CoverageStateV1::Sampled => 1,
        CoverageStateV1::Capped => 2,
        CoverageStateV1::Partial => 3,
        CoverageStateV1::Stale => 4,
        CoverageStateV1::Unknown => 5,
    }
}

pub(super) const fn count_state(complete: bool) -> CoverageStateV1 {
    if complete {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    }
}

/// A distribution is `Known` only when the whole event population was read and
/// every eligible case was actually observed; any shortfall is `Partial`.
pub(super) const fn distribution_state(
    complete: bool,
    eligible: u64,
    observed: u64,
) -> CoverageStateV1 {
    if complete && eligible == observed {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    }
}

/// An exact count needs only a complete event population and at least one
/// eligible case.
pub(super) const fn count_refusal(
    complete: bool,
    eligible: u64,
) -> Option<ExecutionMetricUnavailableV1> {
    if !complete {
        return Some(ExecutionMetricUnavailableV1::CoverageFloorUnmet);
    }
    if eligible == 0 {
        return Some(ExecutionMetricUnavailableV1::NoEligibleEvidence);
    }
    None
}

/// A distribution additionally needs 90% of its eligible population observed.
pub(super) fn distribution_refusal(
    complete: bool,
    eligible: u64,
    observed: u64,
) -> Option<ExecutionMetricUnavailableV1> {
    if let Some(reason) = count_refusal(complete, eligible) {
        return Some(reason);
    }
    if !meets_coverage(eligible, observed) {
        return Some(ExecutionMetricUnavailableV1::CoverageFloorUnmet);
    }
    None
}

/// A rate additionally needs the support floor of eligible cases.
pub(super) fn rate_refusal(
    complete: bool,
    eligible: u64,
    observed: u64,
) -> Option<ExecutionMetricUnavailableV1> {
    if let Some(reason) = count_refusal(complete, eligible) {
        return Some(reason);
    }
    if eligible < RATE_MIN_ELIGIBLE_CASES_V1 {
        return Some(ExecutionMetricUnavailableV1::SupportFloorUnmet);
    }
    if !meets_coverage(eligible, observed) {
        return Some(ExecutionMetricUnavailableV1::CoverageFloorUnmet);
    }
    None
}

/// Conflict precision and recall carry the strictest floors: 50 adjudicated
/// cases, 90% outcome coverage, and at most 10% censoring.
pub(super) fn conflict_refusal(
    complete: bool,
    eligible: u64,
    linked: u64,
    censored: u64,
) -> Option<ExecutionMetricUnavailableV1> {
    if let Some(reason) = count_refusal(complete, eligible) {
        return Some(reason);
    }
    if eligible < CONFLICT_MIN_ADJUDICATED_CASES_V1 {
        return Some(ExecutionMetricUnavailableV1::SupportFloorUnmet);
    }
    if !meets_coverage(eligible, linked) {
        return Some(ExecutionMetricUnavailableV1::CoverageFloorUnmet);
    }
    if exceeds_censoring(eligible, censored) {
        return Some(ExecutionMetricUnavailableV1::CensoringCeilingExceeded);
    }
    None
}

// Coverage ratios compare bounded event counts; the float is a comparison,
// not a reported quantity.
#[allow(clippy::cast_precision_loss)]
fn meets_coverage(eligible: u64, observed: u64) -> bool {
    if eligible == 0 {
        return false;
    }
    observed as f64 / eligible as f64 >= MIN_COVERAGE_RATIO_V1
}

// Censoring ratios compare bounded event counts; the float is a comparison,
// not a reported quantity.
#[allow(clippy::cast_precision_loss)]
fn exceeds_censoring(eligible: u64, censored: u64) -> bool {
    if eligible == 0 {
        return true;
    }
    censored as f64 / eligible as f64 > MAX_CENSORING_RATIO_V1
}

// Recorded counts are bounded by the event budget and stay exactly
// representable in an f64 mantissa.
#[allow(clippy::cast_precision_loss)]
pub(super) fn as_f64(value: u64) -> f64 {
    value as f64
}

pub(super) fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        return None;
    }
    Some(as_f64(numerator) / as_f64(denominator))
}

pub(super) fn seconds(micros: u64) -> f64 {
    as_f64(micros) / 1_000_000.0
}

/// A valid-time interval contributes a duration only when both bounds are
/// recorded and ordered. A missing or inverted bound is censored, never a
/// zero-length interval.
pub(super) fn bounded_interval(from: Option<i64>, until: Option<i64>) -> Option<u64> {
    match (from, until) {
        (Some(from), Some(until)) if until >= from => Some(span(from, until)),
        _ => None,
    }
}

pub(super) fn union_micros(intervals: &mut [(i64, i64)]) -> u64 {
    intervals.sort_unstable();
    let mut total = 0u64;
    let mut current: Option<(i64, i64)> = None;
    for &(start, end) in &*intervals {
        match current {
            None => current = Some((start, end)),
            Some((open_start, open_end)) => {
                if start <= open_end {
                    current = Some((open_start, open_end.max(end)));
                } else {
                    total = total.saturating_add(span(open_start, open_end));
                    current = Some((start, end));
                }
            }
        }
    }
    if let Some((open_start, open_end)) = current {
        total = total.saturating_add(span(open_start, open_end));
    }
    total
}

fn span(start: i64, end: i64) -> u64 {
    end.abs_diff(start)
}

pub(super) fn invalid_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

#[cfg(test)]
#[path = "support_descriptor_tests.rs"]
mod descriptor_tests;
