use crate::{
    CodeSearchChunkId, ContentDigest, DomainError, ProjectionBatchReceiptV1, ProjectionKeyV1,
    canonical_sha256,
};

const VECTOR_OUTPUT_DIGEST_DOMAIN: &str = "tracedecay.semantic-vector-output.v1";
pub const PROJECTION_PUBLICATION_SEPARATOR: &str = "tracedecay.projection-batch-receipt.v1";

pub fn semantic_vector_output_digest(
    projection_key: &ProjectionKeyV1,
    chunk_id: &CodeSearchChunkId,
    chunk_digest: &ContentDigest,
    values: &[f32],
) -> Result<ContentDigest, DomainError> {
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
    ))?;
    ContentDigest::new(digest.as_str().to_owned())
}

pub fn projection_batch_publication_digest(
    batch: &ProjectionBatchReceiptV1,
) -> Result<crate::ManifestDigest, DomainError> {
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
