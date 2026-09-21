//! One retained projection of domain coverage and hydration onto wire results.

use tracedecay_contracts::retained_surfaces::{
    ClosedUtcIntervalV1, SessionCoverageIntervalV1, SessionCoverageModeV1, SessionCoverageReasonV1,
    SessionCoverageRequestV1, SessionCoverageStateV1,
    SessionSourceCoverageV1 as WireSourceCoverageV1, TemporalCoverageV1, TemporalWatermarksV1,
    ValidCoverageIntervalV1,
};
use tracedecay_domain::{
    ClosedUtcIntervalV1 as DomainClosedUtcIntervalV1, SessionSourceCoverageIntervalV1,
    SessionSourceCoverageReasonV1, SessionSourceCoverageStateV1, SessionSourceCoverageV1,
    TemporalCoverageCountsV1, TemporalModeV1,
    ValidCoverageIntervalV1 as DomainValidCoverageIntervalV1,
};

use crate::session_retrieval::SessionTemporalWatermarksView;

pub(super) const fn coverage(value: TemporalCoverageCountsV1) -> TemporalCoverageV1 {
    TemporalCoverageV1 {
        visible: value.visible,
        hidden: value.hidden,
        unknown: value.unknown,
        redacted: value.redacted,
    }
}

pub(super) const fn temporal_watermarks(
    value: SessionTemporalWatermarksView,
) -> TemporalWatermarksV1 {
    TemporalWatermarksV1 {
        generation: value.generation,
        source: value.source,
        projection: value.projection,
        index: value.index,
        summary: value.summary,
    }
}

pub(super) fn source_coverage(value: SessionSourceCoverageV1) -> WireSourceCoverageV1 {
    WireSourceCoverageV1 {
        source_id: value.source_id().as_str().to_owned(),
        observed_frontier: value.observed_frontier().value(),
        committed_frontier: value.committed_frontier().value(),
        target_watermark: value.target_watermark().value(),
        request: SessionCoverageRequestV1 {
            mode: coverage_mode(value.request().mode()),
        },
        covered_intervals: value
            .covered_intervals()
            .iter()
            .cloned()
            .map(coverage_interval)
            .collect(),
        missing_intervals: value
            .missing_intervals()
            .iter()
            .cloned()
            .map(coverage_interval)
            .collect(),
        state: coverage_state(value.state()),
        reason: coverage_reason(value.reason()),
    }
}

fn coverage_interval(value: SessionSourceCoverageIntervalV1) -> SessionCoverageIntervalV1 {
    SessionCoverageIntervalV1 {
        knowledge: closed_interval(value.knowledge),
        valid: match value.valid {
            DomainValidCoverageIntervalV1::Known(interval) => {
                ValidCoverageIntervalV1::Known(closed_interval(interval))
            }
            DomainValidCoverageIntervalV1::Unknown => ValidCoverageIntervalV1::Unknown,
        },
    }
}

fn closed_interval(value: DomainClosedUtcIntervalV1) -> ClosedUtcIntervalV1 {
    ClosedUtcIntervalV1 {
        from_inclusive: value.from_inclusive().map(|value| value.0),
        through_inclusive: value.through_inclusive().map(|value| value.0),
    }
}

const fn coverage_mode(value: TemporalModeV1) -> SessionCoverageModeV1 {
    match value {
        TemporalModeV1::Current => SessionCoverageModeV1::Current,
        TemporalModeV1::AsOf { cutoff } => SessionCoverageModeV1::AsOf { cutoff: cutoff.0 },
        TemporalModeV1::Evolution => SessionCoverageModeV1::Evolution,
        TemporalModeV1::Forensic => SessionCoverageModeV1::Forensic,
    }
}

const fn coverage_state(value: SessionSourceCoverageStateV1) -> SessionCoverageStateV1 {
    match value {
        SessionSourceCoverageStateV1::Fresh => SessionCoverageStateV1::Fresh,
        SessionSourceCoverageStateV1::Stale => SessionCoverageStateV1::Stale,
        SessionSourceCoverageStateV1::Partial => SessionCoverageStateV1::Partial,
        SessionSourceCoverageStateV1::Locked => SessionCoverageStateV1::Locked,
        SessionSourceCoverageStateV1::Redacted => SessionCoverageStateV1::Redacted,
        SessionSourceCoverageStateV1::RetentionWithheld => {
            SessionCoverageStateV1::RetentionWithheld
        }
        SessionSourceCoverageStateV1::Unavailable => SessionCoverageStateV1::Unavailable,
    }
}

fn coverage_reason(value: &SessionSourceCoverageReasonV1) -> SessionCoverageReasonV1 {
    match value {
        SessionSourceCoverageReasonV1::CaughtUp => SessionCoverageReasonV1::CaughtUp,
        SessionSourceCoverageReasonV1::ProjectionBehindSource { lag } => {
            SessionCoverageReasonV1::ProjectionBehindSource { lag: *lag }
        }
        SessionSourceCoverageReasonV1::SourceBehindTarget { lag } => {
            SessionCoverageReasonV1::SourceBehindTarget { lag: *lag }
        }
        SessionSourceCoverageReasonV1::ProjectionAndSourceBehind {
            projection_lag,
            source_lag,
        } => SessionCoverageReasonV1::ProjectionAndSourceBehind {
            projection_lag: *projection_lag,
            source_lag: *source_lag,
        },
        SessionSourceCoverageReasonV1::Locked => SessionCoverageReasonV1::Locked,
        SessionSourceCoverageReasonV1::Redacted => SessionCoverageReasonV1::Redacted,
        SessionSourceCoverageReasonV1::RetentionWithheld => {
            SessionCoverageReasonV1::RetentionWithheld
        }
        SessionSourceCoverageReasonV1::Unavailable => SessionCoverageReasonV1::Unavailable,
    }
}
