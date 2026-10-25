//! Projection execution and atomic publication handoff.
//!
//! Canonical receipt construction authenticates one immutable changed-chunk
//! request before preparing a publication handoff. Reused partitions remain
//! compact; projection-key replays require every current chunk to be explicit.
//! Store-owned transactions, active pointers, retries and scheduling remain
//! outside this module.

use std::sync::Arc;

use serde::ser::{Serialize, SerializeSeq, Serializer};
use thiserror::Error;
use tracedecay_domain::{
    CodeChunkProjectionReceiptV1, CodeGenerationId, ContentDigest, ManifestDigest,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1,
    projection_publication_digest_for_receipts,
};

pub use super::receipts::{
    ChunkProjectionDecisionV1, ProjectionReceiptErrorV1, batch_can_activate,
    batch_proves_zero_work, build_batch_receipt, changeset_is_noop, decisions_for_noop,
    expected_publication_digest, expected_request_digest, verify_batch_receipt,
};
use super::receipts::{applied_receipt, ordered_changes, validate_request, verify_receipt_rows};

/// Why a projection batch cannot cross the atomic publication boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProjectionPublicationErrorV1 {
    #[error(transparent)]
    Receipt(#[from] ProjectionReceiptErrorV1),
    #[error("the complete receipt contains failed or skipped projection work")]
    NotActivatable,
}

/// Verified publication capability. Canonical rows replay from the immutable
/// request; only restored outputs that differ from the source need storage.
/// Store-owned transactions, active pointers and activation remain external.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionPublicationHandoffV1 {
    state: Arc<ProjectionPublicationStateV1>,
}

#[derive(Debug, PartialEq, Eq)]
struct ProjectionPublicationStateV1 {
    request: ProjectionBatchRequestV1,
    publication_digest: ManifestDigest,
    output_overrides: Vec<(usize, ContentDigest)>,
}

impl ProjectionPublicationHandoffV1 {
    pub fn request(&self) -> &ProjectionBatchRequestV1 {
        &self.state.request
    }

    pub fn publication_digest(&self) -> &ManifestDigest {
        &self.state.publication_digest
    }

    pub fn request_digest(&self) -> &ManifestDigest {
        &self.state.request.request_digest
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.state.request.changes.to_generation
    }

    pub fn source_manifest_digest(&self) -> &ManifestDigest {
        &self.state.request.changes.manifest_digest
    }

    pub fn target_projection_key(&self) -> &ProjectionKeyV1 {
        &self.state.request.target_projection_key
    }

    pub fn receipt_count(&self) -> usize {
        self.state.request.changes.added_or_changed.len() + self.state.request.changes.deleted.len()
    }

    pub fn reused_count(&self) -> u64 {
        self.state.request.changes.reused_count
    }

    /// Replay the exact verified rows in canonical chunk order. Each row owns
    /// its wire fields and can be consumed before the next row is allocated.
    pub fn receipts(&self) -> impl Iterator<Item = CodeChunkProjectionReceiptV1> + Clone + '_ {
        self.rows().iter()
    }

    /// Serialize the public receipt array without collecting it. This is also
    /// the sequence passed to the domain's canonical publication digest.
    pub fn receipt_rows(&self) -> impl Serialize + '_ {
        self.rows()
    }

    fn rows(&self) -> ProjectionReceiptRowsV1<'_> {
        ProjectionReceiptRowsV1 {
            request: &self.state.request,
            output_overrides: &self.state.output_overrides,
        }
    }

    pub(crate) fn output_overrides(&self) -> &[(usize, ContentDigest)] {
        &self.state.output_overrides
    }

    pub(crate) fn shared_value_size(&self) -> usize {
        std::mem::size_of::<ProjectionPublicationStateV1>()
    }

    pub(crate) fn output_override_capacity(&self) -> usize {
        self.state.output_overrides.capacity()
    }

    /// Durable evidence must pass the full canonical verifier before it can
    /// be reduced. Applied outputs need not equal source content digests.
    pub(crate) fn restore(
        request: ProjectionBatchRequestV1,
        receipt: ProjectionBatchReceiptV1,
    ) -> Result<Self, ProjectionPublicationErrorV1> {
        verify_batch_receipt(&request, &receipt)?;
        if !batch_can_activate(&receipt) {
            return Err(ProjectionPublicationErrorV1::NotActivatable);
        }
        // The verifier requires Applied for every activatable affected row:
        // the Reused complement has no rows. An Applied upsert has an output;
        // a deletion has none. Thus only nondefault upsert outputs need storage.
        // A fresh vector also prevents in-place iterator collection from
        // retaining the expanded receipt vector's much larger allocation.
        let mut output_overrides = Vec::new();
        for (index, row) in receipt.receipts.into_iter().enumerate() {
            if let Some(output) = row.output_digest
                && Some(&output) != row.current_chunk_digest.as_ref()
            {
                output_overrides.push((index, output));
            }
        }
        Ok(Self {
            state: Arc::new(ProjectionPublicationStateV1 {
                request,
                publication_digest: receipt.publication_digest,
                output_overrides,
            }),
        })
    }
}

#[derive(Clone, Copy)]
struct ProjectionReceiptRowsV1<'a> {
    request: &'a ProjectionBatchRequestV1,
    output_overrides: &'a [(usize, ContentDigest)],
}

impl<'a> ProjectionReceiptRowsV1<'a> {
    fn iter(self) -> impl Iterator<Item = CodeChunkProjectionReceiptV1> + Clone + 'a {
        let mut overrides = self.output_overrides.iter().peekable();
        ordered_changes(&self.request.changes)
            .enumerate()
            .map(move |(index, change)| {
                let mut receipt = applied_receipt(self.request, change);
                if let Some((_, digest)) = overrides.next_if(|(position, _)| *position == index) {
                    receipt.output_digest = Some(digest.clone());
                }
                receipt
            })
    }
}

impl Serialize for ProjectionReceiptRowsV1<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let count =
            self.request.changes.added_or_changed.len() + self.request.changes.deleted.len();
        let mut sequence = serializer.serialize_seq(Some(count))?;
        for receipt in self.iter() {
            sequence.serialize_element(&receipt)?;
        }
        sequence.end()
    }
}

/// Prepare one immutable changed-chunk request using the same row verifier and
/// publication digest as expanded evidence. The store still owns activation.
#[tracing::instrument(
    name = "code_index.build.project_publication",
    level = "trace",
    skip_all
)]
pub fn project_for_publication(
    request: ProjectionBatchRequestV1,
) -> Result<ProjectionPublicationHandoffV1, ProjectionPublicationErrorV1> {
    validate_request(&request)?;
    validate_projection_key_replay(&request)?;
    let rows = ProjectionReceiptRowsV1 {
        request: &request,
        output_overrides: &[],
    };
    verify_receipt_rows(&request, rows.iter())?;
    let publication_digest = projection_publication_digest_for_receipts(
        &request.target_projection_key,
        &request.request_digest,
        &request.changes.to_generation,
        &request.changes.manifest_digest,
        &rows,
        request.changes.reused_count,
    )
    .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    Ok(ProjectionPublicationHandoffV1 {
        state: Arc::new(ProjectionPublicationStateV1 {
            request,
            publication_digest,
            output_overrides: Vec::new(),
        }),
    })
}

/// Live projection-key changes require an explicitly expanded replay.
fn validate_projection_key_replay(
    request: &ProjectionBatchRequestV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    if request.previous_projection_key.is_none() {
        if request.replay_reason != ProjectionReplayReasonV1::InitialProjection {
            return Err(ProjectionReceiptErrorV1::Contract(
                "a request without a prior projection key requires initial_projection replay"
                    .to_owned(),
            ));
        }
        return Ok(());
    }
    if request.previous_projection_key.as_ref() == Some(&request.target_projection_key) {
        return Ok(());
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
    Ok(())
}

#[cfg(test)]
mod tests;
