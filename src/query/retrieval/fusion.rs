//! Deterministic fixed-point fusion stage contracts (Plan 15 pipeline steps
//! 4-8; Plan 25: `src/query/retrieval/fusion.rs` operates on compact
//! candidates with deterministic fixed-point contributions, complete
//! comparator provenance, and source/file caps).
//!
//! RRF may be evaluated as a profile candidate inside this generic
//! fixed-point framework; no constant or weight is production authority
//! before Plan 15 accepts it.

use thiserror::Error;
use tracedecay_domain::{
    ExactClass, FusedCandidate, FusionProfile, OccurrenceProvenance, RankedCandidate,
    RetrieverBatch, RetrieverKind, RetrieverOutcome,
};

/// Failures of the fusion stage. Fusion never substitutes or simulates a
/// missing lane; it composes the typed outcomes it is given.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FusionStageError {
    #[error("a required exact or lexical lane outcome is unavailable")]
    RequiredLaneUnavailable,
    #[error("candidate evidence is missing for a returned occurrence")]
    MissingOccurrenceEvidence,
    #[error("fixed-point arithmetic overflowed")]
    FixedPointOverflow,
    #[error("profile references a retriever outside the admitted lane set")]
    ProfileLaneMismatch,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// One fusion input: the admitted lane batches with their typed outcomes for
/// one pinned snapshot (Plan 15 pipeline step 3: each lane contributes its
/// entire committed prefix or none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusionStageInput {
    pub profile: FusionProfile,
    pub lanes: Vec<(
        RetrieverKind,
        RetrieverOutcome<RetrieverBatch<OccurrenceProvenance>>,
    )>,
}

/// The deterministic fusion stage contract (Plan 15: group contributions by
/// stable anchor plus logical evidence identity; total order is exact class,
/// utility, source validity, stable anchor ID, logical evidence ID, then
/// ordered source occurrence IDs).
pub trait DeterministicFusionStage {
    /// Partition candidates into exact tiers and fuse approximate
    /// contributions with checked fixed-point arithmetic. Exact admission
    /// derives only from validated proofs.
    fn fuse(&self, input: &FusionStageInput) -> Result<Vec<FusedCandidate>, FusionStageError>;

    /// Compute the final deterministic order over fused candidates. One
    /// hundred shuffled producer/completion runs must produce byte-identical
    /// IDs, order, contributions, explanations, coverage, and cursors
    /// (Plan 25 acceptance).
    fn order(&self, candidates: Vec<FusedCandidate>) -> Vec<RankedCandidate>;
}

/// Comparator provenance record for one pairwise ordering decision (Plan 15:
/// explanations render from recorded decisions, never from final scalars).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusionComparatorRecordV1 {
    pub exact_class: ExactClass,
    pub utility_micros: u64,
    pub comparator_revision: tracedecay_domain::ComponentRevision,
}
