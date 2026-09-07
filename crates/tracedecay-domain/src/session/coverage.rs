//! Session source frontier and temporal coverage contracts.

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::UtcMicros;

use super::occurrence::{SessionContractError, SessionSourceIdV1, TemporalModeV1};

/// Representative-view counts that complement shard-level [`crate::CoverageReportV1`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct TemporalCoverageCountsV1 {
    pub visible: u64,
    pub hidden: u64,
    pub unknown: u64,
    pub redacted: u64,
}

impl TemporalCoverageCountsV1 {
    pub const fn total(self) -> Option<u64> {
        match self.visible.checked_add(self.hidden) {
            Some(total) => match total.checked_add(self.unknown) {
                Some(total) => total.checked_add(self.redacted),
                None => None,
            },
            None => None,
        }
    }

    pub const fn has_withheld_or_unknown(self) -> bool {
        self.hidden != 0 || self.unknown != 0 || self.redacted != 0
    }
}

/// Monotonic provider or projector position for one session source.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct SessionSourceFrontierV1(u64);

impl SessionSourceFrontierV1 {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn lag_from(self, target: Self) -> u64 {
        target.0.saturating_sub(self.0)
    }
}

/// Closed time interval on one temporal axis.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ClosedUtcIntervalV1 {
    from_inclusive: Option<UtcMicros>,
    through_inclusive: Option<UtcMicros>,
}

impl ClosedUtcIntervalV1 {
    pub fn new(
        from_inclusive: Option<UtcMicros>,
        through_inclusive: Option<UtcMicros>,
    ) -> Result<Self, SessionContractError> {
        if from_inclusive.is_none() && through_inclusive.is_none() {
            return Err(SessionContractError::EmptyCoverageInterval);
        }
        if matches!(
            (from_inclusive, through_inclusive),
            (Some(from), Some(through)) if from > through
        ) {
            return Err(SessionContractError::ReversedCoverageInterval);
        }
        Ok(Self {
            from_inclusive,
            through_inclusive,
        })
    }

    pub const fn from_inclusive(self) -> Option<UtcMicros> {
        self.from_inclusive
    }

    pub const fn through_inclusive(self) -> Option<UtcMicros> {
        self.through_inclusive
    }
}

impl<'de> Deserialize<'de> for ClosedUtcIntervalV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            from_inclusive: Option<UtcMicros>,
            through_inclusive: Option<UtcMicros>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.from_inclusive, wire.through_inclusive).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", content = "interval", rename_all = "snake_case")]
pub enum ValidCoverageIntervalV1 {
    Known(ClosedUtcIntervalV1),
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageIntervalV1 {
    pub knowledge: ClosedUtcIntervalV1,
    pub valid: ValidCoverageIntervalV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionTemporalCoverageRequestV1 {
    mode: TemporalModeV1,
}

impl SessionTemporalCoverageRequestV1 {
    pub const fn new(mode: TemporalModeV1) -> Self {
        Self { mode }
    }

    pub const fn mode(&self) -> TemporalModeV1 {
        self.mode
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionSourceCoverageStateV1 {
    Fresh,
    Stale,
    Partial,
    Locked,
    Redacted,
    RetentionWithheld,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSourceCoverageReasonV1 {
    CaughtUp,
    ProjectionBehindSource {
        lag: u64,
    },
    SourceBehindTarget {
        lag: u64,
    },
    ProjectionAndSourceBehind {
        projection_lag: u64,
        source_lag: u64,
    },
    Locked,
    Redacted,
    RetentionWithheld,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageV1 {
    source_id: SessionSourceIdV1,
    observed_frontier: SessionSourceFrontierV1,
    committed_frontier: SessionSourceFrontierV1,
    target_watermark: SessionSourceFrontierV1,
    request: SessionTemporalCoverageRequestV1,
    covered_intervals: Vec<SessionSourceCoverageIntervalV1>,
    missing_intervals: Vec<SessionSourceCoverageIntervalV1>,
    state: SessionSourceCoverageStateV1,
    reason: SessionSourceCoverageReasonV1,
}

impl SessionSourceCoverageV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: SessionSourceIdV1,
        observed_frontier: SessionSourceFrontierV1,
        committed_frontier: SessionSourceFrontierV1,
        target_watermark: SessionSourceFrontierV1,
        request: SessionTemporalCoverageRequestV1,
        mut covered_intervals: Vec<SessionSourceCoverageIntervalV1>,
        mut missing_intervals: Vec<SessionSourceCoverageIntervalV1>,
        state: SessionSourceCoverageStateV1,
        reason: SessionSourceCoverageReasonV1,
    ) -> Result<Self, SessionContractError> {
        if committed_frontier > observed_frontier {
            return Err(SessionContractError::InvalidSourceCoverageFrontiers);
        }
        covered_intervals.sort();
        missing_intervals.sort();
        if has_duplicate_intervals(&covered_intervals)
            || has_duplicate_intervals(&missing_intervals)
            || covered_intervals.iter().any(|covered| {
                missing_intervals
                    .iter()
                    .any(|missing| coverage_intervals_touch_or_overlap(covered, missing))
            })
            || !coverage_state_matches_reason(state, &reason)
        {
            return Err(if coverage_state_matches_reason(state, &reason) {
                SessionContractError::NonCanonicalCoverageIntervals
            } else {
                SessionContractError::InvalidSourceCoverageState
            });
        }
        Ok(Self {
            source_id,
            observed_frontier,
            committed_frontier,
            target_watermark,
            request,
            covered_intervals,
            missing_intervals,
            state,
            reason,
        })
    }

    pub fn from_frontiers(
        source_id: SessionSourceIdV1,
        observed_frontier: SessionSourceFrontierV1,
        committed_frontier: SessionSourceFrontierV1,
        target_watermark: SessionSourceFrontierV1,
        request: SessionTemporalCoverageRequestV1,
    ) -> Result<Self, SessionContractError> {
        let projection_lag = committed_frontier.lag_from(observed_frontier);
        let source_lag = observed_frontier.lag_from(target_watermark);
        let (state, reason) = match (projection_lag, source_lag) {
            (0, 0) => (
                SessionSourceCoverageStateV1::Fresh,
                SessionSourceCoverageReasonV1::CaughtUp,
            ),
            (0, lag) => (
                SessionSourceCoverageStateV1::Partial,
                SessionSourceCoverageReasonV1::SourceBehindTarget { lag },
            ),
            (lag, 0) => (
                SessionSourceCoverageStateV1::Stale,
                SessionSourceCoverageReasonV1::ProjectionBehindSource { lag },
            ),
            (projection_lag, source_lag) => (
                SessionSourceCoverageStateV1::Partial,
                SessionSourceCoverageReasonV1::ProjectionAndSourceBehind {
                    projection_lag,
                    source_lag,
                },
            ),
        };
        Self::new(
            source_id,
            observed_frontier,
            committed_frontier,
            target_watermark,
            request,
            Vec::new(),
            Vec::new(),
            state,
            reason,
        )
    }

    pub fn source_id(&self) -> &SessionSourceIdV1 {
        &self.source_id
    }

    pub const fn observed_frontier(&self) -> SessionSourceFrontierV1 {
        self.observed_frontier
    }

    pub const fn committed_frontier(&self) -> SessionSourceFrontierV1 {
        self.committed_frontier
    }

    pub const fn target_watermark(&self) -> SessionSourceFrontierV1 {
        self.target_watermark
    }

    pub fn request(&self) -> &SessionTemporalCoverageRequestV1 {
        &self.request
    }

    pub fn covered_intervals(&self) -> &[SessionSourceCoverageIntervalV1] {
        &self.covered_intervals
    }

    pub fn missing_intervals(&self) -> &[SessionSourceCoverageIntervalV1] {
        &self.missing_intervals
    }

    pub const fn state(&self) -> SessionSourceCoverageStateV1 {
        self.state
    }

    pub fn reason(&self) -> &SessionSourceCoverageReasonV1 {
        &self.reason
    }

    pub const fn frontier_lag(&self) -> u64 {
        self.target_watermark
            .0
            .saturating_sub(self.committed_frontier.0)
    }
}

impl<'de> Deserialize<'de> for SessionSourceCoverageV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source_id: SessionSourceIdV1,
            observed_frontier: SessionSourceFrontierV1,
            committed_frontier: SessionSourceFrontierV1,
            target_watermark: SessionSourceFrontierV1,
            request: SessionTemporalCoverageRequestV1,
            covered_intervals: Vec<SessionSourceCoverageIntervalV1>,
            missing_intervals: Vec<SessionSourceCoverageIntervalV1>,
            state: SessionSourceCoverageStateV1,
            reason: SessionSourceCoverageReasonV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.source_id,
            wire.observed_frontier,
            wire.committed_frontier,
            wire.target_watermark,
            wire.request,
            wire.covered_intervals,
            wire.missing_intervals,
            wire.state,
            wire.reason,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn has_duplicate_intervals(intervals: &[SessionSourceCoverageIntervalV1]) -> bool {
    intervals.iter().enumerate().any(|(index, left)| {
        intervals[index + 1..]
            .iter()
            .any(|right| coverage_intervals_touch_or_overlap(left, right))
    })
}

fn coverage_intervals_touch_or_overlap(
    left: &SessionSourceCoverageIntervalV1,
    right: &SessionSourceCoverageIntervalV1,
) -> bool {
    intervals_touch_or_overlap(left.knowledge, right.knowledge)
        && valid_intervals_touch_or_overlap(&left.valid, &right.valid)
}

fn valid_intervals_touch_or_overlap(
    left: &ValidCoverageIntervalV1,
    right: &ValidCoverageIntervalV1,
) -> bool {
    match (left, right) {
        (ValidCoverageIntervalV1::Unknown, ValidCoverageIntervalV1::Unknown) => true,
        (ValidCoverageIntervalV1::Known(left), ValidCoverageIntervalV1::Known(right)) => {
            intervals_touch_or_overlap(*left, *right)
        }
        _ => false,
    }
}

fn intervals_touch_or_overlap(left: ClosedUtcIntervalV1, right: ClosedUtcIntervalV1) -> bool {
    let left_from = left.from_inclusive.map_or(i64::MIN, |value| value.0);
    let left_through = left.through_inclusive.map_or(i64::MAX, |value| value.0);
    let right_from = right.from_inclusive.map_or(i64::MIN, |value| value.0);
    let right_through = right.through_inclusive.map_or(i64::MAX, |value| value.0);
    left_from <= right_through.saturating_add(1) && right_from <= left_through.saturating_add(1)
}

fn coverage_state_matches_reason(
    state: SessionSourceCoverageStateV1,
    reason: &SessionSourceCoverageReasonV1,
) -> bool {
    matches!(
        (state, reason),
        (
            SessionSourceCoverageStateV1::Fresh,
            SessionSourceCoverageReasonV1::CaughtUp
        ) | (
            SessionSourceCoverageStateV1::Stale,
            SessionSourceCoverageReasonV1::ProjectionBehindSource { .. }
        ) | (
            SessionSourceCoverageStateV1::Partial,
            SessionSourceCoverageReasonV1::SourceBehindTarget { .. }
                | SessionSourceCoverageReasonV1::ProjectionAndSourceBehind { .. }
        ) | (
            SessionSourceCoverageStateV1::Locked,
            SessionSourceCoverageReasonV1::Locked
        ) | (
            SessionSourceCoverageStateV1::Redacted,
            SessionSourceCoverageReasonV1::Redacted
        ) | (
            SessionSourceCoverageStateV1::RetentionWithheld,
            SessionSourceCoverageReasonV1::RetentionWithheld
        ) | (
            SessionSourceCoverageStateV1::Unavailable,
            SessionSourceCoverageReasonV1::Unavailable
        )
    )
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionSourceCoverageAggregateStateV1 {
    Fresh,
    Stale,
    Partial,
}

pub const SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS: usize = 256;
pub const SESSION_TEMPORAL_CURSOR_MAX_CANONICAL_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CursorManifestLimitKindV1 {
    Participants,
    CanonicalBytes,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageReceiptV1 {
    request: SessionTemporalCoverageRequestV1,
    sources: Vec<SessionSourceCoverageV1>,
    aggregate_state: SessionSourceCoverageAggregateStateV1,
}

impl SessionSourceCoverageReceiptV1 {
    pub fn new(
        request: SessionTemporalCoverageRequestV1,
        mut sources: Vec<SessionSourceCoverageV1>,
    ) -> Result<Self, SessionContractError> {
        if sources.is_empty() {
            return Err(SessionContractError::SourceCoverageRequired);
        }
        sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        if sources
            .windows(2)
            .any(|pair| pair[0].source_id == pair[1].source_id)
        {
            return Err(SessionContractError::DuplicateSourceCoverage);
        }
        if sources.iter().any(|source| source.request != request) {
            return Err(SessionContractError::SourceCoverageRequestMismatch);
        }
        let all_fresh = sources
            .iter()
            .all(|source| source.state == SessionSourceCoverageStateV1::Fresh);
        let all_stale = sources
            .iter()
            .all(|source| source.state == SessionSourceCoverageStateV1::Stale);
        let aggregate_state = if all_fresh {
            SessionSourceCoverageAggregateStateV1::Fresh
        } else if all_stale {
            SessionSourceCoverageAggregateStateV1::Stale
        } else {
            SessionSourceCoverageAggregateStateV1::Partial
        };
        Ok(Self {
            request,
            sources,
            aggregate_state,
        })
    }

    pub fn request(&self) -> &SessionTemporalCoverageRequestV1 {
        &self.request
    }

    pub fn sources(&self) -> &[SessionSourceCoverageV1] {
        &self.sources
    }

    pub const fn aggregate_state(&self) -> SessionSourceCoverageAggregateStateV1 {
        self.aggregate_state
    }

    pub fn max_frontier_lag(&self) -> u64 {
        self.sources
            .iter()
            .map(SessionSourceCoverageV1::frontier_lag)
            .max()
            .unwrap_or(0)
    }
}

impl<'de> Deserialize<'de> for SessionSourceCoverageReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            request: SessionTemporalCoverageRequestV1,
            sources: Vec<SessionSourceCoverageV1>,
            aggregate_state: SessionSourceCoverageAggregateStateV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let receipt = Self::new(wire.request, wire.sources).map_err(serde::de::Error::custom)?;
        if receipt.aggregate_state != wire.aggregate_state {
            return Err(serde::de::Error::custom(
                SessionContractError::InvalidSourceCoverageState,
            ));
        }
        Ok(receipt)
    }
}
