use std::error::Error;

use tracedecay_domain::{
    DomainError, FactAssertionId, FactEventId, FactId, FactRelationKindV1, RetrievalAnchorId,
};

use super::write::FactCommitConflict;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FactStoreError {
    #[error("fact write batch must append at least one lineage event")]
    EmptyBatch,
    #[error("{field} count {count} exceeds the maximum of {max}")]
    BatchLimitExceeded {
        field: &'static str,
        count: usize,
        max: usize,
    },
    #[error("fact write contains an item for another fact")]
    FactMismatch,
    #[error("fact write contains an item for another owner")]
    OwnerMismatch,
    #[error("fact assertion {assertion_id} has no matching lineage event")]
    MissingAssertionEvent { assertion_id: FactAssertionId },
    #[error("fact lineage event {event_id} is duplicated")]
    DuplicateEventId { event_id: FactEventId },
    #[error("fact lineage events are not in canonical order")]
    EventsOutOfOrder,
    #[error("retrieval anchor {anchor_id} is declared more than once")]
    DuplicateAnchorId { anchor_id: RetrievalAnchorId },
    #[error("fact evidence references unavailable retrieval anchor {anchor_id}")]
    MissingEvidenceAnchor { anchor_id: RetrievalAnchorId },
    #[error("retrieval anchor lineage references unavailable anchor {anchor_id}")]
    MissingAnchorLineageSource { anchor_id: RetrievalAnchorId },
    #[error("retrieval anchor lineage contains a cycle at {anchor_id}")]
    CyclicAnchorLineage { anchor_id: RetrievalAnchorId },
    #[error("fact projection payload presence disagrees with its access state")]
    PayloadAccessMismatch,
    #[error("canonical fact {fact_id} was not found")]
    FactNotFound { fact_id: FactId },
    #[error("canonical fact {fact_id} is unavailable for mutation")]
    FactUnavailable { fact_id: FactId },
    #[error("canonical fact {fact_id} was deleted")]
    FactDeleted { fact_id: FactId },
    #[error("fact query limit {limit} must be between 1 and {max}")]
    InvalidQueryLimit { limit: usize, max: usize },
    #[error("fact commit receipt is inconsistent with its event list")]
    InvalidCommitReceipt,
    #[error("canonical fact commit conflicted")]
    CommitConflict { conflict: FactCommitConflict },
    #[error("project-memory operation identity was reused with different input")]
    OperationConflict,
    #[error("canonical fact relation conflicts with an existing relation")]
    RelationConflict {
        source_fact_id: FactId,
        target_fact_id: FactId,
        existing: FactRelationKindV1,
        requested: FactRelationKindV1,
    },
    #[error("verified memory graph publication conflicted")]
    GraphConflict,
    #[error("verified memory graph authority is unavailable")]
    GraphUnavailable,
    #[error("verified memory graph operation was cancelled")]
    GraphCancelled,
    #[error("verified memory graph operation exceeded its budget")]
    GraphBudgetExhausted,
    #[error("verified memory graph operation exceeded its deadline")]
    GraphDeadlineExceeded,
    #[error("fact read operation was cancelled")]
    ReadCancelled,
    #[error("holographic vector has dimension {actual}; expected {expected}")]
    HolographicDimensionMismatch { expected: usize, actual: usize },
    #[error("fact contract validation failed")]
    Contract(#[from] DomainError),
    #[error("fact storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type FactStoreResult<T> = Result<T, FactStoreError>;
