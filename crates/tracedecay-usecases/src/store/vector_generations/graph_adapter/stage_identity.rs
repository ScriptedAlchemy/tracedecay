use tracedecay_domain::canonical_sha256;

use super::super::{VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1};
use super::persistence::storage_error;

const STAGE_ATTEMPT_DOMAIN: &str = "tracedecay.semantic-vector.stage-attempt.v1";

pub(super) fn next_stage_attempt(
    logical_build: &VectorGenerationBuildIdV1,
    cancelled_attempt: &VectorGenerationBuildIdV1,
    cancelled_plan_digest: &str,
) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
    canonical_sha256(&(
        STAGE_ATTEMPT_DOMAIN,
        logical_build,
        cancelled_attempt,
        cancelled_plan_digest,
    ))
    .map(VectorGenerationBuildIdV1)
    .map_err(storage_error)
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::ManifestDigest;

    use super::next_stage_attempt;
    use crate::store::vector_generations::VectorGenerationBuildIdV1;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("manifest digest")
    }

    #[test]
    fn cancelled_stage_gets_a_new_deterministic_attempt_identity() {
        let logical_build = VectorGenerationBuildIdV1(digest('a'));
        let cancelled_attempt = VectorGenerationBuildIdV1(digest('b'));
        let cancelled_plan = digest('c');

        let first = next_stage_attempt(&logical_build, &cancelled_attempt, cancelled_plan.as_str())
            .expect("next attempt");
        let replay =
            next_stage_attempt(&logical_build, &cancelled_attempt, cancelled_plan.as_str())
                .expect("same cancelled terminal derives same next attempt");

        assert_ne!(first, logical_build);
        assert_ne!(first, cancelled_attempt);
        assert_eq!(first, replay);
    }
}
