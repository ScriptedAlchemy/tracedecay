use tracedecay_domain::{ManifestDigest, canonical_sha256};

use super::{VectorGenerationPlanV1, VectorGenerationStoreErrorV1};

const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";

/// Stable semantic generation identity known before projection starts.
///
/// The projection key binds the admitted model/profile/privacy inputs, while
/// the source manifest and ordered eligible chunk identities bind the exact
/// source corpus. Vector output digests remain independently verified
/// execution evidence and cannot move this identity.
pub(crate) fn generation_identity_digest(
    plan: &VectorGenerationPlanV1,
) -> Result<ManifestDigest, VectorGenerationStoreErrorV1> {
    canonical_sha256(&(
        VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN,
        &plan.target_projection_key,
        &plan.source_generation,
        &plan.source_manifest_digest,
        &plan.expected_chunk_ids,
    ))
    .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))
}
