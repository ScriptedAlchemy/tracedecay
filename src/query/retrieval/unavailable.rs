//! Typed unavailable-lane reporting (Plan 15: an unavailable authority is
//! capability-reported, never simulated or replaced with a heuristic
//! lookalike; Plan 25: semantic/task lanes report `unavailable` until their
//! delivery PRs).
//!
//! PR9 ships exact, lexical, and graph lanes. Semantic (PR10), temporal
//! export (Plan 23 port), task/session (PR17), and diagnostic adapters land
//! later; until then they report through this contract.

use tracedecay_domain::{RetrievalFailure, RetrieverBatch, RetrieverKind, RetrieverOutcome};

/// A capability-truthful report that one lane is unavailable at PR9 (Plan
/// 15: each lane reports freshness, coverage, cancellation, and
/// partial/unavailable state independently).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnavailableLaneReportV1 {
    pub lane: RetrieverKind,
    pub reason: RetrievalFailure,
    /// The delivery PR that owns this lane's adapter, for capability
    /// surfaces (e.g. `PR10`, `PR17`).
    pub owning_delivery: &'static str,
}

/// Contract for lanes that are capability-reported unavailable at this
/// dependency point. The reported outcome is the lane's entire contribution:
/// it emits no candidates and never fabricates coverage.
pub trait CapabilityReportedLane {
    /// The lane this report covers.
    fn lane(&self) -> RetrieverKind;

    /// The typed unavailable outcome for this lane.
    fn report(&self) -> UnavailableLaneReportV1;

    /// The lane outcome as consumed by fusion: always
    /// `RetrieverOutcome::Unavailable` with the reported reason.
    fn outcome<E>(&self) -> RetrieverOutcome<RetrieverBatch<E>>;
}
