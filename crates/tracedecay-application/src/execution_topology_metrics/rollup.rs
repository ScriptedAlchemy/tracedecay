//! Bounded, mergeable daily evidence for execution-topology metrics.
//!
//! A fragment is deliberately not a cached [`super::ExecutionTopologyMetricsV1`].
//! Rates, interval unions, and late corrections have to be resolved over the
//! requested horizon, so the fragment retains only reduced sufficient
//! statistics and bounded opaque joins needed to finalize once.

use serde::{Deserialize, Serialize};
use tracedecay_domain::CoverageStateV1;

use crate::observability::{ObservabilityHorizonV1, ObservabilityPageV1};

use super::projection::ExecutionTopologyRollupStateErrorV1;
use super::projection::page_projection::{
    ClassifiedExecutionTopologyPageV1, ExecutionTopologyReducedRollupStateV1,
    classify_execution_topology_page, project_reduced_execution_topology_rollup_state,
    reduce_classified_execution_topology_rollup_state,
};
use super::support::unavailable_model_at;
use super::{
    EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1, EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1,
    ExecutionMetricUnavailableV1, ExecutionTopologyMetricsV1,
};

/// Persisted local state is intentionally bounded independently from the raw
/// event-page budget. A producer that needs more cannot silently convert a
/// partial daily population into a durable aggregate.
pub const MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1: usize = 4 * 1024 * 1024;
pub const MAX_EXECUTION_TOPOLOGY_ROLLUP_READ_BYTES_V1: usize = 32 * 1024 * 1024;
pub const MAX_EXECUTION_TOPOLOGY_ROLLUP_DAYS_V1: usize = 395;

const UTC_DAY_MICROS_V1: i64 = 86_400_000_000;
const UNAVAILABLE_WATERMARK_V1: &str = "execution-topology:rollup-unavailable";

/// A serde-stable fragment for one fully covered UTC day. It contains neither
/// source envelopes nor an exported identifier: correction and producer-loss
/// joins are bounded, opaque state entries.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTopologyRollupFragmentV1 {
    descriptor_revision: String,
    projector_revision: String,
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    source_watermark: String,
    state: ExecutionTopologyRollupFragmentStateV1,
}

/// Persisted daily state. A capped day is deliberately a terminal coverage
/// fact, never a partial reduced aggregate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExecutionTopologyRollupFragmentStateV1 {
    Reduced {
        reduced: ExecutionTopologyReducedRollupStateV1,
    },
    Capped,
}

/// Ephemeral classified evidence for a requested-horizon boundary. This type
/// intentionally has no wire or persistence representation: a partial UTC day
/// must be read fresh and never replace a persisted daily fragment.
#[derive(Clone, Debug)]
pub struct ExecutionTopologyBoundaryFragmentV1 {
    descriptor_revision: String,
    projector_revision: String,
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    source_watermark: String,
    evidence: ClassifiedExecutionTopologyPageV1,
}

/// Construction refuses an invalid page or an over-budget canonical fragment;
/// callers can then leave the day absent and return a typed unavailable model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionTopologyRollupErrorV1 {
    #[error("execution topology rollups require one exact UTC day")]
    ExactUtcDayRequired,
    #[error("execution topology rollup page is unavailable")]
    PageUnavailable,
    #[error("execution topology rollup fragment exceeds its bounded state budget")]
    FragmentBudgetExceeded,
    #[error("execution topology rollup correction carry exceeds its bounded capacity")]
    CarryBudgetExceeded,
    #[error("execution topology rollup interval carry exceeds its bounded capacity")]
    IntervalBudgetExceeded,
    #[error("execution topology rollup fragments are incompatible")]
    IncompatibleFragments,
    #[error("execution topology boundary fragments require a nonempty partial UTC day")]
    PartialUtcDayRequired,
}

/// Result of application-owned retention evaluation for one opaque fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionTopologyRollupRetentionV1 {
    Unchanged,
    Updated { fragment_json: String },
}

impl ExecutionTopologyBoundaryFragmentV1 {
    #[must_use]
    pub fn horizon(&self) -> &ObservabilityHorizonV1 {
        &self.horizon
    }

    fn classified_bytes(&self) -> Result<Vec<u8>, ExecutionTopologyRollupErrorV1> {
        serde_json::to_vec(&self.evidence)
            .map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)
    }

    fn valid_for(&self, authorized_scope_ref: &str) -> bool {
        self.descriptor_revision == EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1
            && self.projector_revision == EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1
            && self.authorized_scope_ref == authorized_scope_ref
            && is_partial_utc_day(&self.horizon)
            && safe_local_cursor(&self.source_watermark)
            && self.evidence.is_valid_rollup_state()
    }
}

enum FragmentRefV1<'a> {
    Daily(&'a ExecutionTopologyRollupFragmentV1),
    Boundary(&'a ExecutionTopologyBoundaryFragmentV1),
}

impl FragmentRefV1<'_> {
    fn horizon(&self) -> &ObservabilityHorizonV1 {
        match self {
            Self::Daily(fragment) => &fragment.horizon,
            Self::Boundary(fragment) => &fragment.horizon,
        }
    }

    fn source_watermark(&self) -> &str {
        match self {
            Self::Daily(fragment) => &fragment.source_watermark,
            Self::Boundary(fragment) => &fragment.source_watermark,
        }
    }

    fn is_daily(&self) -> bool {
        matches!(self, Self::Daily(_))
    }

    fn is_valid_for(&self, authorized_scope_ref: &str) -> bool {
        match self {
            Self::Daily(fragment) => fragment.valid_for(authorized_scope_ref),
            Self::Boundary(fragment) => fragment.valid_for(authorized_scope_ref),
        }
    }

    fn source_is_stale(&self) -> bool {
        match self {
            Self::Daily(fragment) => fragment.source_is_stale(),
            Self::Boundary(fragment) => fragment.evidence.source_is_stale(),
        }
    }

    fn retained_bytes(&self) -> Result<Vec<u8>, ExecutionTopologyRollupErrorV1> {
        match self {
            Self::Daily(fragment) => fragment.canonical_bytes(),
            Self::Boundary(fragment) => fragment.classified_bytes(),
        }
    }

    fn reduced_state(
        &self,
    ) -> Result<ExecutionTopologyReducedRollupStateV1, ExecutionTopologyRollupErrorV1> {
        match self {
            Self::Daily(fragment) => fragment
                .reduced_state()
                .cloned()
                .ok_or(ExecutionTopologyRollupErrorV1::IncompatibleFragments),
            Self::Boundary(fragment) => reduce_classified_execution_topology_rollup_state(
                &fragment.horizon,
                &fragment.evidence,
            )
            .map_err(rollup_state_error),
        }
    }

    fn is_capped(&self) -> bool {
        matches!(self, Self::Daily(fragment) if fragment.is_capped())
    }
}

impl ExecutionTopologyRollupFragmentV1 {
    #[must_use]
    pub fn authorized_scope_ref(&self) -> &str {
        &self.authorized_scope_ref
    }

    #[must_use]
    pub fn horizon(&self) -> &ObservabilityHorizonV1 {
        &self.horizon
    }

    #[must_use]
    pub fn source_watermark(&self) -> &str {
        &self.source_watermark
    }

    #[must_use]
    pub const fn observed_at_micros(&self) -> i64 {
        self.observed_at_micros
    }

    /// A replacement decision belongs to the persistence authority because a
    /// watermark is opaque here. That authority must only replace this exact
    /// day for a newer source watermark, or for a changed projector at the
    /// same source watermark.
    #[must_use]
    pub fn can_replace(&self, existing: &Self, source_watermark_is_newer: bool) -> bool {
        self.authorized_scope_ref == existing.authorized_scope_ref
            && self.horizon == existing.horizon
            && (source_watermark_is_newer
                || (self.source_watermark == existing.source_watermark
                    && self.projector_revision != existing.projector_revision))
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>, ExecutionTopologyRollupErrorV1> {
        serde_json::to_vec(self).map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)
    }

    fn reduced_state(&self) -> Option<&ExecutionTopologyReducedRollupStateV1> {
        match &self.state {
            ExecutionTopologyRollupFragmentStateV1::Reduced { reduced } => Some(reduced),
            ExecutionTopologyRollupFragmentStateV1::Capped => None,
        }
    }

    pub(in crate::execution_topology_metrics) fn is_capped(&self) -> bool {
        matches!(self.state, ExecutionTopologyRollupFragmentStateV1::Capped)
    }

    fn source_is_stale(&self) -> bool {
        self.reduced_state()
            .is_some_and(ExecutionTopologyReducedRollupStateV1::source_is_stale)
    }

    /// Advances the evaluated retention frontier without discarding a join or
    /// correction that cannot be settled exactly inside this fragment.
    fn check_retention(&mut self, now_micros: i64) -> Result<(), ExecutionTopologyRollupErrorV1> {
        if let ExecutionTopologyRollupFragmentStateV1::Reduced { reduced } = &mut self.state {
            reduced
                .check_retention(now_micros)
                .map_err(rollup_state_error)?;
        }
        if self.canonical_bytes()?.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
            return Err(ExecutionTopologyRollupErrorV1::FragmentBudgetExceeded);
        }
        Ok(())
    }

    fn valid_for(&self, authorized_scope_ref: &str) -> bool {
        self.descriptor_revision == EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1
            && self.projector_revision == EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1
            && self.authorized_scope_ref == authorized_scope_ref
            && is_exact_utc_day(&self.horizon)
            && safe_local_cursor(&self.source_watermark)
            && self
                .reduced_state()
                .is_none_or(|reduced| reduced.validate_for_horizon(&self.horizon).is_ok())
    }
}

/// Canonically validates and evaluates retention for one fragment document.
/// Storage CAS-publishes `Updated` against the exact generation/content digest;
/// it never parses or reinterprets the opaque reduced state itself.
pub fn check_execution_topology_rollup_retention_json(
    fragment_json: &str,
    now_micros: i64,
) -> Result<ExecutionTopologyRollupRetentionV1, ExecutionTopologyRollupErrorV1> {
    if fragment_json.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
        return Err(ExecutionTopologyRollupErrorV1::FragmentBudgetExceeded);
    }
    let mut fragment = serde_json::from_str::<ExecutionTopologyRollupFragmentV1>(fragment_json)
        .map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)?;
    let canonical = serde_json::to_string(&fragment)
        .map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)?;
    if canonical != fragment_json || !fragment.valid_for(fragment.authorized_scope_ref()) {
        return Err(ExecutionTopologyRollupErrorV1::IncompatibleFragments);
    }
    fragment.check_retention(now_micros)?;
    let compacted = serde_json::to_string(&fragment)
        .map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)?;
    if compacted == fragment_json {
        Ok(ExecutionTopologyRollupRetentionV1::Unchanged)
    } else {
        Ok(ExecutionTopologyRollupRetentionV1::Updated {
            fragment_json: compacted,
        })
    }
}

/// Builds one day of reduced evidence. The page must already be authorized for
/// `authorized_scope_ref`; this function never reads storage or performs
/// authorization itself. A capped source page settles its exact watermark as
/// a durable Capped fragment without retaining its partial events.
pub fn build_execution_topology_rollup_fragment(
    authorized_scope_ref: &str,
    exact_day_horizon: &ObservabilityHorizonV1,
    observed_at_micros: i64,
    page: ObservabilityPageV1,
) -> Result<ExecutionTopologyRollupFragmentV1, ExecutionTopologyRollupErrorV1> {
    if !is_exact_utc_day(exact_day_horizon) {
        return Err(ExecutionTopologyRollupErrorV1::ExactUtcDayRequired);
    }
    let source_watermark = page.watermark.clone();
    if !safe_local_cursor(&source_watermark) {
        return Err(ExecutionTopologyRollupErrorV1::PageUnavailable);
    }
    let state = if page_is_capped(&page) {
        ExecutionTopologyRollupFragmentStateV1::Capped
    } else {
        let evidence =
            classify_execution_topology_page(authorized_scope_ref, exact_day_horizon, page)
                .map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)?;
        if evidence.source_is_stale() {
            return Err(ExecutionTopologyRollupErrorV1::PageUnavailable);
        }
        match reduce_classified_execution_topology_rollup_state(exact_day_horizon, &evidence) {
            Ok(reduced) => ExecutionTopologyRollupFragmentStateV1::Reduced { reduced },
            Err(
                ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded
                | ExecutionTopologyRollupStateErrorV1::IntervalBudgetExceeded,
            ) => ExecutionTopologyRollupFragmentStateV1::Capped,
            Err(error) => return Err(rollup_state_error(error)),
        }
    };
    let mut fragment = ExecutionTopologyRollupFragmentV1 {
        descriptor_revision: EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1.to_owned(),
        projector_revision: EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned(),
        authorized_scope_ref: authorized_scope_ref.to_owned(),
        horizon: exact_day_horizon.clone(),
        observed_at_micros,
        source_watermark,
        state,
    };
    if fragment.canonical_bytes()?.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
        fragment.state = ExecutionTopologyRollupFragmentStateV1::Capped;
        if fragment.canonical_bytes()?.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
            return Err(ExecutionTopologyRollupErrorV1::FragmentBudgetExceeded);
        }
    }
    Ok(fragment)
}

/// Builds a fresh, non-persistable boundary page for an arbitrary requested
/// horizon. Whole UTC days must use [`build_execution_topology_rollup_fragment`]
/// so a transient page can never enter daily retention by accident.
pub fn build_execution_topology_boundary_fragment(
    authorized_scope_ref: &str,
    boundary_horizon: &ObservabilityHorizonV1,
    page: ObservabilityPageV1,
) -> Result<ExecutionTopologyBoundaryFragmentV1, ExecutionTopologyRollupErrorV1> {
    if !is_partial_utc_day(boundary_horizon) {
        return Err(ExecutionTopologyRollupErrorV1::PartialUtcDayRequired);
    }
    let evidence = classify_execution_topology_page(authorized_scope_ref, boundary_horizon, page)
        .map_err(|_| ExecutionTopologyRollupErrorV1::PageUnavailable)?;
    let fragment = ExecutionTopologyBoundaryFragmentV1 {
        descriptor_revision: EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1.to_owned(),
        projector_revision: EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned(),
        authorized_scope_ref: authorized_scope_ref.to_owned(),
        horizon: boundary_horizon.clone(),
        source_watermark: evidence.watermark().to_owned(),
        evidence,
    };
    if fragment.classified_bytes()?.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
        return Err(ExecutionTopologyRollupErrorV1::FragmentBudgetExceeded);
    }
    Ok(fragment)
}

/// Projects an exact requested horizon from complete, non-overlapping daily
/// fragments. This convenience path accepts only persisted full UTC days.
#[must_use]
pub fn project_execution_topology_fragments(
    authorized_scope_ref: &str,
    requested_horizon: &ObservabilityHorizonV1,
    observed_at_micros: i64,
    fragments: &[ExecutionTopologyRollupFragmentV1],
) -> ExecutionTopologyMetricsV1 {
    project_execution_topology_fragments_with_boundaries(
        authorized_scope_ref,
        requested_horizon,
        observed_at_micros,
        fragments,
        &[],
    )
}

/// Projects any requested horizon from persisted full-day interiors plus up to
/// two fresh, transient boundary fragments. Input order is irrelevant; exact
/// contiguous coverage is required after ordering, and a boundary may occur
/// only at either end of the requested horizon.
#[must_use]
pub fn project_execution_topology_fragments_with_boundaries(
    authorized_scope_ref: &str,
    requested_horizon: &ObservabilityHorizonV1,
    observed_at_micros: i64,
    fragments: &[ExecutionTopologyRollupFragmentV1],
    boundary_fragments: &[ExecutionTopologyBoundaryFragmentV1],
) -> ExecutionTopologyMetricsV1 {
    let unavailable = || {
        unavailable_model_at(
            authorized_scope_ref.to_owned(),
            requested_horizon.clone(),
            observed_at_micros,
            UNAVAILABLE_WATERMARK_V1.to_owned(),
            ExecutionMetricUnavailableV1::StoreUnavailable,
        )
    };
    if requested_horizon.until_micros <= requested_horizon.since_micros
        || fragments.len().saturating_add(boundary_fragments.len())
            > MAX_EXECUTION_TOPOLOGY_ROLLUP_DAYS_V1
        || boundary_fragments.len() > 2
        || (fragments.is_empty() && boundary_fragments.is_empty())
    {
        return unavailable();
    }
    let mut ordered = fragments
        .iter()
        .map(FragmentRefV1::Daily)
        .chain(boundary_fragments.iter().map(FragmentRefV1::Boundary))
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (
            left.horizon().since_micros,
            left.horizon().until_micros,
            left.source_watermark(),
        )
            .cmp(&(
                right.horizon().since_micros,
                right.horizon().until_micros,
                right.source_watermark(),
            ))
    });
    let mut total_bytes = 0usize;
    let mut expected_since = requested_horizon.since_micros;
    for (index, fragment) in ordered.iter().enumerate() {
        if (!fragment.is_valid_for(authorized_scope_ref)
            || !fragment.is_daily() && index != 0 && index + 1 != ordered.len())
            || fragment.horizon().since_micros != expected_since
            || fragment.horizon().until_micros > requested_horizon.until_micros
            || fragment.source_is_stale()
        {
            return unavailable();
        }
        let bytes = match fragment.retained_bytes() {
            Ok(bytes) => bytes,
            Err(_) => return unavailable(),
        };
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > MAX_EXECUTION_TOPOLOGY_ROLLUP_READ_BYTES_V1 {
            return unavailable();
        }
        expected_since = fragment.horizon().until_micros;
    }
    if expected_since != requested_horizon.until_micros {
        return unavailable();
    }
    if let Some(fragment) = ordered.iter().find(|fragment| fragment.is_capped()) {
        return unavailable_model_at(
            authorized_scope_ref.to_owned(),
            requested_horizon.clone(),
            observed_at_micros,
            fragment.source_watermark().to_owned(),
            ExecutionMetricUnavailableV1::EventBudgetExceeded,
        );
    }
    let mut reduced = match ordered[0].reduced_state() {
        Ok(state) => state,
        Err(_) => return unavailable(),
    };
    for fragment in ordered.iter().skip(1) {
        let incoming = match fragment.reduced_state() {
            Ok(state) => state,
            Err(_) => return unavailable(),
        };
        if reduced.merge(incoming).is_err() {
            return unavailable();
        }
    }
    let drill_anchors = ordered
        .iter()
        .filter_map(|fragment| match fragment {
            FragmentRefV1::Daily(_) => None,
            FragmentRefV1::Boundary(fragment) => Some(fragment.evidence.drill_cursors()),
        })
        .flatten()
        .take(super::MAX_EXECUTION_TOPOLOGY_DRILL_ANCHORS_V1)
        .cloned()
        .map(|cursor| super::ExecutionTopologyDrillAnchorV1 { cursor })
        .collect();
    match project_reduced_execution_topology_rollup_state(
        authorized_scope_ref.to_owned(),
        requested_horizon.clone(),
        observed_at_micros,
        ordered.last().map_or_else(
            || UNAVAILABLE_WATERMARK_V1.to_owned(),
            |fragment| fragment.source_watermark().to_owned(),
        ),
        drill_anchors,
        &reduced,
    ) {
        Ok(projection) => projection.model,
        Err(_) => unavailable(),
    }
}

fn page_is_capped(page: &ObservabilityPageV1) -> bool {
    page.coverage == CoverageStateV1::Capped
        || page.next_watermark.is_some()
        || page.events.len() as u64 > u64::from(super::MAX_EXECUTION_TOPOLOGY_EVENTS_V1)
}

fn rollup_state_error(
    error: ExecutionTopologyRollupStateErrorV1,
) -> ExecutionTopologyRollupErrorV1 {
    match error {
        ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded => {
            ExecutionTopologyRollupErrorV1::CarryBudgetExceeded
        }
        ExecutionTopologyRollupStateErrorV1::IntervalBudgetExceeded => {
            ExecutionTopologyRollupErrorV1::IntervalBudgetExceeded
        }
        ExecutionTopologyRollupStateErrorV1::IncompatibleState => {
            ExecutionTopologyRollupErrorV1::IncompatibleFragments
        }
    }
}

fn is_exact_utc_day(horizon: &ObservabilityHorizonV1) -> bool {
    horizon.until_micros.saturating_sub(horizon.since_micros) == UTC_DAY_MICROS_V1
        && horizon.since_micros.rem_euclid(UTC_DAY_MICROS_V1) == 0
        && horizon.until_micros.rem_euclid(UTC_DAY_MICROS_V1) == 0
}

fn is_partial_utc_day(horizon: &ObservabilityHorizonV1) -> bool {
    horizon.until_micros > horizon.since_micros
        && !is_exact_utc_day(horizon)
        && horizon.since_micros.div_euclid(UTC_DAY_MICROS_V1)
            == horizon
                .until_micros
                .saturating_sub(1)
                .div_euclid(UTC_DAY_MICROS_V1)
}

fn safe_local_cursor(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
