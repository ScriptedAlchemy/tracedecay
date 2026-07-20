//! Projection execution and atomic publication handoff (Plan 25).
//!
//! Projectors receive one immutable [`ProjectionBatchRequestV1`] and return
//! one complete deterministic receipt. The orchestration helper verifies the
//! request and receipt before constructing a publication handoff; malformed,
//! partial, failed, or skipped batches never cross that boundary. No-op
//! generations are completed locally from the explicit reused partition, so
//! they make zero projector calls.
//!
//! This module defines contracts only. Store-owned transactions, active
//! pointers, retries, checkpoints, and scheduling remain outside the code
//! index.

use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
};

pub use super::receipts::{
    ChunkProjectionDecisionV1, ProjectionReceiptErrorV1, batch_can_activate,
    batch_proves_zero_work, build_batch_receipt, changeset_is_noop, decisions_for_noop,
    expected_publication_digest, expected_request_digest, verify_batch_receipt,
};

/// A projection adapter failure before a complete receipt is available.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProjectionSinkErrorV1 {
    #[error("projection sink rejected the batch: {0}")]
    Rejected(String),
}

/// The storage-neutral projector contract.
pub trait CodeChunkProjectionSink {
    /// Project one complete changed-chunk request. Implementations may return
    /// failed/skipped receipts as inspectable evidence; only the validated
    /// publication handoff decides activation eligibility.
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1>;
}

/// Why a projection batch cannot cross the atomic publication boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProjectionPublicationErrorV1 {
    #[error(transparent)]
    Sink(#[from] ProjectionSinkErrorV1),
    #[error(transparent)]
    Receipt(#[from] ProjectionReceiptErrorV1),
    #[error("the complete receipt contains failed or skipped projection work")]
    NotActivatable,
}

/// A request and complete verified receipt ready for one store-owned atomic
/// publication transaction.
///
/// Fields are private so a handoff cannot be assembled without running the
/// deterministic request/receipt verification gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionPublicationHandoffV1 {
    request: ProjectionBatchRequestV1,
    receipt: ProjectionBatchReceiptV1,
}

impl ProjectionPublicationHandoffV1 {
    pub fn request(&self) -> &ProjectionBatchRequestV1 {
        &self.request
    }

    pub fn receipt(&self) -> &ProjectionBatchReceiptV1 {
        &self.receipt
    }

    pub fn publication_digest(&self) -> &ManifestDigest {
        &self.receipt.publication_digest
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.receipt.source_generation
    }

    /// Consume the validated handoff into the exact request and receipt a
    /// store transaction persists and activates together.
    pub fn into_parts(self) -> (ProjectionBatchRequestV1, ProjectionBatchReceiptV1) {
        (self.request, self.receipt)
    }
}

/// Execute projection work and prepare an atomic publication handoff.
///
/// No-op requests bypass `sink` and deterministically emit reused receipts.
/// All other requests invoke the sink exactly once. In either case the full
/// receipt is verified before activation eligibility is checked.
pub fn project_for_publication<S: CodeChunkProjectionSink>(
    sink: &mut S,
    request: ProjectionBatchRequestV1,
) -> Result<ProjectionPublicationHandoffV1, ProjectionPublicationErrorV1> {
    let receipt = if changeset_is_noop(&request.changes) {
        build_batch_receipt(&request, &decisions_for_noop(&request.changes))?
    } else {
        sink.project_changed_chunks(request.clone())?
    };
    verify_batch_receipt(&request, &receipt)?;
    if !batch_can_activate(&receipt) {
        return Err(ProjectionPublicationErrorV1::NotActivatable);
    }
    Ok(ProjectionPublicationHandoffV1 { request, receipt })
}
