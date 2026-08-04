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

pub struct Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC> {
    pub github_remapper: GR,
    pub github_anchors: GA,
    pub ci_source: CS,
    pub ci_exact_evidence: CE,
    pub proximity_evidence: PE,
    pub github_source_access: Option<Arc<dyn GitHubSourceAccessAuthorityV1>>,
    /// Canonical Plan 20 configuration authority. The proximity owner pins the
    /// effective threshold from this source and has no local default.
    pub configuration: PC,
}

pub struct Pr13AdvisoryRuntimeOpenV1 {
    /// Clone of the project database used to open the PR12 feedback runtime.
    pub database: Database,
    pub project_root: PathBuf,
    pub resolved_scope: ResolvedScope,
    pub feedback_scope: FeedbackScopeV1,
    pub github: Option<GitHubReviewRuntimeOwnerConfigV1>,
    /// The already-open PR12 Plan 09 owner. PR13 uses its exact authorization,
    /// diagnostics/impact ports, publication store, and durable dedupe path.
    pub feedback_cycle: Arc<Pr12FeedbackCycleRuntime>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum Pr13AdvisoryRuntimeOpenErrorV1 {
    #[error("PR13 advisory scope does not match the shared PR12 runtime")]
    ScopeMismatch,
    #[error("PR13 GitHub runtime is unavailable")]
    GitHubRuntimeUnavailable,
    #[error("PR13 proximity runtime is unavailable")]
    ProximityRuntimeUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pr13AdvisoryProviderV1 {
    GitHub,
    Ci,
    Proximity,
}

/// No adapter-local lifecycle axes: source records retain their exact
/// lifecycle/provenance/coverage and composition carries only Plan 09 state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pr13AdvisoryProviderStateV1 {
    pub provider: Pr13AdvisoryProviderV1,
    pub state: ProviderEvaluationStateV1,
}

impl Pr13AdvisoryProviderStateV1 {
    fn absent(provider: Pr13AdvisoryProviderV1) -> Self {
        Self {
            provider,
            state: ProviderEvaluationStateV1::Absent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pr13AdvisoryContributionsV1 {
    pub providers: Vec<Pr13AdvisoryProviderStateV1>,
    pub findings: Vec<FeedbackFindingV1>,
}

impl Pr13AdvisoryContributionsV1 {
    pub fn absent() -> Self {
        Self {
            providers: vec![
                Pr13AdvisoryProviderStateV1::absent(Pr13AdvisoryProviderV1::GitHub),
                Pr13AdvisoryProviderStateV1::absent(Pr13AdvisoryProviderV1::Ci),
                Pr13AdvisoryProviderStateV1::absent(Pr13AdvisoryProviderV1::Proximity),
            ],
            findings: Vec::new(),
        }
    }

    pub fn as_plan09(&self) -> Result<FeedbackCycleAdvisoryV1, ApplicationContractError> {
        self.validate()?;
        let mut findings = self.findings.clone();
        findings.sort_by(|left, right| left.finding_id.as_str().cmp(right.finding_id.as_str()));
        Ok(FeedbackCycleAdvisoryV1 {
            provider_states: self
                .providers
                .iter()
                .map(|provider| provider.state)
                .collect(),
            findings,
        })
    }

    fn capture(
        &mut self,
        provider: Pr13AdvisoryProviderV1,
        batch: Result<AdvisoryFindingContributionBatchV1, ApplicationContractError>,
    ) {
        match batch {
            Ok(batch) if batch.validate().is_ok() => {
                self.set_state(provider, batch.provider_state);
                self.findings.extend(batch.findings);
            }
            Ok(_) | Err(_) => self.set_state(provider, ProviderEvaluationStateV1::Failed),
        }
    }

    fn set_state(&mut self, provider: Pr13AdvisoryProviderV1, state: ProviderEvaluationStateV1) {
        if let Some(current) = self
            .providers
            .iter_mut()
            .find(|current| current.provider == provider)
        {
            current.state = state;
        }
    }

    fn terminalize_pending(&mut self, state: ProviderEvaluationStateV1) {
        for provider in &mut self.providers {
            if provider.state == ProviderEvaluationStateV1::Absent {
                provider.state = state;
            }
        }
    }

    fn validate(&self) -> Result<(), ApplicationContractError> {
        let expected = [
            Pr13AdvisoryProviderV1::GitHub,
            Pr13AdvisoryProviderV1::Ci,
            Pr13AdvisoryProviderV1::Proximity,
        ];
        if self.providers.len() != expected.len()
            || self
                .providers
                .iter()
                .zip(expected)
                .any(|(provider, expected)| provider.provider != expected)
            || self.findings.iter().any(|finding| {
                finding.validate().is_err()
                    || !self
                        .providers
                        .iter()
                        .any(|provider| provider.state == finding.provider_state)
            })
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "pr13 advisory contribution",
            });
        }
        if self.findings.iter().enumerate().any(|(index, finding)| {
            self.findings[index.saturating_add(1)..]
                .iter()
                .any(|other| other.finding_id == finding.finding_id)
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "pr13 advisory finding",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryCycleRequest {
    pub feedback: FeedbackCycleExecutionRequest,
    pub github: Option<GitHubReviewReadRequestV1>,
    pub ci: ProductionCiFailureDiscoveryOutcomeV1,
    pub proximity: Option<ProximityEvaluationRequestV1>,
    pub validity: AdvisoryFindingValidityWindowV1,
}

impl AdvisoryCycleRequest {
    fn validate_for(&self, scope: &FeedbackScopeV1) -> Result<(), ApplicationContractError> {
        self.feedback.validate()?;
        if self.feedback.input.request.scope != *scope
            || self.validity.valid_at < self.feedback.input.observed_at
            || self.validity.valid_at >= self.validity.expires_at
            || self
                .github
                .as_ref()
                .is_some_and(|request| request.validate().is_err() || request.scope != *scope)
            || !self.ci.validate_for(scope)
            || self
                .proximity
                .as_ref()
                .is_some_and(|request| request.validate().is_err() || request.scope != *scope)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "pr13 advisory cycle scope",
            });
        }
        Ok(())
    }
}

/// Operation-stream cancellation and the root application's monotonic
/// deadline are shared by every provider await.
pub struct AdvisoryCycleControl {
    pub operation: OperationEmitter,
    pub deadline: MonotonicDeadline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Boxing the large variant would ripple through in-flight construction/match
// sites; the size gap is accepted here.
#[allow(clippy::large_enum_variant)]
pub enum AdvisoryCycleOutcome {
    Completed {
        cycle: Pr12CanonicalFeedbackResultV1,
        contributions: Pr13AdvisoryContributionsV1,
        observation_input: tracedecay_domain::feedback::FeedbackEvaluationInputV1,
    },
    Cancelled {
        contributions: Pr13AdvisoryContributionsV1,
    },
    TimedOut {
        contributions: Pr13AdvisoryContributionsV1,
    },
}

impl AdvisoryCycleOutcome {
    /// Returns the exact shared-store publication only after its atomic insert
    /// completed. Delivery callers receive no value for duplicate, failed,
    /// cancelled, timed-out, or otherwise unpublished cycles.
    pub fn publication(&self) -> Option<&FeedbackCompletedPublicationV1> {
        match self {
            Self::Completed { cycle, .. } => cycle.publication.as_ref(),
            Self::Cancelled { .. } | Self::TimedOut { .. } => None,
        }
    }
}

pub struct Pr13AdvisoryRuntime<GR, GA, CS, CE, PE, PC> {
    feedback_scope: FeedbackScopeV1,
    feedback_cycle: Arc<Pr12FeedbackCycleRuntime>,
    github: Option<GitHubReviewRuntimeOwnerV1<GR, GA>>,
    ci: ConcreteCiFailureLocalizationOwnerV1<CS, CE>,
    proximity: ConcretePr13ProximityRuntimeOwnerV1<PE, PC>,
    observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

impl<GR, GA, CS, CE, PE, PC> Pr13AdvisoryRuntime<GR, GA, CS, CE, PE, PC>
where
    GR: GitHubCurrentBranchRemapper + Sync,
    GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: CiReadOnlyProviderArchiveV1 + Sync,
    CE: CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: ConfigurationControlStore + Clone + Send + 'static,
{
    pub fn open(
        input: Pr13AdvisoryRuntimeOpenV1,
        providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
    ) -> Result<Self, Pr13AdvisoryRuntimeOpenErrorV1> {
        let Pr13AdvisoryRuntimeOpenV1 {
            database,
            project_root,
            resolved_scope,
            feedback_scope,
            github,
            feedback_cycle,
        } = input;
        let feedback = feedback_cycle.feedback_runtime();
        if resolved_scope.validate().is_err()
            || feedback_scope.validate().is_err()
            || !resolved_scope_matches_feedback_scope(&resolved_scope, &feedback_scope)
            || feedback.project_root() != project_root
            || feedback.scope() != &resolved_scope
        {
            return Err(Pr13AdvisoryRuntimeOpenErrorV1::ScopeMismatch);
        }
        let github = match github {
            Some(mut github) => {
                let github_source_access = providers
                    .github_source_access
                    .as_ref()
                    .map(Arc::clone)
                    .ok_or(Pr13AdvisoryRuntimeOpenErrorV1::GitHubRuntimeUnavailable)?;
                github.database = database;
                github.resolved_scope = resolved_scope;
                github.feedback_scope = feedback_scope.clone();
                Some(
                    build_github_review_runtime_owner_v1(
                        github,
                        providers.github_remapper,
                        providers.github_anchors,
                        github_source_access,
                    )
                    .map_err(|_| Pr13AdvisoryRuntimeOpenErrorV1::GitHubRuntimeUnavailable)?,
                )
            }
            None => None,
        };
        let ci = concrete_ci_failure_localization_owner_v1(
            providers.ci_source,
            providers.ci_exact_evidence,
        );
        let proximity = open_pr13_proximity_runtime(
            feedback_scope.clone(),
            providers.proximity_evidence,
            providers.configuration,
        )
        .ok_or(Pr13AdvisoryRuntimeOpenErrorV1::ProximityRuntimeUnavailable)?;
        let observations = feedback_cycle.source_observation_port();
        Ok(Self {
            feedback_scope,
            feedback_cycle,
            github,
            ci,
            proximity,
            observations,
        })
    }

    pub fn feedback_owner(&self) -> Arc<ConcretePr12FeedbackOwner> {
        self.feedback_cycle.feedback_runtime().owner()
    }

    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.feedback_cycle.publication_store()
    }

    pub fn source_observation_port(
        &self,
    ) -> Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync> {
        Arc::clone(&self.observations)
    }

    pub async fn run_once(
        &self,
        context: &RequestContext,
        mut control: AdvisoryCycleControl,
        request: AdvisoryCycleRequest,
    ) -> Result<AdvisoryCycleOutcome, ApplicationContractError> {
        if let Err(error) = request.validate_for(&self.feedback_scope) {
            self.observations.observe_source_event(
                &request.feedback.input,
                Plan26FeedbackSourceEventV1::ArgumentRejected {
                    operation: Plan26FeedbackOperationV1::FeedbackCycle,
                    outcome: Plan26FeedbackOutcomeV1::Rejected,
                },
            );
            return Err(error);
        }
        let mut contributions = Pr13AdvisoryContributionsV1::absent();
        mark_unrequested_remote_providers(
            &mut contributions,
            request.github.is_some(),
            request.ci.is_configured(),
        );

        if !context_matches_scope(context, &self.feedback_scope) {
            self.observations.observe_source_event(
                &request.feedback.input,
                Plan26FeedbackSourceEventV1::Dispatch {
                    operation: Plan26FeedbackOperationV1::FeedbackCycle,
                    outcome: Plan26FeedbackOutcomeV1::Denied,
                    capacity: saturating_u32(request.feedback.maximum_returned_findings),
                    admitted: 0,
                },
            );
            return Box::pin(self.finish_cycle(context, request.feedback, contributions)).await;
        }
        self.observations.observe_source_event(
            &request.feedback.input,
            Plan26FeedbackSourceEventV1::Dispatch {
                operation: Plan26FeedbackOperationV1::FeedbackCycle,
                outcome: Plan26FeedbackOutcomeV1::Admitted,
                capacity: saturating_u32(request.feedback.maximum_returned_findings),
                admitted: saturating_u32(request.feedback.providers.len() as u64),
            },
        );
        if let Some(interruption) = interruption_before_await(&control) {
            return Ok(self.finish_interruption(
                &request.feedback.input,
                interruption,
                contributions,
            ));
        }

        if let Some(provider_request) = request.github.as_ref() {
            if let Some(github) = self.github.as_ref() {
                let outcome =
                    await_provider(&mut control, github.refresh(context, provider_request)).await;
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(interruption) => {
                        return Ok(self.finish_interruption(
                            &request.feedback.input,
                            interruption,
                            contributions,
                        ));
                    }
                };
                match outcome {
                    GitHubReviewRefreshOutcomeV1::Stored(state) => {
                        let ingress = &state.state.latest_attempt.ingress;
                        self.observe_github(&request.feedback.input, ingress);
                        contributions.capture(
                            Pr13AdvisoryProviderV1::GitHub,
                            ingress.advisory_findings(request.validity),
                        );
                    }
                    GitHubReviewRefreshOutcomeV1::Cancelled => {
                        return Ok(self.finish_interruption(
                            &request.feedback.input,
                            AdvisoryCycleInterruption::Cancelled,
                            contributions,
                        ));
                    }
                    GitHubReviewRefreshOutcomeV1::Denied => {
                        self.observe_github_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::Denied,
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::GitHub,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                    GitHubReviewRefreshOutcomeV1::Unavailable => {
                        self.observe_github_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::Unavailable,
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::GitHub,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                    GitHubReviewRefreshOutcomeV1::Stale => {
                        self.observations.observe_source_event(
                            &request.feedback.input,
                            Plan26FeedbackSourceEventV1::GitHubStale { item_count: 0 },
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::GitHub,
                            ProviderEvaluationStateV1::Stale,
                        );
                    }
                }
            } else {
                self.observe_github_terminal(
                    &request.feedback.input,
                    Plan26FeedbackOutcomeV1::Unavailable,
                );
                contributions.set_state(
                    Pr13AdvisoryProviderV1::GitHub,
                    ProviderEvaluationStateV1::Unavailable,
                );
            }
        }

        match &request.ci {
            ProductionCiFailureDiscoveryOutcomeV1::Found(provider_request) => {
                let outcome =
                    await_provider(&mut control, self.ci.localize(context, provider_request)).await;
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(interruption) => {
                        return Ok(self.finish_interruption(
                            &request.feedback.input,
                            interruption,
                            contributions,
                        ));
                    }
                };
                match outcome {
                    CiFailureLocalizationPortOutcomeV1::Localized(localization) => {
                        self.observe_ci(&request.feedback.input, &localization);
                        contributions.capture(
                            Pr13AdvisoryProviderV1::Ci,
                            localization.advisory_findings(request.validity),
                        );
                    }
                    CiFailureLocalizationPortOutcomeV1::Denied => {
                        self.observe_ci_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::Denied,
                            Plan26CoverageV1::Known,
                            None,
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::Ci,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                    CiFailureLocalizationPortOutcomeV1::RateLimited(checkpoint) => {
                        self.observe_ci_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::RateLimited,
                            Plan26CoverageV1::Unknown,
                            Some(tracedecay_domain::feedback::CiFailureSourceDegradationV1::RateLimited(
                                checkpoint,
                            )),
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::Ci,
                            ProviderEvaluationStateV1::Partial,
                        );
                    }
                    CiFailureLocalizationPortOutcomeV1::Failed(cause) => {
                        self.observe_ci_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::Failed,
                            Plan26CoverageV1::Unknown,
                            Some(
                                tracedecay_domain::feedback::CiFailureSourceDegradationV1::Failed(
                                    cause,
                                ),
                            ),
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::Ci,
                            ProviderEvaluationStateV1::Failed,
                        );
                    }
                    CiFailureLocalizationPortOutcomeV1::Unavailable => {
                        self.observe_ci_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::Unavailable,
                            Plan26CoverageV1::Unknown,
                            None,
                        );
                        contributions.set_state(
                            Pr13AdvisoryProviderV1::Ci,
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
                    self.observe_ci_terminal(
                        &request.feedback.input,
                        outcome,
                        coverage,
                        degradation,
                    );
                    contributions.set_state(Pr13AdvisoryProviderV1::Ci, state);
                } else {
                    // Found is handled above; fail closed if the terminal map regresses.
                    self.observe_ci_terminal(
                        &request.feedback.input,
                        Plan26FeedbackOutcomeV1::Unavailable,
                        Plan26CoverageV1::Unknown,
                        None,
                    );
                    contributions.set_state(
                        Pr13AdvisoryProviderV1::Ci,
                        ProviderEvaluationStateV1::Unavailable,
                    );
                }
            }
        }

        if let Some(provider_request) = request.proximity.as_ref() {
            let outcome = await_provider(
                &mut control,
                self.proximity.evaluate_for_configuration_digest(
                    context,
                    provider_request,
                    &request.feedback.input.request.configuration_digest,
                ),
            )
            .await;
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(interruption) => {
                    return Ok(self.finish_interruption(
                        &request.feedback.input,
                        interruption,
                        contributions,
                    ));
                }
            };
            match outcome {
                Pr13ProximityRuntimeOutcomeV1::Completed(contributor) => {
                    self.observe_proximity(&request.feedback.input, &contributor);
                    contributions.capture(
                        Pr13AdvisoryProviderV1::Proximity,
                        contributor.advisory_findings(request.validity),
                    );
                }
                Pr13ProximityRuntimeOutcomeV1::Denied
                | Pr13ProximityRuntimeOutcomeV1::Unavailable => contributions.set_state(
                    Pr13AdvisoryProviderV1::Proximity,
                    ProviderEvaluationStateV1::Unavailable,
                ),
                Pr13ProximityRuntimeOutcomeV1::Cancelled => {
                    return Ok(self.finish_interruption(
                        &request.feedback.input,
                        AdvisoryCycleInterruption::Cancelled,
                        contributions,
                    ));
                }
                Pr13ProximityRuntimeOutcomeV1::TimedOut => {
                    return Ok(self.finish_interruption(
                        &request.feedback.input,
                        AdvisoryCycleInterruption::TimedOut,
                        contributions,
                    ));
                }
            }
        }

        if let Some(interruption) = interruption_before_await(&control) {
            return Ok(self.finish_interruption(
                &request.feedback.input,
                interruption,
                contributions,
            ));
        }
        Box::pin(self.finish_cycle(context, request.feedback, contributions)).await
    }

    fn finish_interruption(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        interruption: AdvisoryCycleInterruption,
        contributions: Pr13AdvisoryContributionsV1,
    ) -> AdvisoryCycleOutcome {
        self.observations.observe_source_event(
            input,
            Plan26FeedbackSourceEventV1::Cancellation {
                operation: Plan26FeedbackOperationV1::FeedbackCycle,
                outcome: match interruption {
                    AdvisoryCycleInterruption::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
                    AdvisoryCycleInterruption::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
                },
            },
        );
        let outcome = interruption.finish(contributions);
        match &outcome {
            AdvisoryCycleOutcome::Cancelled { contributions }
            | AdvisoryCycleOutcome::TimedOut { contributions } => {
                self.observe_provider_states(input, contributions);
            }
            AdvisoryCycleOutcome::Completed { .. } => {}
        }
        outcome
    }

    fn observe_provider_states(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        contributions: &Pr13AdvisoryContributionsV1,
    ) {
        for provider in &contributions.providers {
            self.observations
                .observe_source_event(input, provider_state_event(provider));
        }
    }

    fn observe_github(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        ingress: &GitHubReviewIngressResultV1,
    ) {
        let outcome = match ingress.outcome {
            GitHubReviewIngressProviderOutcomeV1::Complete => Plan26FeedbackOutcomeV1::Completed,
            GitHubReviewIngressProviderOutcomeV1::Partial => Plan26FeedbackOutcomeV1::Partial,
            GitHubReviewIngressProviderOutcomeV1::Unavailable => {
                Plan26FeedbackOutcomeV1::Unavailable
            }
            GitHubReviewIngressProviderOutcomeV1::Denied => Plan26FeedbackOutcomeV1::Denied,
            GitHubReviewIngressProviderOutcomeV1::RateLimited => {
                Plan26FeedbackOutcomeV1::RateLimited
            }
            GitHubReviewIngressProviderOutcomeV1::Stale => Plan26FeedbackOutcomeV1::Stale,
            GitHubReviewIngressProviderOutcomeV1::Failed => Plan26FeedbackOutcomeV1::Failed,
        };
        self.observations.observe_source_event(
            input,
            Plan26FeedbackSourceEventV1::GitHubIngress {
                outcome,
                item_count: saturating_u32(ingress.items.len() as u64),
                duration_micros: None,
            },
        );
        if ingress.outcome == GitHubReviewIngressProviderOutcomeV1::RateLimited {
            self.observations.observe_source_event(
                input,
                Plan26FeedbackSourceEventV1::GitHubRateLimit {
                    duration_micros: None,
                },
            );
        }
        if ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Stale {
            self.observations.observe_source_event(
                input,
                Plan26FeedbackSourceEventV1::GitHubStale {
                    item_count: saturating_u32(ingress.items.len() as u64),
                },
            );
        }
        for lifecycle in [
            GitHubReviewLifecycleV1::Current,
            GitHubReviewLifecycleV1::Outdated,
            GitHubReviewLifecycleV1::Resolved,
            GitHubReviewLifecycleV1::Edited,
            GitHubReviewLifecycleV1::Deleted,
        ] {
            let count = ingress
                .items
                .iter()
                .filter(|item| item.lifecycle == lifecycle)
                .count();
            if count == 0 {
                continue;
            }
            let lifecycle = match lifecycle {
                GitHubReviewLifecycleV1::Current => Plan26GitHubLifecycleV1::Current,
                GitHubReviewLifecycleV1::Outdated => Plan26GitHubLifecycleV1::Outdated,
                GitHubReviewLifecycleV1::Resolved => Plan26GitHubLifecycleV1::Resolved,
                GitHubReviewLifecycleV1::Edited => Plan26GitHubLifecycleV1::Edited,
                GitHubReviewLifecycleV1::Deleted => Plan26GitHubLifecycleV1::Deleted,
            };
            self.observations.observe_source_event(
                input,
                Plan26FeedbackSourceEventV1::GitHubLifecycle {
                    lifecycle,
                    item_count: saturating_u32(count as u64),
                },
            );
        }
    }

    fn observe_github_terminal(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        outcome: Plan26FeedbackOutcomeV1,
    ) {
        self.observations.observe_source_event(
            input,
            Plan26FeedbackSourceEventV1::GitHubIngress {
                outcome,
                item_count: 0,
                duration_micros: None,
            },
        );
    }

    fn observe_ci(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        localization: &CiFailureLocalizationResultV1,
    ) {
        let outcome = match localization.state {
            CiFailureLocalizationStateV1::Complete => Plan26FeedbackOutcomeV1::Completed,
            CiFailureLocalizationStateV1::Partial => Plan26FeedbackOutcomeV1::Partial,
            CiFailureLocalizationStateV1::Stale => Plan26FeedbackOutcomeV1::Stale,
            CiFailureLocalizationStateV1::Unavailable => Plan26FeedbackOutcomeV1::Unavailable,
            CiFailureLocalizationStateV1::Denied => Plan26FeedbackOutcomeV1::Denied,
            CiFailureLocalizationStateV1::Failed => Plan26FeedbackOutcomeV1::Failed,
        };
        let coverage = match localization.coverage {
            CiFailureCoverageV1::Complete => Plan26CoverageV1::Known,
            CiFailureCoverageV1::Partial => Plan26CoverageV1::Partial,
            CiFailureCoverageV1::Unavailable | CiFailureCoverageV1::Denied => {
                Plan26CoverageV1::Unknown
            }
            CiFailureCoverageV1::Stale => Plan26CoverageV1::Stale,
        };
        let localized_count = u32::from(localization.symbol.is_some())
            .saturating_add(saturating_u32(localization.callers.len() as u64))
            .saturating_add(saturating_u32(localization.tests.len() as u64));
        self.observations.observe_source_event(
            input,
            Plan26FeedbackSourceEventV1::CiLocalization {
                outcome,
                provider: Plan26CiProviderV1::GitHubActions,
                exact_evidence: localization.generation.is_some(),
                coverage,
                source_degradation: localization.source_degradation.clone(),
                localized_count,
                candidate_count: localized_count,
                duration_micros: None,
            },
        );
    }

    fn observe_ci_terminal(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        outcome: Plan26FeedbackOutcomeV1,
        coverage: Plan26CoverageV1,
        source_degradation: Option<tracedecay_domain::feedback::CiFailureSourceDegradationV1>,
    ) {
        self.observations.observe_source_event(
            input,
            Plan26FeedbackSourceEventV1::CiLocalization {
                outcome,
                provider: Plan26CiProviderV1::GitHubActions,
                exact_evidence: false,
                coverage,
                source_degradation,
                localized_count: 0,
                candidate_count: 0,
                duration_micros: None,
            },
        );
    }

    fn observe_proximity(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        contributor: &super::Pr13ProximityFindingContributorV1,
    ) {
        let contributions = contributor.contributions();
        let emitted_count = contributions
            .iter()
            .filter(|item| item.inclusion == ProximityInclusionV1::Included)
            .count();
        let suppressed_count = contributions
            .iter()
            .filter(|item| {
                matches!(
                    item.inclusion,
                    ProximityInclusionV1::BelowThreshold
                        | ProximityInclusionV1::SuppressedDuplicate
                        | ProximityInclusionV1::Denied
                        | ProximityInclusionV1::Private
                )
            })
            .count();
        let expired_count = contributions
            .iter()
            .filter(|item| item.inclusion == ProximityInclusionV1::Stale)
            .count();
        let candidate_count = saturating_u32(contributions.len() as u64);
        for (transition, risk, count) in [
            (
                Plan26ProximityTransitionV1::Emitted,
                Plan26ProximityRiskV1::AtOrAboveThreshold,
                emitted_count,
            ),
            (
                Plan26ProximityTransitionV1::Suppressed,
                if contributions
                    .iter()
                    .any(|item| item.inclusion == ProximityInclusionV1::BelowThreshold)
                {
                    Plan26ProximityRiskV1::BelowThreshold
                } else {
                    Plan26ProximityRiskV1::None
                },
                suppressed_count,
            ),
            (
                Plan26ProximityTransitionV1::Expired,
                Plan26ProximityRiskV1::None,
                expired_count,
            ),
        ] {
            if count == 0 {
                continue;
            }
            self.observations.observe_source_event(
                input,
                Plan26FeedbackSourceEventV1::Proximity {
                    transition,
                    risk,
                    configuration_revision: input.request.configuration_digest.clone(),
                    candidate_count,
                    affected_count: saturating_u32(count as u64),
                },
            );
        }
        if contributions.is_empty() {
            self.observations.observe_source_event(
                input,
                Plan26FeedbackSourceEventV1::Proximity {
                    transition: Plan26ProximityTransitionV1::Suppressed,
                    risk: Plan26ProximityRiskV1::None,
                    configuration_revision: input.request.configuration_digest.clone(),
                    candidate_count: 0,
                    affected_count: 0,
                },
            );
        }
    }

    async fn finish_cycle(
        &self,
        context: &RequestContext,
        request: FeedbackCycleExecutionRequest,
        contributions: Pr13AdvisoryContributionsV1,
    ) -> Result<AdvisoryCycleOutcome, ApplicationContractError> {
        let observation_input = request.input.clone();
        let advisory = contributions.as_plan09()?;
        self.observe_provider_states(&observation_input, &contributions);
        let cycle = self
            .feedback_cycle
            .run_once_with_advisory(context, request, advisory)
            .await?;
        Ok(AdvisoryCycleOutcome::Completed {
            cycle,
            contributions,
            observation_input,
        })
    }
}

pub struct Pr13AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC> {
    pub advisory: Pr13AdvisoryRuntime<GR, GA, CS, CE, PE, PC>,
    pub feedback_owner: Arc<ConcretePr12FeedbackOwner>,
    pub publication_store: ProjectFeedbackStore,
    pub source_observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

pub fn open_pr13_advisory_daemon_registration<GR, GA, CS, CE, PE, PC>(
    input: Pr13AdvisoryRuntimeOpenV1,
    providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
) -> Result<Pr13AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC>, Pr13AdvisoryRuntimeOpenErrorV1>
where
    GR: GitHubCurrentBranchRemapper + Sync,
    GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: CiReadOnlyProviderArchiveV1 + Sync,
    CE: CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: ConfigurationControlStore + Clone + Send + 'static,
{
    let advisory = Pr13AdvisoryRuntime::open(input, providers)?;
    let feedback_owner = advisory.feedback_owner();
    let publication_store = advisory.publication_store();
    let source_observations = advisory.source_observation_port();
    Ok(Pr13AdvisoryDaemonRegistrationV1 {
        advisory,
        feedback_owner,
        publication_store,
        source_observations,
    })
}

#[derive(Clone, Copy)]
enum AdvisoryCycleInterruption {
    Cancelled,
    TimedOut,
}

impl AdvisoryCycleInterruption {
    fn finish(self, mut contributions: Pr13AdvisoryContributionsV1) -> AdvisoryCycleOutcome {
        match self {
            Self::Cancelled => {
                contributions.terminalize_pending(ProviderEvaluationStateV1::Cancelled);
                AdvisoryCycleOutcome::Cancelled { contributions }
            }
            Self::TimedOut => {
                contributions.terminalize_pending(ProviderEvaluationStateV1::TimedOut);
                AdvisoryCycleOutcome::TimedOut { contributions }
            }
        }
    }
}

fn interruption_before_await(control: &AdvisoryCycleControl) -> Option<AdvisoryCycleInterruption> {
    if control.operation.is_cancelled() {
        Some(AdvisoryCycleInterruption::Cancelled)
    } else if control.deadline.is_elapsed_at(Instant::now()) {
        Some(AdvisoryCycleInterruption::TimedOut)
    } else {
        None
    }
}

async fn await_provider<T>(
    control: &mut AdvisoryCycleControl,
    future: impl Future<Output = T>,
) -> Result<T, AdvisoryCycleInterruption> {
    if let Some(interruption) = interruption_before_await(control) {
        return Err(interruption);
    }
    tokio::pin!(future);
    let deadline_at = control.deadline.instant();
    let cancelled = control.operation.cancelled();
    tokio::pin!(cancelled);
    let deadline = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline_at));
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        () = &mut cancelled => Err(AdvisoryCycleInterruption::Cancelled),
        () = &mut deadline => Err(AdvisoryCycleInterruption::TimedOut),
        outcome = &mut future => Ok(outcome),
    }
}

fn resolved_scope_matches_feedback_scope(
    resolved_scope: &ResolvedScope,
    feedback_scope: &FeedbackScopeV1,
) -> bool {
    resolved_scope.project_id == feedback_scope.project_id
        && resolved_scope.repository_id == feedback_scope.repository_id
        && resolved_scope.worktree_id == feedback_scope.worktree_id
        && resolved_scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(feedback_scope.branch_ref.as_str())
}

fn saturating_u32(value: u64) -> u32 {
    value.try_into().unwrap_or(u32::MAX)
}

fn provider_state_event(provider: &Pr13AdvisoryProviderStateV1) -> Plan26FeedbackSourceEventV1 {
    let provider_kind = match provider.provider {
        Pr13AdvisoryProviderV1::GitHub => Plan26AdvisoryProviderV1::GitHubReview,
        Pr13AdvisoryProviderV1::Ci => Plan26AdvisoryProviderV1::CiLocalization,
        Pr13AdvisoryProviderV1::Proximity => Plan26AdvisoryProviderV1::Proximity,
    };
    Plan26FeedbackSourceEventV1::ProviderState {
        provider: provider_kind,
        state: provider.state,
    }
}

fn mark_unrequested_remote_providers(
    contributions: &mut Pr13AdvisoryContributionsV1,
    github_requested: bool,
    ci_requested: bool,
) {
    if !github_requested {
        contributions.set_state(
            Pr13AdvisoryProviderV1::GitHub,
            ProviderEvaluationStateV1::Unavailable,
        );
    }
    if !ci_requested {
        contributions.set_state(
            Pr13AdvisoryProviderV1::Ci,
            ProviderEvaluationStateV1::Unavailable,
        );
    }
}

fn ci_discovery_terminal_state(
    discovery: &ProductionCiFailureDiscoveryOutcomeV1,
) -> Option<(
    ProviderEvaluationStateV1,
    Plan26FeedbackOutcomeV1,
    Plan26CoverageV1,
)> {
    match discovery {
        ProductionCiFailureDiscoveryOutcomeV1::Found(_) => None,
        ProductionCiFailureDiscoveryOutcomeV1::NotFound => Some((
            ProviderEvaluationStateV1::SupportedCompletedComplete,
            Plan26FeedbackOutcomeV1::Completed,
            Plan26CoverageV1::Known,
        )),
        ProductionCiFailureDiscoveryOutcomeV1::Ambiguous => Some((
            ProviderEvaluationStateV1::Failed,
            Plan26FeedbackOutcomeV1::Failed,
            Plan26CoverageV1::Unknown,
        )),
        ProductionCiFailureDiscoveryOutcomeV1::Denied => Some((
            ProviderEvaluationStateV1::Unavailable,
            Plan26FeedbackOutcomeV1::Denied,
            Plan26CoverageV1::Unknown,
        )),
        ProductionCiFailureDiscoveryOutcomeV1::RateLimited(_) => Some((
            ProviderEvaluationStateV1::Partial,
            Plan26FeedbackOutcomeV1::RateLimited,
            Plan26CoverageV1::Unknown,
        )),
        ProductionCiFailureDiscoveryOutcomeV1::Failed(_) => Some((
            ProviderEvaluationStateV1::Failed,
            Plan26FeedbackOutcomeV1::Failed,
            Plan26CoverageV1::Unknown,
        )),
        ProductionCiFailureDiscoveryOutcomeV1::NotConfigured
        | ProductionCiFailureDiscoveryOutcomeV1::Unavailable => Some((
            ProviderEvaluationStateV1::Unavailable,
            Plan26FeedbackOutcomeV1::Unavailable,
            Plan26CoverageV1::Unknown,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_provider_states_remain_explicit() {
        let advisory = Pr13AdvisoryContributionsV1::absent()
            .as_plan09()
            .expect("canonical advisory");
        assert_eq!(
            advisory.provider_states,
            vec![
                ProviderEvaluationStateV1::Absent,
                ProviderEvaluationStateV1::Absent,
                ProviderEvaluationStateV1::Absent,
            ]
        );
        assert!(advisory.findings.is_empty());
    }

    #[test]
    fn interrupted_cycle_has_no_delivery_publication() {
        let outcome = AdvisoryCycleOutcome::Cancelled {
            contributions: Pr13AdvisoryContributionsV1::absent(),
        };
        assert!(outcome.publication().is_none());
    }

    #[test]
    fn provider_state_events_preserve_each_closed_provider_identity() {
        let events = Pr13AdvisoryContributionsV1::absent()
            .providers
            .iter()
            .map(provider_state_event)
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                Plan26FeedbackSourceEventV1::ProviderState {
                    provider: Plan26AdvisoryProviderV1::GitHubReview,
                    state: ProviderEvaluationStateV1::Absent,
                },
                Plan26FeedbackSourceEventV1::ProviderState {
                    provider: Plan26AdvisoryProviderV1::CiLocalization,
                    state: ProviderEvaluationStateV1::Absent,
                },
                Plan26FeedbackSourceEventV1::ProviderState {
                    provider: Plan26AdvisoryProviderV1::Proximity,
                    state: ProviderEvaluationStateV1::Absent,
                },
            ]
        );
    }

    #[test]
    fn unrequested_remote_providers_are_typed_unavailable_not_omitted() {
        let mut contributions = Pr13AdvisoryContributionsV1::absent();
        mark_unrequested_remote_providers(&mut contributions, false, false);
        assert_eq!(
            contributions
                .providers
                .iter()
                .map(|provider| provider.state)
                .collect::<Vec<_>>(),
            vec![
                ProviderEvaluationStateV1::Unavailable,
                ProviderEvaluationStateV1::Unavailable,
                ProviderEvaluationStateV1::Absent,
            ]
        );
        assert_eq!(
            contributions
                .providers
                .iter()
                .take(2)
                .map(provider_state_event)
                .collect::<Vec<_>>(),
            vec![
                Plan26FeedbackSourceEventV1::ProviderState {
                    provider: Plan26AdvisoryProviderV1::GitHubReview,
                    state: ProviderEvaluationStateV1::Unavailable,
                },
                Plan26FeedbackSourceEventV1::ProviderState {
                    provider: Plan26AdvisoryProviderV1::CiLocalization,
                    state: ProviderEvaluationStateV1::Unavailable,
                },
            ]
        );
    }

    #[test]
    fn ci_discovery_degradation_never_collapses_to_clean() {
        assert_eq!(
            ci_discovery_terminal_state(&ProductionCiFailureDiscoveryOutcomeV1::NotFound),
            Some((
                ProviderEvaluationStateV1::SupportedCompletedComplete,
                Plan26FeedbackOutcomeV1::Completed,
                Plan26CoverageV1::Known,
            ))
        );
        for (discovery, expected) in [
            (
                ProductionCiFailureDiscoveryOutcomeV1::Ambiguous,
                ProviderEvaluationStateV1::Failed,
            ),
            (
                ProductionCiFailureDiscoveryOutcomeV1::Denied,
                ProviderEvaluationStateV1::Unavailable,
            ),
            (
                ProductionCiFailureDiscoveryOutcomeV1::Unavailable,
                ProviderEvaluationStateV1::Unavailable,
            ),
            (
                ProductionCiFailureDiscoveryOutcomeV1::NotConfigured,
                ProviderEvaluationStateV1::Unavailable,
            ),
        ] {
            assert_eq!(
                ci_discovery_terminal_state(&discovery).map(|state| state.0),
                Some(expected)
            );
        }
    }
}
