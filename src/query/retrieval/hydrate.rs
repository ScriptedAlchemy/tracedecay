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

/// Internal authorization result. `Denied` is intentionally absent from the
/// public hydration outcome and coalesces with authority unavailability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrationAuthorizationV1 {
    Authorized,
    Denied,
    Unavailable(HydrationUnavailableV1),
}

/// Sanitized typed hydration failure. It carries no source-identifying detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrationUnavailableV1 {
    AuthorityUnavailable,
    Incompatible,
    Stale,
    Invalid,
    Internal,
    BudgetExceeded,
    Cancelled,
}

/// Owning-store read outcome after authorization succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HydrationReadOutcomeV1<P> {
    Complete {
        payload: P,
        receipt: HydrationReceipt,
    },
    Partial {
        payload: P,
        receipt: HydrationReceipt,
        reason: HydrationUnavailableV1,
    },
    Unavailable(HydrationUnavailableV1),
    BudgetExceeded,
    Cancelled,
}

/// Public positional outcome for one selected rank. Hydration cannot reorder
/// or backfill this slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HydrationOutcomeV1<P> {
    Complete(P),
    Partial {
        payload: P,
        reason: HydrationUnavailableV1,
    },
    Unavailable(HydrationUnavailableV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HydratedRankedCandidateV1<P> {
    pub ranked: RankedCandidate,
    pub outcome: HydrationOutcomeV1<P>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HydrationPageV1<P> {
    pub results: Vec<HydratedRankedCandidateV1<P>>,
    pub receipts: Vec<HydrationReceipt>,
}

/// Two-phase owning-source port. A denied authorization result must not call
/// `hydrate_authorized`, which keeps denial observationally equivalent to an
/// unavailable source and prevents payload reads.
pub trait LateHydrationSource<P> {
    fn authorize(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
    ) -> HydrationAuthorizationV1;

    fn hydrate_authorized(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        remaining_bytes: u64,
    ) -> HydrationReadOutcomeV1<P>;
}

pub struct DeterministicLateHydration<'a, S> {
    source: &'a mut S,
}

impl<'a, S> DeterministicLateHydration<'a, S> {
    pub fn new(source: &'a mut S) -> Self {
        Self { source }
    }

    pub fn plan(selected: &[RankedCandidate], budget: &RetrievalBudget) -> HydrationPlanV1 {
        HydrationPlanV1 {
            anchors: selected
                .iter()
                .take(budget.max_hydrated_results as usize)
                .map(|ranked| ranked.candidate.anchor_id.clone())
                .collect(),
            budget: *budget,
        }
    }

    pub fn hydrate<P>(
        &mut self,
        request: &RetrievalRequest,
        selected: &[RankedCandidate],
        budget: &RetrievalBudget,
    ) -> Result<HydrationPageV1<P>, HydrationStageError>
    where
        S: LateHydrationSource<P>,
    {
        budget
            .validate()
            .map_err(|error| HydrationStageError::Contract(error.to_string()))?;
        let selected = &selected[..selected.len().min(budget.max_hydrated_results as usize)];
        let mut bytes_hydrated = 0_u64;
        let mut results = Vec::with_capacity(selected.len());
        let mut receipts = Vec::with_capacity(selected.len());

        for ranked in selected {
            let outcome = if bytes_hydrated >= budget.max_hydration_bytes {
                HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
            } else {
                match self.source.authorize(request, ranked) {
                    HydrationAuthorizationV1::Denied => HydrationOutcomeV1::Unavailable(
                        HydrationUnavailableV1::AuthorityUnavailable,
                    ),
                    HydrationAuthorizationV1::Unavailable(reason) => {
                        HydrationOutcomeV1::Unavailable(reason)
                    }
                    HydrationAuthorizationV1::Authorized => {
                        let remaining = budget.max_hydration_bytes - bytes_hydrated;
                        match self.source.hydrate_authorized(request, ranked, remaining) {
                            HydrationReadOutcomeV1::Complete { payload, receipt } => {
                                if !receipt.authorized {
                                    HydrationOutcomeV1::Unavailable(
                                        HydrationUnavailableV1::AuthorityUnavailable,
                                    )
                                } else if receipt.bytes_hydrated > remaining {
                                    HydrationOutcomeV1::Unavailable(
                                        HydrationUnavailableV1::BudgetExceeded,
                                    )
                                } else {
                                    validate_receipt(ranked, &receipt)?;
                                    bytes_hydrated += receipt.bytes_hydrated;
                                    receipts.push(receipt);
                                    HydrationOutcomeV1::Complete(payload)
                                }
                            }
                            HydrationReadOutcomeV1::Partial {
                                payload,
                                receipt,
                                reason,
                            } => {
                                if !receipt.authorized {
                                    HydrationOutcomeV1::Unavailable(
                                        HydrationUnavailableV1::AuthorityUnavailable,
                                    )
                                } else if receipt.bytes_hydrated > remaining {
                                    HydrationOutcomeV1::Unavailable(
                                        HydrationUnavailableV1::BudgetExceeded,
                                    )
                                } else {
                                    validate_receipt(ranked, &receipt)?;
                                    bytes_hydrated += receipt.bytes_hydrated;
                                    receipts.push(receipt);
                                    HydrationOutcomeV1::Partial { payload, reason }
                                }
                            }
                            HydrationReadOutcomeV1::Unavailable(reason) => {
                                HydrationOutcomeV1::Unavailable(reason)
                            }
                            HydrationReadOutcomeV1::BudgetExceeded => {
                                HydrationOutcomeV1::Unavailable(
                                    HydrationUnavailableV1::BudgetExceeded,
                                )
                            }
                            HydrationReadOutcomeV1::Cancelled => {
                                HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::Cancelled)
                            }
                        }
                    }
                }
            };
            results.push(HydratedRankedCandidateV1 {
                ranked: ranked.clone(),
                outcome,
            });
        }
        Ok(HydrationPageV1 { results, receipts })
    }
}

fn validate_receipt(
    ranked: &RankedCandidate,
    receipt: &HydrationReceipt,
) -> Result<(), HydrationStageError> {
    if receipt.anchor_id != ranked.candidate.anchor_id
        || !ranked.candidate.occurrences.iter().any(|occurrence| {
            occurrence.source_occurrence_id == receipt.source_occurrence_id
                && occurrence.freshness == receipt.freshness
        })
    {
        return Err(HydrationStageError::Contract(
            "hydration receipt is not bound to selected occurrence provenance".to_owned(),
        ));
    }
    Ok(())
}
