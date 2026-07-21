//! Compatibility facade for the canonical Plan 37 advisory contracts.
//!
//! The domain crate owns these wire values. The application crate deliberately
//! re-exports them instead of defining adapter-local GitHub, CI, proximity, or
//! packet shapes that could drift from the shared feedback-cycle result.

pub use tracedecay_domain::feedback::{
    CiCallerRelationV1, CiFailureBranchEvidenceV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRunIdentityV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, CiInertRerunHintV1, CiInertRerunTargetV1,
    FeedbackReferenceCoverageV1, FeedbackReferenceFindingKindV1, FeedbackReferenceFindingV1,
    FeedbackReferencePacketV1, FeedbackReferenceSourceRecordIdV1, FeedbackReferenceSourceStateV1,
    GitHubPullRequestIdV1, GitHubReviewAuthorClassV1, GitHubReviewCommentIdV1,
    GitHubReviewCoverageV1, GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitHubReviewLifecycleV1, GitHubReviewReadOperationV1, GitHubReviewRemapStateV1,
    GitHubReviewStateV1, GitHubReviewThreadIdV1, ProximityAddressV1,
    ProximityBranchWorktreeIncompatibilityV1, ProximityContributionIdV1, ProximityContributionV1,
    ProximityCoverageV1, ProximityInclusionV1, ProximityObservationIdV1,
    ProximityRelationPathKindV1, ProximityRelationPathV1, ProximityRelationStrengthV1,
    ProximityRiskInputsV1, ProximityTierV1, ProximityWarningClassV1, ProximityWarningIdV1,
};
