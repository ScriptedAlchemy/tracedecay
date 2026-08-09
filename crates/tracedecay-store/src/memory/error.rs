use std::error::Error;

use tracedecay_domain::{DomainError, FactAssertionId, FactEventId, RetrievalAnchorId};

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
    #[error("legacy fact id {legacy_fact_id} must be positive")]
    InvalidLegacyFactId { legacy_fact_id: i64 },
    #[error("fact query limit {limit} must be between 1 and {max}")]
    InvalidQueryLimit { limit: usize, max: usize },
    #[error("fact commit receipt is inconsistent with its event list")]
    InvalidCommitReceipt,
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

#[derive(Debug, thiserror::Error)]
pub enum ProjectMemoryStoreError {
    #[error(transparent)]
    Store(#[from] FactStoreError),
}

pub type ProjectMemoryResult<T> = Result<T, ProjectMemoryStoreError>;
