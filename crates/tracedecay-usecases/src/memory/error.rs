//! Memory application error surface and the immutable V1 compatibility scope.

use thiserror::Error;

use tracedecay_domain::{DomainError, FactOwnerV1, SourceStoreId};
use tracedecay_store::{
    FactProposalStoreError, FactStoreError, ProjectMemoryFeedbackRepairProgressV1,
    ProjectMemoryStoreError,
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
    #[error("memory authority operation failed")]
    Authority(#[from] FactProposalStoreError),
    #[error("memory compatibility authority operation failed: {0}")]
    Compatibility(#[from] ProjectMemoryStoreError),
    #[error("memory compatibility input is invalid: {invariant}")]
    InvalidCompatibilityInput { invariant: &'static str },
    #[error("memory compatibility projection cannot be represented by the V1 surface: {invariant}")]
    IncompatibleLegacyProjection { invariant: &'static str },
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
    #[error("memory feedback history is unavailable while repair is {progress:?}")]
    FeedbackHistoryUnavailable {
        progress: ProjectMemoryFeedbackRepairProgressV1,
    },
    #[error("evidence anchor resolution failed")]
    EvidenceAnchor(#[from] EvidenceAnchorResolutionError),
}

/// Stable source identity for the V1 memory mirror. It is product-owned, not
/// derived from a path, database name, or caller input.
pub const RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE: &str = "legacy-memory-v1";

/// Immutable identity boundary for V1 numeric fact IDs. The authority remains
/// the sole resolver of the numeric mapping inside its transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryCompatibilityScope {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
}

impl MemoryCompatibilityScope {
    pub fn runtime(owner: FactOwnerV1) -> Result<Self, MemoryApplicationError> {
        Self::new(
            owner,
            SourceStoreId::new(RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE).map_err(|_| {
                MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "runtime compatibility source store identity",
                }
            })?,
        )
    }

    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
    ) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        source_store_id.validate().map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "compatibility source store identity",
            }
        })?;
        if source_store_id.as_str() != RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE {
            return Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "fixed V1 compatibility source store identity",
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
