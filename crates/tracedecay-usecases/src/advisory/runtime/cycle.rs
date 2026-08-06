use super::*;

pub struct AdvisoryRuntime<GR, GA, CS, CE, PE, PC> {
    feedback_scope: FeedbackScopeV1,
    feedback_cycle: Arc<FeedbackCycleRuntime>,
    github: Option<Arc<GitHubReviewRuntimeOwnerV1<GR, GA>>>,
    ci: ConcreteCiFailureLocalizationOwnerV1<CS, CE>,
    proximity: ConcreteProximityRuntimeOwnerV1<PE, PC>,
    observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

impl<GR, GA, CS, CE, PE, PC> AdvisoryRuntime<GR, GA, CS, CE, PE, PC>
where
    GR: GitHubCurrentBranchRemapper + Sync,
    GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: CiReadOnlyProviderArchiveV1 + Sync,
    CE: CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: ConfigurationControlStore + Clone + Send + 'static,
{
    pub fn open(
        input: AdvisoryRuntimeOpenV1,
        providers: AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
    ) -> Result<Self, AdvisoryRuntimeOpenErrorV1> {
        let AdvisoryRuntimeOpenV1 {
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
            return Err(AdvisoryRuntimeOpenErrorV1::ScopeMismatch);
        }
        let github = match github {
            Some(mut github) => {
                let github_source_access = providers
                    .github_source_access
                    .as_ref()
                    .map(Arc::clone)
                    .ok_or(AdvisoryRuntimeOpenErrorV1::GitHubRuntimeUnavailable)?;
                github.database = database;
                github.resolved_scope = resolved_scope;
                github.feedback_scope = feedback_scope.clone();
                Some(Arc::new(
                    build_github_review_runtime_owner_v1(
                        github,
                        providers.github_remapper,
                        providers.github_anchors,
                        github_source_access,
                    )
                    .map_err(|_| AdvisoryRuntimeOpenErrorV1::GitHubRuntimeUnavailable)?,
                ))
            }
            None => None,
        };
        let ci = concrete_ci_failure_localization_owner_v1(
            providers.ci_source,
            providers.ci_exact_evidence,
        );
        let proximity = open_proximity_runtime(
            feedback_scope.clone(),
            providers.proximity_evidence,
            providers.configuration,
        )
        .ok_or(AdvisoryRuntimeOpenErrorV1::ProximityRuntimeUnavailable)?;
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

    pub fn feedback_owner(&self) -> Arc<ConcreteFeedbackOwner> {
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

    pub fn github_owner(&self) -> Option<Arc<GitHubReviewRuntimeOwnerV1<GR, GA>>> {
        self.github.as_ref().map(Arc::clone)
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
        let mut contributions = AdvisoryContributionsV1::absent();
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
                            AdvisoryProviderV1::GitHub,
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
                            AdvisoryProviderV1::GitHub,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                    GitHubReviewRefreshOutcomeV1::Unavailable => {
                        self.observe_github_terminal(
                            &request.feedback.input,
                            Plan26FeedbackOutcomeV1::Unavailable,
                        );
                        contributions.set_state(
                            AdvisoryProviderV1::GitHub,
                            ProviderEvaluationStateV1::Unavailable,
                        );
                    }
                    GitHubReviewRefreshOutcomeV1::Stale => {
                        self.observations.observe_source_event(
                            &request.feedback.input,
                            Plan26FeedbackSourceEventV1::GitHubStale { item_count: 0 },
                        );
                        contributions.set_state(
                            AdvisoryProviderV1::GitHub,
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
                    AdvisoryProviderV1::GitHub,
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
                            AdvisoryProviderV1::Ci,
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
                            AdvisoryProviderV1::Ci,
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
                            AdvisoryProviderV1::Ci,
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
                            AdvisoryProviderV1::Ci,
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
                    self.observe_ci_terminal(
                        &request.feedback.input,
                        outcome,
                        coverage,
                        degradation,
                    );
                    contributions.set_state(AdvisoryProviderV1::Ci, state);
                } else {
                    // Found is handled above; fail closed if the terminal map regresses.
                    self.observe_ci_terminal(
                        &request.feedback.input,
                        Plan26FeedbackOutcomeV1::Unavailable,
                        Plan26CoverageV1::Unknown,
                        None,
                    );
                    contributions.set_state(
                        AdvisoryProviderV1::Ci,
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
                ProximityRuntimeOutcomeV1::Completed(contributor) => {
                    self.observe_proximity(&request.feedback.input, &contributor);
                    contributions.capture(
                        AdvisoryProviderV1::Proximity,
                        contributor.advisory_findings(request.validity),
                    );
                }
                ProximityRuntimeOutcomeV1::Denied
                | ProximityRuntimeOutcomeV1::Unavailable => contributions.set_state(
                    AdvisoryProviderV1::Proximity,
                    ProviderEvaluationStateV1::Unavailable,
                ),
                ProximityRuntimeOutcomeV1::Cancelled => {
                    return Ok(self.finish_interruption(
                        &request.feedback.input,
                        AdvisoryCycleInterruption::Cancelled,
                        contributions,
                    ));
                }
                ProximityRuntimeOutcomeV1::TimedOut => {
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
        contributions: AdvisoryContributionsV1,
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
        contributions: &AdvisoryContributionsV1,
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
        contributor: &super::super::ProximityFindingContributorV1,
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
        contributions: AdvisoryContributionsV1,
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

#[derive(Clone, Copy)]
pub(super) enum AdvisoryCycleInterruption {
    Cancelled,
    TimedOut,
}

impl AdvisoryCycleInterruption {
    fn finish(self, mut contributions: AdvisoryContributionsV1) -> AdvisoryCycleOutcome {
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

pub(super) fn interruption_before_await(
    control: &AdvisoryCycleControl,
) -> Option<AdvisoryCycleInterruption> {
    if control.operation.is_cancelled() {
        Some(AdvisoryCycleInterruption::Cancelled)
    } else if control.deadline.is_elapsed_at(Instant::now()) {
        Some(AdvisoryCycleInterruption::TimedOut)
    } else {
        None
    }
}

pub(super) async fn await_provider<T>(
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

pub(super) fn resolved_scope_matches_feedback_scope(
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

pub(super) fn saturating_u32(value: u64) -> u32 {
    value.try_into().unwrap_or(u32::MAX)
}

pub(super) fn provider_state_event(
    provider: &AdvisoryProviderStateV1,
) -> Plan26FeedbackSourceEventV1 {
    let provider_kind = match provider.provider {
        AdvisoryProviderV1::GitHub => Plan26AdvisoryProviderV1::GitHubReview,
        AdvisoryProviderV1::Ci => Plan26AdvisoryProviderV1::CiLocalization,
        AdvisoryProviderV1::Proximity => Plan26AdvisoryProviderV1::Proximity,
    };
    Plan26FeedbackSourceEventV1::ProviderState {
        provider: provider_kind,
        state: provider.state,
    }
}

pub(super) fn mark_unrequested_remote_providers(
    contributions: &mut AdvisoryContributionsV1,
    github_requested: bool,
    ci_requested: bool,
) {
    if !github_requested {
        contributions.set_state(
            AdvisoryProviderV1::GitHub,
            ProviderEvaluationStateV1::Unavailable,
        );
    }
    if !ci_requested {
        contributions.set_state(
            AdvisoryProviderV1::Ci,
            ProviderEvaluationStateV1::Unavailable,
        );
    }
}

pub(super) fn ci_discovery_terminal_state(
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
        ProductionCiFailureDiscoveryOutcomeV1::Stale => Some((
            ProviderEvaluationStateV1::Stale,
            Plan26FeedbackOutcomeV1::Stale,
            Plan26CoverageV1::Stale,
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
