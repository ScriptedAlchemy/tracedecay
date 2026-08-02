//! One-shot composition root for PR13 advisory providers.
//!
//! Provider records retain their own provenance and coverage. This owner
//! projects canonical anchored findings into the existing Plan 09 cycle and
//! PR12 durable publication store without another packet, ledger, or loop.

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
use crate::feedback::concrete::{ConcretePr12FeedbackOwner, ProjectFeedbackStore};
use crate::feedback::cycle_runtime::{Pr12CanonicalFeedbackResultV1, Pr12FeedbackCycleRuntime};
use crate::feedback::observations::{
    Plan26AdvisoryProviderV1, Plan26CiProviderV1, Plan26CoverageV1,
    Plan26FeedbackObservationEmitterV1, Plan26FeedbackOperationV1, Plan26FeedbackOutcomeV1,
    Plan26FeedbackSourceEventV1, Plan26GitHubLifecycleV1, Plan26ProximityRiskV1,
    Plan26ProximityTransitionV1,
};
use crate::operation_stream::OperationEmitter;
use tracedecay_runtime_core::db::Database;

use super::ci_runtime::{
    CiExactEvidenceAuthorityV1, CiReadOnlyProviderArchiveV1, ConcreteCiFailureLocalizationOwnerV1,
    ProductionCiFailureDiscoveryOutcomeV1,
};
use super::github_runtime::GitHubSourceAccessAuthorityV1;
use super::proximity_runtime::{
    ConcretePr13ProximityRuntimeOwnerV1, Pr13ProximityRuntimeOutcomeV1,
};
use super::{
    CanonicalProximityEvidenceAuthorityV1, GitHubCanonicalReviewAnchorAuthorityV1,
    GitHubCurrentBranchRemapper, GitHubReviewRefreshOutcomeV1, GitHubReviewRuntimeOwnerConfigV1,
    GitHubReviewRuntimeOwnerV1, build_github_review_runtime_owner_v1,
    concrete_ci_failure_localization_owner_v1, context_matches_scope, open_pr13_proximity_runtime,
};

mod cycle;
mod model;
mod registration;

pub use cycle::Pr13AdvisoryRuntime;
pub use model::{
    Pr13AdvisoryContributionsV1, Pr13AdvisoryCycleControlV1, Pr13AdvisoryCycleOutcomeV1,
    Pr13AdvisoryCycleRequestV1, Pr13AdvisoryProviderAuthoritiesV1, Pr13AdvisoryProviderStateV1,
    Pr13AdvisoryProviderV1, Pr13AdvisoryRuntimeOpenErrorV1, Pr13AdvisoryRuntimeOpenV1,
};
pub use registration::{Pr13AdvisoryDaemonRegistrationV1, open_pr13_advisory_daemon_registration};

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;
