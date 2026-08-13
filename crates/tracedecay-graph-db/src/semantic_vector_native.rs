use tracedecay_domain::canonical_sha256;

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
