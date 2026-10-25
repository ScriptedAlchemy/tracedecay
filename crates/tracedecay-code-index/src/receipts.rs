//! Projection receipt construction and verification (Plan 25, "Code-search
//! chunk and projection contract").
//!
//! A projector answers one [`ProjectionBatchRequestV1`] with one
//! [`ProjectionBatchReceiptV1`]: one [`CodeChunkProjectionReceiptV1`] per
//! affected chunk (added, changed, or deleted), carrying the generation
//! watermarks (`prior_generation`, `source_generation`,
//! `source_manifest_digest`), the prior/current chunk digests, the operation,
//! the outcome, and the output digest. Unchanged chunks are authenticated by
//! the request digest and summarized by `reused_count` without per-chunk rows.
//! Receipts are deterministic, the domain contract excludes store-owned
//! operational timestamps from receipt identity, so replaying an identical
//! request with identical decisions produces an identical receipt and
//! publication digest (idempotent replay).
//!
//! Construction enforces the publication rules: duplicate, missing, extra,
//! cross-generation, wrong-digest, or wrong-projection-key receipts are
//! typed rejections. A no-op batch (empty added/changed and deleted
//! partitions, explicit reused) emits zero rows, a positive `reused_count`,
//! and proves zero work. Failed or skipped receipts remain inspectable but
//! cannot activate a projection generation.

use std::borrow::Borrow;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeChunkProjectionReceiptV1, CodeSearchChunkId,
    ContentDigest, DomainError, ManifestDigest, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
    ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1, canonical_sha256,
    projection_publication_digest_for_receipts,
};

/// Domain separator for the canonical projection-batch-request digest.
pub const PROJECTION_REQUEST_SEPARATOR: &str = "tracedecay.projection-batch-request.v1";

pub use tracedecay_domain::PROJECTION_PUBLICATION_SEPARATOR;

/// Receipt construction/verification failures (Plan 25: publication rejects
/// duplicate, missing, extra, cross-generation, wrong-digest, or
/// wrong-projection-key receipts).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProjectionReceiptErrorV1 {
    #[error("chunk {0} is in the request but has no receipt")]
    MissingChunkReceipt(CodeSearchChunkId),
    #[error("chunk {0} has a receipt but is not in the request")]
    ExtraChunkReceipt(CodeSearchChunkId),
    #[error("chunk {0} has more than one receipt")]
    DuplicateChunkReceipt(CodeSearchChunkId),
    #[error("chunk {0} carries a foreign generation or manifest watermark")]
    CrossGenerationReceipt(CodeSearchChunkId),
    #[error("a receipt carries the wrong projection key")]
    WrongProjectionKey,
    #[error("a projection-key replay must apply every retained chunk under the target key")]
    ProjectionKeyReplayRequiresAppliedWork,
    #[error("chunk {0} has an operation, outcome, or digest inconsistent with the request")]
    InconsistentOperation(CodeSearchChunkId),
    #[error("the receipt set is not in canonical chunk order")]
    NonCanonicalReceiptOrder,
    #[error("a request or publication digest does not recompute")]
    DigestMismatch,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// What the projector did with one chunk of the request: the receipt input.
/// The operation, outcome, and output digest are the projector's declared
/// result; the digests must match the request partition the chunk belongs
/// to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChunkProjectionDecisionV1 {
    pub chunk_id: CodeSearchChunkId,
    pub prior_chunk_digest: Option<ContentDigest>,
    pub current_chunk_digest: Option<ContentDigest>,
    pub operation: ProjectionOperationV1,
    pub outcome: ProjectionOutcomeV1,
    pub output_digest: Option<ContentDigest>,
}

/// The canonical request digest: every request field except the digest
/// itself, under the request domain separator.
pub fn expected_request_digest(
    request: &ProjectionBatchRequestV1,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        PROJECTION_REQUEST_SEPARATOR,
        &request.changes,
        &request.previous_projection_key,
        &request.target_projection_key,
        request.replay_reason,
    ))
}

/// The canonical publication digest of one batch receipt: every batch field
/// except the digest itself, under the publication domain separator.
pub fn expected_publication_digest(
    batch: &ProjectionBatchReceiptV1,
) -> Result<ManifestDigest, DomainError> {
    tracedecay_domain::projection_batch_publication_digest(batch)
}

/// Whether the changed-chunk set requests no projection work: empty
/// added/changed and deleted partitions (Plan 25: a no-op generation emits
/// empty `added_or_changed` and `deleted` sets plus an authenticated reused
/// complement).
pub fn changeset_is_noop(changes: &ChangedCodeChunkSetV1) -> bool {
    changes.added_or_changed.is_empty() && changes.deleted.is_empty()
}

/// Whether a batch receipt proves zero projection work.
///
/// Work-free reused chunks are authenticated by the request digest and
/// represented by `reused_count`; only chunks that required projector work
/// carry rows. Profile replay arrives pre-expanded (every current chunk in
/// `added_or_changed`, `reused_count == 0`) and records Applied rows.
pub fn batch_proves_zero_work(batch: &ProjectionBatchReceiptV1) -> bool {
    batch.reused_count > 0 && batch.receipts.is_empty()
}

/// Whether a batch receipt can activate a projection generation (Plan 25:
/// failed or partial receipt sets remain inspectable but cannot activate).
pub fn batch_can_activate(batch: &ProjectionBatchReceiptV1) -> bool {
    batch.receipts.iter().all(|receipt| {
        matches!(
            receipt.outcome,
            ProjectionOutcomeV1::Applied | ProjectionOutcomeV1::Reused
        )
    })
}

/// No-op projection decisions: reused chunks are request-authenticated and do
/// not emit per-chunk decisions after the parent-delta receipt cut.
pub fn decisions_for_noop(_changes: &ChangedCodeChunkSetV1) -> Vec<ChunkProjectionDecisionV1> {
    Vec::new()
}

/// Request authentication shared by the expanded and replayed receipt paths.
pub(crate) fn validate_request(
    request: &ProjectionBatchRequestV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    let expected = expected_request_digest(request)
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    if request.request_digest != expected {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    request
        .changes
        .validate()
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    let projection_changed =
        request.previous_projection_key.as_ref() != Some(&request.target_projection_key);
    if projection_changed && request.changes.reused_count > 0 {
        if request.replay_reason == ProjectionReplayReasonV1::ProjectionProfileChange {
            return Err(ProjectionReceiptErrorV1::Contract(
                "projection profile change must arrive pre-expanded without a reused complement"
                    .to_owned(),
            ));
        }
        return Err(ProjectionReceiptErrorV1::ProjectionKeyReplayRequiresAppliedWork);
    }
    Ok(())
}

/// Merge the already canonical partitions without copying their identities or
/// digests. The request validator owns ordering and partition disjointness.
#[derive(Clone)]
pub(crate) struct OrderedProjectionChangesV1<'a> {
    added: &'a [ChangedCodeChunkV1],
    deleted: &'a [ChangedCodeChunkV1],
}

pub(crate) fn ordered_changes(changes: &ChangedCodeChunkSetV1) -> OrderedProjectionChangesV1<'_> {
    OrderedProjectionChangesV1 {
        added: &changes.added_or_changed,
        deleted: &changes.deleted,
    }
}

impl<'a> Iterator for OrderedProjectionChangesV1<'a> {
    type Item = &'a ChangedCodeChunkV1;

    fn next(&mut self) -> Option<Self::Item> {
        let partition = match (self.added.first(), self.deleted.first()) {
            (Some(added), Some(deleted)) if added.chunk_id < deleted.chunk_id => &mut self.added,
            (Some(_), Some(_)) | (None, Some(_)) => &mut self.deleted,
            (Some(_), None) => &mut self.added,
            (None, None) => return None,
        };
        let (first, rest) = (*partition).split_first()?;
        *partition = rest;
        Some(first)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.added.len() + self.deleted.len();
        (len, Some(len))
    }
}

impl ExactSizeIterator for OrderedProjectionChangesV1<'_> {}

/// Operation and default output are derived from the canonical request row.
pub(crate) fn applied_operation(change: &ChangedCodeChunkV1) -> ProjectionOperationV1 {
    match (&change.prior_digest, &change.current_digest) {
        (_, None) => ProjectionOperationV1::Deleted,
        (None, Some(_)) => ProjectionOperationV1::Added,
        (Some(_), Some(_)) => ProjectionOperationV1::Updated,
    }
}

fn receipt_from_decision(
    request: &ProjectionBatchRequestV1,
    decision: ChunkProjectionDecisionV1,
) -> CodeChunkProjectionReceiptV1 {
    CodeChunkProjectionReceiptV1 {
        projection_key: request.target_projection_key.clone(),
        request_digest: request.request_digest.clone(),
        prior_generation: request.changes.from_generation.clone(),
        source_generation: request.changes.to_generation.clone(),
        source_manifest_digest: request.changes.manifest_digest.clone(),
        chunk_id: decision.chunk_id,
        prior_chunk_digest: decision.prior_chunk_digest,
        current_chunk_digest: decision.current_chunk_digest,
        operation: decision.operation,
        outcome: decision.outcome,
        output_digest: decision.output_digest,
    }
}

pub(crate) fn applied_receipt(
    request: &ProjectionBatchRequestV1,
    change: &ChangedCodeChunkV1,
) -> CodeChunkProjectionReceiptV1 {
    receipt_from_decision(
        request,
        ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: applied_operation(change),
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: change.current_digest.clone(),
        },
    )
}

/// Build expanded public evidence for callers that need the public DTO. Live
/// publication instead replays these same rows without retaining the vector.
pub fn build_batch_receipt(
    request: &ProjectionBatchRequestV1,
    decisions: &[ChunkProjectionDecisionV1],
) -> Result<ProjectionBatchReceiptV1, ProjectionReceiptErrorV1> {
    validate_request(request)?;
    let mut receipts = decisions
        .iter()
        .cloned()
        .map(|decision| receipt_from_decision(request, decision))
        .collect::<Vec<_>>();
    receipts.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    if let Some(pair) = receipts
        .windows(2)
        .find(|pair| pair[0].chunk_id == pair[1].chunk_id)
    {
        return Err(ProjectionReceiptErrorV1::DuplicateChunkReceipt(
            pair[0].chunk_id.clone(),
        ));
    }
    verify_receipt_rows(request, receipts.iter())?;
    let publication_digest = projection_publication_digest_for_receipts(
        &request.target_projection_key,
        &request.request_digest,
        &request.changes.to_generation,
        &request.changes.manifest_digest,
        &receipts,
        request.changes.reused_count,
    )
    .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    Ok(ProjectionBatchReceiptV1 {
        target_projection_key: request.target_projection_key.clone(),
        request_digest: request.request_digest.clone(),
        source_generation: request.changes.to_generation.clone(),
        source_manifest_digest: request.changes.manifest_digest.clone(),
        receipts,
        reused_count: request.changes.reused_count,
        publication_digest,
    })
}

/// Verify every stored batch header, row and digest before accepting evidence
/// across a trust boundary. Prepared evidence carries no implicit trust here.
pub fn verify_batch_receipt(
    request: &ProjectionBatchRequestV1,
    batch: &ProjectionBatchReceiptV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    validate_request(request)?;
    if batch.request_digest != request.request_digest {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    if batch.target_projection_key != request.target_projection_key {
        return Err(ProjectionReceiptErrorV1::WrongProjectionKey);
    }
    if batch.source_generation != request.changes.to_generation
        || batch.source_manifest_digest != request.changes.manifest_digest
    {
        let chunk_id = match batch.receipts.first() {
            Some(receipt) => receipt.chunk_id.clone(),
            None => CodeSearchChunkId::new("chunk.v1.empty-batch")
                .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?,
        };
        return Err(ProjectionReceiptErrorV1::CrossGenerationReceipt(chunk_id));
    }
    verify_receipt_rows(request, batch.receipts.iter())?;
    if batch.reused_count != request.changes.reused_count {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    let expected = expected_publication_digest(batch)
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    if batch.publication_digest != expected {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    Ok(())
}

/// The single row verifier for expanded receipts and live deterministic
/// replay. It assumes the immutable request has passed `validate_request`.
/// Only one receipt and one request row need be live at a time.
pub(crate) fn verify_receipt_rows<I>(
    request: &ProjectionBatchRequestV1,
    receipts: I,
) -> Result<(), ProjectionReceiptErrorV1>
where
    I: Iterator + Clone,
    I::Item: Borrow<CodeChunkProjectionReceiptV1>,
{
    let mut previous = None;
    for row in receipts.clone() {
        let receipt = row.borrow();
        if previous
            .as_ref()
            .is_some_and(|prior| prior >= &receipt.chunk_id)
        {
            return Err(ProjectionReceiptErrorV1::NonCanonicalReceiptOrder);
        }
        previous = Some(receipt.chunk_id.clone());
    }
    let mut changes = ordered_changes(&request.changes);
    for row in receipts {
        let receipt = row.borrow();
        if receipt.projection_key != request.target_projection_key {
            return Err(ProjectionReceiptErrorV1::WrongProjectionKey);
        }
        if receipt.request_digest != request.request_digest
            || receipt.prior_generation != request.changes.from_generation
            || receipt.source_generation != request.changes.to_generation
            || receipt.source_manifest_digest != request.changes.manifest_digest
        {
            return Err(ProjectionReceiptErrorV1::CrossGenerationReceipt(
                receipt.chunk_id.clone(),
            ));
        }
        let Some(change) = changes.next() else {
            return Err(ProjectionReceiptErrorV1::ExtraChunkReceipt(
                receipt.chunk_id.clone(),
            ));
        };
        if receipt.chunk_id != change.chunk_id {
            let belongs = request
                .changes
                .added_or_changed
                .binary_search_by(|change| change.chunk_id.cmp(&receipt.chunk_id))
                .is_ok()
                || request
                    .changes
                    .deleted
                    .binary_search_by(|change| change.chunk_id.cmp(&receipt.chunk_id))
                    .is_ok();
            return Err(if belongs {
                ProjectionReceiptErrorV1::MissingChunkReceipt(change.chunk_id.clone())
            } else {
                ProjectionReceiptErrorV1::ExtraChunkReceipt(receipt.chunk_id.clone())
            });
        }
        check_receipt(change, receipt)?;
    }
    if let Some(change) = changes.next() {
        return Err(ProjectionReceiptErrorV1::MissingChunkReceipt(
            change.chunk_id.clone(),
        ));
    }
    Ok(())
}

fn check_receipt(
    change: &ChangedCodeChunkV1,
    receipt: &CodeChunkProjectionReceiptV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    if receipt.prior_chunk_digest != change.prior_digest
        || receipt.current_chunk_digest != change.current_digest
        || receipt.operation != applied_operation(change)
        || receipt.outcome == ProjectionOutcomeV1::Reused
    {
        return Err(ProjectionReceiptErrorV1::InconsistentOperation(
            receipt.chunk_id.clone(),
        ));
    }
    // Applied upserts require an output, but that output may legitimately
    // differ from the source digest. Deletions and failed/skipped work have none.
    let emits_output = receipt.operation != ProjectionOperationV1::Deleted
        && receipt.outcome == ProjectionOutcomeV1::Applied;
    if receipt.output_digest.is_some() != emits_output {
        return Err(ProjectionReceiptErrorV1::InconsistentOperation(
            receipt.chunk_id.clone(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ChangedCodeChunkV1, CodeGenerationId, ProjectionKeyV1, ProjectionKindV1,
        ProjectionReplayReasonV1,
    };

    fn digest(byte: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid digest")
    }

    fn manifest_digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn generation(sequence: u64) -> CodeGenerationId {
        CodeGenerationId::new(format!("generation.v1.aaaaaaaa.{sequence:08}"))
            .expect("valid generation id")
    }

    fn chunk(name: &str) -> CodeSearchChunkId {
        CodeSearchChunkId::new(format!("chunk.v1.{name}")).expect("valid chunk id")
    }

    fn projection_key() -> ProjectionKeyV1 {
        ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "projection.v1".to_owned(),
            profile_digest: manifest_digest('e'),
        }
    }

    fn changeset(
        added_or_changed: Vec<ChangedCodeChunkV1>,
        deleted: Vec<ChangedCodeChunkV1>,
        reused: Vec<ChangedCodeChunkV1>,
    ) -> ChangedCodeChunkSetV1 {
        let reused_pairs = reused
            .into_iter()
            .map(|change| {
                (
                    change.chunk_id,
                    change
                        .current_digest
                        .expect("reused fixture carries a current digest"),
                )
            })
            .collect::<Vec<_>>();
        let (reused_count, reused_digest) =
            ChangedCodeChunkSetV1::seal_reused_partition(&reused_pairs).expect("reused seal");
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: Some(generation(1)),
            to_generation: generation(2),
            manifest_digest: manifest_digest('0'),
            added_or_changed,
            deleted,
            reused_count,
            reused_digest,
        };
        changes.manifest_digest = changes.compute_digest().expect("changeset digest");
        changes.validate().expect("canonical changeset");
        changes
    }

    fn mixed_changeset() -> ChangedCodeChunkSetV1 {
        changeset(
            vec![
                ChangedCodeChunkV1 {
                    chunk_id: chunk("added"),
                    prior_digest: None,
                    current_digest: Some(digest('a')),
                },
                ChangedCodeChunkV1 {
                    chunk_id: chunk("updated"),
                    prior_digest: Some(digest('b')),
                    current_digest: Some(digest('c')),
                },
            ],
            vec![ChangedCodeChunkV1 {
                chunk_id: chunk("deleted"),
                prior_digest: Some(digest('d')),
                current_digest: None,
            }],
            vec![ChangedCodeChunkV1 {
                chunk_id: chunk("reused"),
                prior_digest: Some(digest('f')),
                current_digest: Some(digest('f')),
            }],
        )
    }

    fn batch_request(changes: ChangedCodeChunkSetV1) -> ProjectionBatchRequestV1 {
        let target_projection_key = projection_key();
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: Some(target_projection_key.clone()),
            target_projection_key,
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        };
        request.request_digest = expected_request_digest(&request).expect("request digest");
        request
    }

    fn mixed_decisions() -> Vec<ChunkProjectionDecisionV1> {
        vec![
            ChunkProjectionDecisionV1 {
                chunk_id: chunk("added"),
                prior_chunk_digest: None,
                current_chunk_digest: Some(digest('a')),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(digest('5')),
            },
            ChunkProjectionDecisionV1 {
                chunk_id: chunk("updated"),
                prior_chunk_digest: Some(digest('b')),
                current_chunk_digest: Some(digest('c')),
                operation: ProjectionOperationV1::Updated,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(digest('6')),
            },
            ChunkProjectionDecisionV1 {
                chunk_id: chunk("deleted"),
                prior_chunk_digest: Some(digest('d')),
                current_chunk_digest: None,
                operation: ProjectionOperationV1::Deleted,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: None,
            },
        ]
    }

    #[test]
    fn construction_is_complete_canonical_digest_stable_and_idempotent() {
        let request = batch_request(mixed_changeset());
        let batch = build_batch_receipt(&request, &mixed_decisions()).expect("batch builds");

        // Generation watermarks propagate to the batch and every receipt.
        assert_eq!(batch.source_generation, generation(2));
        assert_eq!(
            batch.source_manifest_digest,
            request.changes.manifest_digest
        );
        assert_eq!(batch.reused_count, 1);
        for receipt in &batch.receipts {
            assert_eq!(receipt.prior_generation, Some(generation(1)));
            assert_eq!(receipt.source_generation, generation(2));
            assert_eq!(receipt.projection_key, projection_key());
            assert_eq!(receipt.request_digest, request.request_digest);
        }
        // Affected rows only; reused stays in reused_count.
        let ids: Vec<&str> = batch
            .receipts
            .iter()
            .map(|receipt| receipt.chunk_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["chunk.v1.added", "chunk.v1.deleted", "chunk.v1.updated"]
        );

        // The publication digest recomputes, and verification passes.
        assert_eq!(
            expected_publication_digest(&batch).expect("publication digest"),
            batch.publication_digest
        );
        verify_batch_receipt(&request, &batch).expect("verification passes");
        assert!(batch_can_activate(&batch));
        assert!(!batch_proves_zero_work(&batch));

        // Idempotent replay: identical request and decisions produce an
        // identical receipt and publication digest.
        let replayed = build_batch_receipt(&request, &mixed_decisions()).expect("replay builds");
        assert_eq!(batch, replayed);
        assert_eq!(batch.publication_digest, replayed.publication_digest);

        // Serde round trip preserves the receipt.
        let bytes = serde_json::to_vec(&batch).expect("serialize");
        let decoded: ProjectionBatchReceiptV1 =
            serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(batch, decoded);
    }

    #[test]
    fn noop_replay_proves_zero_work() {
        // A no-op generation: empty added/changed and deleted partitions,
        // explicit reused set (Plan 25).
        let changes = changeset(
            vec![],
            vec![],
            vec![
                ChangedCodeChunkV1 {
                    chunk_id: chunk("alpha"),
                    prior_digest: Some(digest('a')),
                    current_digest: Some(digest('a')),
                },
                ChangedCodeChunkV1 {
                    chunk_id: chunk("beta"),
                    prior_digest: Some(digest('b')),
                    current_digest: Some(digest('b')),
                },
            ],
        );
        assert!(changeset_is_noop(&changes));
        let request = batch_request(changes);
        let decisions = decisions_for_noop(&request.changes);
        let batch = build_batch_receipt(&request, &decisions).expect("no-op batch builds");

        // Zero work: reused_count covers the set; no affected rows.
        assert!(batch_proves_zero_work(&batch));
        assert_eq!(batch.reused_count, 2);
        assert!(batch.receipts.is_empty());
        assert!(batch_can_activate(&batch));
        verify_batch_receipt(&request, &batch).expect("verification passes");

        // Replaying the identical no-op batch reproduces the identical
        // publication digest: the receipt proves the replay did nothing.
        let replayed = build_batch_receipt(&request, &decisions_for_noop(&request.changes))
            .expect("replay builds");
        assert_eq!(batch, replayed);
    }

    #[test]
    fn failed_receipts_remain_inspectable_but_cannot_activate() {
        let request = batch_request(mixed_changeset());
        let mut decisions = mixed_decisions();
        decisions[0].outcome = ProjectionOutcomeV1::Failed {
            reason: "projector timeout".to_owned(),
        };
        decisions[0].output_digest = None;
        let batch = build_batch_receipt(&request, &decisions).expect("failed batch builds");

        // The failure is recorded and verifiable, but the batch cannot
        // activate a projection generation.
        assert!(!batch_can_activate(&batch));
        assert!(matches!(
            batch.receipts[0].outcome,
            ProjectionOutcomeV1::Failed { .. }
        ));
        verify_batch_receipt(&request, &batch).expect("failed batches stay inspectable");
    }

    #[test]
    fn construction_rejects_missing_extra_duplicate_and_inconsistent() {
        let request = batch_request(mixed_changeset());

        // Missing: an affected chunk has no decision. Reused chunks need none.
        let missing: Vec<_> = mixed_decisions()
            .into_iter()
            .filter(|decision| decision.chunk_id != chunk("updated"))
            .collect();
        assert_eq!(
            build_batch_receipt(&request, &missing),
            Err(ProjectionReceiptErrorV1::MissingChunkReceipt(chunk(
                "updated"
            )))
        );
        let without_reused: Vec<_> = mixed_decisions()
            .into_iter()
            .filter(|decision| decision.chunk_id != chunk("reused"))
            .collect();
        let compact = build_batch_receipt(&request, &without_reused).expect("reused needs no row");
        assert_eq!(compact.reused_count, 1);
        assert!(
            !compact
                .receipts
                .iter()
                .any(|receipt| receipt.chunk_id == chunk("reused"))
        );

        // Extra: a decision for a chunk the request does not name.
        let mut extra = mixed_decisions();
        extra.push(ChunkProjectionDecisionV1 {
            chunk_id: chunk("zzextra"),
            prior_chunk_digest: None,
            current_chunk_digest: Some(digest('a')),
            operation: ProjectionOperationV1::Added,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: Some(digest('5')),
        });
        assert_eq!(
            build_batch_receipt(&request, &extra),
            Err(ProjectionReceiptErrorV1::ExtraChunkReceipt(chunk(
                "zzextra"
            )))
        );

        // Duplicate: one chunk decided twice.
        let mut duplicate = mixed_decisions();
        duplicate.push(mixed_decisions()[0].clone());
        assert_eq!(
            build_batch_receipt(&request, &duplicate),
            Err(ProjectionReceiptErrorV1::DuplicateChunkReceipt(chunk(
                "added"
            )))
        );

        // Inconsistent: an add claimed for a chunk the request updated, and
        // a reused chunk claimed as applied work.
        let mut wrong_operation = mixed_decisions();
        wrong_operation[1].operation = ProjectionOperationV1::Added;
        assert_eq!(
            build_batch_receipt(&request, &wrong_operation),
            Err(ProjectionReceiptErrorV1::InconsistentOperation(chunk(
                "updated"
            )))
        );
        let mut wrong_digest = mixed_decisions();
        wrong_digest[0].current_chunk_digest = Some(digest('8'));
        assert_eq!(
            build_batch_receipt(&request, &wrong_digest),
            Err(ProjectionReceiptErrorV1::InconsistentOperation(chunk(
                "added"
            )))
        );
    }

    #[test]
    fn verification_rejects_tampered_cross_generation_and_wrong_key_receipts() {
        let request = batch_request(mixed_changeset());
        let batch = build_batch_receipt(&request, &mixed_decisions()).expect("batch builds");

        // Tampered publication digest.
        let mut tampered = batch.clone();
        tampered.publication_digest = manifest_digest('9');
        assert_eq!(
            verify_batch_receipt(&request, &tampered),
            Err(ProjectionReceiptErrorV1::DigestMismatch)
        );

        // Tampered reused count.
        let mut tampered = batch.clone();
        tampered.reused_count = 0;
        assert_eq!(
            verify_batch_receipt(&request, &tampered),
            Err(ProjectionReceiptErrorV1::DigestMismatch)
        );

        // Wrong batch projection key.
        let mut tampered = batch.clone();
        tampered.target_projection_key = ProjectionKeyV1 {
            kind: ProjectionKindV1::Graph,
            ..projection_key()
        };
        assert_eq!(
            verify_batch_receipt(&request, &tampered),
            Err(ProjectionReceiptErrorV1::WrongProjectionKey)
        );

        // Cross-generation batch watermark.
        let mut tampered = batch.clone();
        tampered.source_generation = generation(3);
        assert!(matches!(
            verify_batch_receipt(&request, &tampered),
            Err(ProjectionReceiptErrorV1::CrossGenerationReceipt(_))
        ));

        // Cross-generation per-receipt watermark: the batch digest is
        // re-sealed so only the receipt watermark is wrong.
        let mut tampered = batch.clone();
        tampered.receipts[0].source_generation = generation(3);
        tampered.publication_digest = expected_publication_digest(&tampered).expect("reseal");
        assert_eq!(
            verify_batch_receipt(&request, &tampered),
            Err(ProjectionReceiptErrorV1::CrossGenerationReceipt(chunk(
                "added"
            )))
        );

        // Wrong request digest: a request whose digest does not recompute.
        let mut bad_request = batch_request(mixed_changeset());
        bad_request.request_digest = manifest_digest('9');
        assert_eq!(
            verify_batch_receipt(&bad_request, &batch),
            Err(ProjectionReceiptErrorV1::DigestMismatch)
        );

        // Non-canonical receipt order.
        let mut tampered = batch.clone();
        tampered.receipts.swap(0, 1);
        assert_eq!(
            verify_batch_receipt(&request, &tampered),
            Err(ProjectionReceiptErrorV1::NonCanonicalReceiptOrder)
        );
    }

    fn profile_replay_request(changes: ChangedCodeChunkSetV1) -> ProjectionBatchRequestV1 {
        let previous = projection_key();
        let target = ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "projection.v1".to_owned(),
            profile_digest: manifest_digest('c'),
        };
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: Some(previous),
            target_projection_key: target,
            replay_reason: ProjectionReplayReasonV1::ProjectionProfileChange,
        };
        request.request_digest = expected_request_digest(&request).expect("request digest");
        request
    }

    fn expanded_profile_decisions(
        changes: &ChangedCodeChunkSetV1,
    ) -> Vec<ChunkProjectionDecisionV1> {
        changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .collect()
    }

    #[test]
    fn profile_replay_arrives_pre_expanded_with_zero_reused_count() {
        let changes = changeset(
            vec![
                ChangedCodeChunkV1 {
                    chunk_id: chunk("alpha"),
                    prior_digest: None,
                    current_digest: Some(digest('a')),
                },
                ChangedCodeChunkV1 {
                    chunk_id: chunk("beta"),
                    prior_digest: None,
                    current_digest: Some(digest('b')),
                },
            ],
            vec![],
            vec![],
        );
        assert_eq!(changes.reused_count, 0);
        let request = profile_replay_request(changes);

        assert_eq!(
            build_batch_receipt(&request, &[]),
            Err(ProjectionReceiptErrorV1::MissingChunkReceipt(chunk(
                "alpha"
            )))
        );

        let decisions = expanded_profile_decisions(&request.changes);
        let batch = build_batch_receipt(&request, &decisions).expect("expanded batch builds");
        assert_eq!(batch.reused_count, 0);
        assert_eq!(batch.receipts.len(), 2);
        assert!(batch.receipts.iter().all(|receipt| {
            receipt.operation == ProjectionOperationV1::Added
                && receipt.outcome == ProjectionOutcomeV1::Applied
        }));
        assert!(!batch_proves_zero_work(&batch));
        assert!(batch_can_activate(&batch));
        verify_batch_receipt(&request, &batch).expect("verification passes");

        let mut forged = batch.clone();
        forged.receipts.clear();
        forged.reused_count = 2;
        forged.publication_digest =
            expected_publication_digest(&forged).expect("reseal empty reuse");
        assert_eq!(
            verify_batch_receipt(&request, &forged),
            Err(ProjectionReceiptErrorV1::MissingChunkReceipt(chunk(
                "alpha"
            )))
        );
    }
}
