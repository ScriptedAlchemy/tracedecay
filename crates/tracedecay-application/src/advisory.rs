//! Canonical Plan 09 advisory finding adapters.
//!
//! GitHub, CI, and proximity sources retain provenance and coverage in their
//! owning records. This facade projects only canonical findings with durable
//! retrieval anchors; it creates no parallel reference packet or identity.

use tracedecay_domain::feedback::{
    FeedbackDiagnosticClassificationV1, FeedbackDiagnosticProducerV1,
    FeedbackDiagnosticProjectionV1, FeedbackFindingLifecycleV1, FeedbackFindingV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{DiagnosticSeverityV1, UtcMicros};

use crate::ApplicationContractError;

pub use tracedecay_domain::feedback::{
    CiCallerRelationV1, CiFailureBranchEvidenceV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRunIdentityV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, CiInertRerunHintV1, CiInertRerunTargetV1,
    GitHubPullRequestIdV1, GitHubReviewAuthorClassV1, GitHubReviewCommentIdV1,
    GitHubReviewCoverageV1, GitHubReviewCurrentBranchRemapV1, GitHubReviewCursorV1,
    GitHubReviewEtagV1, GitHubReviewIdV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitHubReviewLifecycleV1, GitHubReviewRateLimitCheckpointV1, GitHubReviewReadCheckpointV1,
    GitHubReviewReadOperationV1, GitHubReviewRemapStateV1, GitHubReviewStateV1,
    GitHubReviewThreadIdV1, MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_RERUN_HINTS_V1,
    MAX_CI_FAILURE_TEST_EVIDENCE_V1, PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1, ProximityAddressV1,
    ProximityBranchWorktreeIncompatibilityV1, ProximityContributionIdV1, ProximityContributionV1,
    ProximityCoverageV1, ProximityInclusionV1, ProximityObservationIdV1,
    ProximityRelationPathKindV1, ProximityRelationPathV1, ProximityRelationStrengthV1,
    ProximityRiskInputsV1, ProximityTierV1, ProximityWarningClassV1, ProximityWarningIdV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvisoryFindingValidityWindowV1 {
    pub valid_at: UtcMicros,
    pub expires_at: UtcMicros,
}

impl AdvisoryFindingValidityWindowV1 {
    fn validate_for(self, observed_at: UtcMicros) -> Result<(), ApplicationContractError> {
        if observed_at.0 > self.valid_at.0 || self.valid_at.0 >= self.expires_at.0 {
            return Err(inconsistent("advisory finding validity window"));
        }
        Ok(())
    }
}

/// Canonical findings plus the source's existing Plan 09 provider state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryFindingContributionBatchV1 {
    pub provider_state: ProviderEvaluationStateV1,
    pub findings: Vec<FeedbackFindingV1>,
}

impl AdvisoryFindingContributionBatchV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for finding in &self.findings {
            finding
                .validate()
                .map_err(|_| inconsistent("advisory finding"))?;
            if finding.provider_state != self.provider_state {
                return Err(inconsistent("advisory finding provider state"));
            }
        }
        if self.findings.iter().enumerate().any(|(index, finding)| {
            self.findings[index.saturating_add(1)..]
                .iter()
                .any(|other| other.finding_id == finding.finding_id)
        }) {
            return Err(inconsistent("duplicate advisory finding"));
        }
        Ok(())
    }
}

pub trait AdvisoryFindingContributorV1 {
    fn advisory_findings(
        &self,
        window: AdvisoryFindingValidityWindowV1,
    ) -> Result<AdvisoryFindingContributionBatchV1, ApplicationContractError>;
}

impl AdvisoryFindingContributorV1 for GitHubReviewIngressResultV1 {
    fn advisory_findings(
        &self,
        window: AdvisoryFindingValidityWindowV1,
    ) -> Result<AdvisoryFindingContributionBatchV1, ApplicationContractError> {
        self.validate()
            .map_err(|_| inconsistent("github review contribution source"))?;
        let provider_state = github_provider_state(self.outcome);
        let mut findings = Vec::with_capacity(self.items.len());
        for item in &self.items {
            window.validate_for(item.observed_at)?;
            let finding_id = tracedecay_domain::feedback::FeedbackFindingId::new(format!(
                "finding.github-review.{}",
                item.comment_id.as_str()
            ))
            .map_err(|_| inconsistent("github review finding id"))?;
            let lifecycle = match item.lifecycle {
                GitHubReviewLifecycleV1::Current => FeedbackFindingLifecycleV1::Active,
                GitHubReviewLifecycleV1::Outdated | GitHubReviewLifecycleV1::Edited => {
                    FeedbackFindingLifecycleV1::Superseded
                }
                GitHubReviewLifecycleV1::Resolved => FeedbackFindingLifecycleV1::Resolved,
                GitHubReviewLifecycleV1::Deleted => FeedbackFindingLifecycleV1::Cleared,
            };
            findings.push(FeedbackFindingV1 {
                finding_id,
                classification: FeedbackDiagnosticClassificationV1::Unknown,
                lifecycle,
                retrieval_anchor_id: Some(item.body_anchor.clone()),
                provider_state,
                safe_bounded_preview: None,
                diagnostic_projection: (lifecycle == FeedbackFindingLifecycleV1::Active
                    && item.remap.state == GitHubReviewRemapStateV1::ExactCurrent)
                    .then_some(item.remap.current.as_ref())
                    .flatten()
                    .and_then(|current| {
                        Some(FeedbackDiagnosticProjectionV1 {
                            file: current.file.clone(),
                            span: current.span?,
                            symbol: current.symbol.clone(),
                            code: "github-review".to_owned(),
                            severity: DiagnosticSeverityV1::Information,
                            safe_bounded_message: "Unresolved GitHub review comment".to_owned(),
                            producer: FeedbackDiagnosticProducerV1::GitHubReview,
                            code_description_uri: item.safe_url.clone(),
                        })
                    }),
            });
        }
        validated_batch(provider_state, findings)
    }
}

impl AdvisoryFindingContributorV1 for CiFailureLocalizationResultV1 {
    fn advisory_findings(
        &self,
        window: AdvisoryFindingValidityWindowV1,
    ) -> Result<AdvisoryFindingContributionBatchV1, ApplicationContractError> {
        self.validate()
            .map_err(|_| inconsistent("ci localization contribution source"))?;
        window.validate_for(self.observed_at)?;
        let provider_state = ci_provider_state(self.state);
        let source_record = format!("{}.{}", self.run.check_run_id, self.run.attempt_id);
        let finding_id = tracedecay_domain::feedback::FeedbackFindingId::new(format!(
            "finding.ci-localization.{source_record}"
        ))
        .map_err(|_| inconsistent("ci localization finding id"))?;
        let lifecycle = match self.state {
            CiFailureLocalizationStateV1::Complete | CiFailureLocalizationStateV1::Partial => {
                FeedbackFindingLifecycleV1::Active
            }
            CiFailureLocalizationStateV1::Stale | CiFailureLocalizationStateV1::Failed => {
                FeedbackFindingLifecycleV1::Superseded
            }
            CiFailureLocalizationStateV1::Unavailable | CiFailureLocalizationStateV1::Denied => {
                FeedbackFindingLifecycleV1::Cleared
            }
        };
        validated_batch(
            provider_state,
            vec![FeedbackFindingV1 {
                finding_id,
                classification: FeedbackDiagnosticClassificationV1::Unknown,
                lifecycle,
                retrieval_anchor_id: Some(self.failure_anchor.clone()),
                provider_state,
                safe_bounded_preview: None,
                diagnostic_projection: (lifecycle == FeedbackFindingLifecycleV1::Active)
                    .then_some(self.symbol.as_ref())
                    .flatten()
                    .map(|symbol| FeedbackDiagnosticProjectionV1 {
                        file: symbol.file.clone(),
                        span: symbol.span,
                        symbol: Some(symbol.symbol.clone()),
                        code: "ci-failure".to_owned(),
                        severity: DiagnosticSeverityV1::Error,
                        safe_bounded_message: "CI failure localized to this symbol".to_owned(),
                        producer: FeedbackDiagnosticProducerV1::CiLocalization,
                        code_description_uri: None,
                    }),
            }],
        )
    }
}

impl AdvisoryFindingContributorV1 for ProximityContributionV1 {
    fn advisory_findings(
        &self,
        window: AdvisoryFindingValidityWindowV1,
    ) -> Result<AdvisoryFindingContributionBatchV1, ApplicationContractError> {
        self.validate()
            .map_err(|_| inconsistent("proximity contribution source"))?;
        let provider_state = proximity_provider_state(self.coverage);
        if self.inclusion != ProximityInclusionV1::Included {
            return validated_batch(provider_state, Vec::new());
        }
        window.validate_for(self.observed_at)?;
        if window.valid_at.0 >= window.expires_at.0.min(self.expires_at.0) {
            return Err(inconsistent("proximity contribution expiry"));
        }
        let retrieval_anchor_id = self
            .retrieval_anchor_ids
            .first()
            .cloned()
            .ok_or_else(|| inconsistent("proximity contribution anchor"))?;
        let finding_id = tracedecay_domain::feedback::FeedbackFindingId::new(format!(
            "finding.proximity.{}",
            self.warning_id.as_str()
        ))
        .map_err(|_| inconsistent("proximity finding id"))?;
        validated_batch(
            provider_state,
            vec![FeedbackFindingV1 {
                finding_id,
                classification: FeedbackDiagnosticClassificationV1::Unknown,
                lifecycle: FeedbackFindingLifecycleV1::Active,
                retrieval_anchor_id: Some(retrieval_anchor_id),
                provider_state,
                safe_bounded_preview: None,
                diagnostic_projection: self.address.as_ref().and_then(|address| {
                    Some(FeedbackDiagnosticProjectionV1 {
                        file: address.file.clone(),
                        span: address.span?,
                        symbol: address.symbol.clone(),
                        code: "agent-proximity".to_owned(),
                        severity: DiagnosticSeverityV1::Warning,
                        safe_bounded_message: "Concurrent agent activity overlaps this code"
                            .to_owned(),
                        producer: FeedbackDiagnosticProducerV1::Proximity,
                        code_description_uri: None,
                    })
                }),
            }],
        )
    }
}

fn validated_batch(
    provider_state: ProviderEvaluationStateV1,
    findings: Vec<FeedbackFindingV1>,
) -> Result<AdvisoryFindingContributionBatchV1, ApplicationContractError> {
    let batch = AdvisoryFindingContributionBatchV1 {
        provider_state,
        findings,
    };
    batch.validate()?;
    Ok(batch)
}

const fn github_provider_state(
    outcome: GitHubReviewIngressProviderOutcomeV1,
) -> ProviderEvaluationStateV1 {
    match outcome {
        GitHubReviewIngressProviderOutcomeV1::Complete => {
            ProviderEvaluationStateV1::SupportedCompletedComplete
        }
        GitHubReviewIngressProviderOutcomeV1::Partial
        | GitHubReviewIngressProviderOutcomeV1::RateLimited => ProviderEvaluationStateV1::Partial,
        GitHubReviewIngressProviderOutcomeV1::Stale => ProviderEvaluationStateV1::Stale,
        GitHubReviewIngressProviderOutcomeV1::Failed => ProviderEvaluationStateV1::Failed,
        GitHubReviewIngressProviderOutcomeV1::Unavailable
        | GitHubReviewIngressProviderOutcomeV1::Denied => ProviderEvaluationStateV1::Unavailable,
    }
}

const fn ci_provider_state(state: CiFailureLocalizationStateV1) -> ProviderEvaluationStateV1 {
    match state {
        CiFailureLocalizationStateV1::Complete => {
            ProviderEvaluationStateV1::SupportedCompletedComplete
        }
        CiFailureLocalizationStateV1::Partial => ProviderEvaluationStateV1::Partial,
        CiFailureLocalizationStateV1::Stale => ProviderEvaluationStateV1::Stale,
        CiFailureLocalizationStateV1::Failed => ProviderEvaluationStateV1::Failed,
        CiFailureLocalizationStateV1::Unavailable | CiFailureLocalizationStateV1::Denied => {
            ProviderEvaluationStateV1::Unavailable
        }
    }
}

const fn proximity_provider_state(coverage: ProximityCoverageV1) -> ProviderEvaluationStateV1 {
    match coverage {
        ProximityCoverageV1::Complete => ProviderEvaluationStateV1::SupportedCompletedComplete,
        ProximityCoverageV1::Partial => ProviderEvaluationStateV1::Partial,
        ProximityCoverageV1::Stale => ProviderEvaluationStateV1::Stale,
        ProximityCoverageV1::Unavailable
        | ProximityCoverageV1::Denied
        | ProximityCoverageV1::Private => ProviderEvaluationStateV1::Unavailable,
    }
}

const fn inconsistent(field: &'static str) -> ApplicationContractError {
    ApplicationContractError::Inconsistent { field }
}
