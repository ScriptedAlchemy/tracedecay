//! Encoded code-index storage owned by the registered project runtime.

use tracedecay_domain::{CodeGenerationId, ManifestDigest, WorktreeId};
use thiserror::Error;

use super::StoreOperationIdV1;

/// A region fits independently through exact SQL. A payload has no total-size
/// ceiling; the region bound is not a codec or generation-size limit.
pub const CODE_INDEX_REGION_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexGenerationKey {
    pub worktree_id: WorktreeId,
    pub generation_id: CodeGenerationId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexPayloadIdentity {
    pub digest: ManifestDigest,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexGenerationRecord {
    pub key: CodeIndexGenerationKey,
    pub manifest: CodeIndexPayloadIdentity,
}

pub struct CodeIndexGenerationPage {
    records: Vec<CodeIndexGenerationRecord>,
    _reservation: Box<dyn Send + Sync>,
}

impl CodeIndexGenerationPage {
    pub fn from_reserved(
        records: Vec<CodeIndexGenerationRecord>,
        reservation: Box<dyn Send + Sync>,
    ) -> Self {
        Self { records, _reservation: reservation }
    }

    pub fn records(&self) -> &[CodeIndexGenerationRecord] {
        &self.records
    }
}

#[derive(Clone, Debug)]
pub struct CodeIndexPublicationStage {
    pub operation_id: StoreOperationIdV1,
    pub generation: CodeIndexGenerationKey,
    /// Exact incumbent publication bytes, not a decoded generation guess.
    pub expected_head: Option<std::sync::Arc<CodeIndexStoredBytes>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexPublishOutcome {
    Published,
    AlreadyPublished,
    HeadConflict,
}

#[derive(Debug, Error)]
pub enum CodeIndexStorageError {
    #[error("invalid code-index storage request: {0}")]
    InvalidRequest(String),
    #[error("code-index storage is incomplete or corrupt: {0}")]
    Corrupt(String),
    #[error("code-index staging operation conflicts with its persisted owner")]
    StagingConflict,
    #[error("code-index generation is retained by a head or graph reader")]
    Retained,
    #[error("code-index generation was retired by its canonical receipt")]
    Retired,
    #[error("code-index payload reclamation must finish before this digest is restaged")]
    ReclamationPending,
    #[error("code-index storage allocation was refused: {0}")]
    AllocationRefused(String),
    #[error("registered code-index storage failed: {0}")]
    Infrastructure(String),
}

pub type CodeIndexStorageResult<T> = Result<T, CodeIndexStorageError>;

/// The encoded capacity and its resident charge have the same lifetime.
/// Field order releases the bytes before releasing their reservation.
pub struct CodeIndexStoredBytes {
    bytes: Vec<u8>,
    reservation: Box<dyn Send + Sync>,
}

impl CodeIndexStoredBytes {
    pub fn from_reserved(bytes: Vec<u8>, reservation: Box<dyn Send + Sync>) -> Self {
        Self { bytes, reservation }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    /// The caller must immediately retain both values in their next owner.
    pub fn into_parts(self) -> (Vec<u8>, Box<dyn Send + Sync>) {
        (self.bytes, self.reservation)
    }
}

impl std::ops::Deref for CodeIndexStoredBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl PartialEq for CodeIndexStoredBytes {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for CodeIndexStoredBytes {}

impl std::fmt::Debug for CodeIndexStoredBytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CodeIndexStoredBytes")
            .field("len", &self.bytes.len())
            .field("capacity", &self.bytes.capacity())
            .finish_non_exhaustive()
    }
}

/// Charge before SQL, transport and destination materialization. The returned
/// guard stays live until all allocations covered by the charge are released.
/// Database purpose mounting binds this to the existing process resident ledger.
pub trait CodeIndexAllocationAdmission: Send + Sync {
    fn reserve(&self, bytes: u64) -> CodeIndexStorageResult<Box<dyn Send + Sync>>;
}
