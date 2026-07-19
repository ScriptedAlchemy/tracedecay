//! Bounded late hydration stage contracts (Plan 15 pipeline steps 10-11:
//! only the selected result set hydrates, after a repeated authorization
//! check, under byte/token/deadline budgets; every anchor earns a
//! `HydrationReceipt`).
//!
//! Rank-before-hydrate is a hard boundary: ranking, fusion, dedupe, and
//! diversity operate on compact candidates; final context hydration occurs
//! only here.

use thiserror::Error;
use tracedecay_domain::{
    HydrationReceipt, RankedCandidate, RetrievalAnchorId, RetrievalBudget, RetrievalRequest,
};

/// Failures of the hydration stage. Hydration denial removes the anchor and
/// is indistinguishable from absence in public results (Plan 15).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HydrationStageError {
    #[error("hydration exceeded its byte or deadline budget")]
    BudgetExceeded,
    #[error("hydration was cancelled")]
    Cancelled,
    #[error("an anchor failed its authorization recheck")]
    AuthorizationRecheckFailed,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// The bounded hydration plan derived from the final ranked set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HydrationPlanV1 {
    pub anchors: Vec<RetrievalAnchorId>,
    pub budget: RetrievalBudget,
}

/// The late hydration stage contract (Plan 15: recheck authorization and
/// hydrate final context for the selected anchors through each owning store;
/// record one receipt per anchor).
pub trait LateHydrationStage {
    /// Derive the bounded hydration plan for the selected ranked candidates.
    fn plan(&self, selected: &[RankedCandidate], budget: &RetrievalBudget) -> HydrationPlanV1;

    /// Execute the plan against the pinned request, re-checking
    /// authorization per anchor and emitting one receipt per anchor.
    fn hydrate(
        &self,
        request: &RetrievalRequest,
        plan: &HydrationPlanV1,
    ) -> Result<Vec<HydrationReceipt>, HydrationStageError>;
}
