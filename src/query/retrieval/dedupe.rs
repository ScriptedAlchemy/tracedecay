//! Source-aware dedupe stage contracts (Plan 15 pipeline steps 4 and 8:
//! duplicate rows from one immutable source occurrence are collapsed before
//! fusion; cross-source copies collapse only through an evidence-backed
//! logical-copy relation; independent corroboration and contradictions are
//! preserved).
//!
//! Dedupe never collapses merely by content hash, title, timestamp, or
//! embedding similarity.

use thiserror::Error;
use tracedecay_domain::{
    FusedCandidate, LogicalCopyClusterId, OccurrenceProvenance, RankingDecision, SourceOccurrenceId,
};

/// Failures of the dedupe stage.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DedupeStageError {
    #[error("duplicate candidate rows for one source occurrence lack a collapse decision")]
    UncollapsedDuplicate,
    #[error("a logical-copy relation lacks its evidence anchor")]
    CopyRelationWithoutEvidence,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// One recorded dedupe decision (Plan 15: `RankingDecision` records
/// same-source duplicate collapse and logical-copy representative
/// selection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedupeDecisionV1 {
    pub kept_occurrence: SourceOccurrenceId,
    pub collapsed_occurrences: Vec<SourceOccurrenceId>,
    pub copy_cluster: Option<LogicalCopyClusterId>,
    pub decision: RankingDecision,
}

/// The same-source duplicate collapse contract (Plan 15 pipeline step 4).
pub trait SameSourceDedupeStage {
    /// Collapse duplicate rows for the same source occurrence before fusion,
    /// recording one decision per collapse.
    fn collapse_same_source(
        &self,
        candidates: &[OccurrenceProvenance],
    ) -> Result<Vec<DedupeDecisionV1>, DedupeStageError>;
}

/// The evidence-backed logical-copy collapse contract (Plan 15 pipeline
/// step 8): resolve copy clusters, preserve independent corroboration and
/// every admitted contradiction, then choose representatives.
pub trait LogicalCopyCollapseStage {
    /// Choose cluster representatives over fused candidates, recording one
    /// representative-selection decision per cluster.
    fn select_representatives(
        &self,
        candidates: Vec<FusedCandidate>,
    ) -> Result<Vec<FusedCandidate>, DedupeStageError>;
}
