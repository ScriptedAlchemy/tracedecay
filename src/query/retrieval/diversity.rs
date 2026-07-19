//! Deterministic diversity-cap stage contracts (Plan 15 pipeline step 9:
//! profile-owned caps per source namespace, source instance, repository,
//! session/thread, logical-copy cluster, and evidence role apply after
//! fusion; a cap must carry its locked evaluation anchor — absent evidence
//! leaves the cap disabled except resource-safety ceilings).

use thiserror::Error;
use tracedecay_domain::{DiversityPolicy, FusedCandidate, RankedCandidate, RankingDecision};

/// Failures of the diversity stage.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DiversityStageError {
    #[error("an enabled diversity cap lacks its locked evaluation anchor")]
    CapWithoutEvidenceAnchor,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// One recorded diversity-cap decision (Plan 15: `RankingDecision` records
/// each diversity-cap decision).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiversityDecisionV1 {
    pub capped: Vec<tracedecay_domain::RetrievalAnchorId>,
    pub decision: RankingDecision,
}

/// The deterministic diversity-cap stage contract. Caps apply after fusion
/// and preserve the fused total order of the survivors.
pub trait DiversityCapStage {
    /// Apply `policy` to an ordered fused candidate list, recording one
    /// decision per cap application. Disabled caps (no evaluation anchor)
    /// apply only as resource-safety ceilings.
    fn apply_caps(
        &self,
        policy: &DiversityPolicy,
        candidates: Vec<FusedCandidate>,
    ) -> Result<(Vec<RankedCandidate>, Vec<DiversityDecisionV1>), DiversityStageError>;
}
