//! Daemon-ready injected adapters for PR13 advisory application ports.
//!
//! These adapters own no daemon trigger, host transport, GitHub write client,
//! CI runner, scheduler, lock, or agent continuation.

use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::FeedbackScopeV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

pub mod ci;
pub mod ci_runtime;
/// Checked-in provider source captures for the PR13 composite acceptance
/// scenario. Integration tests are a separate crate and can only reach `pub`
/// items, so the opt-in `test-transport` feature keeps these captures out of
/// the default production surface without weakening them.
#[cfg(any(test, feature = "test-transport"))]
pub mod fixtures;
pub mod github;
pub mod github_runtime;
pub mod host_delivery;
pub mod production;
pub mod proximity_runtime;
pub mod runtime;

pub use ci::{CiFailureLocalizationAdapter, CiReadOnlyEvidenceSource};
pub use ci_runtime::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiExactEvidenceAuthorityV1, CiProviderReadResultV1,
    CiReadOnlyProviderArchiveV1, CiRetainedProviderObservationAuthorityV1,
    CiRetainedProviderObservationV1, CiRetainedProviderRecordV1, CiSourceAccessAuthorityV1,
    CiSourceAccessOutcomeV1, ConcreteCiFailureLocalizationOwnerV1,
    DaemonCiReadOnlyEvidenceSourceV1, GitHubCiAnnotationLevelV1, GitHubCiCheckAnnotationV1,
    GitHubCiCheckRunV1, GitHubCiCheckSuiteRefV1, GitHubCiOfficialResponseDecoderV1,
    GitHubCiProviderRecordV1, GitHubCiPullRequestRefV1, GitHubCiWorkflowRunV1,
    MAX_CI_RETAINED_ANNOTATIONS_V1, MAX_CI_RETAINED_CHECKS_V1, MAX_CI_RETAINED_FAILURES_V1,
    ProductionCiArchiveHandleV1, ProductionCiExactEvidenceHandleV1,
    ProductionCiFailureDiscoveryOutcomeV1, ProductionCiProviderAuthoritiesV1,
    ProductionCiProviderConfigV1, ProductionCiProviderOpenErrorV1, ProjectCiCodeAnchorStoreV1,
    ProjectCiRetainedObservationStoreV1, concrete_ci_failure_localization_owner_v1,
    discover_production_ci_failure_request_v1, open_production_ci_provider_authorities_v1,
    unavailable_production_ci_provider_authorities_v1,
};
#[cfg(any(test, feature = "test-transport"))]
pub use fixtures::{
    PR13_CHECK_ANNOTATIONS_FIXTURE_V1, PR13_CHECK_RUN_FIXTURE_V1, PR13_FIXTURE_ROOT_V1,
    PR13_PROXIMITY_SESSIONS_FIXTURE_V1, PR13_PULL_REQUEST_FIXTURE_V1,
    PR13_REVIEW_COMMENT_FIXTURE_V1, PR13_REVIEW_FIXTURE_V1, PR13_REVIEW_THREAD_FIXTURE_V1,
    PR13_SCENARIO_FIXTURE_V1, PR13_WORKFLOW_JOB_FIXTURE_V1, PR13_WORKFLOW_RUN_FIXTURE_V1,
    Pr13CiFixtureEvidenceV1, Pr13CiFixtureV1, Pr13GitHubFixtureAnchorsV1,
    Pr13GitHubReviewFixtureV1, Pr13ProximityFixtureEvidenceV1, Pr13ProximityFixtureV1,
};
pub use github::{
    GitHubCurrentBranchRemapper, GitHubReadOnlyAdmissionError, GitHubReadOnlyConnector,
    GitHubReadOnlyDescriptorSetV1, GitHubReadOnlyTransport, GitHubRestDescriptorV1,
};
pub use github_runtime::{
    GITHUB_REVIEW_THREADS_QUERY_V1, GitHubCanonicalReviewAnchorAuthorityV1,
    GitHubCanonicalReviewAnchorsV1, GitHubCiRepositoryTargetV1, GitHubCiTransportOutcomeV1,
    GitHubGraphQlReadRequestV1, GitHubHttpReadConfigV1, GitHubOfficialResponseDecoderV1,
    GitHubProviderLifecycleV1, GitHubReadCheckpointAuthorityV1, GitHubReadCheckpointLoadOutcomeV1,
    GitHubReadNetworkMetadataV1, GitHubReadNetworkOutcomeV1, GitHubReadNetworkResponseV1,
    GitHubReadNetworkStatusV1, GitHubReadOnlyClientV1, GitHubReadOnlyCredentialAuthorityOutcomeV1,
    GitHubReadOnlyCredentialAuthorityV1, GitHubReadOnlyCredentialSecretV1,
    GitHubReadOnlyCredentialV1, GitHubReadOnlyNetworkAuthorityV1, GitHubReadOnlyRuntimeTransportV1,
    GitHubReadPermissionV1, GitHubReadResponseDecoderV1, GitHubReadResumeV1,
    GitHubRepositoryTargetV1, GitHubRestReadRequestV1, GitHubReviewAnchorSeedV1,
    GitHubReviewAtomicRefreshStoreV1, GitHubReviewBodyEvidenceAuthorityV1,
    GitHubReviewBodyEvidenceV1, GitHubReviewBodyReadOutcomeV1, GitHubReviewCompleteGenerationV1,
    GitHubReviewProviderIdentityV1, GitHubReviewRefreshCoordinatorV1, GitHubReviewRefreshOutcomeV1,
    GitHubReviewRefreshReceiptV1, GitHubReviewRefreshStateV1,
    GitHubReviewRefreshStoreCommitOutcomeV1, GitHubReviewRefreshStoreReadOutcomeV1,
    GitHubReviewRuntimeOwnerBuildErrorV1, GitHubReviewRuntimeOwnerConfigV1,
    GitHubReviewRuntimeOwnerV1, MAX_GITHUB_READ_RESPONSE_BYTES_V1, ProjectGitHubAnchorAuthorityV1,
    ProjectGitHubRegistrarAuthoritiesV1, ProjectGitHubReviewStoreV1,
    build_github_review_runtime_owner_v1, github_anchor_authorities_arc_v1,
    github_anchor_authorities_v1, register_github_read_only_credential_authority_v1,
    register_profile_github_read_only_credential_authority_v1,
    unregister_github_read_only_credential_authority_v1,
    unregister_profile_github_read_only_credential_authority_v1,
};
pub use host_delivery::{
    Pr13AdvisoryCompletedDeliveryV1, Pr13AdvisoryDaemonStartupErrorV1,
    Pr13AdvisoryDaemonStartupRegistrationV1, Pr13AdvisoryHookDeliveryPortV1,
    Pr13AdvisoryHookDeliveryV1, Pr13AdvisoryHookLookupNoticeV1, Pr13AdvisoryHookNoticeQueueV1,
    Pr13AdvisoryHookNoticeSinkV1, Pr13AdvisoryHostDeliveryErrorV1, Pr13AdvisoryHostDeliveryPathV1,
    Pr13AdvisoryHostDeliveryRegistrationV1, Pr13AdvisoryHostDeliveryRouteV1,
    Pr13AdvisoryRunErrorV1, Pr13AdvisoryRunResultV1, mount_pr13_advisory_host_delivery,
    new_pr13_advisory_hook_delivery_port, register_pr13_advisory_daemon_startup,
};
pub use host_delivery::{
    acknowledge_pr13_advisory_hook_notice, peek_pr13_advisory_hook_notice,
    register_pr13_advisory_hook_notice_queue,
};
pub use production::{
    Pr13AdvisoryProductionAuthoritiesV1, Pr13AdvisoryProductionHookDeliveryPortV1,
    Pr13AdvisoryProductionOpenErrorV1, Pr13AdvisoryProductionOpenV1,
    Pr13AdvisoryProductionProviderAuthoritiesV1, Pr13AdvisoryProductionStartupRegistrationV1,
    open_pr13_advisory_production_authorities,
};
pub use proximity_runtime::{
    CanonicalProximityEvidenceAuthorityV1, CanonicalProximityEvidenceBatchV1,
    CanonicalProximityEvidenceV1, ConcretePr13ProximityRuntimeOwnerV1,
    Pr13ProximityFindingContributorV1, Pr13ProximityRuntimeOutcomeV1, Pr13ProximityRuntimeOwnerV1,
    ProductionProximityEvidenceAuthorityV1, ProximityThresholdPinV1,
    SharedCanonicalProximityEvidenceAuthorityV1, open_pr13_proximity_runtime,
};
pub use runtime::{
    AdvisoryCycleControl, AdvisoryCycleOutcome, AdvisoryCycleRequest, Pr13AdvisoryContributionsV1,
    Pr13AdvisoryDaemonRegistrationV1, Pr13AdvisoryProviderAuthoritiesV1,
    Pr13AdvisoryProviderStateV1, Pr13AdvisoryProviderV1, Pr13AdvisoryRuntime,
    Pr13AdvisoryRuntimeOpenErrorV1, Pr13AdvisoryRuntimeOpenV1,
    open_pr13_advisory_daemon_registration,
};

/// Every adapter preserves the already-admitted project/repository/worktree/
/// branch root. A path or mutable label never substitutes for this comparison.
pub(crate) fn context_matches_scope(context: &RequestContext, scope: &FeedbackScopeV1) -> bool {
    let actual = context.scope();
    context.validate().is_ok()
        && actual.project_id == scope.project_id
        && actual.repository_id == scope.repository_id
        && actual.worktree_id == scope.worktree_id
        && actual
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(scope.branch_ref.as_str())
}

pub(crate) fn context_allows_feedback_operation(
    context: &RequestContext,
    scope: &FeedbackScopeV1,
    capability: &str,
    use_case: &str,
) -> bool {
    let (Ok(capability), Ok(use_case)) = (CapabilityId::new(capability), UseCaseId::new(use_case))
    else {
        return false;
    };
    context_matches_scope(context, scope)
        && context.admission_at(now_micros()) == RequestAdmission::Admitted
        && context.allows(&capability, &use_case)
}
