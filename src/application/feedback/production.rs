//! Production feedback-cycle runtime-state owner for project-open registration.

use std::sync::Arc;

use tracedecay_application::feedback::{
    FeedbackPortFuture, FeedbackRuntimeStatePort, FeedbackRuntimeStateV1,
};
use tracedecay_application::{RequestAdmission, RequestContext};
use tracedecay_domain::feedback::{
    FeedbackAuthoritativeRuntimeStateV1, FeedbackCycleRuntimeSnapshotV1, FeedbackEvaluationInputV1,
};
use tracedecay_domain::{CodeGenerationId, ManifestDigest, canonical_sha256};

use crate::tracedecay::TraceDecay;

/// Resolves feedback runtime state from the admitted evaluation input and the
/// live graph watermark. It never invents scope/content identities; those come
/// from the caller-owned evaluation request.
pub struct ProductionFeedbackRuntimeStateV1 {
    graph: Arc<TraceDecay>,
    configuration_digest: ManifestDigest,
    policy_digest: ManifestDigest,
}

impl ProductionFeedbackRuntimeStateV1 {
    pub fn new(
        graph: Arc<TraceDecay>,
        configuration_digest: ManifestDigest,
        policy_digest: ManifestDigest,
    ) -> Self {
        Self {
            graph,
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
            let watermark = self
                .graph
                .get_stats()
                .await
                .ok()
                .and_then(|stats| {
                    canonical_sha256(&(
                        "tracedecay.feedback.runtime-watermark.v1",
                        stats.node_count,
                        stats.edge_count,
                        &self.configuration_digest,
                        &self.policy_digest,
                    ))
                    .ok()
                })
                .unwrap_or_else(|| self.configuration_digest.clone());
            let generation_id = match &input.request.content {
                tracedecay_domain::feedback::FeedbackContentIdentityV1::SavedContent { .. } => {
                    input.target.generation_id.clone().or_else(|| {
                        CodeGenerationId::new(format!(
                            "generation.project-open.{}",
                            watermark.as_str().trim_start_matches("sha256:")
                        ))
                        .ok()
                    })
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
