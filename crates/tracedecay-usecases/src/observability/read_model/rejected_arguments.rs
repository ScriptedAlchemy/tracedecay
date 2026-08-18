//! Rejected-argument Observatory projection over canonical observation envelopes.

use std::collections::BTreeMap;

use tracedecay_application::{
    MetricCoverageV1, RejectedArgumentAnalyticsV1, RejectedArgumentGroupV1,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, RejectedArgumentErrorClassV1,
    RejectedArgumentNameV1, RejectedArgumentSurfaceV1,
};

use crate::feedback::observations::{FeedbackCoverageV1, FeedbackObservationReadModelV1};

const PROJECTOR_REVISION: &str = "observatory-rejected-argument-projector.v1";
const RATE_SUPPORT_FLOOR: u64 = 20;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    surface: RejectedArgumentSurfaceV1,
    operation: String,
    argument: RejectedArgumentNameV1,
    error_class: RejectedArgumentErrorClassV1,
}

pub(crate) fn unavailable_rejected_arguments(
    watermark: &str,
    reason: &str,
) -> RejectedArgumentAnalyticsV1 {
    RejectedArgumentAnalyticsV1 {
        coverage: unknown_coverage(),
        projector_revision: PROJECTOR_REVISION.to_owned(),
        watermark: watermark.to_owned(),
        eligible_attempts: None,
        rejected_total: None,
        rejection_rate: None,
        redacted_name_count: 0,
        groups: Vec::new(),
        unavailable_reason: Some(reason.to_owned()),
    }
}

pub(crate) fn project_rejected_arguments(
    events: &[&ObservabilityEnvelopeV1],
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> RejectedArgumentAnalyticsV1 {
    let mut counts = BTreeMap::<GroupKey, u64>::new();
    for event in events {
        if let ObservabilityPayloadV1::RejectedArgument(value) = &event.payload {
            increment(
                &mut counts,
                value.surface,
                &value.operation,
                value.argument,
                value.error_class,
            );
        }
    }
    if counts.is_empty() {
        return unavailable_rejected_arguments(
            watermark,
            if source_complete {
                "rejected_argument_observations_not_recorded"
            } else {
                "incomplete_observability_coverage"
            },
        );
    }
    finish(
        counts,
        None,
        watermark,
        source_complete,
        source_unknown,
        "incomplete_observability_coverage",
    )
}

pub(crate) fn project_rejected_arguments_from_feedback(
    feedback: &FeedbackObservationReadModelV1,
    watermark: &str,
) -> RejectedArgumentAnalyticsV1 {
    let mut counts = BTreeMap::<GroupKey, u64>::new();
    for group in &feedback.rejected_argument_groups {
        add_count(
            &mut counts,
            group.surface,
            &group.operation,
            group.argument,
            group.error_class,
            group.count,
        );
    }
    let eligible_attempts = feedback
        .event_counts
        .get("feedback.dispatch.observed.v1")
        .copied();
    let source_complete = feedback.coverage == FeedbackCoverageV1::Known;
    let source_unknown = feedback
        .denominators
        .delayed
        .saturating_add(feedback.denominators.dropped)
        .saturating_add(feedback.denominators.retention_dropped)
        .saturating_add(feedback.denominators.incomplete_boots);
    finish(
        counts,
        eligible_attempts,
        watermark,
        source_complete,
        source_unknown,
        if source_complete {
            "rejected_argument_observations_not_recorded"
        } else {
            "incomplete_feedback_coverage"
        },
    )
}

fn increment(
    counts: &mut BTreeMap<GroupKey, u64>,
    surface: RejectedArgumentSurfaceV1,
    operation: &str,
    argument: RejectedArgumentNameV1,
    error_class: RejectedArgumentErrorClassV1,
) {
    add_count(counts, surface, operation, argument, error_class, 1);
}

fn add_count(
    counts: &mut BTreeMap<GroupKey, u64>,
    surface: RejectedArgumentSurfaceV1,
    operation: &str,
    argument: RejectedArgumentNameV1,
    error_class: RejectedArgumentErrorClassV1,
    count: u64,
) {
    let key = GroupKey {
        surface,
        operation: operation.to_owned(),
        argument,
        error_class,
    };
    let total = counts.entry(key).or_default();
    *total = total.saturating_add(count);
}

fn finish(
    counts: BTreeMap<GroupKey, u64>,
    eligible_attempts: Option<u64>,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
    missing_reason: &'static str,
) -> RejectedArgumentAnalyticsV1 {
    let observed = counts
        .values()
        .fold(0_u64, |total, count| total.saturating_add(*count));
    // Absence of this family in a complete store is a measured empty window.
    // Incomplete coverage withholds the numeric total instead of claiming zero.
    let (state, rejected_total, unavailable_reason) = if source_complete {
        (CoverageStateV1::Known, Some(observed), None)
    } else {
        (
            CoverageStateV1::Partial,
            None,
            Some(missing_reason.to_owned()),
        )
    };
    let rate_ready = matches!(
        (rejected_total, eligible_attempts, state),
        (Some(rejected), Some(eligible), CoverageStateV1::Known)
            if eligible >= RATE_SUPPORT_FLOOR && rejected <= eligible
    );
    let rejection_rate = if rate_ready {
        eligible_attempts.and_then(|eligible| {
            rejected_total.map(|rejected| rejected as f64 / eligible as f64)
        })
    } else {
        None
    };
    let groups = if state == CoverageStateV1::Known {
        counts
            .into_iter()
            .map(|(key, count)| RejectedArgumentGroupV1 {
                surface: key.surface,
                operation: key.operation,
                argument: key.argument,
                error_class: key.error_class,
                count,
                rate: if rate_ready {
                    eligible_attempts.map(|eligible| count as f64 / eligible as f64)
                } else {
                    None
                },
            })
            .collect()
    } else {
        Vec::new()
    };
    RejectedArgumentAnalyticsV1 {
        coverage: MetricCoverageV1 {
            eligible: rejected_total.and(eligible_attempts),
            observed,
            completed: if state == CoverageStateV1::Known {
                observed
            } else {
                0
            },
            censored: 0,
            unknown: if state == CoverageStateV1::Known {
                0
            } else {
                source_unknown.max(1)
            },
            excluded: 0,
            state,
        },
        projector_revision: PROJECTOR_REVISION.to_owned(),
        watermark: watermark.to_owned(),
        eligible_attempts,
        rejected_total,
        rejection_rate,
        redacted_name_count: 0,
        groups,
        unavailable_reason,
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
