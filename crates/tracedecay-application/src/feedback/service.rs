use tracedecay_domain::feedback::{
    FeedbackAuthoritativeRuntimeStateV1, FeedbackBaselineStateV1, FeedbackContentIdentityV1,
    FeedbackCycleObservationV1, FeedbackCycleResultV1, FeedbackCycleTerminationV1,
    FeedbackDedupeKeyV1, FeedbackDiagnosticBaselineIdentityV1, FeedbackDiagnosticBaselineV1,
    FeedbackDiagnosticClassificationV1, FeedbackDiagnosticV1, FeedbackDurabilityV1,
    FeedbackEvaluationInputV1, FeedbackEvaluationStageV1, FeedbackFindingLifecycleV1,
    FeedbackFindingV1, FeedbackImpactStateV1, FeedbackImpactV1, ProviderEvaluationStateV1,
    derive_feedback_finding_id, derive_overlay_feedback_finding_id,
};
use tracedecay_domain::{
    DiagnosticRecordStateV1, GenerationDiagnosticV1, UtcMicros, canonical_sha256,
};
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;

use crate::authorization::{AuthorizationAdmission, AuthorizationPort, AuthorizationService};
use crate::context::RequestContext;
use crate::diagnostics::{
    DiagnosticProviderIdentity, DiagnosticProviderResult, ProviderSourceIdentity,
};
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{ApplicationProblem, ApplicationProblemKind, AuthorityReceipt};

use super::ports::{
    FeedbackCycleDedupePort, FeedbackCycleDedupeState, FeedbackDiagnosticsPort,
    FeedbackDiagnosticsRequest, FeedbackImpactPort, FeedbackImpactPortOutcome,
    FeedbackImpactRequest, FeedbackObservationPort, FeedbackRuntimeStatePort,
};

/// Explicit accounting supplied by the caller/runtime that owns clock, token,
/// and cost measurements. The feedback service never reads a clock or calls a
/// model to manufacture this evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedbackBudgetUsage {
    pub completed_at: UtcMicros,
    pub tokens_consumed: u64,
    pub cost_microunits: u64,
}

impl FeedbackBudgetUsage {
    fn validate_for(
        &self,
        input: &FeedbackEvaluationInputV1,
    ) -> Result<(), ApplicationContractError> {
        if self.completed_at < input.observed_at {
            return Err(ApplicationContractError::InvalidRange {
                field: "feedback budget interval",
            });
        }
        Ok(())
    }

    pub fn elapsed_micros(&self, input: &FeedbackEvaluationInputV1) -> u64 {
        u64::try_from(self.completed_at.0.saturating_sub(input.observed_at.0)).unwrap_or(u64::MAX)
    }

    pub fn exceeds(&self, input: &FeedbackEvaluationInputV1) -> bool {
        let budget = &input.request.budget;
        let elapsed_micros = self.elapsed_micros(input);
        elapsed_micros > budget.deadline_millis.saturating_mul(1_000)
            || elapsed_micros > budget.maximum_latency_millis.saturating_mul(1_000)
            || self.tokens_consumed > budget.maximum_tokens
            || self.cost_microunits > budget.maximum_cost_microunits
    }
}

/// An explicit caller-controlled stop is distinct from runtime cancellation:
/// it ends this one advisory cycle without granting a retry or continuation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedbackCycleControl {
    #[default]
    Continue,
    UserStop,
}

/// Complete, bounded input for one post-edit feedback evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackCycleExecutionRequest {
    pub input: FeedbackEvaluationInputV1,
    pub providers: Vec<DiagnosticProviderIdentity>,
    pub maximum_returned_findings: u64,
    pub usage: FeedbackBudgetUsage,
    pub control: FeedbackCycleControl,
}

impl FeedbackCycleExecutionRequest {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.input.validate()?;
        self.usage.validate_for(&self.input)?;
        if self.maximum_returned_findings == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "feedback maximum returned findings",
            });
        }
        for provider in &self.providers {
            if !provider_matches_input(provider, &self.input) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "feedback diagnostic provider identity",
                });
            }
        }
        if self
            .providers
            .iter()
            .enumerate()
            .any(|(index, provider)| self.providers[index.saturating_add(1)..].contains(provider))
        {
            return Err(ApplicationContractError::Duplicate {
                field: "feedback diagnostic provider identity",
            });
        }
        Ok(())
    }
}

/// One terminal application result. It contains references to authoritative
/// diagnostics and graph/test evidence, not a second durable finding store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackCycleExecutionResult {
    pub cycle: FeedbackCycleResultV1,
    /// Present only for durable saved-content evaluations after authoritative
    /// evidence was assembled. Overlay cycles never enter durable dedupe.
    pub dedupe_key: Option<FeedbackDedupeKeyV1>,
    pub authority: Option<AuthorityReceipt>,
    pub usage: FeedbackBudgetUsage,
}

/// One-shot application service for PR11 feedback. Every external dependency
/// is a narrow port; the service neither schedules work nor persists a
/// feedback/dedupe/observation store of its own.
pub struct FeedbackCycleService<R, D, I, K, O, A, E> {
    runtime: R,
    diagnostics: D,
    impact: I,
    dedupe: K,
    observations: O,
    authorization: AuthorizationService<A, E>,
    operation: ApplicationOperation,
}

impl<R, D, I, K, O, A, E> FeedbackCycleService<R, D, I, K, O, A, E>
where
    R: FeedbackRuntimeStatePort,
    D: FeedbackDiagnosticsPort,
    I: FeedbackImpactPort,
    K: FeedbackCycleDedupePort,
    O: FeedbackObservationPort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        runtime: R,
        diagnostics: D,
        impact: I,
        dedupe: K,
        observations: O,
        authorization: AuthorizationService<A, E>,
        operation: ApplicationOperation,
    ) -> Self {
        Self {
            runtime,
            diagnostics,
            impact,
            dedupe,
            observations,
            authorization,
            operation,
        }
    }

    pub fn execute(
        &self,
        context: &RequestContext,
        request: FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        request.validate()?;
        if !scope_matches(context, &request.input) {
            return self.finish(
                &request,
                None,
                FeedbackCycleTerminationV1::Blocked,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                None,
            );
        }

        let admission =
            match self
                .authorization
                .admit(context, &self.operation, request.input.observed_at)
            {
                Ok(admission) => admission,
                Err(problem) => {
                    let (termination, states) = terminal_for_problem(&problem);
                    return self.finish(
                        &request,
                        None,
                        termination,
                        states,
                        Vec::new(),
                        None,
                        None,
                        Vec::new(),
                        None,
                    );
                }
            };

        let initial_runtime = match self.runtime.resolve(context, &request.input) {
            Some(runtime) => runtime,
            None => {
                return self.finish_after_runtime(
                    context,
                    &request,
                    &admission,
                    None,
                    None,
                    FeedbackCycleTerminationV1::DaemonUnavailable,
                    vec![ProviderEvaluationStateV1::Unavailable],
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                    &[],
                );
            }
        };
        if initial_runtime.validate_for(&request.input).is_err() {
            return self.finish_after_runtime(
                context,
                &request,
                &admission,
                Some(&initial_runtime),
                None,
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
                Vec::new(),
                None,
                None,
                Vec::new(),
                &[],
            );
        }
        if !initial_runtime
            .snapshot
            .has_same_root(&request.input.request)
        {
            return self.finish_after_runtime(
                context,
                &request,
                &admission,
                Some(&initial_runtime),
                None,
                FeedbackCycleTerminationV1::Blocked,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                &[],
            );
        }
        if !initial_runtime
            .snapshot
            .is_current_for(&request.input.request)
        {
            return self.finish_after_runtime(
                context,
                &request,
                &admission,
                Some(&initial_runtime),
                None,
                FeedbackCycleTerminationV1::StaleReplanRequired,
                vec![ProviderEvaluationStateV1::Stale],
                Vec::new(),
                None,
                None,
                Vec::new(),
                &[],
            );
        }

        if request.control == FeedbackCycleControl::UserStop {
            return self.finish_after_runtime(
                context,
                &request,
                &admission,
                Some(&initial_runtime),
                None,
                FeedbackCycleTerminationV1::UserStop,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                &[],
            );
        }

        let mut completed_stages = vec![FeedbackEvaluationStageV1::Admission];
        if request.usage.exceeds(&request.input) {
            return self.finish_after_runtime(
                context,
                &request,
                &admission,
                Some(&initial_runtime),
                None,
                FeedbackCycleTerminationV1::BudgetExceeded,
                vec![ProviderEvaluationStateV1::TimedOut],
                Vec::new(),
                None,
                None,
                Vec::new(),
                &completed_stages,
            );
        }
        if request.providers.is_empty() {
            return self.finish_after_runtime(
                context,
                &request,
                &admission,
                Some(&initial_runtime),
                None,
                FeedbackCycleTerminationV1::Blocked,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                &completed_stages,
            );
        }

        completed_stages.push(FeedbackEvaluationStageV1::Diagnostics);
        let diagnostics_request = FeedbackDiagnosticsRequest {
            input: request.input.clone(),
            providers: request.providers.clone(),
        };
        // Resolve authoritative history before asking providers for current
        // diagnostics. A known absence of prior history stays explicit and does
        // not manufacture a comparison horizon.
        let baselines = if request.input.request.durability() == FeedbackDurabilityV1::Durable
            && initial_runtime.baseline_horizon.is_some()
        {
            let baselines = self.diagnostics.diagnostic_history(&diagnostics_request);
            if let Some(runtime_override) =
                self.runtime_override(context, &request, Some(&initial_runtime))
            {
                return self.finish_after_checked_runtime(
                    context,
                    &request,
                    &admission,
                    Some(runtime_override),
                    None,
                    FeedbackCycleTerminationV1::StaleReplanRequired,
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                    &[],
                );
            }
            baselines
        } else {
            Vec::new()
        };
        let diagnostics = self.diagnostics.diagnostics(&diagnostics_request);
        if let Some(runtime_override) =
            self.runtime_override(context, &request, Some(&initial_runtime))
        {
            return self.finish_after_checked_runtime(
                context,
                &request,
                &admission,
                Some(runtime_override),
                None,
                FeedbackCycleTerminationV1::StaleReplanRequired,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                &[],
            );
        }
        let resolved_baselines = resolve_baselines(&request, &initial_runtime, &baselines)?;
        let baseline_states = resolved_baselines
            .iter()
            .map(|resolved| resolved.state)
            .collect::<Vec<_>>();
        let (provider_states, findings) =
            collect_diagnostics(&request, &diagnostics, &resolved_baselines)?;
        if let Some(termination) = terminal_before_impact(&provider_states, &baseline_states) {
            return self.finish_after_checked_runtime(
                context,
                &request,
                &admission,
                None,
                None,
                termination,
                provider_states,
                baseline_states,
                None,
                None,
                Vec::new(),
                &completed_stages,
            );
        }

        completed_stages.push(FeedbackEvaluationStageV1::BaselineClassification);
        completed_stages.push(FeedbackEvaluationStageV1::Impact);
        let (impact, impact_state) = self.resolve_impact(&request.input);
        if let Some(runtime_override) =
            self.runtime_override(context, &request, Some(&initial_runtime))
        {
            return self.finish_after_checked_runtime(
                context,
                &request,
                &admission,
                Some(runtime_override),
                None,
                FeedbackCycleTerminationV1::StaleReplanRequired,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                &[],
            );
        }
        let affected_tests_state = impact
            .as_ref()
            .map(|impact| impact.affected_tests_state)
            .unwrap_or(impact_state);
        completed_stages.push(FeedbackEvaluationStageV1::AffectedTests);
        completed_stages.push(FeedbackEvaluationStageV1::ResultAssembly);

        let dedupe_key = if request.input.request.durability() == FeedbackDurabilityV1::Durable {
            let evidence_identity = canonical_sha256(&(
                "tracedecay.feedback.evidence-identity.v1",
                &initial_runtime,
                &diagnostics,
                &baselines,
                &impact,
                impact_state,
            ))?;
            let key = request.input.dedupe_key(&evidence_identity)?;
            let dedupe_state = self.dedupe.check(&key);
            if let Some(runtime_override) =
                self.runtime_override(context, &request, Some(&initial_runtime))
            {
                return self.finish_after_checked_runtime(
                    context,
                    &request,
                    &admission,
                    Some(runtime_override),
                    None,
                    FeedbackCycleTerminationV1::StaleReplanRequired,
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                    &[],
                );
            }
            match dedupe_state {
                FeedbackCycleDedupeState::Duplicate => {
                    return self.finish_after_checked_runtime(
                        context,
                        &request,
                        &admission,
                        None,
                        Some(key),
                        FeedbackCycleTerminationV1::DuplicateNoop,
                        Vec::new(),
                        Vec::new(),
                        None,
                        None,
                        Vec::new(),
                        &completed_stages,
                    );
                }
                FeedbackCycleDedupeState::Unavailable => {
                    return self.finish_after_checked_runtime(
                        context,
                        &request,
                        &admission,
                        None,
                        Some(key),
                        FeedbackCycleTerminationV1::DaemonUnavailable,
                        vec![ProviderEvaluationStateV1::Unavailable],
                        Vec::new(),
                        None,
                        None,
                        Vec::new(),
                        &completed_stages,
                    );
                }
                FeedbackCycleDedupeState::Unique => Some(key),
            }
        } else {
            None
        };

        let termination = determine_termination(
            &provider_states,
            &baseline_states,
            &findings,
            impact_state,
            affected_tests_state,
            request.input.request.durability(),
        );
        self.finish_after_checked_runtime(
            context,
            &request,
            &admission,
            None,
            dedupe_key,
            termination,
            provider_states,
            baseline_states,
            impact,
            Some(impact_state),
            findings,
            &completed_stages,
        )
    }

    fn resolve_impact(
        &self,
        input: &FeedbackEvaluationInputV1,
    ) -> (Option<FeedbackImpactV1>, FeedbackImpactStateV1) {
        match self.impact.impact(&FeedbackImpactRequest {
            input: input.clone(),
        }) {
            FeedbackImpactPortOutcome::Complete(impact)
                if impact.state == FeedbackImpactStateV1::Complete
                    && impact.target == input.target
                    && (input.request.durability() == FeedbackDurabilityV1::Durable
                        || impact.evidence_anchors.is_empty())
                    && impact.validate().is_ok() =>
            {
                (Some(impact), FeedbackImpactStateV1::Complete)
            }
            FeedbackImpactPortOutcome::Partial(impact)
                if impact.state == FeedbackImpactStateV1::Partial
                    && impact.target == input.target
                    && (input.request.durability() == FeedbackDurabilityV1::Durable
                        || impact.evidence_anchors.is_empty())
                    && impact.validate().is_ok() =>
            {
                (Some(impact), FeedbackImpactStateV1::Partial)
            }
            FeedbackImpactPortOutcome::Stale => (None, FeedbackImpactStateV1::Stale),
            FeedbackImpactPortOutcome::Unavailable => (None, FeedbackImpactStateV1::Unavailable),
            FeedbackImpactPortOutcome::Complete(_) | FeedbackImpactPortOutcome::Partial(_) => {
                (None, FeedbackImpactStateV1::Unavailable)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_after_runtime(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        admission: &AuthorizationAdmission,
        initial_runtime: Option<&FeedbackAuthoritativeRuntimeStateV1>,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        baseline_states: Vec<FeedbackBaselineStateV1>,
        impact: Option<FeedbackImpactV1>,
        impact_state: Option<FeedbackImpactStateV1>,
        findings: Vec<FeedbackFindingV1>,
        completed_stages: &[FeedbackEvaluationStageV1],
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        let runtime_override = self.runtime_override(context, request, initial_runtime);
        self.finish_after_checked_runtime(
            context,
            request,
            admission,
            runtime_override,
            dedupe_key,
            termination,
            provider_states,
            baseline_states,
            impact,
            impact_state,
            findings,
            completed_stages,
        )
    }

    fn runtime_override(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        initial_runtime: Option<&FeedbackAuthoritativeRuntimeStateV1>,
    ) -> Option<(FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>)> {
        match self.runtime.resolve(context, &request.input) {
            None => Some((
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
            )),
            Some(latest_runtime) if latest_runtime.validate_for(&request.input).is_err() => Some((
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
            )),
            Some(latest_runtime)
                if !latest_runtime
                    .snapshot
                    .has_same_root(&request.input.request) =>
            {
                Some((FeedbackCycleTerminationV1::Blocked, Vec::new()))
            }
            Some(latest_runtime)
                if !latest_runtime
                    .snapshot
                    .is_current_for(&request.input.request)
                    || initial_runtime.is_none_or(|initial| initial != &latest_runtime) =>
            {
                Some((
                    FeedbackCycleTerminationV1::StaleReplanRequired,
                    vec![ProviderEvaluationStateV1::Stale],
                ))
            }
            Some(_) => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_after_checked_runtime(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        admission: &AuthorizationAdmission,
        runtime_override: Option<(FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>)>,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        baseline_states: Vec<FeedbackBaselineStateV1>,
        impact: Option<FeedbackImpactV1>,
        impact_state: Option<FeedbackImpactStateV1>,
        findings: Vec<FeedbackFindingV1>,
        completed_stages: &[FeedbackEvaluationStageV1],
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        let authority = match self.authorization.recheck_publication(
            context,
            &self.operation,
            admission,
            request.usage.completed_at,
        ) {
            Ok(authority) => authority,
            Err(problem) => {
                let (termination, states) = terminal_for_problem(&problem);
                return self.finish(
                    request,
                    None,
                    termination,
                    states,
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                    None,
                );
            }
        };
        if let Some((termination, states)) = runtime_override {
            return self.finish(
                request,
                None,
                termination,
                states,
                Vec::new(),
                None,
                None,
                Vec::new(),
                Some(authority),
            );
        }

        if !completed_stages.is_empty() {
            self.emit_trigger(&request.input);
            for stage in completed_stages {
                self.emit_stage(&request.input, *stage);
            }
        }
        if termination == FeedbackCycleTerminationV1::DuplicateNoop
            && let Some(key) = dedupe_key.clone()
        {
            self.emit_dedupe(&request.input, key);
        }
        self.finish(
            request,
            dedupe_key,
            termination,
            provider_states,
            baseline_states,
            impact,
            impact_state,
            findings,
            Some(authority),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        request: &FeedbackCycleExecutionRequest,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        baseline_states: Vec<FeedbackBaselineStateV1>,
        impact: Option<FeedbackImpactV1>,
        impact_state: Option<FeedbackImpactStateV1>,
        findings: Vec<FeedbackFindingV1>,
        authority: Option<AuthorityReceipt>,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        let total_findings = findings.len() as u64;
        let returned_findings = total_findings.min(request.maximum_returned_findings);
        let omitted_findings = total_findings.saturating_sub(returned_findings);
        let visible_findings = findings
            .into_iter()
            .take(usize::try_from(returned_findings).unwrap_or(usize::MAX))
            .collect::<Vec<_>>();
        let affected_tests_state = impact
            .as_ref()
            .map(|impact| impact.affected_tests_state)
            .or(impact_state);
        let cycle = FeedbackCycleResultV1::new(
            &request.input.request,
            termination,
            provider_states,
            baseline_states,
            impact,
            impact_state,
            affected_tests_state,
            visible_findings,
            total_findings,
            returned_findings,
            omitted_findings,
        )?;
        let authority = (request.input.request.durability() == FeedbackDurabilityV1::Durable)
            .then_some(authority)
            .flatten();
        if authority.is_some() {
            self.emit_terminal(&request.input, termination);
            self.emit_latency(&request.input, request.usage.elapsed_micros(&request.input));
        }
        Ok(FeedbackCycleExecutionResult {
            cycle,
            dedupe_key,
            authority,
            usage: request.usage,
        })
    }

    fn emit_trigger(&self, input: &FeedbackEvaluationInputV1) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) = FeedbackCycleObservationV1::trigger(input)
        {
            self.observations.observe(observation);
        }
    }

    fn emit_stage(&self, input: &FeedbackEvaluationInputV1, stage: FeedbackEvaluationStageV1) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) = FeedbackCycleObservationV1::stage(input, stage)
        {
            self.observations.observe(observation);
        }
    }

    fn emit_dedupe(&self, input: &FeedbackEvaluationInputV1, dedupe_key: FeedbackDedupeKeyV1) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) =
                FeedbackCycleObservationV1::dedupe_suppressed(input, dedupe_key)
        {
            self.observations.observe(observation);
        }
    }

    fn emit_terminal(
        &self,
        input: &FeedbackEvaluationInputV1,
        termination: FeedbackCycleTerminationV1,
    ) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) = FeedbackCycleObservationV1::terminal(input, termination)
        {
            self.observations.observe(observation);
        }
    }

    fn emit_latency(&self, input: &FeedbackEvaluationInputV1, elapsed_micros: u64) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) = FeedbackCycleObservationV1::latency(
                input,
                FeedbackEvaluationStageV1::Total,
                elapsed_micros,
            )
        {
            self.observations.observe(observation);
        }
    }
}

fn scope_matches(context: &RequestContext, input: &FeedbackEvaluationInputV1) -> bool {
    let scope = context.scope();
    scope.project_id == input.request.scope.project_id
        && scope.repository_id == input.request.scope.repository_id
        && scope.worktree_id == input.request.scope.worktree_id
        && scope
            .reference
            .as_ref()
            .is_none_or(|reference| reference.as_str() == input.request.scope.branch_ref)
}

fn provider_matches_input(
    identity: &DiagnosticProviderIdentity,
    input: &FeedbackEvaluationInputV1,
) -> bool {
    if identity.validate().is_err()
        || identity.scope.project_id != input.request.scope.project_id
        || identity.scope.repository_id != input.request.scope.repository_id
        || identity.scope.worktree_id != input.request.scope.worktree_id
        || identity
            .scope
            .reference
            .as_ref()
            .map(|reference| reference.as_str())
            != Some(input.request.scope.branch_ref.as_str())
        || identity.document.file != input.target.file
        || identity.configuration.digest != input.request.configuration_digest
        || identity.policy.digest != input.request.policy_digest
    {
        return false;
    }
    match (&identity.source, &input.request.content) {
        (
            ProviderSourceIdentity::CleanGeneration { generation },
            tracedecay_domain::feedback::FeedbackContentIdentityV1::SavedContent {
                file_digest,
                ..
            },
        ) => {
            input.target.generation_id.as_ref() == Some(generation)
                && identity.document.document_version.is_none()
                && identity.document.content_digest.as_str() == file_digest.as_str()
        }
        (
            ProviderSourceIdentity::SessionOverlay {
                session_id,
                client_id,
                document_version,
                overlay_digest,
            },
            tracedecay_domain::feedback::FeedbackContentIdentityV1::EphemeralOverlay {
                session_id: expected_session,
                owner_client_id,
                document_version: expected_version,
                overlay_digest: expected_digest,
                ..
            },
        ) => {
            session_id == expected_session
                && client_id == owner_client_id
                && document_version == expected_version
                && overlay_digest == expected_digest
                && identity.document.document_version == Some(*expected_version)
                && identity.document.content_digest.as_str() == expected_digest.as_str()
        }
        _ => false,
    }
}

struct ResolvedBaseline<'a> {
    expected: Option<FeedbackDiagnosticBaselineIdentityV1>,
    baseline: Option<&'a FeedbackDiagnosticBaselineV1>,
    state: FeedbackBaselineStateV1,
}

fn expected_baseline_identity(
    request: &FeedbackCycleExecutionRequest,
    runtime: &FeedbackAuthoritativeRuntimeStateV1,
    provider: &DiagnosticProviderIdentity,
) -> Result<FeedbackDiagnosticBaselineIdentityV1, ApplicationContractError> {
    let input = &request.input;
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest,
        file_digest,
    } = &input.request.content
    else {
        return Err(ApplicationContractError::Inconsistent {
            field: "overlay feedback baseline request",
        });
    };
    Ok(FeedbackDiagnosticBaselineIdentityV1 {
        current_generation_id: input.target.generation_id.clone().ok_or(
            ApplicationContractError::Inconsistent {
                field: "feedback baseline generation",
            },
        )?,
        current_generation_digest: generation_digest.clone(),
        current_head_commit_id: input.request.scope.head_commit_id.clone(),
        current_content_digest: file_digest.clone(),
        provider_identity_digest: provider.compute_digest()?,
        horizon: runtime.baseline_horizon.clone().ok_or(
            ApplicationContractError::Inconsistent {
                field: "feedback baseline horizon",
            },
        )?,
    })
}

fn resolve_baselines<'a>(
    request: &FeedbackCycleExecutionRequest,
    runtime: &FeedbackAuthoritativeRuntimeStateV1,
    baselines: &'a [FeedbackDiagnosticBaselineV1],
) -> Result<Vec<ResolvedBaseline<'a>>, ApplicationContractError> {
    if request.input.request.durability() == FeedbackDurabilityV1::SessionOnly {
        return Ok(Vec::new());
    }
    if runtime.baseline_horizon.is_none() {
        return Ok(request
            .providers
            .iter()
            .map(|_| ResolvedBaseline {
                expected: None,
                baseline: None,
                state: FeedbackBaselineStateV1::NoPriorBaseline,
            })
            .collect());
    }

    let mut resolved = Vec::with_capacity(request.providers.len());
    let mut expected_provider_digests = Vec::with_capacity(request.providers.len());
    for provider in &request.providers {
        let expected = expected_baseline_identity(request, runtime, provider)?;
        expected_provider_digests.push(expected.provider_identity_digest.clone());
        let exact = baselines
            .iter()
            .filter(|baseline| baseline.validate().is_ok() && baseline.identity == expected)
            .collect::<Vec<_>>();
        let (baseline, state) = match exact.as_slice() {
            [baseline] => (Some(*baseline), baseline.state),
            [] if baselines.iter().any(|baseline| {
                baseline.identity.provider_identity_digest == expected.provider_identity_digest
            }) =>
            {
                (None, FeedbackBaselineStateV1::Stale)
            }
            [] => (None, FeedbackBaselineStateV1::Unavailable),
            _ => (None, FeedbackBaselineStateV1::Partial),
        };
        resolved.push(ResolvedBaseline {
            expected: Some(expected),
            baseline,
            state,
        });
    }

    if baselines.iter().any(|baseline| {
        baseline.validate().is_err()
            || !expected_provider_digests.contains(&baseline.identity.provider_identity_digest)
    }) {
        let expected = resolved
            .first()
            .and_then(|resolved| resolved.expected.clone())
            .ok_or(ApplicationContractError::Inconsistent {
                field: "unexpected feedback baseline",
            })?;
        resolved.push(ResolvedBaseline {
            expected: Some(expected),
            baseline: None,
            state: FeedbackBaselineStateV1::Partial,
        });
    }
    Ok(resolved)
}

fn collect_diagnostics(
    request: &FeedbackCycleExecutionRequest,
    results: &[DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>],
    baselines: &[ResolvedBaseline<'_>],
) -> Result<(Vec<ProviderEvaluationStateV1>, Vec<FeedbackFindingV1>), ApplicationContractError> {
    let mut states = Vec::with_capacity(request.providers.len());
    let mut findings = Vec::new();
    let unexpected_result = results
        .iter()
        .any(|result| !request.providers.contains(&result.identity));

    for (provider_index, expected) in request.providers.iter().enumerate() {
        let matched = results
            .iter()
            .filter(|result| result.identity == *expected)
            .collect::<Vec<_>>();
        if matched.len() != 1 {
            states.push(if matched.is_empty() {
                ProviderEvaluationStateV1::Absent
            } else {
                ProviderEvaluationStateV1::Failed
            });
            continue;
        }

        let result = matched[0];
        if result.validate().is_err() || !provider_matches_input(&result.identity, &request.input) {
            states.push(ProviderEvaluationStateV1::Failed);
            continue;
        }

        let mut state = result.state.feedback_state();
        let mut provider_findings = Vec::new();
        if let Some(payload) = &result.payload {
            let provider_digest = result.identity.compute_digest()?;
            for diagnostic in payload {
                match diagnostic {
                    FeedbackDiagnosticV1::Saved(diagnostic)
                        if diagnostic_matches_input(
                            diagnostic,
                            &result.identity,
                            &request.input,
                        ) && diagnostic.validate().is_ok() =>
                    {
                        let classification = baselines
                            .get(provider_index)
                            .map(|resolved| {
                                resolved
                                    .baseline
                                    .zip(resolved.expected.as_ref())
                                    .map(|(baseline, expected)| {
                                        baseline.classify(expected, &diagnostic.diagnostic_anchor)
                                    })
                                    .unwrap_or_else(|| {
                                        if resolved.state
                                            == FeedbackBaselineStateV1::NoPriorBaseline
                                        {
                                            FeedbackDiagnosticClassificationV1::New
                                        } else {
                                            FeedbackDiagnosticClassificationV1::Unknown
                                        }
                                    })
                            })
                            .unwrap_or(FeedbackDiagnosticClassificationV1::Unknown);
                        provider_findings.push(FeedbackFindingV1 {
                            finding_id: derive_feedback_finding_id(
                                &diagnostic.diagnostic_anchor,
                                &provider_digest,
                            )?,
                            classification,
                            lifecycle: finding_lifecycle(diagnostic),
                            retrieval_anchor_id: Some(diagnostic.diagnostic_anchor.clone()),
                            provider_state: result.state.feedback_state(),
                            safe_bounded_preview: Some(bounded_preview(&diagnostic.message)),
                        });
                    }
                    FeedbackDiagnosticV1::SessionOverlay(diagnostic)
                        if overlay_diagnostic_matches_input(diagnostic, &request.input)
                            && diagnostic.validate().is_ok() =>
                    {
                        provider_findings.push(FeedbackFindingV1 {
                            finding_id: derive_overlay_feedback_finding_id(
                                diagnostic,
                                &provider_digest,
                            )?,
                            classification: FeedbackDiagnosticClassificationV1::Unknown,
                            lifecycle: FeedbackFindingLifecycleV1::Active,
                            retrieval_anchor_id: None,
                            provider_state: result.state.feedback_state(),
                            safe_bounded_preview: Some(diagnostic.safe_bounded_message.clone()),
                        });
                    }
                    FeedbackDiagnosticV1::Saved(_) | FeedbackDiagnosticV1::SessionOverlay(_) => {
                        state = ProviderEvaluationStateV1::Failed;
                    }
                }
            }
        }
        provider_findings.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
        let conflicting_duplicates = provider_findings
            .windows(2)
            .any(|pair| pair[0].finding_id == pair[1].finding_id && pair[0] != pair[1]);
        if conflicting_duplicates {
            state = ProviderEvaluationStateV1::Failed;
            provider_findings.clear();
        } else {
            provider_findings.dedup();
        }
        findings.extend(provider_findings);
        states.push(state);
    }
    if unexpected_result {
        states.push(ProviderEvaluationStateV1::Failed);
    }
    findings.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    Ok((states, findings))
}

fn overlay_diagnostic_matches_input(
    diagnostic: &tracedecay_domain::feedback::FeedbackSessionDiagnosticV1,
    input: &FeedbackEvaluationInputV1,
) -> bool {
    matches!(
        input.request.content,
        FeedbackContentIdentityV1::EphemeralOverlay { .. }
    ) && input.target.generation_id.is_none()
        && input
            .target
            .span
            .as_ref()
            .is_none_or(|span| diagnostic.span == *span)
        && input
            .target
            .symbol
            .as_ref()
            .is_none_or(|symbol| diagnostic.symbol.as_ref() == Some(symbol))
}

fn diagnostic_matches_input(
    diagnostic: &GenerationDiagnosticV1,
    provider: &DiagnosticProviderIdentity,
    input: &FeedbackEvaluationInputV1,
) -> bool {
    let tracedecay_domain::feedback::FeedbackContentIdentityV1::SavedContent {
        file_digest, ..
    } = &input.request.content
    else {
        return false;
    };
    diagnostic.file_occurrence_id == input.target.file
        && diagnostic.repository == input.request.scope.repository_id
        && diagnostic.worktree.as_ref() == Some(&input.request.scope.worktree_id)
        && diagnostic
            .reference
            .as_ref()
            .map(|reference| reference.as_str())
            == Some(input.request.scope.branch_ref.as_str())
        && diagnostic.source_revision.as_ref() == Some(&input.request.scope.head_commit_id)
        && diagnostic.content_digest.as_str() == file_digest.as_str()
        && diagnostic.provenance.producer == provider.producer.provider
        && diagnostic.provenance.analyzer_revision == provider.producer.analyzer_revision
        && diagnostic.provenance.configuration_revision == provider.configuration.revision
        && input
            .target
            .span
            .as_ref()
            .is_none_or(|span| diagnostic.span == *span)
        && input
            .target
            .symbol
            .as_ref()
            .is_none_or(|symbol| diagnostic.symbol_occurrence_id.as_ref() == Some(symbol))
        && match &provider.source {
            ProviderSourceIdentity::CleanGeneration { generation } => {
                &diagnostic.generation_id == generation
                    && input.target.generation_id.as_ref() == Some(generation)
            }
            ProviderSourceIdentity::SessionOverlay { .. } => false,
        }
}

fn finding_lifecycle(diagnostic: &GenerationDiagnosticV1) -> FeedbackFindingLifecycleV1 {
    match &diagnostic.state {
        DiagnosticRecordStateV1::Current => FeedbackFindingLifecycleV1::Active,
        DiagnosticRecordStateV1::Superseded { .. } => FeedbackFindingLifecycleV1::Superseded,
        DiagnosticRecordStateV1::Cleared { .. } => FeedbackFindingLifecycleV1::Cleared,
    }
}

fn bounded_preview(message: &str) -> String {
    if message.len() <= 512 {
        return message.to_owned();
    }
    let mut end = 512;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

fn determine_termination(
    provider_states: &[ProviderEvaluationStateV1],
    baseline_states: &[FeedbackBaselineStateV1],
    findings: &[FeedbackFindingV1],
    impact_state: FeedbackImpactStateV1,
    affected_tests_state: FeedbackImpactStateV1,
    durability: FeedbackDurabilityV1,
) -> FeedbackCycleTerminationV1 {
    if provider_states.is_empty() {
        return FeedbackCycleTerminationV1::Blocked;
    }
    if provider_states.contains(&ProviderEvaluationStateV1::Stale)
        || baseline_states.contains(&FeedbackBaselineStateV1::Stale)
        || impact_state == FeedbackImpactStateV1::Stale
    {
        return FeedbackCycleTerminationV1::StaleReplanRequired;
    }
    if provider_states.contains(&ProviderEvaluationStateV1::Cancelled) {
        return FeedbackCycleTerminationV1::Cancelled;
    }
    if provider_states.contains(&ProviderEvaluationStateV1::TimedOut) {
        return FeedbackCycleTerminationV1::BudgetExceeded;
    }
    if provider_states
        .iter()
        .all(|state| *state == ProviderEvaluationStateV1::Unavailable)
    {
        return FeedbackCycleTerminationV1::DaemonUnavailable;
    }
    if provider_states
        .iter()
        .any(|state| *state != ProviderEvaluationStateV1::SupportedCompletedComplete)
        || (durability == FeedbackDurabilityV1::Durable
            && (baseline_states.is_empty()
                || baseline_states
                    .iter()
                    .any(|state| !state.supports_complete_comparison())))
        || impact_state != FeedbackImpactStateV1::Complete
        || affected_tests_state != FeedbackImpactStateV1::Complete
    {
        return FeedbackCycleTerminationV1::IncompleteCoverage;
    }
    if findings.is_empty() {
        FeedbackCycleTerminationV1::Clean
    } else {
        FeedbackCycleTerminationV1::Blocked
    }
}

fn terminal_before_impact(
    provider_states: &[ProviderEvaluationStateV1],
    baseline_states: &[FeedbackBaselineStateV1],
) -> Option<FeedbackCycleTerminationV1> {
    if provider_states.contains(&ProviderEvaluationStateV1::Stale)
        || baseline_states.contains(&FeedbackBaselineStateV1::Stale)
    {
        Some(FeedbackCycleTerminationV1::StaleReplanRequired)
    } else if provider_states.contains(&ProviderEvaluationStateV1::Cancelled) {
        Some(FeedbackCycleTerminationV1::Cancelled)
    } else if provider_states.contains(&ProviderEvaluationStateV1::TimedOut) {
        Some(FeedbackCycleTerminationV1::BudgetExceeded)
    } else {
        None
    }
}

fn terminal_for_problem(
    problem: &ApplicationProblem,
) -> (FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>) {
    match problem.kind() {
        ApplicationProblemKind::Cancelled => (
            FeedbackCycleTerminationV1::Cancelled,
            vec![ProviderEvaluationStateV1::Cancelled],
        ),
        ApplicationProblemKind::TimedOut => (
            FeedbackCycleTerminationV1::BudgetExceeded,
            vec![ProviderEvaluationStateV1::TimedOut],
        ),
        ApplicationProblemKind::Stale => (
            FeedbackCycleTerminationV1::StaleReplanRequired,
            vec![ProviderEvaluationStateV1::Stale],
        ),
        ApplicationProblemKind::Unavailable => (
            FeedbackCycleTerminationV1::DaemonUnavailable,
            vec![ProviderEvaluationStateV1::Unavailable],
        ),
        ApplicationProblemKind::InvalidRequest
        | ApplicationProblemKind::NotFoundOrNotAuthorized
        | ApplicationProblemKind::Conflict
        | ApplicationProblemKind::Unsupported
        | ApplicationProblemKind::Saturated => (FeedbackCycleTerminationV1::Blocked, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_selection_never_conflates_complete_and_incomplete_truth() {
        assert_eq!(
            determine_termination(
                &[ProviderEvaluationStateV1::SupportedCompletedComplete],
                &[FeedbackBaselineStateV1::Complete],
                &[],
                FeedbackImpactStateV1::Complete,
                FeedbackImpactStateV1::Complete,
                FeedbackDurabilityV1::Durable,
            ),
            FeedbackCycleTerminationV1::Clean
        );
        assert_eq!(
            determine_termination(
                &[ProviderEvaluationStateV1::Partial],
                &[FeedbackBaselineStateV1::Complete],
                &[],
                FeedbackImpactStateV1::Complete,
                FeedbackImpactStateV1::Complete,
                FeedbackDurabilityV1::Durable,
            ),
            FeedbackCycleTerminationV1::IncompleteCoverage
        );
        assert_eq!(
            determine_termination(
                &[ProviderEvaluationStateV1::Stale],
                &[FeedbackBaselineStateV1::Complete],
                &[],
                FeedbackImpactStateV1::Complete,
                FeedbackImpactStateV1::Complete,
                FeedbackDurabilityV1::Durable,
            ),
            FeedbackCycleTerminationV1::StaleReplanRequired
        );
        assert_eq!(
            determine_termination(
                &[ProviderEvaluationStateV1::Cancelled],
                &[FeedbackBaselineStateV1::Complete],
                &[],
                FeedbackImpactStateV1::Complete,
                FeedbackImpactStateV1::Complete,
                FeedbackDurabilityV1::Durable,
            ),
            FeedbackCycleTerminationV1::Cancelled
        );
        assert_eq!(
            determine_termination(
                &[ProviderEvaluationStateV1::TimedOut],
                &[FeedbackBaselineStateV1::Complete],
                &[],
                FeedbackImpactStateV1::Complete,
                FeedbackImpactStateV1::Complete,
                FeedbackDurabilityV1::Durable,
            ),
            FeedbackCycleTerminationV1::BudgetExceeded
        );
        assert_eq!(
            determine_termination(
                &[ProviderEvaluationStateV1::Unavailable],
                &[FeedbackBaselineStateV1::Complete],
                &[],
                FeedbackImpactStateV1::Complete,
                FeedbackImpactStateV1::Complete,
                FeedbackDurabilityV1::Durable,
            ),
            FeedbackCycleTerminationV1::DaemonUnavailable
        );
    }
}
