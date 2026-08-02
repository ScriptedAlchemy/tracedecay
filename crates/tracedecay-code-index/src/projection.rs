//! Projection execution and atomic publication handoff.
//!
//! Projectors receive one immutable [`ProjectionBatchRequestV1`] and return
//! one complete deterministic receipt. The orchestration helper verifies the
//! request and receipt before constructing a publication handoff; malformed,
//! partial, failed, or skipped batches never cross that boundary. A true
//! no-op keeps its projection key and is completed locally from the explicit
//! reused partition; a projection-key replay always reaches the projector.
//!
//! This module defines contracts only. Store-owned transactions, active
//! pointers, retries, checkpoints, and scheduling remain outside the code
//! index.

use thiserror::Error;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, ManifestDigest,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionReplayReasonV1,
};

pub use super::receipts::{
    ChunkProjectionDecisionV1, ProjectionReceiptErrorV1, batch_can_activate,
    batch_proves_zero_work, build_batch_receipt, changeset_is_noop, decisions_for_noop,
    expected_publication_digest, expected_request_digest, verify_batch_receipt,
};
use super::receipts::{
    ProjectionRequestEvidenceV1, PublicationDigestTrustV1, build_batch_receipt_verified,
    verify_batch_receipt_verified,
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

    /// Restore a durable handoff only after repeating the same receipt and
    /// activation checks used by live projection publication.
    pub(crate) fn restore(
        request: ProjectionBatchRequestV1,
        receipt: ProjectionBatchReceiptV1,
    ) -> Result<Self, ProjectionPublicationErrorV1> {
        verify_batch_receipt(&request, &receipt)?;
        if !batch_can_activate(&receipt) {
            return Err(ProjectionPublicationErrorV1::NotActivatable);
        }
        Ok(Self { request, receipt })
    }
}

/// Execute projection work and prepare an atomic publication handoff.
///
/// True no-op requests bypass `sink` and deterministically emit reused
/// receipts. A projection-key change replays even an otherwise unchanged
/// chunk set, so every other request invokes the sink exactly once. In either
/// case the full receipt is verified before activation eligibility is checked.
///
/// The request digest and changed-chunk validation are O(chunk set) canonical
/// hashes, so they run once here and the evidence is threaded into receipt
/// construction and verification. What crosses a trust boundary still gets
/// recomputed: a receipt from `sink` is external, so its publication digest is
/// always recomputed, and every receipt's self-declared request digest is
/// always compared against the recomputed expectation.
pub fn project_for_publication<S: CodeChunkProjectionSink>(
    sink: &mut S,
    request: ProjectionBatchRequestV1,
) -> Result<ProjectionPublicationHandoffV1, ProjectionPublicationErrorV1> {
    let (request, request_digest) = expand_projection_key_replay(request)?;
    let evidence = ProjectionRequestEvidenceV1::recorded(request_digest, &request.changes);
    let (receipt, publication) = if request_is_true_noop(&request) {
        (
            build_batch_receipt_verified(
                &request,
                &evidence,
                &decisions_for_noop(&request.changes),
            )?,
            // Sealed one statement ago from these exact fields.
            PublicationDigestTrustV1::SealedInThisChain,
        )
    } else {
        (
            sink.project_changed_chunks(request.clone())?,
            PublicationDigestTrustV1::Unverified,
        )
    };
    verify_batch_receipt_verified(&request, &evidence, &receipt, publication)?;
    if !batch_can_activate(&receipt) {
        return Err(ProjectionPublicationErrorV1::NotActivatable);
    }
    Ok(ProjectionPublicationHandoffV1 { request, receipt })
}

/// Verify the incoming request and expand a projection-key replay, returning
/// the request alongside its recomputed canonical digest.
fn expand_projection_key_replay(
    mut request: ProjectionBatchRequestV1,
) -> Result<(ProjectionBatchRequestV1, ManifestDigest), ProjectionReceiptErrorV1> {
    let expected_request = expected_request_digest(&request)
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    if request.request_digest != expected_request {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    request
        .changes
        .validate()
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    if request.previous_projection_key.is_none() {
        if request.replay_reason != ProjectionReplayReasonV1::InitialProjection {
            return Err(ProjectionReceiptErrorV1::Contract(
                "a request without a prior projection key requires initial_projection replay"
                    .to_owned(),
            ));
        }
        return Ok((request, expected_request));
    }
    if request.previous_projection_key.as_ref() == Some(&request.target_projection_key) {
        return Ok((request, expected_request));
    }
    if request.replay_reason != ProjectionReplayReasonV1::ProjectionProfileChange {
        return Err(ProjectionReceiptErrorV1::Contract(
            "a projection-key change requires projection_profile_change replay".to_owned(),
        ));
    }

    let mut added_or_changed = request
        .changes
        .added_or_changed
        .iter()
        .chain(&request.changes.reused)
        .filter_map(|change| {
            change
                .current_digest
                .clone()
                .map(|current_digest| ChangedCodeChunkV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_digest: None,
                    current_digest: Some(current_digest),
                })
        })
        .collect::<Vec<_>>();
    added_or_changed.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: request.changes.from_generation.clone(),
        to_generation: request.changes.to_generation.clone(),
        manifest_digest: request.changes.manifest_digest.clone(),
        added_or_changed,
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes
        .compute_digest()
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    changes
        .validate()
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    request.changes = changes;
    let expanded_request_digest = expected_request_digest(&request)
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    request.request_digest = expanded_request_digest.clone();
    Ok((request, expanded_request_digest))
}

fn request_is_true_noop(request: &ProjectionBatchRequestV1) -> bool {
    changeset_is_noop(&request.changes)
        && request.previous_projection_key.as_ref() == Some(&request.target_projection_key)
}
