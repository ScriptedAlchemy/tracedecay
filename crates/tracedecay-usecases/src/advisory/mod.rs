//! Daemon-ready injected adapters for advisory application ports.
//!
//! These adapters own no daemon trigger, host transport, GitHub write client,
//! CI runner, scheduler, lock, or agent continuation.

use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::FeedbackScopeV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

pub mod ci;
pub mod ci_runtime;
/// Checked-in provider source captures for the composite advisory acceptance
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
    CiReadOnlyProviderArchiveV1, CiRetainedObservationManifestEntryV1,
    CiRetainedObservationManifestLoadOutcomeV1, CiRetainedObservationManifestV1,
    CiRetainedProviderObservationAuthorityV1, CiRetainedProviderObservationV1,
    CiRetainedProviderRecordV1, CiSourceAccessAuthorityV1, CiSourceAccessOutcomeV1,
    ConcreteCiFailureLocalizationOwnerV1, DaemonCiReadOnlyEvidenceSourceV1,
    GitHubCiAnnotationLevelV1, GitHubCiCheckAnnotationV1, GitHubCiCheckRunV1,
    GitHubCiCheckSuiteRefV1, GitHubCiOfficialResponseDecoderV1, GitHubCiProviderRecordV1,
    GitHubCiPullRequestRefV1, GitHubCiWorkflowRunV1, MAX_CI_RETAINED_ANNOTATIONS_V1,
    MAX_CI_RETAINED_CHECKS_V1, MAX_CI_RETAINED_FAILURES_V1,
    MAX_CI_RETAINED_OBSERVATION_MANIFEST_ENTRIES_V1, ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1, ProductionCiFailureDiscoveryOutcomeV1,
    ProductionCiProviderAuthoritiesV1, ProductionCiProviderConfigV1,
    ProductionCiProviderOpenErrorV1, ProjectCiCodeAnchorStoreV1,
    ProjectCiRetainedObservationStoreV1, concrete_ci_failure_localization_owner_v1,
    discover_production_ci_failure_request_v1, open_production_ci_provider_authorities_v1,
    unavailable_production_ci_provider_authorities_v1,
};
#[cfg(any(test, feature = "test-transport"))]
pub use fixtures::{
    ADVISORY_CHECK_ANNOTATIONS_FIXTURE_V1, ADVISORY_CHECK_RUN_FIXTURE_V1, ADVISORY_FIXTURE_ROOT_V1,
    ADVISORY_PROXIMITY_SESSIONS_FIXTURE_V1, ADVISORY_PULL_REQUEST_FIXTURE_V1,
    ADVISORY_REVIEW_COMMENT_FIXTURE_V1, ADVISORY_REVIEW_FIXTURE_V1,
    ADVISORY_REVIEW_THREAD_FIXTURE_V1, ADVISORY_SCENARIO_FIXTURE_V1,
    ADVISORY_WORKFLOW_JOB_FIXTURE_V1, ADVISORY_WORKFLOW_RUN_FIXTURE_V1,
    AdvisoryCiFixtureEvidenceV1, AdvisoryCiFixtureV1, AdvisoryGitHubFixtureAnchorsV1,
    AdvisoryGitHubReviewFixtureV1, AdvisoryProximityFixtureEvidenceV1, AdvisoryProximityFixtureV1,
};
pub use github::{
    GitHubCurrentBranchRemapper, GitHubReadOnlyAdmissionError, GitHubReadOnlyConnector,
    GitHubReadOnlyDescriptorSetV1, GitHubReadOnlyTransport, GitHubRestDescriptorV1,
};
pub use github_runtime::{
    GITHUB_REVIEW_THREADS_QUERY_V1, GitHubActionsConclusionV1, GitHubActionsStatusV1,
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1,
    GitHubCiRepositoryTargetV1, GitHubCiTransportOutcomeV1, GitHubGraphQlReadRequestV1,
    GitHubHttpReadConfigV1, GitHubOfficialResponseDecoderV1, GitHubProviderLifecycleV1,
    GitHubReadCheckpointAuthorityV1, GitHubReadCheckpointLoadOutcomeV1,
    GitHubReadNetworkMetadataV1, GitHubReadNetworkOutcomeV1, GitHubReadNetworkResponseV1,
    GitHubReadNetworkStatusV1, GitHubReadOnlyClientV1, GitHubReadOnlyCredentialAuthorityOutcomeV1,
    GitHubReadOnlyCredentialAuthorityV1, GitHubReadOnlyCredentialSecretV1,
    GitHubReadOnlyCredentialV1, GitHubReadOnlyNetworkAuthorityV1, GitHubReadOnlyRuntimeTransportV1,
    GitHubReadPermissionV1, GitHubReadResponseDecoderV1, GitHubReadResumeV1, GitHubReleaseAssetV1,
    GitHubReleaseReadControlV1, GitHubReleaseTagV1, GitHubReleaseV1, GitHubRepositoryTargetV1,
    GitHubRestReadRequestV1, GitHubReviewAnchorSeedV1, GitHubReviewAtomicRefreshStoreV1,
    GitHubReviewBodyEvidenceAuthorityV1, GitHubReviewBodyEvidenceV1, GitHubReviewBodyReadOutcomeV1,
    GitHubReviewCompleteGenerationV1, GitHubReviewProviderIdentityV1,
    GitHubReviewRefreshCoordinatorV1, GitHubReviewRefreshOutcomeV1, GitHubReviewRefreshReceiptV1,
    GitHubReviewRefreshStateV1, GitHubReviewRefreshStoreCommitOutcomeV1,
    GitHubReviewRefreshStoreReadOutcomeV1, GitHubReviewRuntimeOwnerBuildErrorV1,
    GitHubReviewRuntimeOwnerConfigV1, GitHubReviewRuntimeOwnerV1, GitHubReviewStoreManifestEntryV1,
    GitHubReviewStoreManifestLoadOutcomeV1, GitHubReviewStoreManifestV1,
    GitHubStackObservabilityV1, MAX_GITHUB_READ_RESPONSE_BYTES_V1,
    MAX_GITHUB_REVIEW_STORE_MANIFEST_ENTRIES_V1, ProjectGitHubAnchorAuthorityV1,
    ProjectGitHubRegistrarAuthoritiesV1, ProjectGitHubReleaseAuthorityOpenOutcomeV1,
    ProjectGitHubReleasePageV1, ProjectGitHubReleaseReadAuthorityV1,
    ProjectGitHubReleaseReadOutcomeV1, ProjectGitHubReleaseReadRequestV1,
    ProjectGitHubReviewStoreV1, build_github_review_runtime_owner_v1,
    github_anchor_authorities_arc_v1, github_anchor_authorities_v1,
    open_project_github_release_read_authority_v1,
    register_github_read_only_credential_authority_v1,
    register_profile_github_read_only_credential_authority_v1,
    unregister_github_read_only_credential_authority_v1,
    unregister_profile_github_read_only_credential_authority_v1,
};
pub use host_delivery::{
    AdvisoryCompletedDeliveryV1, AdvisoryDaemonStartupErrorV1, AdvisoryDaemonStartupRegistrationV1,
    AdvisoryHookDeliveryPortV1, AdvisoryHookDeliveryV1, AdvisoryHookLookupNoticeV1,
    AdvisoryHookNoticeQueueV1, AdvisoryHookNoticeSinkV1, AdvisoryHostDeliveryErrorV1,
    AdvisoryHostDeliveryPathV1, AdvisoryHostDeliveryRegistrationV1, AdvisoryHostDeliveryRouteV1,
    mount_advisory_host_delivery, new_advisory_hook_delivery_port,
    register_advisory_daemon_startup,
};
pub use host_delivery::{
    acknowledge_advisory_hook_notice, peek_advisory_hook_notice,
    register_advisory_hook_notice_queue, unregister_advisory_hook_notice_queue,
};
pub use production::{
    AdvisoryProductionAuthoritiesV1, AdvisoryProductionHookDeliveryPortV1,
    AdvisoryProductionOpenErrorV1, AdvisoryProductionOpenV1,
    AdvisoryProductionProviderAuthoritiesV1, AdvisoryProductionStartupRegistrationV1,
    open_advisory_production_authorities,
};
pub use proximity_runtime::{
    CanonicalProximityEvidenceAuthorityV1, CanonicalProximityEvidenceBatchV1,
    CanonicalProximityEvidenceV1, ConcreteProximityRuntimeOwnerV1,
    ProductionProximityEvidenceAuthorityV1, ProximityFindingContributorV1,
    ProximityRuntimeOutcomeV1, ProximityRuntimeOwnerV1, ProximityThresholdPinV1,
    SharedCanonicalProximityEvidenceAuthorityV1, open_proximity_runtime,
};
pub use runtime::{
    AdvisoryContributionsV1, AdvisoryCycleControl, AdvisoryCycleOutcome, AdvisoryCycleRequest,
    AdvisoryDaemonRegistrationV1, AdvisoryProviderAuthoritiesV1, AdvisoryProviderStateV1,
    AdvisoryProviderV1, AdvisoryRuntime, AdvisoryRuntimeOpenErrorV1, AdvisoryRuntimeOpenV1,
    open_advisory_daemon_registration,
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
