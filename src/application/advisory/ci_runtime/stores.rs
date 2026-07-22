//! Project-owned CI retention and code-anchor stores for PR13 production open.

use std::sync::Arc;

use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationRequestV1, FeedbackPortFuture,
};
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::feedback::{
    CiFailureCoverageV1, CiFailureKindV1, CiFailureLocalizationStateV1, FeedbackScopeV1,
};
use tracedecay_domain::{CanonicalObservationIdV1, RetrievalAnchorId};

use super::production::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiRetainedProviderObservationAuthorityV1,
    CiRetainedProviderObservationV1, CiRetainedProviderRecordV1,
};
use super::GitHubCiProviderRecordV1;
use crate::application::advisory::context_allows_feedback_operation;
use crate::db::Database;
use crate::tracedecay::TraceDecay;

const RETAINED_KEY_DOMAIN_V1: &str = "tracedecay.pr13.ci.retained-key.v1";
const RETAINED_KEY_PREFIX_V1: &str = "feedback.ci-failure.retained.v1.";
const MAX_RETAINED_BYTES_V1: usize = 4 * 1024 * 1024;

/// Durable CI retained-observation authority mirrored on the project graph DB.
#[derive(Clone)]
pub struct ProjectCiRetainedObservationStoreV1 {
    database: Database,
    scope: FeedbackScopeV1,
}

impl ProjectCiRetainedObservationStoreV1 {
    pub fn new(database: Database, scope: FeedbackScopeV1) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self { database, scope })
    }

    fn key(&self, request: &CiFailureLocalizationRequestV1) -> Option<String> {
        if request.scope != self.scope {
            return None;
        }
        canonical_sha256(&(RETAINED_KEY_DOMAIN_V1, &request.scope, &request.run))
            .ok()
            .map(|digest| format!("{RETAINED_KEY_PREFIX_V1}{}", digest.as_str()))
    }

    fn observation_for(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
        record: &GitHubCiProviderRecordV1,
    ) -> Option<CiRetainedProviderObservationV1> {
        let digest = canonical_sha256(&(
            "tracedecay.pr13.ci.retained-observation.v1",
            &request.scope,
            &request.run,
            record.run_identity(),
        ))
        .ok()?;
        let observation_id = CanonicalObservationIdV1::new(digest.as_str().to_owned()).ok()?;
        let failure_anchor = match record.failed_annotation() {
            Some(annotation) => {
                let anchor_digest = canonical_sha256(&(
                    "tracedecay.pr13.ci.failure-anchor.v1",
                    &annotation.path,
                    annotation.start_line,
                    annotation.end_line,
                    &request.run,
                ))
                .ok()?;
                RetrievalAnchorId::new(format!(
                    "anchor.ci.failure.{}",
                    anchor_digest.as_str().trim_start_matches("sha256:")
                ))
                .ok()?
            }
            None => {
                let anchor_digest = canonical_sha256(&(
                    "tracedecay.pr13.ci.failure-anchor.job.v1",
                    &request.run,
                    record.failed_step().map(|step| step.number),
                ))
                .ok()?;
                RetrievalAnchorId::new(format!(
                    "anchor.ci.failure.{}",
                    anchor_digest.as_str().trim_start_matches("sha256:")
                ))
                .ok()?
            }
        };
        Some(CiRetainedProviderObservationV1 {
            observation_id,
            failure_anchor,
            provider_head_commit_id: request.scope.head_commit_id.clone(),
            failure_kind: CiFailureKindV1::Unknown,
            observed_at: context.grant().issued_at,
        })
    }
}

impl CiRetainedProviderObservationAuthorityV1 for ProjectCiRetainedObservationStoreV1 {
    fn load<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderRecordV1>> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return None;
            }
            let key = self.key(request)?;
            let encoded = self.database.get_metadata(&key).await.ok()??;
            if encoded.len() > MAX_RETAINED_BYTES_V1 {
                return None;
            }
            let record = serde_json::from_str::<CiRetainedProviderRecordV1>(&encoded).ok()?;
            record.validate_for(request).then_some(record)
        })
    }

    fn retain<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        record: &'a GitHubCiProviderRecordV1,
        state: CiFailureLocalizationStateV1,
        coverage: CiFailureCoverageV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderObservationV1>> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return None;
            }
            if !matches!(
                (state, coverage),
                (
                    CiFailureLocalizationStateV1::Complete | CiFailureLocalizationStateV1::Partial,
                    CiFailureCoverageV1::Complete | CiFailureCoverageV1::Partial
                )
            ) {
                return None;
            }
            let observation = self.observation_for(context, request, record)?;
            let retained = CiRetainedProviderRecordV1 {
                provider_record: record.clone(),
                observation: observation.clone(),
            };
            if !retained.validate_for(request) {
                return None;
            }
            let key = self.key(request)?;
            let encoded = serde_json::to_string(&retained).ok()?;
            if encoded.len() > MAX_RETAINED_BYTES_V1 {
                return None;
            }
            self.database.set_metadata(&key, &encoded).await.ok()?;
            Some(observation)
        })
    }
}

/// Graph-backed CI code-anchor resolver over the sealed project index.
#[derive(Clone)]
pub struct ProjectCiCodeAnchorStoreV1 {
    graph: Arc<TraceDecay>,
    scope: FeedbackScopeV1,
}

impl ProjectCiCodeAnchorStoreV1 {
    pub fn new(graph: Arc<TraceDecay>, scope: FeedbackScopeV1) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self { graph, scope })
    }
}

impl CiCodeAnchorStoreV1 for ProjectCiCodeAnchorStoreV1 {
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        record: &'a CiRetainedProviderRecordV1,
    ) -> FeedbackPortFuture<'a, Option<CiExactCodeEvidenceV1>> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) || request.scope != self.scope
                || !record.validate_for(request)
            {
                return None;
            }
            // Exact generation/symbol mapping requires graph occurrence binding.
            // Until annotations resolve to indexed symbols, return authoritative
            // partial evidence rather than inventing IDs.
            let _ = self.graph.as_ref();
            Some(CiExactCodeEvidenceV1 {
                state: CiFailureLocalizationStateV1::Partial,
                coverage: CiFailureCoverageV1::Partial,
                generation: None,
                symbol: None,
                callers: Vec::new(),
                tests: Vec::new(),
            })
        })
    }
}
