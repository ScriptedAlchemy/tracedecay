//! Driver-neutral persistence records for authorized scope-set CAS.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{ManifestDigest, ScopeSetId, ScopeSetRevision};

pub const MAX_AUTHORIZED_SCOPE_SET_BYTES_V1: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ScopeSetStoreContractError {
    #[error("authorized scope-set payload is empty or exceeds its bound")]
    InvalidPayload,
    #[error("authorized scope-set CAS must create revision one or advance exactly once")]
    NonSequentialRevision,
    #[error("authorized scope-set record is invalid: {0}")]
    InvalidRecord(String),
}

/// Canonical application payload retained byte-for-byte by the lower store.
///
/// The store does not reinterpret resolved roots. The runtime adapter decodes
/// this payload through the application contract before admitting it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedScopeSetRecordV1 {
    pub scope_set_id: ScopeSetId,
    pub revision: ScopeSetRevision,
    pub digest: ManifestDigest,
    pub canonical_payload: Vec<u8>,
}

impl AuthorizedScopeSetRecordV1 {
    pub fn new(
        scope_set_id: ScopeSetId,
        revision: ScopeSetRevision,
        digest: ManifestDigest,
        canonical_payload: Vec<u8>,
    ) -> Result<Self, ScopeSetStoreContractError> {
        let record = Self {
            scope_set_id,
            revision,
            digest,
            canonical_payload,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), ScopeSetStoreContractError> {
        self.scope_set_id
            .validate()
            .map_err(|error| ScopeSetStoreContractError::InvalidRecord(error.to_string()))?;
        self.revision
            .validate()
            .map_err(|error| ScopeSetStoreContractError::InvalidRecord(error.to_string()))?;
        self.digest
            .validate()
            .map_err(|error| ScopeSetStoreContractError::InvalidRecord(error.to_string()))?;
        if self.canonical_payload.is_empty()
            || self.canonical_payload.len() > MAX_AUTHORIZED_SCOPE_SET_BYTES_V1
        {
            return Err(ScopeSetStoreContractError::InvalidPayload);
        }
        Ok(())
    }
}

/// One optimistic compare-and-swap command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeSetCompareAndSwapV1 {
    pub expected_revision: Option<ScopeSetRevision>,
    pub next: AuthorizedScopeSetRecordV1,
}

impl ScopeSetCompareAndSwapV1 {
    pub fn new(
        expected_revision: Option<ScopeSetRevision>,
        next: AuthorizedScopeSetRecordV1,
    ) -> Result<Self, ScopeSetStoreContractError> {
        let command = Self {
            expected_revision,
            next,
        };
        command.validate()?;
        Ok(command)
    }

    pub fn validate(&self) -> Result<(), ScopeSetStoreContractError> {
        self.next.validate()?;
        let expected_next = match self.expected_revision {
            Some(revision) => revision
                .checked_next()
                .map_err(|_| ScopeSetStoreContractError::NonSequentialRevision)?,
            None => ScopeSetRevision::new(1)
                .map_err(|_| ScopeSetStoreContractError::NonSequentialRevision)?,
        };
        if self.next.revision != expected_next {
            return Err(ScopeSetStoreContractError::NonSequentialRevision);
        }
        Ok(())
    }
}

/// Truthful CAS result. A conflict returns the exact observed revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeSetCasOutcomeV1 {
    Applied(AuthorizedScopeSetRecordV1),
    Conflict {
        expected_revision: Option<ScopeSetRevision>,
        actual_revision: Option<ScopeSetRevision>,
    },
}
