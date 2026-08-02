//! Projection receipt construction and verification (Plan 25, "Code-search
//! chunk and projection contract").
//!
//! A projector answers one [`ProjectionBatchRequestV1`] with one
//! [`ProjectionBatchReceiptV1`]: one [`CodeChunkProjectionReceiptV1`] per
//! chunk in the request's changed/reused/deleted partitions, carrying the
//! generation watermarks (`prior_generation`, `source_generation`,
//! `source_manifest_digest`), the prior/current chunk digests, the operation,
//! the outcome, and the output digest. Receipts are deterministic — the
//! domain contract excludes store-owned operational timestamps from receipt
//! identity — so replaying an identical request with identical decisions
//! produces an identical receipt and publication digest (idempotent replay).
//!
//! Construction enforces the publication rules: duplicate, missing, extra,
//! cross-generation, wrong-digest, or wrong-projection-key receipts are
//! typed rejections. A no-op batch (empty added/changed and deleted
//! partitions, explicit reused) builds only `Reused` receipts and proves
//! zero work. Failed or skipped receipts remain inspectable but cannot
//! activate a projection generation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, CodeChunkProjectionReceiptV1, CodeSearchChunkId, ContentDigest,
    DomainError, ManifestDigest, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
    ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1, canonical_sha256,
};

/// Domain separator for the canonical projection-batch-request digest.
pub const PROJECTION_REQUEST_SEPARATOR: &str = "tracedecay.projection-batch-request.v1";

/// Domain separator for the canonical projection-batch publication digest.
pub const PROJECTION_PUBLICATION_SEPARATOR: &str = "tracedecay.projection-batch-receipt.v1";

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

/// The partition of the changed-chunk set a chunk belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Partition {
    AddedOrChanged,
    Deleted,
    Reused,
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
    canonical_sha256(&(
        PROJECTION_PUBLICATION_SEPARATOR,
        &batch.target_projection_key,
        &batch.request_digest,
        &batch.source_generation,
        &batch.source_manifest_digest,
        &batch.receipts,
        batch.reused_count,
    ))
}

/// Whether the changed-chunk set requests no projection work: empty
/// added/changed and deleted partitions (Plan 25: a no-op generation emits
/// empty `added_or_changed` and `deleted` sets plus explicit `reused`).
pub fn changeset_is_noop(changes: &ChangedCodeChunkSetV1) -> bool {
    changes.added_or_changed.is_empty() && changes.deleted.is_empty()
}

/// Whether a batch receipt proves zero projection work: every receipt is a
/// `Reused` operation with a `Reused` outcome and the reused count covers
/// the whole set. An identical replay of an already-projected batch always
/// satisfies this.
pub fn batch_proves_zero_work(batch: &ProjectionBatchReceiptV1) -> bool {
    batch.reused_count == batch.receipts.len() as u64
        && batch.receipts.iter().all(|receipt| {
            receipt.operation == ProjectionOperationV1::Reused
                && receipt.outcome == ProjectionOutcomeV1::Reused
        })
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

/// The decisions for a no-op replay: every reused chunk maps to a `Reused`
/// operation with a `Reused` outcome and no output digest.
pub fn decisions_for_noop(changes: &ChangedCodeChunkSetV1) -> Vec<ChunkProjectionDecisionV1> {
    changes
        .reused
        .iter()
        .map(|change| ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: ProjectionOperationV1::Reused,
            outcome: ProjectionOutcomeV1::Reused,
            output_digest: None,
        })
        .collect()
}

/// Request digest work already done, once, for one immutable request.
///
/// Every entry point that accepts a request from outside its call chain
/// recomputes the canonical request digest and re-validates the changed-chunk
/// set — both O(request) canonical hashes over sets that reach six figures.
/// Once a chain has done that for a request it does not mutate again, the
/// same evidence is threaded to the remaining steps instead of hashing the
/// request two more times.
pub(crate) struct ProjectionRequestEvidenceV1 {
    /// `expected_request_digest(request)`, already compared against
    /// `request.request_digest`.
    request_digest: ManifestDigest,
    /// The validated changed-chunk set indexed by chunk identity.
    partitions: BTreeMap<CodeSearchChunkId, (Partition, DigestPair)>,
}

impl ProjectionRequestEvidenceV1 {
    /// Record evidence for a request whose digest was just recomputed and
    /// whose changed-chunk set was just validated by the caller.
    pub(crate) fn recorded(
        request_digest: ManifestDigest,
        changes: &ChangedCodeChunkSetV1,
    ) -> Self {
        Self {
            request_digest,
            partitions: index_partitions(changes),
        }
    }
}

/// Whether a batch receipt's publication digest still has to be recomputed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationDigestTrustV1 {
    /// The receipt crossed a trust boundary — a projection sink, durable
    /// storage, or any other caller — so its self-declared publication digest
    /// proves nothing until it is recomputed from the receipt's own fields.
    Unverified,
    /// The receipt was sealed by [`build_batch_receipt`] earlier in this same
    /// call chain and has not been touched since, so its publication digest is
    /// the value that recomputation would produce.
    SealedInThisChain,
}

/// Build the complete batch receipt for one projection request from the
/// projector's per-chunk decisions. The decisions must cover every chunk in
/// the request exactly once, with operations and digests consistent with the
/// request partitions; the receipts are canonically ordered and the
/// publication digest seals the batch. Construction is deterministic, so
/// identical requests and decisions yield identical receipts.
pub fn build_batch_receipt(
    request: &ProjectionBatchRequestV1,
    decisions: &[ChunkProjectionDecisionV1],
) -> Result<ProjectionBatchReceiptV1, ProjectionReceiptErrorV1> {
    build_batch_receipt_with(request, None, decisions)
}

/// [`build_batch_receipt`] for a request this chain already verified.
pub(crate) fn build_batch_receipt_verified(
    request: &ProjectionBatchRequestV1,
    evidence: &ProjectionRequestEvidenceV1,
    decisions: &[ChunkProjectionDecisionV1],
) -> Result<ProjectionBatchReceiptV1, ProjectionReceiptErrorV1> {
    build_batch_receipt_with(request, Some(evidence), decisions)
}

fn build_batch_receipt_with(
    request: &ProjectionBatchRequestV1,
    evidence: Option<&ProjectionRequestEvidenceV1>,
    decisions: &[ChunkProjectionDecisionV1],
) -> Result<ProjectionBatchReceiptV1, ProjectionReceiptErrorV1> {
    let expected_request = match evidence {
        Some(evidence) => evidence.request_digest.clone(),
        None => expected_request_digest(request)
            .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?,
    };
    if request.request_digest != expected_request {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    let reembed_reused = reembeds_reused_chunks(request)?;
    let recomputed_partitions;
    let partitions = match evidence {
        Some(evidence) => &evidence.partitions,
        None => {
            recomputed_partitions = partitions_of(&request.changes)?;
            &recomputed_partitions
        }
    };

    let mut seen = BTreeSet::new();
    let mut receipts = Vec::with_capacity(decisions.len());
    for decision in decisions {
        let Some((partition, change)) = partitions.get(&decision.chunk_id) else {
            return Err(ProjectionReceiptErrorV1::ExtraChunkReceipt(
                decision.chunk_id.clone(),
            ));
        };
        if !seen.insert(decision.chunk_id.clone()) {
            return Err(ProjectionReceiptErrorV1::DuplicateChunkReceipt(
                decision.chunk_id.clone(),
            ));
        }
        check_decision(*partition, change, decision, reembed_reused)?;
        receipts.push(CodeChunkProjectionReceiptV1 {
            projection_key: request.target_projection_key.clone(),
            request_digest: request.request_digest.clone(),
            prior_generation: request.changes.from_generation.clone(),
            source_generation: request.changes.to_generation.clone(),
            source_manifest_digest: request.changes.manifest_digest.clone(),
            chunk_id: decision.chunk_id.clone(),
            prior_chunk_digest: decision.prior_chunk_digest.clone(),
            current_chunk_digest: decision.current_chunk_digest.clone(),
            operation: decision.operation,
            outcome: decision.outcome.clone(),
            output_digest: decision.output_digest.clone(),
        });
    }
    for chunk_id in partitions.keys() {
        if !seen.contains(chunk_id) {
            return Err(ProjectionReceiptErrorV1::MissingChunkReceipt(
                chunk_id.clone(),
            ));
        }
    }
    receipts.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));

    let reused_count = receipts
        .iter()
        .filter(|receipt| receipt.operation == ProjectionOperationV1::Reused)
        .count() as u64;
    let mut batch = ProjectionBatchReceiptV1 {
        target_projection_key: request.target_projection_key.clone(),
        request_digest: request.request_digest.clone(),
        source_generation: request.changes.to_generation.clone(),
        source_manifest_digest: request.changes.manifest_digest.clone(),
        receipts,
        reused_count,
        publication_digest: placeholder_digest(),
    };
    batch.publication_digest = expected_publication_digest(&batch)
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    Ok(batch)
}

/// Verify a batch receipt against its request: request and publication
/// digests recompute, the receipt set covers the request exactly (no
/// missing, extra, or duplicate receipts), every receipt carries the
/// request's projection key and generation watermarks, and every operation,
/// outcome, and digest is consistent with the request partitions.
pub fn verify_batch_receipt(
    request: &ProjectionBatchRequestV1,
    batch: &ProjectionBatchReceiptV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    verify_batch_receipt_with(request, None, batch, PublicationDigestTrustV1::Unverified)
}

/// [`verify_batch_receipt`] for a request this chain already verified, and a
/// receipt whose publication digest may already be known good.
pub(crate) fn verify_batch_receipt_verified(
    request: &ProjectionBatchRequestV1,
    evidence: &ProjectionRequestEvidenceV1,
    batch: &ProjectionBatchReceiptV1,
    publication: PublicationDigestTrustV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    verify_batch_receipt_with(request, Some(evidence), batch, publication)
}

fn verify_batch_receipt_with(
    request: &ProjectionBatchRequestV1,
    evidence: Option<&ProjectionRequestEvidenceV1>,
    batch: &ProjectionBatchReceiptV1,
    publication: PublicationDigestTrustV1,
) -> Result<(), ProjectionReceiptErrorV1> {
    let expected_request = match evidence {
        Some(evidence) => evidence.request_digest.clone(),
        None => expected_request_digest(request)
            .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?,
    };
    // The batch is never trusted about the request it answers, even when the
    // request digest itself was recomputed earlier in this chain.
    if request.request_digest != expected_request || batch.request_digest != expected_request {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    let reembed_reused = reembeds_reused_chunks(request)?;
    if batch.target_projection_key != request.target_projection_key {
        return Err(ProjectionReceiptErrorV1::WrongProjectionKey);
    }
    if batch.source_generation != request.changes.to_generation
        || batch.source_manifest_digest != request.changes.manifest_digest
    {
        return Err(ProjectionReceiptErrorV1::CrossGenerationReceipt(
            batch.receipts.first().map_or_else(
                || {
                    CodeSearchChunkId::new("chunk.v1.empty-batch")
                        .expect("canonical chunk identity")
                },
                |receipt| receipt.chunk_id.clone(),
            ),
        ));
    }
    if batch
        .receipts
        .windows(2)
        .any(|pair| pair[0].chunk_id >= pair[1].chunk_id)
    {
        return Err(ProjectionReceiptErrorV1::NonCanonicalReceiptOrder);
    }

    let recomputed_partitions;
    let partitions = match evidence {
        Some(evidence) => &evidence.partitions,
        None => {
            recomputed_partitions = partitions_of(&request.changes)?;
            &recomputed_partitions
        }
    };
    let mut seen = BTreeSet::new();
    for receipt in &batch.receipts {
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
        let Some((partition, change)) = partitions.get(&receipt.chunk_id) else {
            return Err(ProjectionReceiptErrorV1::ExtraChunkReceipt(
                receipt.chunk_id.clone(),
            ));
        };
        if !seen.insert(receipt.chunk_id.clone()) {
            return Err(ProjectionReceiptErrorV1::DuplicateChunkReceipt(
                receipt.chunk_id.clone(),
            ));
        }
        let decision = ChunkProjectionDecisionV1 {
            chunk_id: receipt.chunk_id.clone(),
            prior_chunk_digest: receipt.prior_chunk_digest.clone(),
            current_chunk_digest: receipt.current_chunk_digest.clone(),
            operation: receipt.operation,
            outcome: receipt.outcome.clone(),
            output_digest: receipt.output_digest.clone(),
        };
        check_decision(*partition, change, &decision, reembed_reused)?;
    }
    for chunk_id in partitions.keys() {
        if !seen.contains(chunk_id) {
            return Err(ProjectionReceiptErrorV1::MissingChunkReceipt(
                chunk_id.clone(),
            ));
        }
    }

    let reused_count = batch
        .receipts
        .iter()
        .filter(|receipt| receipt.operation == ProjectionOperationV1::Reused)
        .count() as u64;
    if batch.reused_count != reused_count {
        return Err(ProjectionReceiptErrorV1::DigestMismatch);
    }
    if publication == PublicationDigestTrustV1::Unverified {
        let expected_publication = expected_publication_digest(batch)
            .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
        if batch.publication_digest != expected_publication {
            return Err(ProjectionReceiptErrorV1::DigestMismatch);
        }
    }
    Ok(())
}

fn reembeds_reused_chunks(
    request: &ProjectionBatchRequestV1,
) -> Result<bool, ProjectionReceiptErrorV1> {
    let projection_changed =
        request.previous_projection_key.as_ref() != Some(&request.target_projection_key);
    if projection_changed
        && !request.changes.reused.is_empty()
        && request.replay_reason != ProjectionReplayReasonV1::ProjectionProfileChange
    {
        return Err(ProjectionReceiptErrorV1::ProjectionKeyReplayRequiresAppliedWork);
    }
    Ok(projection_changed
        && !request.changes.reused.is_empty()
        && request.replay_reason == ProjectionReplayReasonV1::ProjectionProfileChange)
}

/// Index the changed-chunk set's partitions by chunk identity, validating
/// the set first (canonical order, partition shape, and manifest digest are
/// owned by the domain contract).
fn partitions_of(
    changes: &ChangedCodeChunkSetV1,
) -> Result<BTreeMap<CodeSearchChunkId, (Partition, DigestPair)>, ProjectionReceiptErrorV1> {
    changes
        .validate()
        .map_err(|error| ProjectionReceiptErrorV1::Contract(error.to_string()))?;
    Ok(index_partitions(changes))
}

/// Index an already-validated changed-chunk set by chunk identity.
///
/// `ChangedCodeChunkSetV1::validate` recomputes the whole set's manifest
/// digest, so it runs once per call chain and the index is threaded onward.
fn index_partitions(
    changes: &ChangedCodeChunkSetV1,
) -> BTreeMap<CodeSearchChunkId, (Partition, DigestPair)> {
    let mut partitions = BTreeMap::new();
    for change in &changes.added_or_changed {
        partitions.insert(
            change.chunk_id.clone(),
            (
                Partition::AddedOrChanged,
                DigestPair {
                    prior: change.prior_digest.clone(),
                    current: change.current_digest.clone(),
                },
            ),
        );
    }
    for change in &changes.deleted {
        partitions.insert(
            change.chunk_id.clone(),
            (
                Partition::Deleted,
                DigestPair {
                    prior: change.prior_digest.clone(),
                    current: change.current_digest.clone(),
                },
            ),
        );
    }
    for change in &changes.reused {
        partitions.insert(
            change.chunk_id.clone(),
            (
                Partition::Reused,
                DigestPair {
                    prior: change.prior_digest.clone(),
                    current: change.current_digest.clone(),
                },
            ),
        );
    }
    partitions
}

/// The digests a request partition declares for one chunk.
#[derive(Clone, Debug)]
struct DigestPair {
    prior: Option<ContentDigest>,
    current: Option<ContentDigest>,
}

/// Enforce operation, outcome, and digest consistency between one decision
/// (or receipt) and the request partition its chunk belongs to.
fn check_decision(
    partition: Partition,
    expected: &DigestPair,
    decision: &ChunkProjectionDecisionV1,
    reembed_reused: bool,
) -> Result<(), ProjectionReceiptErrorV1> {
    let inconsistent =
        || ProjectionReceiptErrorV1::InconsistentOperation(decision.chunk_id.clone());
    if decision.prior_chunk_digest != expected.prior
        || decision.current_chunk_digest != expected.current
    {
        return Err(inconsistent());
    }
    if partition != Partition::Reused && decision.outcome == ProjectionOutcomeV1::Reused {
        return Err(inconsistent());
    }
    match partition {
        Partition::AddedOrChanged => {
            let expected_operation = if expected.prior.is_none() {
                ProjectionOperationV1::Added
            } else {
                ProjectionOperationV1::Updated
            };
            if decision.operation != expected_operation {
                return Err(inconsistent());
            }
        }
        Partition::Deleted => {
            if decision.operation != ProjectionOperationV1::Deleted {
                return Err(inconsistent());
            }
        }
        Partition::Reused => {
            let valid = if reembed_reused {
                decision.operation == ProjectionOperationV1::Updated
                    && decision.outcome == ProjectionOutcomeV1::Applied
            } else {
                decision.operation == ProjectionOperationV1::Reused
                    && decision.outcome == ProjectionOutcomeV1::Reused
            };
            if !valid {
                return Err(inconsistent());
            }
        }
    }
    // Output digests: an applied upsert emits output; deletions, reuses,
    // skips, and failures emit none.
    let emits_output = decision.operation != ProjectionOperationV1::Deleted
        && decision.outcome == ProjectionOutcomeV1::Applied;
    if decision.output_digest.is_some() != emits_output {
        return Err(inconsistent());
    }
    Ok(())
}

/// A well-formed placeholder digest, replaced by the computed publication
/// digest before the batch is returned.
fn placeholder_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
        .expect("a zeroed sha256 digest is canonical")
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
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: Some(generation(1)),
            to_generation: generation(2),
            manifest_digest: manifest_digest('0'),
            added_or_changed,
            deleted,
            reused,
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
            ChunkProjectionDecisionV1 {
                chunk_id: chunk("reused"),
                prior_chunk_digest: Some(digest('f')),
                current_chunk_digest: Some(digest('f')),
                operation: ProjectionOperationV1::Reused,
                outcome: ProjectionOutcomeV1::Reused,
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
        // Canonical receipt order by chunk identity.
        let ids: Vec<&str> = batch
            .receipts
            .iter()
            .map(|receipt| receipt.chunk_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "chunk.v1.added",
                "chunk.v1.deleted",
                "chunk.v1.reused",
                "chunk.v1.updated"
            ]
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

        // The receipt proves zero work: every chunk reused, no output.
        assert!(batch_proves_zero_work(&batch));
        assert_eq!(batch.reused_count, 2);
        assert_eq!(batch.reused_count as usize, batch.receipts.len());
        assert!(batch_can_activate(&batch));
        assert!(
            batch
                .receipts
                .iter()
                .all(|receipt| receipt.output_digest.is_none())
        );
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

        // Missing: the "reused" chunk has no decision.
        let missing: Vec<_> = mixed_decisions()
            .into_iter()
            .filter(|decision| decision.chunk_id != chunk("reused"))
            .collect();
        assert_eq!(
            build_batch_receipt(&request, &missing),
            Err(ProjectionReceiptErrorV1::MissingChunkReceipt(chunk(
                "reused"
            )))
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
        let mut wrong_reuse = mixed_decisions();
        wrong_reuse[3].outcome = ProjectionOutcomeV1::Applied;
        wrong_reuse[3].output_digest = Some(digest('7'));
        assert_eq!(
            build_batch_receipt(&request, &wrong_reuse),
            Err(ProjectionReceiptErrorV1::InconsistentOperation(chunk(
                "reused"
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
}
