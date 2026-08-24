//! Production feedback-cycle runtime-state owner for project-open registration.

use std::sync::Arc;

use tracedecay_application::feedback::{
    FeedbackPortFuture, FeedbackRuntimeStatePort, FeedbackRuntimeStateV1,
};
use tracedecay_application::{RequestAdmission, RequestContext};
use tracedecay_domain::feedback::{
    FeedbackAuthoritativeRuntimeStateV1, FeedbackCycleRuntimeSnapshotV1, FeedbackEvaluationInputV1,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use crate::graph::{CodeGraphProjectionReadPort, CodeGraphReadRequest};

/// Resolves feedback runtime state from the admitted evaluation input and the
/// live graph watermark. It never invents scope/content identities; those come
/// from the caller-owned evaluation request.
pub struct ProductionFeedbackRuntimeStateV1 {
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    configuration_digest: ManifestDigest,
    policy_digest: ManifestDigest,
}

impl ProductionFeedbackRuntimeStateV1 {
    pub fn new(
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
        configuration_digest: ManifestDigest,
        policy_digest: ManifestDigest,
    ) -> Self {
        Self {
            code_graph,
            configuration_digest,
            policy_digest,
        }
    }
}

impl FeedbackRuntimeStatePort for ProductionFeedbackRuntimeStateV1 {
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>> {
        Box::pin(async move {
            if context.validate().is_err()
                || context.admission_at(input.observed_at) != RequestAdmission::Admitted
            {
                return None;
            }
            if input.request.configuration_digest != self.configuration_digest
                || input.request.policy_digest != self.policy_digest
            {
                return None;
            }
            let Ok(graph) = self
                .code_graph
                .open(CodeGraphReadRequest::from_context(
                    context,
                    input.observed_at,
                ))
                .await
            else {
                return None;
            };
            let graph_generation = graph.generation().clone();
            let watermark = canonical_sha256(&(
                "tracedecay.feedback.runtime-watermark.v1",
                &graph_generation,
                &self.configuration_digest,
                &self.policy_digest,
            ))
            .ok()?;
            let generation_id = match &input.request.content {
                tracedecay_domain::feedback::FeedbackContentIdentityV1::SavedContent { .. } => {
                    match input.target.generation_id.as_ref() {
                        Some(generation) if generation == &graph_generation => {
                            Some(graph_generation)
                        }
                        Some(_) | None => return None,
                    }
                }
                tracedecay_domain::feedback::FeedbackContentIdentityV1::EphemeralOverlay {
                    ..
                } => None,
            };
            FeedbackRuntimeStateV1::new(
                FeedbackAuthoritativeRuntimeStateV1 {
                    snapshot: FeedbackCycleRuntimeSnapshotV1::from_request(&input.request),
                    baseline_horizon: None,
                    runtime_watermark: watermark,
                },
                generation_id,
            )
            .ok()
        })
    }
}
