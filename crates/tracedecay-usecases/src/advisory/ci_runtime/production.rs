use std::sync::Arc;

use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationRequestV1, FeedbackPortFuture,
};
use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::{
    CiFailureBranchEvidenceV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRateLimitCheckpointV1,
    CiFailureRunIdentityV1, CiFailureSourceDegradationV1, CiFailureSourceFailureV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, FeedbackScopeV1,
    MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_TEST_EVIDENCE_V1,
};
use tracedecay_domain::{
    CanonicalObservationIdV1, CommitId, ProviderId, RetrievalAnchorId, UtcMicros,
};

use super::super::context_allows_feedback_operation;
use super::super::github_runtime::{
    GitHubActionsCheckRunV1, GitHubActionsConclusionV1, GitHubActionsStatusV1,
    GitHubActionsWorkflowJobV1, GitHubActionsWorkflowRunV1, GitHubActionsWorkflowStepV1,
    GitHubCheckAnnotationV1, GitHubCiReadOnlyClientV1, GitHubCiRepositoryTargetV1,
    GitHubCiTransportOutcomeV1,
};
#[cfg(test)]
use super::super::github_runtime::{
    GitHubHttpReadClientV1, GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1,
};
use super::{
    CiExactEvidenceAuthorityV1, CiProviderReadResultV1, CiReadOnlyProviderArchiveV1,
    CiSourceAccessAuthorityV1, CiSourceAccessOutcomeV1, GitHubCiProviderRecordV1,
    MAX_CI_RETAINED_ANNOTATIONS_V1, MAX_CI_RETAINED_FAILURES_V1,
};

mod discovery;
mod provider;

pub use discovery::{
    ProductionCiFailureDiscoveryOutcomeV1, ProductionCiProviderConfigV1,
    discover_production_ci_failure_request_v1,
};
pub use provider::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiRetainedProviderObservationAuthorityV1,
    CiRetainedProviderObservationV1, CiRetainedProviderRecordV1, ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1, ProductionCiProviderAuthoritiesV1,
    ProductionCiProviderOpenErrorV1, open_production_ci_provider_authorities_v1,
    unavailable_production_ci_provider_authorities_v1,
};

#[cfg(test)]
#[path = "production/tests/concurrency.rs"]
mod concurrency_tests;
#[cfg(test)]
#[path = "production/tests/discovery.rs"]
mod discovery_tests;
