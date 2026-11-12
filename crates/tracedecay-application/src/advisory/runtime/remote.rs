use super::cycle::{AdvisoryCycleInterruption, await_provider, ci_discovery_terminal_state};
use super::{
    AdvisoryContributionsV1, AdvisoryCycleControl, AdvisoryFindingContributorV1,
    AdvisoryFindingValidityWindowV1, AdvisoryProviderV1, AdvisoryRuntime,
    CanonicalProximityEvidenceAuthorityV1, CiExactEvidenceAuthorityV1, CiFailureCoverageV1,
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, CiReadOnlyProviderArchiveV1,
    ConfigurationControlStore, FeedbackCoverageV1, FeedbackOutcomeV1,
    FeedbackSourceEventV1, GitHubCanonicalReviewAnchorAuthorityV1, GitHubCurrentBranchRemapper,
    GitHubReviewReadOperationV1, GitHubReviewReadRequestV1, GitHubReviewRefreshOutcomeV1,
    ProductionCiFailureDiscoveryOutcomeV1, ProviderEvaluationStateV1, RequestContext,
};
use crate::advisory::github_runtime::GitHubReviewRefreshStoreReadOutcomeV1;
use tracedecay_domain::feedback::{
    FeedbackEvaluationInputV1, GitHubReviewCoverageV1, GitHubReviewIngressProviderOutcomeV1,
};

impl<GR, GA, CS, CE, PE, PC> AdvisoryRuntime<GR, GA, CS, CE, PE, PC>
where
    GR: GitHubCurrentBranchRemapper + Sync,
    GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: CiReadOnlyProviderArchiveV1 + Sync,
    CE: CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: ConfigurationControlStore + Clone + Send + 'static,
{
    pub(super) async fn attach_retained_github(
        &self,
        context: &RequestContext,
        control: &mut AdvisoryCycleControl,
        validity: AdvisoryFindingValidityWindowV1,
        provider_request: &GitHubReviewReadRequestV1,
        contributions: &mut AdvisoryContributionsV1,
    ) -> Result<(), AdvisoryCycleInterruption> {
        let Some(github) = self.github.as_ref() else {
            contributions.set_state(
                AdvisoryProviderV1::GitHub,
                ProviderEvaluationStateV1::Unavailable,
            );
            return Ok(());
        };
        match await_provider(control, github.retained(context, provider_request)).await? {
            GitHubReviewRefreshStoreReadOutcomeV1::State(state) => {
                let mut ingress = state.latest_attempt.ingress;
                if matches!(
                    ingress.outcome,
                    GitHubReviewIngressProviderOutcomeV1::Complete
                        | GitHubReviewIngressProviderOutcomeV1::Partial
                ) {
                    ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
                    ingress.coverage = GitHubReviewCoverageV1::Stale;
                    for item in &mut ingress.items {
                        item.provider_outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
                    }
                }
                contributions.capture(
                    AdvisoryProviderV1::GitHub,
                    ingress.advisory_findings(validity),
                );
            }
            GitHubReviewRefreshStoreReadOutcomeV1::Empty => {
                contributions.set_state(
                    AdvisoryProviderV1::GitHub,
                    ProviderEvaluationStateV1::Absent,
                );
            }
            GitHubReviewRefreshStoreReadOutcomeV1::Unavailable => {
                contributions.set_state(
                    AdvisoryProviderV1::GitHub,
                    ProviderEvaluationStateV1::Unavailable,
                );
            }
        }
        Ok(())
    }

    pub(super) async fn refresh_github(
        &self,
        context: &RequestContext,
        control: &mut AdvisoryCycleControl,
        input: &FeedbackEvaluationInputV1,
        validity: AdvisoryFindingValidityWindowV1,
        provider_request: &GitHubReviewReadRequestV1,
        contributions: &mut AdvisoryContributionsV1,
    ) -> Result<(), AdvisoryCycleInterruption> {
        if let Some(github) = self.github.as_ref() {
            let outcome =
                await_provider(control, github.refresh(context, provider_request)).await?;
            if matches!(
                provider_request.operation,
                GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads
                    | GitHubReviewReadOperationV1::RestListPullRequestReviewComments
            ) && matches!(
                outcome,
                GitHubReviewRefreshOutcomeV1::Stored(_) | GitHubReviewRefreshOutcomeV1::Stale
            ) {
                let identity_request = GitHubReviewReadRequestV1 {
                    operation: GitHubReviewReadOperationV1::RestGetPullRequest,
                    scope: provider_request.scope.clone(),
                    pull_request_id: provider_request.pull_request_id.clone(),
                };
                match await_provider(control, github.refresh(context, &identity_request)).await? {
                    GitHubReviewRefreshOutcomeV1::Stored(_)
                    | GitHubReviewRefreshOutcomeV1::Stale => {}
                    identity_outcome => {
                        tracing::warn!(
                            event = "github_pull_request_identity_refresh_not_stored",
                            outcome = ?identity_outcome,
                            "the pull-request identity read was not retained; Delivery keeps serving without a PR identity"
                        );
                    }
                }
            }
            match outcome {
                GitHubReviewRefreshOutcomeV1::Stored(state) => {
                    let ingress = &state.state.latest_attempt.ingress;
                    self.observe_github(input, ingress);
                    contributions.capture(
                        AdvisoryProviderV1::GitHub,
                        ingress.advisory_findings(validity),
                    );
                }
                GitHubReviewRefreshOutcomeV1::Cancelled => {
                    return Err(AdvisoryCycleInterruption::Cancelled);
                }
                GitHubReviewRefreshOutcomeV1::Denied => {
                    self.observe_github_terminal(input, FeedbackOutcomeV1::Denied);
                    contributions.set_state(
                        AdvisoryProviderV1::GitHub,
                        ProviderEvaluationStateV1::Unavailable,
                    );
                }
                GitHubReviewRefreshOutcomeV1::Unavailable => {
                    self.observe_github_terminal(input, FeedbackOutcomeV1::Unavailable);
                    contributions.set_state(
                        AdvisoryProviderV1::GitHub,
                        ProviderEvaluationStateV1::Unavailable,
                    );
                }
                GitHubReviewRefreshOutcomeV1::Stale => {
                    self.observations.observe_source_event(
                        input,
                        FeedbackSourceEventV1::GitHubStale { item_count: 0 },
                    );
                    contributions
                        .set_state(AdvisoryProviderV1::GitHub, ProviderEvaluationStateV1::Stale);
                }
            }
        } else {
            self.observe_github_terminal(input, FeedbackOutcomeV1::Unavailable);
            contributions.set_state(
                AdvisoryProviderV1::GitHub,
                ProviderEvaluationStateV1::Unavailable,
            );
        }
        Ok(())
    }

    pub(super) async fn refresh_ci(
        &self,
        context: &RequestContext,
        control: &mut AdvisoryCycleControl,
        input: &FeedbackEvaluationInputV1,
        validity: AdvisoryFindingValidityWindowV1,
        ci: &ProductionCiFailureDiscoveryOutcomeV1,
        contributions: &mut AdvisoryContributionsV1,
    ) -> Result<(), AdvisoryCycleInterruption> {
        match ci {
            ProductionCiFailureDiscoveryOutcomeV1::Found(provider_request) => {
                let outcome =
                    await_provider(control, self.ci.localize(context, provider_request)).await?;
                match outcome {
                    CiFailureLocalizationPortOutcomeV1::Localized(localization) => {
                        self.observe_ci(input, &localization);
                        contributions.capture(
                            AdvisoryProviderV1::Ci,
                            localization.advisory_findings(validity),
                        );
                    }
                    CiFailureLocalizationPortOutcomeV1::Denied => {
                        self.observe_ci_terminal(
                            input,
                            FeedbackOutcomeV1::Denied,
                            FeedbackCoverageV1::Known,
                            None,
                        );
                        contributions.set_state(
                            AdvisoryProviderV1::Ci,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                    CiFailureLocalizationPortOutcomeV1::RateLimited(checkpoint) => {
                        self.observe_ci_terminal(
                            input,
                            FeedbackOutcomeV1::RateLimited,
                            FeedbackCoverageV1::Unknown,
                            Some(tracedecay_domain::feedback::CiFailureSourceDegradationV1::RateLimited(
                                checkpoint,
                            )),
                        );
                        contributions
                            .set_state(AdvisoryProviderV1::Ci, ProviderEvaluationStateV1::Partial);
                    }
                    CiFailureLocalizationPortOutcomeV1::Failed(cause) => {
                        self.observe_ci_terminal(
                            input,
                            FeedbackOutcomeV1::Failed,
                            FeedbackCoverageV1::Unknown,
                            Some(
                                tracedecay_domain::feedback::CiFailureSourceDegradationV1::Failed(
                                    cause,
                                ),
                            ),
                        );
                        contributions
                            .set_state(AdvisoryProviderV1::Ci, ProviderEvaluationStateV1::Failed);
                    }
                    CiFailureLocalizationPortOutcomeV1::Unavailable => {
                        self.observe_ci_terminal(
                            input,
                            FeedbackOutcomeV1::Unavailable,
                            FeedbackCoverageV1::Unknown,
                            None,
                        );
                        contributions.set_state(
                            AdvisoryProviderV1::Ci,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                }
            }
            discovery => {
                if let Some((state, outcome, coverage)) = ci_discovery_terminal_state(discovery) {
                    let degradation = match discovery {
                        ProductionCiFailureDiscoveryOutcomeV1::RateLimited(checkpoint) => Some(
                            tracedecay_domain::feedback::CiFailureSourceDegradationV1::RateLimited(
                                checkpoint.clone(),
                            ),
                        ),
                        ProductionCiFailureDiscoveryOutcomeV1::Failed(cause) => Some(
                            tracedecay_domain::feedback::CiFailureSourceDegradationV1::Failed(
                                *cause,
                            ),
                        ),
                        _ => None,
                    };
                    self.observe_ci_terminal(input, outcome, coverage, degradation);
                    contributions.set_state(AdvisoryProviderV1::Ci, state);
                } else {
                    self.observe_ci_terminal(
                        input,
                        FeedbackOutcomeV1::Unavailable,
                        FeedbackCoverageV1::Unknown,
                        None,
                    );
                    contributions.set_state(
                        AdvisoryProviderV1::Ci,
                        ProviderEvaluationStateV1::Unavailable,
                    );
                }
            }
        }
        Ok(())
    }
}
