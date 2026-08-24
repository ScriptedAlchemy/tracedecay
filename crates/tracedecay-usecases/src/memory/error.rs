//! Memory application errors.

use std::fmt::Debug;

use thiserror::Error;

use tracedecay_domain::{DomainError, FactOwnerV1};
use tracedecay_store::FactStoreError;

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
    #[error("memory input is invalid: {invariant}")]
    InvalidInput { invariant: &'static str },
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
    #[error("evidence anchor resolution failed")]
    EvidenceAnchor(#[from] EvidenceAnchorResolutionError),
}

/// Mutation failure that preserves any canonical authority result returned
/// after the durable transaction settled.
///
/// Callers with an effect boundary can emit a truthful partial effect from the
/// embedded result. Callers without one may convert this error back to
/// [`MemoryApplicationError`], deliberately discarding only their own ability
/// to settle that external effect.
#[derive(Debug, Error)]
pub enum MemoryMutationError<T: Debug> {
    #[error(transparent)]
    Application(#[from] MemoryApplicationError),
    #[error("memory authority returned a settled result that failed validation: {error}")]
    InvalidAuthorityResult {
        #[source]
        error: MemoryApplicationError,
        authority_result: T,
    },
}

impl<T: Debug> MemoryMutationError<T> {
    pub fn map_authority_result<U: Debug>(
        self,
        map: impl FnOnce(T) -> U,
    ) -> MemoryMutationError<U> {
        match self {
            Self::Application(error) => MemoryMutationError::Application(error),
            Self::InvalidAuthorityResult {
                error,
                authority_result,
            } => MemoryMutationError::InvalidAuthorityResult {
                error,
                authority_result: map(authority_result),
            },
        }
    }
}

impl<T: Debug> From<MemoryMutationError<T>> for MemoryApplicationError {
    fn from(error: MemoryMutationError<T>) -> Self {
        match error {
            MemoryMutationError::Application(error) => error,
            MemoryMutationError::InvalidAuthorityResult { error, .. } => error,
        }
    }
}

pub(super) fn settle_authority_result<T: Debug>(
    authority_result: T,
    validate: impl FnOnce(&T) -> Result<(), MemoryApplicationError>,
) -> Result<T, MemoryMutationError<T>> {
    match validate(&authority_result) {
        Ok(()) => Ok(authority_result),
        Err(error) => Err(MemoryMutationError::InvalidAuthorityResult {
            error,
            authority_result,
        }),
    }
}
