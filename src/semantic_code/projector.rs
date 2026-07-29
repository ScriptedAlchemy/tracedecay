//! PR10 vector-generation projector.
//!
//! This module consumes PR9's canonical, generation-bound chunks and emits
//! Plan 25 projection receipts plus a store-neutral vector-generation handoff.
//! It owns no scheduler, query path, profile activation, ANN, or quantization.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1,
    ContentDigest, EmbeddingProjectionKeyV1, ManifestDigest, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionOperationV1, ProjectionOutcomeV1,
    ProjectionReplayReasonV1, canonical_sha256,
};

use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, ProjectionReceiptErrorV1, build_batch_receipt,
    expected_request_digest, verify_batch_receipt,
};

const VECTOR_OUTPUT_DIGEST_DOMAIN: &str = "tracedecay.semantic-vector-output.v1";
const VECTOR_ENCODING_BATCH_SIZE: usize = 8;

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
    #[error("Plan25 projection receipt rejected: {0}")]
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
    for changes in request
        .changes
        .added_or_changed
        .chunks(VECTOR_ENCODING_BATCH_SIZE)
    {
        let batch = changes
            .iter()
            .map(|change| {
                chunks.get(&change.chunk_id).copied().ok_or_else(|| {
                    SemanticProjectionErrorV1::CanonicalChunkSetMismatch(change.chunk_id.clone())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let encoded = encoder
            .encode_batch(embedding_key, &batch)
            .map_err(|reason| SemanticProjectionErrorV1::Encoder {
                chunk_id: batch
                    .first()
                    .map_or_else(|| changes[0].chunk_id.clone(), |chunk| chunk.id.clone()),
                reason,
            })?;
        if encoded.len() != batch.len() {
            return Err(SemanticProjectionErrorV1::Encoder {
                chunk_id: batch[0].id.clone(),
                reason: "semantic projector returned an unexpected vector batch size".to_owned(),
            });
        }
        for ((change, chunk), values) in changes.iter().zip(batch).zip(encoded) {
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
        }
    }

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
    for changes in reembedded_changes.chunks(VECTOR_ENCODING_BATCH_SIZE) {
        let batch = changes
            .iter()
            .map(|change| {
                chunks
                    .get(&change.chunk_id)
                    .copied()
                    .ok_or(SemanticProjectionErrorV1::KeyReplayRequiresExplicitEmbeds)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let encoded = encoder
            .encode_batch(embedding_key, &batch)
            .map_err(|reason| SemanticProjectionErrorV1::Encoder {
                chunk_id: batch
                    .first()
                    .map_or_else(|| changes[0].chunk_id.clone(), |chunk| chunk.id.clone()),
                reason,
            })?;
        if encoded.len() != batch.len() {
            return Err(SemanticProjectionErrorV1::Encoder {
                chunk_id: batch[0].id.clone(),
                reason: "semantic projector returned an unexpected vector batch size".to_owned(),
            });
        }
        for ((change, chunk), values) in changes.iter().zip(batch).zip(encoded) {
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
        }
    }

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

pub(crate) fn vector_output_digest(
    projection_key: &ProjectionKeyV1,
    chunk_id: &CodeSearchChunkId,
    chunk_digest: &ContentDigest,
    values: &[f32],
) -> Result<ContentDigest, SemanticProjectionErrorV1> {
    let bits = values
        .iter()
        .map(|value| value.to_bits())
        .collect::<Vec<_>>();
    let digest = canonical_sha256(&(
        VECTOR_OUTPUT_DIGEST_DOMAIN,
        projection_key,
        chunk_id,
        chunk_digest,
        bits,
    ))
    .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))?;
    ContentDigest::new(digest.as_str().to_string())
        .map_err(|error| SemanticProjectionErrorV1::Contract(error.to_string()))
}
