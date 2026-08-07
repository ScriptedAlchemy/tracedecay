//! One-shot composition root for advisory providers.
//!
//! Provider records retain their own provenance and coverage. This owner
//! projects canonical anchored findings into the existing Plan 09 cycle and
//! durable feedback publication store without another packet, ledger, or loop.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;
use tracedecay_application::feedback::{
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, FeedbackCompletedPublicationV1,
    FeedbackCycleAdvisoryV1, FeedbackCycleExecutionRequest, GitHubReviewReadRequestV1,
    ProximityEvaluationRequestV1,
};
use tracedecay_application::{
    AdvisoryFindingContributionBatchV1, AdvisoryFindingContributorV1,
    AdvisoryFindingValidityWindowV1, ApplicationContractError, RequestContext, ResolvedScope,
};
use tracedecay_domain::feedback::{
    CiFailureCoverageV1, CiFailureLocalizationResultV1, CiFailureLocalizationStateV1,
    FeedbackFindingV1, FeedbackScopeV1, GitHubReviewIngressProviderOutcomeV1,
    GitHubReviewIngressResultV1, GitHubReviewLifecycleV1, ProviderEvaluationStateV1,
    ProximityInclusionV1,
};

use crate::configuration::ConfigurationControlStore;
use crate::context::MonotonicDeadline;
use crate::feedback::concrete::{ConcreteFeedbackOwner, ProjectFeedbackStore};
use crate::feedback::cycle_runtime::{CanonicalFeedbackResultV1, FeedbackCycleRuntime};
use crate::feedback::observations::{
    FeedbackAdvisoryProviderV1, FeedbackCiProviderV1, FeedbackCoverageV1,
    FeedbackGitHubLifecycleV1, FeedbackObservationEmitterV1, FeedbackOperationV1,
    FeedbackOutcomeV1, FeedbackProximityRiskV1, FeedbackProximityTransitionV1,
    FeedbackSourceEventV1,
};
use crate::operation_stream::OperationEmitter;
use tracedecay_runtime_core::db::Database;

use super::ci_runtime::{
    CiExactEvidenceAuthorityV1, CiReadOnlyProviderArchiveV1, ConcreteCiFailureLocalizationOwnerV1,
    ProductionCiFailureDiscoveryOutcomeV1,
};
use super::github_runtime::GitHubSourceAccessAuthorityV1;
use super::proximity_runtime::{ConcreteProximityRuntimeOwnerV1, ProximityRuntimeOutcomeV1};
use super::{
    CanonicalProximityEvidenceAuthorityV1, GitHubCanonicalReviewAnchorAuthorityV1,
    GitHubCurrentBranchRemapper, GitHubReviewRefreshOutcomeV1, GitHubReviewRuntimeOwnerConfigV1,
    GitHubReviewRuntimeOwnerV1, build_github_review_runtime_owner_v1,
    concrete_ci_failure_localization_owner_v1, context_matches_scope, open_proximity_runtime,
};

mod cycle;
mod model;
mod registration;

pub use cycle::AdvisoryRuntime;
pub use model::{
    AdvisoryContributionsV1, AdvisoryCycleControl, AdvisoryCycleOutcome, AdvisoryCycleRequest,
    AdvisoryProviderAuthoritiesV1, AdvisoryProviderStateV1, AdvisoryProviderV1,
    AdvisoryRuntimeOpenErrorV1, AdvisoryRuntimeOpenV1,
};
pub use registration::{AdvisoryDaemonRegistrationV1, open_advisory_daemon_registration};

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;
