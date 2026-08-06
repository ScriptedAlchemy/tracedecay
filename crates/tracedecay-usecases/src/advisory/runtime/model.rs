use super::*;

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
    #[error("advisory scope does not match the shared feedback runtime")]
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

    pub(super) fn capture(
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

    pub(super) fn set_state(
        &mut self,
        provider: Pr13AdvisoryProviderV1,
        state: ProviderEvaluationStateV1,
    ) {
        if let Some(current) = self
            .providers
            .iter_mut()
            .find(|current| current.provider == provider)
        {
            current.state = state;
        }
    }

    pub(super) fn terminalize_pending(&mut self, state: ProviderEvaluationStateV1) {
        for provider in &mut self.providers {
            if provider.state == ProviderEvaluationStateV1::Absent {
                provider.state = state;
            }
        }
    }

    pub(super) fn validate(&self) -> Result<(), ApplicationContractError> {
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
    pub(super) fn validate_for(
        &self,
        scope: &FeedbackScopeV1,
    ) -> Result<(), ApplicationContractError> {
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
