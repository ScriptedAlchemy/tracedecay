//! Memory application errors and the persisted numeric fact-id boundary.

use thiserror::Error;

use tracedecay_domain::{DomainError, FactOwnerV1, SourceStoreId};
use tracedecay_store::{
    FactStoreError, ProjectMemoryFeedbackRepairProgressV1, ProjectMemoryStoreError,
};

use super::anchors::EvidenceAnchorResolutionError;

#[derive(Debug, Error)]
pub enum MemoryApplicationError {
    #[error("memory owner is invalid")]
    InvalidOwner(#[from] DomainError),
    #[error("evidence anchor is invalid")]
    InvalidEvidenceAnchor(#[source] DomainError),
    #[error("memory request owner does not match the application scope")]
    OwnerMismatch {
        scope: FactOwnerV1,
        request_owner: FactOwnerV1,
    },
    #[error("fact store operation failed")]
    Store(#[from] FactStoreError),
    #[error("project-memory authority operation failed: {0}")]
    ProjectMemoryAuthority(#[from] ProjectMemoryStoreError),
    #[error("memory input is invalid: {invariant}")]
    InvalidInput { invariant: &'static str },
    #[error("persisted numeric fact cannot be represented: {invariant}")]
    UnrepresentablePersistedFact { invariant: &'static str },
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
    #[error("memory feedback history is unavailable while repair is {progress:?}")]
    FeedbackHistoryUnavailable {
        progress: ProjectMemoryFeedbackRepairProgressV1,
    },
    #[error("evidence anchor resolution failed")]
    EvidenceAnchor(#[from] EvidenceAnchorResolutionError),
}

/// Stable source identity for shipped numeric fact identifiers. It is product-owned, not
/// derived from a path, database name, or caller input.
pub const PERSISTED_FACT_ID_SOURCE_STORE: &str = "persisted-numeric-fact-id";

/// Immutable identity boundary for persisted numeric fact IDs. The authority remains
/// the sole resolver of the numeric mapping inside its transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedFactIdScope {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
}

impl PersistedFactIdScope {
    pub fn runtime(owner: FactOwnerV1) -> Result<Self, MemoryApplicationError> {
        Self::new(
            owner,
            SourceStoreId::new(PERSISTED_FACT_ID_SOURCE_STORE).map_err(|_| {
                MemoryApplicationError::InvalidInput {
                    invariant: "persisted fact-id source store identity",
                }
            })?,
        )
    }

    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
    ) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        source_store_id
            .validate()
            .map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "persisted fact-id source store identity",
            })?;
        if source_store_id.as_str() != PERSISTED_FACT_ID_SOURCE_STORE {
            return Err(MemoryApplicationError::InvalidInput {
                invariant: "fixed persisted fact-id source store identity",
            });
        }
        Ok(Self {
            owner,
            source_store_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }
}
