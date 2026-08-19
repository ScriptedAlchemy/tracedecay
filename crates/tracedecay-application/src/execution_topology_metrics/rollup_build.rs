//! Application-owned daily rollup construction for execution-topology metrics.
//!
//! This module stops at the canonical retained fragment. Storage adapters
//! persist it opaquely and never reconstruct a parallel cell authority.

use serde::{Deserialize, Serialize};
use tracedecay_domain::CoverageStateV1;

use crate::observability::{ObservabilityHorizonV1, ObservabilityPageV1};

use super::rollup::{
    ExecutionTopologyRollupErrorV1, ExecutionTopologyRollupFragmentV1,
    MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1, build_execution_topology_rollup_fragment,
    canonical_execution_topology_rollup_fragment_bytes,
};

/// Complete application artifact for publishing one retained UTC-day rollup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionTopologyRollupBuildV1 {
    /// Exact source coverage retained for this day. `Capped` artifacts carry
    /// no cells; adapters persist this state directly rather than inferring it
    /// from an empty cell set or parsing the opaque fragment document.
    pub coverage: CoverageStateV1,
    pub fragment: ExecutionTopologyRollupFragmentV1,
    pub fragment_json: String,
}

/// Reason a daily topology rollup cannot be retained or published.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionTopologyRollupBuildErrorV1 {
    #[error(transparent)]
    Fragment(#[from] ExecutionTopologyRollupErrorV1),
    #[error("execution topology rollup fragment cannot be serialized")]
    FragmentSerialization,
    #[error("execution topology rollup exceeds its storage byte budget")]
    StorageBudgetExceeded,
}
const EMPTY_EXECUTION_TOPOLOGY_WATERMARK_V1: &str = "analytics:empty";

/// Builds the canonical Known artifact for a fully observed UTC day with no
/// eligible topology events. It uses the ordinary projector, so typed
/// the ordinary reduced fragment remains the retained authority.
pub fn build_empty_execution_topology_daily_rollup(
    authorized_scope_ref: &str,
    exact_day_horizon: &ObservabilityHorizonV1,
    observed_at_micros: i64,
) -> Result<ExecutionTopologyRollupBuildV1, ExecutionTopologyRollupBuildErrorV1> {
    build_execution_topology_daily_rollup(
        authorized_scope_ref,
        exact_day_horizon,
        observed_at_micros,
        ObservabilityPageV1 {
            events: Vec::new(),
            event_cursors: Vec::new(),
            watermark: EMPTY_EXECUTION_TOPOLOGY_WATERMARK_V1.to_owned(),
            coverage: CoverageStateV1::Known,
            next_watermark: None,
        },
    )
}

/// Builds the one canonical exact-day projection and its retained fragment.
///
/// The page is classified once while making the fragment; the projection then
/// uses that exact fragment, preventing an independently reconstructed cell
/// set from drifting away from the retained merge evidence.
///
/// # Errors
///
/// Refuses non-exact, stale, partial, oversized, or duplicate daily evidence.
/// A capped page becomes a durable typed Capped artifact with zero cells, so a
/// storage adapter never publishes values from its observed prefix.
pub fn build_execution_topology_daily_rollup(
    authorized_scope_ref: &str,
    exact_day_horizon: &ObservabilityHorizonV1,
    observed_at_micros: i64,
    page: ObservabilityPageV1,
) -> Result<ExecutionTopologyRollupBuildV1, ExecutionTopologyRollupBuildErrorV1> {
    let fragment = build_execution_topology_rollup_fragment(
        authorized_scope_ref,
        exact_day_horizon,
        observed_at_micros,
        page,
    )?;
    let fragment_bytes = canonical_execution_topology_rollup_fragment_bytes(&fragment)?;
    if fragment_bytes.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
        return Err(ExecutionTopologyRollupBuildErrorV1::StorageBudgetExceeded);
    }
    let fragment_json = String::from_utf8(fragment_bytes)
        .map_err(|_| ExecutionTopologyRollupBuildErrorV1::FragmentSerialization)?;
    // A capped source page is retained only as its exact watermark and typed
    // coverage fact. Publishing values or cells from its prefix would turn a
    // bounded observation into a misleading complete-day aggregate.
    if fragment.is_capped() {
        return Ok(ExecutionTopologyRollupBuildV1 {
            coverage: CoverageStateV1::Capped,
            fragment,
            fragment_json,
        });
    }

    Ok(ExecutionTopologyRollupBuildV1 {
        coverage: CoverageStateV1::Known,
        fragment,
        fragment_json,
    })
}
