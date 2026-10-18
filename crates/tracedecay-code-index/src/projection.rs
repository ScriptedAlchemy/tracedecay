//! Projection execution and atomic publication handoff.
//!
//! Canonical receipt construction authenticates one immutable changed-chunk
//! request before preparing a publication handoff. Reused partitions remain
//! compact; projection-key replays require every current chunk to be explicit.
//! Store-owned transactions, active pointers, retries and scheduling remain
//! outside this module.

use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
    ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1,
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

/// Why a projection batch cannot cross the atomic publication boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProjectionPublicationErrorV1 {
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

/// Prepare the canonical receipt for one immutable changed-chunk request.
///
/// This records prepared evidence, not durable activation. The store must still
/// atomically publish the returned handoff. Reused chunks are authenticated by
/// their partition count and digest, so a true no-op needs no per-chunk rows.
/// A projection-key replay must already expand every current chunk.
///
/// The request digest and partition validation run once. The canonical builder
/// carries that evidence through receipt construction and verification; restored
/// receipts must recompute their publication digest at the durable boundary.
#[tracing::instrument(
    name = "code_index.build.project_publication",
    level = "trace",
    skip_all
)]
pub fn project_for_publication(
    request: ProjectionBatchRequestV1,
) -> Result<ProjectionPublicationHandoffV1, ProjectionPublicationErrorV1> {
    let (request, request_digest) = expand_projection_key_replay(request)?;
    let evidence = ProjectionRequestEvidenceV1::recorded(request_digest, &request.changes);
    let mut decisions = request
        .changes
        .added_or_changed
        .iter()
        .chain(&request.changes.deleted)
        .map(|change| ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: match (&change.prior_digest, &change.current_digest) {
                (_, None) => ProjectionOperationV1::Deleted,
                (None, Some(_)) => ProjectionOperationV1::Added,
                (Some(_), Some(_)) => ProjectionOperationV1::Updated,
            },
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: change.current_digest.clone(),
        })
        .collect::<Vec<_>>();
    decisions.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    let receipt = build_batch_receipt_verified(&request, &evidence, &decisions)?;
    verify_batch_receipt_verified(
        &request,
        &evidence,
        &receipt,
        PublicationDigestTrustV1::SealedInThisChain,
    )?;
    if !batch_can_activate(&receipt) {
        return Err(ProjectionPublicationErrorV1::NotActivatable);
    }
    Ok(ProjectionPublicationHandoffV1 { request, receipt })
}

/// Verify the incoming request and expand a projection-key replay, returning
/// the request alongside its recomputed canonical digest.
fn expand_projection_key_replay(
    request: ProjectionBatchRequestV1,
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
    // Profile change must already list every current chunk as added_or_changed.
    if request.changes.reused_count != 0 || !request.changes.deleted.is_empty() {
        return Err(ProjectionReceiptErrorV1::Contract(
            "projection profile change must arrive pre-expanded without a reused complement"
                .to_owned(),
        ));
    }
    Ok((request, expected_request))
}

#[cfg(test)]
mod tests;
