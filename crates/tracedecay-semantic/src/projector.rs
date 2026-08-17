//! semantic vector-generation projector.
//!
//! This module consumes query fallback's canonical, generation-bound chunks and emits
//! Plan 25 projection receipts plus a store-neutral vector-generation handoff.
//! It owns no scheduler, query path, profile activation, ANN, or quantization.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId,
    CodeSearchChunkId, CodeSearchChunkV1, ContentDigest, EmbeddingProjectionKeyV1, ManifestDigest,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionOperationV1,
    ProjectionOutcomeV1, ProjectionReplayReasonV1,
};

use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, ProjectionReceiptErrorV1, build_batch_receipt,
    expected_request_digest, verify_batch_receipt,
};

/// How many encoder groups the projector keeps in flight at once, per unit of
/// encoder concurrency.
///
/// Bounds two things at the same time: the number of chunk texts handed to the
/// encoder before any result comes back, and the number of encoded vectors
/// held outside `vectors` at any moment. Deeper windows buy nothing once every
/// session is busy; they only raise peak RSS.
const ENCODING_WINDOW_GROUPS_PER_WORKER: usize = 4;

/// The only projector dependency that may produce vector values.
pub trait CanonicalChunkVectorEncoderV1 {
    fn encode(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<Vec<f32>, String>;

    fn encode_batch(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        chunks: &[&CodeSearchChunkV1],
    ) -> Result<Vec<Vec<f32>>, String> {
        chunks.iter().map(|chunk| self.encode(key, chunk)).collect()
    }

    /// Encode several already-composed groups.
    ///
    /// Group composition and output order are fixed by the caller, so an
    /// implementation that runs the groups concurrently produces byte-identical
    /// vectors to this sequential default. That equivalence is what makes
    /// [`Self::encode_concurrency`] pure sizing policy.
    fn encode_batches(
        &mut self,
        key: &EmbeddingProjectionKeyV1,
        groups: &[&[&CodeSearchChunkV1]],
    ) -> Result<Vec<Vec<Vec<f32>>>, String> {
        groups
            .iter()
            .map(|group| self.encode_batch(key, group))
            .collect()
    }

    /// How many groups this encoder can usefully run at once. The default is
    /// a single in-order worker.
    fn encode_concurrency(&self) -> usize {
        1
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticProjectionErrorV1 {
    #[error("semantic projection contract violation: {0}")]
    Contract(String),
    #[error("the request target does not match the embedding projection key")]
    ProjectionKeyMismatch,
    #[error("a projection-key change must expand reused chunks into explicit embeds")]
    KeyReplayRequiresExplicitEmbeds,
    #[error("canonical chunk input is missing, duplicated, or extra: {0}")]
    CanonicalChunkSetMismatch(CodeSearchChunkId),
    #[error("canonical chunk {chunk_id} belongs to a foreign generation")]
    ForeignChunkGeneration { chunk_id: CodeSearchChunkId },
    #[error("canonical chunk {chunk_id} carries a digest not named by the request")]
    ChunkDigestMismatch { chunk_id: CodeSearchChunkId },
    #[error(
        "canonical chunk {chunk_id} has {actual_bytes} sanitized bytes, exceeding the admitted inference batch ceiling of {inference_batch_bytes} bytes"
    )]
    InferenceBatchByteCeilingExceeded {
        chunk_id: CodeSearchChunkId,
        actual_bytes: usize,
        inference_batch_bytes: usize,
    },
    #[error("vector encoder rejected chunk {chunk_id}: {reason}")]
    Encoder {
        chunk_id: CodeSearchChunkId,
        reason: String,
    },
    #[error("vector for chunk {chunk_id} has dimension {actual}, expected {expected}")]
    VectorDimensionMismatch {
        chunk_id: CodeSearchChunkId,
        expected: u32,
        actual: usize,
    },
    #[error("vector for chunk {chunk_id} contains a non-finite value")]
    NonFiniteVector { chunk_id: CodeSearchChunkId },
    #[error("vector output digest does not recompute for chunk {chunk_id}")]
    VectorDigestMismatch { chunk_id: CodeSearchChunkId },
    #[error("background semantic projection worker did not complete")]
    WorkerTerminated,
    #[error("semantic projection receipt rejected: {0}")]
    Receipt(#[from] ProjectionReceiptErrorV1),
}

/// One immutable vector row prepared from a canonical chunk.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectedChunkVectorV1 {
    pub projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub chunk_id: CodeSearchChunkId,
    pub chunk_digest: ContentDigest,
    pub values: Vec<f32>,
    pub output_digest: ContentDigest,
}

impl ProjectedChunkVectorV1 {
    fn new(
        projection_key: ProjectionKeyV1,
        source_generation: CodeGenerationId,
        source_manifest_digest: ManifestDigest,
        chunk: &CodeSearchChunkV1,
        values: Vec<f32>,
        dimensions: u32,
    ) -> Result<Self, SemanticProjectionErrorV1> {
        validate_vector(&chunk.id, &values, dimensions)?;
        let output_digest =
            vector_output_digest(&projection_key, &chunk.id, &chunk.content_digest, &values)?;
        Ok(Self {
            projection_key,
            source_generation,
            source_manifest_digest,
            chunk_id: chunk.id.clone(),
            chunk_digest: chunk.content_digest.clone(),
            values,
            output_digest,
        })
    }

    pub fn validate(&self, dimensions: u32) -> Result<(), SemanticProjectionErrorV1> {
        validate_vector(&self.chunk_id, &self.values, dimensions)?;
        let expected = vector_output_digest(
            &self.projection_key,
            &self.chunk_id,
            &self.chunk_digest,
            &self.values,
        )?;
        if self.output_digest != expected {
            return Err(SemanticProjectionErrorV1::VectorDigestMismatch {
                chunk_id: self.chunk_id.clone(),
            });
        }
        Ok(())
    }
}

/// Deletion evidence carried into the immutable vector-generation manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorTombstoneV1 {
    pub chunk_id: CodeSearchChunkId,
    pub prior_chunk_digest: ContentDigest,
}

/// Store-neutral handoff for one complete Plan 25 projection batch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PreparedVectorGenerationV1 {
    pub embedding_key: AdmittedEmbeddingProjectionKeyV1,
    pub request: ProjectionBatchRequestV1,
    pub receipt: ProjectionBatchReceiptV1,
    pub vectors: Vec<ProjectedChunkVectorV1>,
    pub tombstones: Vec<VectorTombstoneV1>,
}

/// Project one bounded request from canonical chunks. Only
/// `added_or_changed` chunks are supplied to the encoder. A projection-profile
/// change also embeds content-identical `reused` chunks into the new profile's
/// generation. Deleted chunks become tombstones; ordinary reused chunks remain
/// receipt-only so the store can copy their compatible prior vectors.
pub fn prepare_vector_generation<E: CanonicalChunkVectorEncoderV1>(
    admitted_projection: &AdmittedEmbeddingProjectionKeyV1,
    request: ProjectionBatchRequestV1,
    canonical_chunks: &[CodeSearchChunkV1],
    encoder: &mut E,
) -> Result<PreparedVectorGenerationV1, SemanticProjectionErrorV1> {
    let embedding_key = admitted_projection.embedding_key();
    let target_key = admitted_projection.projection_key().clone();
    if request.target_projection_key != target_key {
        return Err(SemanticProjectionErrorV1::ProjectionKeyMismatch);
    }
    let expected_digest = expected_request_digest(&request)
        .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))?;
    if request.request_digest != expected_digest {
        return Err(ProjectionReceiptErrorV1::DigestMismatch.into());
    }
    request
        .changes
        .validate()
        .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))?;
    let projection_changed =
        request.previous_projection_key.as_ref() != Some(&request.target_projection_key);
    let reembed_reused = projection_changed
        && request.replay_reason == ProjectionReplayReasonV1::ProjectionProfileChange;
    if projection_changed && !request.changes.reused.is_empty() && !reembed_reused {
        return Err(SemanticProjectionErrorV1::KeyReplayRequiresExplicitEmbeds);
    }

    let reembedded_changes = if reembed_reused {
        request.changes.reused.as_slice()
    } else {
        &[]
    };
    let expected_chunks = request
        .changes
        .added_or_changed
        .iter()
        .chain(reembedded_changes)
        .map(|change| (change.chunk_id.clone(), change.current_digest.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut chunks = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for chunk in canonical_chunks {
        chunk
            .validate()
            .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))?;
        if !seen.insert(chunk.id.clone()) || !expected_chunks.contains_key(&chunk.id) {
            return Err(SemanticProjectionErrorV1::CanonicalChunkSetMismatch(
                chunk.id.clone(),
            ));
        }
        if chunk.anchor.generation_id != request.changes.to_generation {
            return Err(SemanticProjectionErrorV1::ForeignChunkGeneration {
                chunk_id: chunk.id.clone(),
            });
        }
        if expected_chunks.get(&chunk.id).and_then(Option::as_ref) != Some(&chunk.content_digest) {
            return Err(SemanticProjectionErrorV1::ChunkDigestMismatch {
                chunk_id: chunk.id.clone(),
            });
        }
        chunks.insert(chunk.id.clone(), chunk);
    }
    if let Some(missing) = expected_chunks
        .keys()
        .find(|chunk_id| !chunks.contains_key(*chunk_id))
    {
        if reembedded_changes
            .iter()
            .any(|change| &change.chunk_id == missing)
        {
            return Err(SemanticProjectionErrorV1::KeyReplayRequiresExplicitEmbeds);
        }
        return Err(SemanticProjectionErrorV1::CanonicalChunkSetMismatch(
            missing.clone(),
        ));
    }

    let mut vectors =
        Vec::with_capacity(request.changes.added_or_changed.len() + reembedded_changes.len());
    let mut decisions = Vec::with_capacity(
        request.changes.added_or_changed.len()
            + request.changes.deleted.len()
            + request.changes.reused.len(),
    );
    encode_changes_windowed(
        encoder,
        embedding_key,
        &request.changes.added_or_changed,
        &chunks,
        |chunk_id| SemanticProjectionErrorV1::CanonicalChunkSetMismatch(chunk_id.clone()),
        |change, chunk, values| {
            let vector = ProjectedChunkVectorV1::new(
                target_key.clone(),
                request.changes.to_generation.clone(),
                request.changes.manifest_digest.clone(),
                chunk,
                values,
                embedding_key.dimensions,
            )?;
            decisions.push(ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(vector.output_digest.clone()),
            });
            vectors.push(vector);
            Ok(())
        },
    )?;

    let mut tombstones = Vec::with_capacity(request.changes.deleted.len());
    for change in &request.changes.deleted {
        let prior_chunk_digest = change.prior_digest.clone().ok_or_else(|| {
            SemanticProjectionErrorV1::ChunkDigestMismatch {
                chunk_id: change.chunk_id.clone(),
            }
        })?;
        tombstones.push(VectorTombstoneV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest,
        });
        decisions.push(ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: None,
            operation: ProjectionOperationV1::Deleted,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: None,
        });
    }
    for change in &request.changes.reused {
        if reembed_reused {
            continue;
        }
        decisions.push(ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: ProjectionOperationV1::Reused,
            outcome: ProjectionOutcomeV1::Reused,
            output_digest: None,
        });
    }
    encode_changes_windowed(
        encoder,
        embedding_key,
        reembedded_changes,
        &chunks,
        |_chunk_id| SemanticProjectionErrorV1::KeyReplayRequiresExplicitEmbeds,
        |change, chunk, values| {
            let vector = ProjectedChunkVectorV1::new(
                target_key.clone(),
                request.changes.to_generation.clone(),
                request.changes.manifest_digest.clone(),
                chunk,
                values,
                embedding_key.dimensions,
            )?;
            decisions.push(ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: ProjectionOperationV1::Updated,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(vector.output_digest.clone()),
            });
            vectors.push(vector);
            Ok(())
        },
    )?;

    let receipt = build_batch_receipt(&request, &decisions)?;
    verify_batch_receipt(&request, &receipt)?;
    Ok(PreparedVectorGenerationV1 {
        embedding_key: admitted_projection.clone(),
        request,
        receipt,
        vectors,
        tombstones,
    })
}

/// One bounded slice of a whole-corpus projection request.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectionRequestBatchV1 {
    pub request: ProjectionBatchRequestV1,
    pub canonical_chunks: Vec<CodeSearchChunkV1>,
}

/// One native encoder invocation derived from a file-local canonical order.
///
/// The request's change partitions remain chunk-ID sorted for their durable
/// digest contract; this grouping records the independent native-input order.
struct CanonicalEncoderGroupV1<'a> {
    changes: Vec<&'a ChangedCodeChunkV1>,
}

/// Split one whole-corpus projection request into batches that commit
/// independently.
///
/// Committing per batch is what bounds the live float set: a batch's vectors
/// are persisted and released before the next batch is embedded, instead of
/// the whole corpus accumulating in memory until a single terminal commit.
///
/// Splitting is identity-preserving, which is the load-bearing property:
///
/// - Boundaries preserve complete canonical encoder groups, greedily bounded
///   by both the admitted `inference_batch_size` and
///   `inference_batch_bytes`. The tensor shape the model sees never changes,
///   so vector bytes — and therefore every `output_digest`, and the generation
///   manifest digest built from those digests — are byte-identical.
/// - `added_or_changed` is split only between complete groups from its
///   already-canonical list, so each batch's partition is canonical too.
/// - Deletions and ordinary reuse are receipt-only decisions with no encoder
///   work, so they may fill page capacity without moving an encoder boundary.
/// - Re-embedded reuse is encoded by its own windowed pass. Profile-change
///   pages finish the added/deleted lane before beginning that reuse lane, so
///   no residual added or deleted change can shift a reused encoder group.
///
/// What legitimately does change is execution evidence: the run produces one
/// receipt per batch instead of one for the corpus, each with its own request
/// and publication digest. The immutable generation identity deliberately does
/// not depend on that lineage.
///
/// A request the projector would reject outright is returned unsplit, so the
/// rejection stays exactly where it was.
pub fn split_projection_request(
    request: &ProjectionBatchRequestV1,
    canonical_chunks: &[CodeSearchChunkV1],
    max_embeds_per_batch: usize,
    inference_batch_size: usize,
    inference_batch_bytes: usize,
) -> Result<Vec<ProjectionRequestBatchV1>, SemanticProjectionErrorV1> {
    let unsplit = || {
        Ok(vec![ProjectionRequestBatchV1 {
            request: request.clone(),
            canonical_chunks: canonical_chunks.to_vec(),
        }])
    };
    let projection_changed =
        request.previous_projection_key.as_ref() != Some(&request.target_projection_key);
    let reembed_reused = projection_changed
        && request.replay_reason == ProjectionReplayReasonV1::ProjectionProfileChange;
    if projection_changed && !request.changes.reused.is_empty() && !reembed_reused {
        return unsplit();
    }
    if inference_batch_size == 0 {
        return Err(SemanticProjectionErrorV1::Contract(
            "semantic projection inference batch size is zero".to_owned(),
        ));
    }
    if inference_batch_bytes == 0 {
        return Err(SemanticProjectionErrorV1::Contract(
            "semantic projection inference batch byte ceiling is zero".to_owned(),
        ));
    }
    // Round down to whole encoder groups; never below one group.
    let window = max_embeds_per_batch
        .saturating_sub(max_embeds_per_batch % inference_batch_size)
        .max(inference_batch_size);
    let total_changes = request
        .changes
        .added_or_changed
        .len()
        .saturating_add(request.changes.deleted.len())
        .saturating_add(request.changes.reused.len());
    if total_changes <= window {
        return unsplit();
    }

    let chunks_by_id = canonical_chunks
        .iter()
        .map(|chunk| (chunk.id.clone(), chunk))
        .collect::<BTreeMap<_, _>>();
    let added_groups = match canonical_encoder_groups(
        &request.changes.added_or_changed,
        &chunks_by_id,
        inference_batch_size,
        inference_batch_bytes,
        |chunk_id| SemanticProjectionErrorV1::CanonicalChunkSetMismatch(chunk_id.clone()),
    ) {
        Ok(group_lengths) => group_lengths,
        Err(_) => return unsplit(),
    };
    let reused_groups = if reembed_reused {
        match canonical_encoder_groups(
            &request.changes.reused,
            &chunks_by_id,
            inference_batch_size,
            inference_batch_bytes,
            |_chunk_id| SemanticProjectionErrorV1::KeyReplayRequiresExplicitEmbeds,
        ) {
            Ok(group_lengths) => group_lengths,
            Err(_) => return unsplit(),
        }
    } else {
        Vec::new()
    };
    let mut deleted = request.changes.deleted.as_slice();
    let mut reused = request.changes.reused.as_slice();
    let mut next_added_group = 0;
    let mut next_reused_group = 0;
    let mut batches = Vec::new();
    while next_added_group < added_groups.len()
        || !deleted.is_empty()
        || (reembed_reused && next_reused_group < reused_groups.len())
        || (!reembed_reused && !reused.is_empty())
    {
        // A profile change has two encoder lanes: added/changed chunks and
        // reembedded reuse. `prepare_vector_generation` encodes those lanes
        // separately, so co-filling a page with both would create a boundary
        // the whole request never had. Finish added/deleted pages first; once
        // they drain, reuse pages retain their full inference groups.
        let added_or_deleted_pending = next_added_group < added_groups.len() || !deleted.is_empty();
        let mut room = window;
        let mut embeds = take_full_encoder_groups(&added_groups, &mut next_added_group, &mut room);
        embeds.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        let take_deleted = deleted.len().min(room);
        let page_deleted = &deleted[..take_deleted];
        deleted = &deleted[take_deleted..];
        room -= take_deleted;
        let mut page_reused = if reembed_reused {
            if added_or_deleted_pending {
                Vec::new()
            } else {
                take_full_encoder_groups(&reused_groups, &mut next_reused_group, &mut room)
            }
        } else {
            let take_reused = reused.len().min(room);
            let page_reused = reused[..take_reused].iter().collect::<Vec<_>>();
            reused = &reused[take_reused..];
            page_reused
        };
        page_reused.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: request.changes.from_generation.clone(),
            to_generation: request.changes.to_generation.clone(),
            manifest_digest: request.changes.manifest_digest.clone(),
            added_or_changed: embeds.iter().map(|change| (*change).clone()).collect(),
            deleted: page_deleted.to_vec(),
            reused: page_reused.iter().map(|change| (*change).clone()).collect(),
        };
        changes.manifest_digest = changes
            .compute_digest()
            .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))?;
        let mut batch_request = ProjectionBatchRequestV1 {
            request_digest: request.request_digest.clone(),
            changes,
            previous_projection_key: request.previous_projection_key.clone(),
            target_projection_key: request.target_projection_key.clone(),
            replay_reason: request.replay_reason,
        };
        batch_request.request_digest = expected_request_digest(&batch_request)
            .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))?;
        // The projector rejects a canonical chunk it did not ask for, so each
        // batch carries exactly the chunks its own embeds name.
        let mut wanted = embeds
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        if reembed_reused {
            wanted.extend(page_reused.iter().map(|change| &change.chunk_id));
        }
        let batch_chunks = wanted
            .into_iter()
            .filter_map(|chunk_id| chunks_by_id.get(chunk_id).map(|chunk| (*chunk).clone()))
            .collect::<Vec<_>>();
        batches.push(ProjectionRequestBatchV1 {
            request: batch_request,
            canonical_chunks: batch_chunks,
        });
    }
    Ok(batches)
}

/// Take as many complete canonical encoder groups as fit in the current
/// projection page. The page limit may combine groups but never divide one,
/// because a split boundary otherwise changes native tensor input.
fn take_full_encoder_groups<'a>(
    groups: &[CanonicalEncoderGroupV1<'a>],
    next_group: &mut usize,
    room: &mut usize,
) -> Vec<&'a ChangedCodeChunkV1> {
    let mut changes = Vec::new();
    while let Some(group) = groups.get(*next_group) {
        if group.changes.len() > *room {
            break;
        }
        *room = room.saturating_sub(group.changes.len());
        changes.extend(group.changes.iter().copied());
        *next_group = next_group.saturating_add(1);
    }
    changes
}

/// Derive the canonical native encoder groups for one ordered projection lane.
///
/// Group membership is projection identity. Multi-chunk file buckets are
/// traversed in deterministic file-occurrence order; chunks inside a file are
/// ordered by source span, grain, and identity before greedy count/byte
/// grouping. Runs of one-chunk files are coalesced under the same limits so a
/// corpus of small files does not degenerate into one model call per chunk.
/// This keeps copied multi-chunk file tensors invariant when unrelated chunk
/// IDs interleave in the changed-set's durable chunk-ID order without
/// sacrificing the admitted production batch shape for singleton files.
fn canonical_encoder_groups<'a, Missing>(
    changes: &'a [ChangedCodeChunkV1],
    chunks: &BTreeMap<CodeSearchChunkId, &'a CodeSearchChunkV1>,
    inference_batch_size: usize,
    inference_batch_bytes: usize,
    missing: Missing,
) -> Result<Vec<CanonicalEncoderGroupV1<'a>>, SemanticProjectionErrorV1>
where
    Missing: Fn(&CodeSearchChunkId) -> SemanticProjectionErrorV1,
{
    if inference_batch_size == 0 {
        return Err(SemanticProjectionErrorV1::Contract(
            "semantic projection inference batch size is zero".to_owned(),
        ));
    }
    if inference_batch_bytes == 0 {
        return Err(SemanticProjectionErrorV1::Contract(
            "semantic projection inference batch byte ceiling is zero".to_owned(),
        ));
    }

    let mut ordered_changes = changes
        .iter()
        .map(|change| {
            let chunk = chunks
                .get(&change.chunk_id)
                .copied()
                .ok_or_else(|| missing(&change.chunk_id))?;
            let chunk_bytes = chunk.sanitized_text.as_str().len();
            if chunk_bytes > inference_batch_bytes {
                return Err(
                    SemanticProjectionErrorV1::InferenceBatchByteCeilingExceeded {
                        chunk_id: chunk.id.clone(),
                        actual_bytes: chunk_bytes,
                        inference_batch_bytes,
                    },
                );
            }
            Ok((change, chunk))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ordered_changes.sort_unstable_by(|(_, left), (_, right)| {
        left.anchor
            .file_occurrence_id
            .cmp(&right.anchor.file_occurrence_id)
            .then_with(|| {
                left.anchor
                    .source_span
                    .start_byte
                    .cmp(&right.anchor.source_span.start_byte)
            })
            .then_with(|| {
                left.anchor
                    .source_span
                    .end_byte
                    .cmp(&right.anchor.source_span.end_byte)
            })
            .then_with(|| left.anchor.grain.cmp(&right.anchor.grain))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut groups = Vec::new();
    let mut singleton_run = Vec::new();
    let mut next_file = 0;
    while next_file < ordered_changes.len() {
        let file = &ordered_changes[next_file].1.anchor.file_occurrence_id;
        let mut file_end = next_file.saturating_add(1);
        while ordered_changes
            .get(file_end)
            .is_some_and(|(_, chunk)| &chunk.anchor.file_occurrence_id == file)
        {
            file_end = file_end.saturating_add(1);
        }
        let file_changes = &ordered_changes[next_file..file_end];
        if file_changes.len() == 1 {
            singleton_run.push(file_changes[0]);
        } else {
            append_canonical_encoder_groups(
                &mut groups,
                &singleton_run,
                inference_batch_size,
                inference_batch_bytes,
            );
            singleton_run.clear();
            append_canonical_encoder_groups(
                &mut groups,
                file_changes,
                inference_batch_size,
                inference_batch_bytes,
            );
        }
        next_file = file_end;
    }
    append_canonical_encoder_groups(
        &mut groups,
        &singleton_run,
        inference_batch_size,
        inference_batch_bytes,
    );
    Ok(groups)
}

fn append_canonical_encoder_groups<'a>(
    groups: &mut Vec<CanonicalEncoderGroupV1<'a>>,
    ordered_changes: &[(&'a ChangedCodeChunkV1, &'a CodeSearchChunkV1)],
    inference_batch_size: usize,
    inference_batch_bytes: usize,
) {
    let mut group = Vec::new();
    let mut group_bytes = 0;
    for (change, chunk) in ordered_changes.iter().copied() {
        let chunk_bytes = chunk.sanitized_text.as_str().len();
        if !group.is_empty()
            && (group.len() == inference_batch_size
                || chunk_bytes > inference_batch_bytes.saturating_sub(group_bytes))
        {
            groups.push(CanonicalEncoderGroupV1 { changes: group });
            group = Vec::new();
            group_bytes = 0;
        }
        group.push(change);
        group_bytes = group_bytes.saturating_add(chunk_bytes);
    }
    if !group.is_empty() {
        groups.push(CanonicalEncoderGroupV1 { changes: group });
    }
}

/// Encode `changes` group by group, dispatching a bounded window of groups to
/// the encoder at a time and draining each window in input order.
///
/// Two invariants make this safe to widen:
///
/// - Groups greedily use the exact `inference_batch_size` and
///   `inference_batch_bytes` pinned by the admitted embedding projection. The
///   tensor shape therefore never depends on the dispatch window or the
///   encoder's concurrency.
/// - Results are drained in input order, so vectors, decisions, and the
///   lowest-index failure are identical at any width.
fn encode_changes_windowed<E, Missing, Sink>(
    encoder: &mut E,
    embedding_key: &EmbeddingProjectionKeyV1,
    changes: &[ChangedCodeChunkV1],
    chunks: &BTreeMap<CodeSearchChunkId, &CodeSearchChunkV1>,
    missing: Missing,
    mut sink: Sink,
) -> Result<(), SemanticProjectionErrorV1>
where
    E: CanonicalChunkVectorEncoderV1 + ?Sized,
    Missing: Fn(&CodeSearchChunkId) -> SemanticProjectionErrorV1,
    Sink: FnMut(
        &ChangedCodeChunkV1,
        &CodeSearchChunkV1,
        Vec<f32>,
    ) -> Result<(), SemanticProjectionErrorV1>,
{
    if changes.is_empty() {
        return Ok(());
    }
    let inference_batch_size =
        usize::try_from(embedding_key.inference_batch_size).map_err(|_| {
            SemanticProjectionErrorV1::Contract(
                "semantic projection inference batch size exceeds this platform".to_owned(),
            )
        })?;
    let inference_batch_bytes =
        usize::try_from(embedding_key.inference_batch_bytes).map_err(|_| {
            SemanticProjectionErrorV1::Contract(
                "semantic projection inference batch byte ceiling exceeds this platform".to_owned(),
            )
        })?;
    let canonical_groups = canonical_encoder_groups(
        changes,
        chunks,
        inference_batch_size,
        inference_batch_bytes,
        missing,
    )?;
    let window_groups = ENCODING_WINDOW_GROUPS_PER_WORKER
        .saturating_mul(encoder.encode_concurrency().max(1))
        .max(1);

    for group_window in canonical_groups.chunks(window_groups) {
        let groups = group_window
            .iter()
            .map(|group| {
                let group_chunks = group
                    .changes
                    .iter()
                    .map(|change| {
                        chunks.get(&change.chunk_id).copied().ok_or_else(|| {
                            SemanticProjectionErrorV1::Contract(
                                "canonical encoder group lost a previously validated chunk"
                                    .to_owned(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((group, group_chunks))
            })
            .collect::<Result<Vec<_>, SemanticProjectionErrorV1>>()?;
        let group_refs = groups
            .iter()
            .map(|(_, group_chunks)| group_chunks.as_slice())
            .collect::<Vec<&[&CodeSearchChunkV1]>>();
        let first_chunk_id = groups
            .first()
            .and_then(|(group, _)| group.changes.first())
            .map(|change| change.chunk_id.clone())
            .ok_or_else(|| {
                SemanticProjectionErrorV1::Contract(
                    "canonical encoder grouping produced an empty dispatch window".to_owned(),
                )
            })?;
        let encoded = encoder
            .encode_batches(embedding_key, &group_refs)
            .map_err(|reason| SemanticProjectionErrorV1::Encoder {
                chunk_id: first_chunk_id.clone(),
                reason,
            })?;
        if encoded.len() != groups.len() {
            return Err(SemanticProjectionErrorV1::Encoder {
                chunk_id: first_chunk_id,
                reason: "semantic projector returned an unexpected vector group count".to_owned(),
            });
        }
        for ((group, group_chunks), values) in groups.into_iter().zip(encoded) {
            if values.len() != group.changes.len() {
                return Err(SemanticProjectionErrorV1::Encoder {
                    chunk_id: group
                        .changes
                        .first()
                        .map(|change| change.chunk_id.clone())
                        .ok_or_else(|| {
                            SemanticProjectionErrorV1::Contract(
                                "canonical encoder grouping produced an empty group".to_owned(),
                            )
                        })?,
                    reason: "semantic projector returned an unexpected vector batch size"
                        .to_owned(),
                });
            }
            for ((change, chunk), vector) in group.changes.iter().zip(group_chunks).zip(values) {
                sink(change, chunk, vector)?;
            }
        }
    }
    Ok(())
}

/// Run one bounded projection batch on the blocking worker pool.
///
/// Projection never executes on retrieval tasks. The returned handoff is
/// still store-neutral: cancellation or worker failure cannot change the
/// active vector-generation pointer, and publication remains a separate
/// complete-generation compare-and-swap.
pub async fn prepare_vector_generation_async<E>(
    admitted_projection: AdmittedEmbeddingProjectionKeyV1,
    request: ProjectionBatchRequestV1,
    canonical_chunks: Vec<CodeSearchChunkV1>,
    mut encoder: E,
) -> Result<PreparedVectorGenerationV1, SemanticProjectionErrorV1>
where
    E: CanonicalChunkVectorEncoderV1 + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        prepare_vector_generation(
            &admitted_projection,
            request,
            &canonical_chunks,
            &mut encoder,
        )
    })
    .await
    .map_err(|_| SemanticProjectionErrorV1::WorkerTerminated)?
}

fn validate_vector(
    chunk_id: &CodeSearchChunkId,
    values: &[f32],
    dimensions: u32,
) -> Result<(), SemanticProjectionErrorV1> {
    if values.len() != dimensions as usize {
        return Err(SemanticProjectionErrorV1::VectorDimensionMismatch {
            chunk_id: chunk_id.clone(),
            expected: dimensions,
            actual: values.len(),
        });
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(SemanticProjectionErrorV1::NonFiniteVector {
            chunk_id: chunk_id.clone(),
        });
    }
    Ok(())
}

pub fn vector_output_digest(
    projection_key: &ProjectionKeyV1,
    chunk_id: &CodeSearchChunkId,
    chunk_digest: &ContentDigest,
    values: &[f32],
) -> Result<ContentDigest, SemanticProjectionErrorV1> {
    tracedecay_domain::semantic_vector_output_digest(projection_key, chunk_id, chunk_digest, values)
        .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))
}
