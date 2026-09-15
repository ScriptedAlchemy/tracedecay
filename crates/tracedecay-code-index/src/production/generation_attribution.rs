//! Generation-bound affected-test attribution authority.

use tracedecay_domain::{CodeGenerationId, ProviderEvaluationStateV1};

use super::GenerationTestAttributionJoinReadPort;
use super::{GenerationProviderCoverageV1, GenerationProviderReadV1, GenerationTestJoinV1};

/// Immutable test-attribution reader derived from one sealed production code
/// generation. The reader owns no second graph or test store: it projects
/// conservative candidates from the generation's canonical relation graph and
/// retains the exact generation/test watermark produced at construction.
#[derive(Clone, Debug)]
pub struct PublishedGenerationTestAttributionAuthorityV1 {
    pub(super) generation_id: CodeGenerationId,
    pub(super) read: GenerationProviderReadV1<GenerationTestJoinV1>,
}

impl GenerationTestAttributionJoinReadPort for PublishedGenerationTestAttributionAuthorityV1 {
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
        if generation == &self.generation_id {
            self.read.clone()
        } else {
            GenerationProviderReadV1::new(
                ProviderEvaluationStateV1::Stale,
                GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .unwrap_or_else(|_| panic!("static stale attribution read"))
        }
    }
}
