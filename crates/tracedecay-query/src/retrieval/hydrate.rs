//! Bounded late hydration stage contracts (Plan 15 pipeline steps 10-11:
//! only the selected result set hydrates, after a repeated authorization
//! check, under byte/token/deadline budgets; every anchor earns a
//! `HydrationReceipt`).
//!
//! Rank-before-hydrate is a hard boundary: ranking, fusion, dedupe, and
//! diversity operate on compact candidates; final context hydration occurs
//! only here.

use std::time::Instant;

use thiserror::Error;
use tracedecay_domain::{
    HydrationReceipt, RankedCandidate, RetrievalAnchorId, RetrievalBudget, RetrievalRequest,
    SourceOccurrenceId,
};

/// Failures of the hydration stage. Hydration denial removes the anchor and
/// is indistinguishable from absence in public results (Plan 15).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HydrationStageError {
    #[error("hydration exceeded its byte or deadline budget")]
    BudgetExceeded,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// Request-execution state sampled before each authorized hydration step.
/// Implementations may read a cancellation token or a monotonic clock, but
/// must not expose source payload.
pub trait HydrationExecutionControlV1 {
    fn elapsed_micros(&self) -> u64;

    fn is_cancelled(&self) -> bool;
}

/// Bounded permission issued only after a selected anchor passes its repeated
/// authorization check. Sources receive this before payload work begins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HydrationWorkPermitV1 {
    pub anchor_id: RetrievalAnchorId,
    pub source_occurrence_ids: Vec<SourceOccurrenceId>,
    pub remaining_bytes: u64,
    pub remaining_deadline_micros: Option<u64>,
}

/// A payload-free estimate from an authorized source. The stage rejects an
/// over-budget estimate before calling `hydrate_authorized`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrationPreflightOutcomeV1 {
    Ready { estimated_bytes: u64 },
    Unavailable(HydrationUnavailableV1),
    BudgetExceeded,
    Cancelled,
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

    fn preflight_authorized(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1;

    fn hydrate_authorized(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<P>;
}

/// Canonical production rank-before-hydrate executor.
///
/// Determinism describes ordering and receipt formation; payloads still come
/// from the authorized owning source for each selected candidate.
pub struct CanonicalLateHydration<'a, S> {
    source: &'a mut S,
}

struct SystemHydrationExecutionControl {
    started: Instant,
}

impl HydrationExecutionControlV1 for SystemHydrationExecutionControl {
    fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

impl<'a, S> CanonicalLateHydration<'a, S> {
    pub fn new(source: &'a mut S) -> Self {
        Self { source }
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
        let control = SystemHydrationExecutionControl {
            started: Instant::now(),
        };
        self.hydrate_with_control(request, selected, budget, &control)
    }

    pub fn hydrate_with_control<P>(
        &mut self,
        request: &RetrievalRequest,
        selected: &[RankedCandidate],
        budget: &RetrievalBudget,
        control: &dyn HydrationExecutionControlV1,
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
            let outcome = if let Some(reason) = prework_unavailable(budget, control, bytes_hydrated)
            {
                HydrationOutcomeV1::Unavailable(reason)
            } else {
                match self.source.authorize(request, ranked) {
                    HydrationAuthorizationV1::Denied => HydrationOutcomeV1::Unavailable(
                        HydrationUnavailableV1::AuthorityUnavailable,
                    ),
                    HydrationAuthorizationV1::Unavailable(reason) => {
                        HydrationOutcomeV1::Unavailable(reason)
                    }
                    HydrationAuthorizationV1::Authorized => {
                        let permit = work_permit(ranked, budget, control, bytes_hydrated);
                        match self.source.preflight_authorized(request, ranked, &permit) {
                            HydrationPreflightOutcomeV1::Unavailable(reason) => {
                                HydrationOutcomeV1::Unavailable(reason)
                            }
                            HydrationPreflightOutcomeV1::BudgetExceeded => {
                                HydrationOutcomeV1::Unavailable(
                                    HydrationUnavailableV1::BudgetExceeded,
                                )
                            }
                            HydrationPreflightOutcomeV1::Cancelled => {
                                HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::Cancelled)
                            }
                            HydrationPreflightOutcomeV1::Ready { estimated_bytes } => {
                                if let Some(reason) =
                                    prework_unavailable(budget, control, bytes_hydrated)
                                {
                                    HydrationOutcomeV1::Unavailable(reason)
                                } else {
                                    let permit =
                                        work_permit(ranked, budget, control, bytes_hydrated);
                                    if estimated_bytes > permit.remaining_bytes {
                                        HydrationOutcomeV1::Unavailable(
                                            HydrationUnavailableV1::BudgetExceeded,
                                        )
                                    } else {
                                        self.hydrate_authorized(
                                            request,
                                            ranked,
                                            &permit,
                                            budget,
                                            control,
                                            &mut bytes_hydrated,
                                            &mut receipts,
                                        )?
                                    }
                                }
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

    #[allow(clippy::too_many_arguments)]
    fn hydrate_authorized<P>(
        &mut self,
        request: &RetrievalRequest,
        ranked: &RankedCandidate,
        permit: &HydrationWorkPermitV1,
        budget: &RetrievalBudget,
        control: &dyn HydrationExecutionControlV1,
        bytes_hydrated: &mut u64,
        receipts: &mut Vec<HydrationReceipt>,
    ) -> Result<HydrationOutcomeV1<P>, HydrationStageError>
    where
        S: LateHydrationSource<P>,
    {
        let read = self.source.hydrate_authorized(request, ranked, permit);
        if let Some(reason) = prework_unavailable(budget, control, *bytes_hydrated) {
            return Ok(HydrationOutcomeV1::Unavailable(reason));
        }
        Ok(match read {
            HydrationReadOutcomeV1::Complete { payload, receipt } => {
                if !receipt.authorized {
                    HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable)
                } else if receipt.bytes_hydrated > permit.remaining_bytes {
                    HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
                } else {
                    validate_receipt(ranked, permit, &receipt)?;
                    *bytes_hydrated = bytes_hydrated
                        .checked_add(receipt.bytes_hydrated)
                        .ok_or(HydrationStageError::BudgetExceeded)?;
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
                    HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable)
                } else if receipt.bytes_hydrated > permit.remaining_bytes {
                    HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
                } else {
                    validate_receipt(ranked, permit, &receipt)?;
                    *bytes_hydrated = bytes_hydrated
                        .checked_add(receipt.bytes_hydrated)
                        .ok_or(HydrationStageError::BudgetExceeded)?;
                    receipts.push(receipt);
                    HydrationOutcomeV1::Partial { payload, reason }
                }
            }
            HydrationReadOutcomeV1::Unavailable(reason) => HydrationOutcomeV1::Unavailable(reason),
            HydrationReadOutcomeV1::BudgetExceeded => {
                HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
            }
            HydrationReadOutcomeV1::Cancelled => {
                HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::Cancelled)
            }
        })
    }
}

fn prework_unavailable(
    budget: &RetrievalBudget,
    control: &dyn HydrationExecutionControlV1,
    bytes_hydrated: u64,
) -> Option<HydrationUnavailableV1> {
    if control.is_cancelled() {
        return Some(HydrationUnavailableV1::Cancelled);
    }
    if bytes_hydrated >= budget.max_hydration_bytes {
        return Some(HydrationUnavailableV1::BudgetExceeded);
    }
    let elapsed = control.elapsed_micros();
    budget
        .deadline_micros
        .is_some_and(|deadline| elapsed >= deadline)
        .then_some(HydrationUnavailableV1::BudgetExceeded)
}

fn work_permit(
    ranked: &RankedCandidate,
    budget: &RetrievalBudget,
    control: &dyn HydrationExecutionControlV1,
    bytes_hydrated: u64,
) -> HydrationWorkPermitV1 {
    let mut source_occurrence_ids = ranked
        .candidate
        .occurrences
        .iter()
        .map(|occurrence| occurrence.source_occurrence_id.clone())
        .collect::<Vec<_>>();
    source_occurrence_ids.sort();
    source_occurrence_ids.dedup();
    let elapsed = control.elapsed_micros();
    HydrationWorkPermitV1 {
        anchor_id: ranked.candidate.anchor_id.clone(),
        source_occurrence_ids,
        remaining_bytes: budget.max_hydration_bytes.saturating_sub(bytes_hydrated),
        remaining_deadline_micros: budget
            .deadline_micros
            .map(|deadline| deadline.saturating_sub(elapsed)),
    }
}

fn validate_receipt(
    ranked: &RankedCandidate,
    permit: &HydrationWorkPermitV1,
    receipt: &HydrationReceipt,
) -> Result<(), HydrationStageError> {
    if receipt.anchor_id != permit.anchor_id
        || !permit
            .source_occurrence_ids
            .contains(&receipt.source_occurrence_id)
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
