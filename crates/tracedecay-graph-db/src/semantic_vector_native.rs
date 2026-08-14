use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeChunkProjectionReceiptV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionOperationV1,
    ProjectionOutcomeV1, canonical_sha256,
};

use crate::{GraphDbError, GraphEntityId, GraphLabel, GraphPropertyName, GraphRelationId};

pub const CONTROL_ID: &str = "semantic-vector:control";
pub const CONTROL_LABEL: &str = "semantic-vector-control-v1";
pub const BUILD_LABEL: &str = "semantic-vector-build-v1";
pub const BUILD_MEMBER_LABEL: &str = "semantic-vector-build-member-v1";
pub const STAGED_VECTOR_LABEL: &str = "semantic-vector-staged-vector-v1";
pub const STAGED_TOMBSTONE_LABEL: &str = "semantic-vector-staged-tombstone-v1";
pub const BUILD_BATCH_LABEL: &str = "semantic-vector-build-batch-v1";
pub const GENERATION_LABEL: &str = "semantic-vector-generation-v1";
pub const GENERATION_VECTOR_LABEL: &str = "semantic-vector-generation-vector-v1";
pub const GENERATION_TOMBSTONE_LABEL: &str = "semantic-vector-generation-tombstone-v1";
pub const GENERATION_RECEIPT_LABEL: &str = "semantic-vector-generation-receipt-v1";
pub const CONTAINS_KIND: &str = "semantic_vector_contains";
pub const BASE_KIND: &str = "semantic_vector_base";
pub const GENERATION_CATALOG_KIND: &str = "semantic_vector_generation_catalog";
pub const REVISION: &str = "revision";
pub const BUILD_ID: &str = "build_id";
pub const GENERATION_ID: &str = "generation_id";
pub const CHUNK_ID: &str = "chunk_id";
pub const CHUNK_DIGEST: &str = "chunk_digest";
pub const OUTPUT_DIGEST: &str = "output_digest";
pub const TARGET_PROJECTION: &str = "target_projection";
pub const SOURCE_GENERATION: &str = "source_generation";
pub const SOURCE_MANIFEST: &str = "source_manifest";
pub const BASE_GENERATION: &str = "base_generation";
pub const EMBEDDING_KEY: &str = "embedding_key";
pub const CHECKPOINT: &str = "checkpoint";
pub const MANIFEST_DIGEST: &str = "manifest_digest";
pub const REQUEST_DIGEST: &str = "request_digest";
pub const PREPARED_DIGEST: &str = "prepared_digest";
pub const RECEIPT: &str = "receipt";
pub const PRIOR_DIGEST: &str = "prior_digest";
pub const ORDINAL: &str = "ordinal";
pub const ROW_COUNT: &str = "row_count";
pub const VECTOR_BYTES: &str = "vector_bytes";
pub const EXPECTED_COUNT: &str = "expected_count";
pub const VECTOR_COUNT: &str = "vector_count";
pub const TOMBSTONE_COUNT: &str = "tombstone_count";
pub const BATCH_COUNT: &str = "batch_count";
pub const RECEIPT_COUNT: &str = "receipt_count";
pub const VECTOR: &str = "vector";

const ID_DOMAIN: &str = "tracedecay.semantic-vector.record-id.v1";

pub fn build_entity_id(build: &str) -> Result<GraphEntityId, GraphDbError> {
    GraphEntityId::new(format!("semantic-vector:build:{build}"))
}

pub fn generation_entity_id(generation: &str) -> Result<GraphEntityId, GraphDbError> {
    GraphEntityId::new(format!("semantic-vector:generation:{generation}"))
}

pub fn generation_label(generation: &str) -> Result<GraphLabel, GraphDbError> {
    GraphLabel::new(format!("semantic-vector-generation:{generation}"))
}

pub fn vector_property(generation: &str) -> Result<GraphPropertyName, GraphDbError> {
    GraphPropertyName::new(format!("{VECTOR}:{generation}"))
}

pub fn scoped_entity_id(
    kind: &str,
    owner: &str,
    member: &str,
) -> Result<GraphEntityId, GraphDbError> {
    let digest = canonical_sha256(&(ID_DOMAIN, kind, owner, member))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    GraphEntityId::new(format!("semantic-vector:{kind}:{}", digest.as_str()))
}

pub fn relation_id(
    from: &GraphEntityId,
    to: &GraphEntityId,
    kind: &str,
    discriminator: &str,
) -> Result<GraphRelationId, GraphDbError> {
    let digest = canonical_sha256(&(
        ID_DOMAIN,
        "relation",
        from.as_str(),
        to.as_str(),
        kind,
        discriminator,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    GraphRelationId::new(format!("semantic-vector:relation:{}", digest.as_str()))
}

/// Page receipts repeat projection/source identity on every chunk. Persist
/// that identity once and reconstruct on read so a production-width page
/// stays inside the 1 MiB property ceiling.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersistedBatchReceiptV1 {
    target_projection_key: ProjectionKeyV1,
    request_digest: ManifestDigest,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    reused_count: u64,
    publication_digest: ManifestDigest,
    receipts: Vec<PersistedChunkReceiptV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersistedChunkReceiptV1 {
    chunk_id: CodeSearchChunkId,
    prior_generation: Option<CodeGenerationId>,
    prior_chunk_digest: Option<ContentDigest>,
    current_chunk_digest: Option<ContentDigest>,
    operation: ProjectionOperationV1,
    outcome: ProjectionOutcomeV1,
    output_digest: Option<ContentDigest>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum StoredBatchReceiptV1 {
    Fat(ProjectionBatchReceiptV1),
    Slim(PersistedBatchReceiptV1),
}

pub fn encode_generation_receipt(
    receipt: &ProjectionBatchReceiptV1,
) -> Result<Vec<u8>, GraphDbError> {
    serde_json::to_vec(&PersistedBatchReceiptV1 {
        target_projection_key: receipt.target_projection_key.clone(),
        request_digest: receipt.request_digest.clone(),
        source_generation: receipt.source_generation.clone(),
        source_manifest_digest: receipt.source_manifest_digest.clone(),
        reused_count: receipt.reused_count,
        publication_digest: receipt.publication_digest.clone(),
        receipts: receipt
            .receipts
            .iter()
            .map(|chunk| PersistedChunkReceiptV1 {
                chunk_id: chunk.chunk_id.clone(),
                prior_generation: chunk.prior_generation.clone(),
                prior_chunk_digest: chunk.prior_chunk_digest.clone(),
                current_chunk_digest: chunk.current_chunk_digest.clone(),
                operation: chunk.operation,
                outcome: chunk.outcome.clone(),
                output_digest: chunk.output_digest.clone(),
            })
            .collect(),
    })
    .map_err(|error| GraphDbError::invalid(error.to_string()))
}

pub fn decode_generation_receipt(bytes: &[u8]) -> Result<ProjectionBatchReceiptV1, GraphDbError> {
    match serde_json::from_slice::<StoredBatchReceiptV1>(bytes)
        .map_err(|_| GraphDbError::Conflict)?
    {
        StoredBatchReceiptV1::Fat(receipt) => Ok(receipt),
        StoredBatchReceiptV1::Slim(receipt) => Ok(ProjectionBatchReceiptV1 {
            target_projection_key: receipt.target_projection_key.clone(),
            request_digest: receipt.request_digest.clone(),
            source_generation: receipt.source_generation.clone(),
            source_manifest_digest: receipt.source_manifest_digest.clone(),
            reused_count: receipt.reused_count,
            publication_digest: receipt.publication_digest,
            receipts: receipt
                .receipts
                .into_iter()
                .map(|chunk| CodeChunkProjectionReceiptV1 {
                    projection_key: receipt.target_projection_key.clone(),
                    request_digest: receipt.request_digest.clone(),
                    prior_generation: chunk.prior_generation,
                    source_generation: receipt.source_generation.clone(),
                    source_manifest_digest: receipt.source_manifest_digest.clone(),
                    chunk_id: chunk.chunk_id,
                    prior_chunk_digest: chunk.prior_chunk_digest,
                    current_chunk_digest: chunk.current_chunk_digest,
                    operation: chunk.operation,
                    outcome: chunk.outcome,
                    output_digest: chunk.output_digest,
                })
                .collect(),
        }),
    }
}
