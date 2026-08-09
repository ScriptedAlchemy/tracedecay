use crate::context::{RequestAdmission, RequestContext};
use crate::diagnostics::{
    DiagnosticProviderIdentity, DiagnosticProviderResult, ProviderSourceIdentity,
};
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::AuthorityReceipt;
use crate::storage::findings::truncate_at_char_boundary;
use tracedecay_domain::feedback::{
    FeedbackAdvisoryProviderStateV1, FeedbackBaselineStateV1, FeedbackContentIdentityV1,
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

use super::adapters::feedback_baseline_identity;
use super::ports::{
    FeedbackCompletedPublicationV1, FeedbackCycleDedupePort, FeedbackCycleDedupePublicationState,
    FeedbackCycleDedupeState, FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest,
    FeedbackImpactPort, FeedbackImpactPortOutcome, FeedbackImpactRequest, FeedbackObservationPort,
    FeedbackRouteAdmission, FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort,
    FeedbackRuntimeStateV1,
};
use super::problem_terminal::terminal_for_problem;

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

/// Additional, source-backed advisory findings composed into one Plan 09
/// cycle. The caller owns the provider lifecycle and exact source evidence;
/// this service only validates, accounts for, and atomically publishes the
/// canonical finding projection through its existing dedupe port.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedbackCycleAdvisoryV1 {
    pub providers: Vec<FeedbackAdvisoryProviderStateV1>,
    pub findings: Vec<FeedbackFindingV1>,
}

impl FeedbackCycleAdvisoryV1 {
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.findings.is_empty()
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.is_empty() {
            return Ok(());
        }
        if self.providers.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback advisory coverage",
            });
        }
        if self.findings.iter().any(|finding| {
            finding.validate().is_err()
                || !self
                    .providers
                    .iter()
                    .any(|provider| provider.state == finding.provider_state)
                || finding
                    .diagnostic_projection
                    .as_ref()
                    .is_some_and(|projection| {
                        !self.providers.iter().any(|provider| {
                            provider.producer == projection.producer
                                && provider.state == finding.provider_state
                        })
                    })
        }) {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback advisory finding",
            });
        }
        if self.findings.iter().enumerate().any(|(index, finding)| {
            self.findings[index.saturating_add(1)..]
                .iter()
                .any(|other| other.finding_id == finding.finding_id)
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "feedback advisory finding",
            });
        }
        if self.providers.iter().enumerate().any(|(index, provider)| {
            self.providers[index.saturating_add(1)..]
                .iter()
                .any(|other| other.producer == provider.producer)
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "feedback advisory provider",
            });
        }
        Ok(())
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

/// Accumulated state carried between typed feedback-cycle stages.
struct FeedbackCycleProgress {
    admission: FeedbackRouteAdmission,
    runtime: Option<FeedbackRuntimeStateV1>,
    completed_stages: Vec<FeedbackEvaluationStageV1>,
    baselines: Vec<FeedbackDiagnosticBaselineV1>,
    diagnostics: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    provider_states: Vec<ProviderEvaluationStateV1>,
    advisory_provider_states: Vec<FeedbackAdvisoryProviderStateV1>,
    baseline_states: Vec<FeedbackBaselineStateV1>,
    impact: Option<FeedbackImpactV1>,
    impact_state: Option<FeedbackImpactStateV1>,
    findings: Vec<FeedbackFindingV1>,
    dedupe_key: Option<FeedbackDedupeKeyV1>,
}

fn admitted_progress(
    progress: &Option<FeedbackCycleProgress>,
) -> Result<&FeedbackCycleProgress, ApplicationContractError> {
    progress
        .as_ref()
        .ok_or(ApplicationContractError::Inconsistent {
            field: "feedback cycle admission state",
        })
}

fn admitted_progress_mut(
    progress: &mut Option<FeedbackCycleProgress>,
) -> Result<&mut FeedbackCycleProgress, ApplicationContractError> {
    progress
        .as_mut()
        .ok_or(ApplicationContractError::Inconsistent {
            field: "feedback cycle admission state",
        })
}

fn resolved_runtime(
    progress: &FeedbackCycleProgress,
) -> Result<&FeedbackRuntimeStateV1, ApplicationContractError> {
    progress
        .runtime
        .as_ref()
        .ok_or(ApplicationContractError::Inconsistent {
            field: "feedback cycle runtime state",
        })
}

fn resolved_impact_state(
    progress: &FeedbackCycleProgress,
) -> Result<FeedbackImpactStateV1, ApplicationContractError> {
    progress
        .impact_state
        .ok_or(ApplicationContractError::Inconsistent {
            field: "feedback cycle impact state",
        })
}

/// One step in the feedback-cycle state machine.
enum FeedbackCycleStage {
    ValidateAndScope,
    Admit,
    CheckInterruption,
    ResolveRuntime,
    ValidateRuntime,
    CheckUserStop,
    CheckBudgetAndProviders,
    LoadBaselines,
    LoadDiagnostics,
    ClassifyDiagnostics,
    ResolveImpact,
    LookupDedupe,
    AssembleResult,
}

/// Whether stage observations should be emitted on terminal completion.
enum FeedbackCycleStageEmission {
    /// Runtime override mid-pipeline suppresses staged observations.
    Suppressed,
    FromProgress,
}

/// Terminal payload routed to the existing finish helpers.
struct FeedbackCycleTerminal {
    termination: FeedbackCycleTerminationV1,
    provider_states: Vec<ProviderEvaluationStateV1>,
    baseline_states: Vec<FeedbackBaselineStateV1>,
    impact: Option<FeedbackImpactV1>,
    impact_state: Option<FeedbackImpactStateV1>,
    findings: Vec<FeedbackFindingV1>,
    dedupe_key: Option<FeedbackDedupeKeyV1>,
    finish_path: FeedbackCycleFinishPath,
}

enum FeedbackCycleFinishPath {
    Immediate,
    AfterRuntime {
        runtime: Option<FeedbackRuntimeStateV1>,
        stage_emission: FeedbackCycleStageEmission,
    },
    AfterCheckedRuntime {
        runtime: Option<FeedbackRuntimeStateV1>,
        stage_emission: FeedbackCycleStageEmission,
    },
}

enum FeedbackCycleStep {
    Continue(Box<FeedbackCycleStage>),
    Terminal(Box<FeedbackCycleTerminal>),
    Complete(Box<FeedbackCycleExecutionResult>),
}

impl FeedbackCycleStep {
    fn continue_with(stage: FeedbackCycleStage) -> Self {
        Self::Continue(Box::new(stage))
    }

    fn terminal(terminal: FeedbackCycleTerminal) -> Self {
        Self::Terminal(Box::new(terminal))
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
    /// Present only after the shared durable store atomically records this
    /// exact completed publication. Duplicate, failed, cancelled, timed-out,
    /// and non-durable outcomes never expose a delivery handoff.
    pub publication: Option<FeedbackCompletedPublicationV1>,
}

/// One-shot application service for post-edit feedback. Every external dependency
/// is a narrow port; the service neither schedules work nor persists a
/// feedback/dedupe/observation store of its own.
pub struct FeedbackCycleService<R, D, I, K, O, A> {
    runtime: R,
    diagnostics: D,
    impact: I,
    dedupe: K,
    observations: O,
    authorization: A,
    operation: ApplicationOperation,
}

impl<R, D, I, K, O, A> FeedbackCycleService<R, D, I, K, O, A>
where
    R: FeedbackRuntimeStatePort,
    D: FeedbackDiagnosticsPort,
    I: FeedbackImpactPort,
    K: FeedbackCycleDedupePort,
    O: FeedbackObservationPort,
    A: FeedbackRouteAuthorizationPort,
{
    pub fn new(
        runtime: R,
        diagnostics: D,
        impact: I,
        dedupe: K,
        observations: O,
        authorization: A,
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

    pub async fn execute(
        &self,
        context: &RequestContext,
        request: FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        self.execute_with_advisory(context, request, FeedbackCycleAdvisoryV1::default())
            .await
    }

    /// Runs one Plan 09 cycle with source-backed advisory evidence. The
    /// supplied evidence becomes part of the canonical result and its durable
    /// dedupe identity; it never creates a second publication path.
    pub async fn execute_with_advisory(
        &self,
        context: &RequestContext,
        request: FeedbackCycleExecutionRequest,
        advisory: FeedbackCycleAdvisoryV1,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        request.validate()?;
        advisory.validate()?;
        let mut stage = FeedbackCycleStage::ValidateAndScope;
        let mut progress = None::<FeedbackCycleProgress>;
        loop {
            match self
                .advance_feedback_cycle_stage(
                    context,
                    &mut progress,
                    request.clone(),
                    stage,
                    &advisory,
                )
                .await?
            {
                FeedbackCycleStep::Continue(next) => stage = *next,
                FeedbackCycleStep::Terminal(terminal) => {
                    let terminal = *terminal;
                    let Some(progress) = progress else {
                        return self
                            .finish_terminal(context, &request, None, terminal)
                            .await;
                    };
                    return self
                        .finish_terminal(context, &request, Some(&progress), terminal)
                        .await;
                }
                FeedbackCycleStep::Complete(result) => return Ok(*result),
            }
        }
    }

    async fn advance_feedback_cycle_stage(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: FeedbackCycleExecutionRequest,
        stage: FeedbackCycleStage,
        advisory: &FeedbackCycleAdvisoryV1,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        match stage {
            FeedbackCycleStage::ValidateAndScope => {
                self.handle_validate_and_scope(context, &request)
            }
            FeedbackCycleStage::Admit => self.handle_admit(context, progress, &request),
            FeedbackCycleStage::CheckInterruption => {
                self.handle_check_interruption(context, &request)
            }
            FeedbackCycleStage::ResolveRuntime => {
                self.handle_resolve_runtime(context, progress, &request)
                    .await
            }
            FeedbackCycleStage::ValidateRuntime => self.handle_validate_runtime(progress, &request),
            FeedbackCycleStage::CheckUserStop => self.handle_check_user_stop(progress, &request),
            FeedbackCycleStage::CheckBudgetAndProviders => {
                self.handle_check_budget_and_providers(progress, &request)
            }
            FeedbackCycleStage::LoadBaselines => {
                self.handle_load_baselines(context, progress, &request)
                    .await
            }
            FeedbackCycleStage::LoadDiagnostics => {
                self.handle_load_diagnostics(context, progress, &request)
                    .await
            }
            FeedbackCycleStage::ClassifyDiagnostics => {
                self.handle_classify_diagnostics(progress, &request, advisory)
            }
            FeedbackCycleStage::ResolveImpact => {
                self.handle_resolve_impact(context, progress, &request)
                    .await
            }
            FeedbackCycleStage::LookupDedupe => {
                self.handle_lookup_dedupe(context, progress, &request, advisory)
                    .await
            }
            FeedbackCycleStage::AssembleResult => {
                self.handle_assemble_result(context, progress, &request)
                    .await
            }
        }
    }

    fn handle_validate_and_scope(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        if !scope_matches(context, &request.input) {
            return Ok(FeedbackCycleStep::terminal(FeedbackCycleTerminal {
                termination: FeedbackCycleTerminationV1::Blocked,
                provider_states: Vec::new(),
                baseline_states: Vec::new(),
                impact: None,
                impact_state: None,
                findings: Vec::new(),
                dedupe_key: None,
                finish_path: FeedbackCycleFinishPath::Immediate,
            }));
        }
        Ok(FeedbackCycleStep::continue_with(FeedbackCycleStage::Admit))
    }

    fn handle_admit(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        match self
            .authorization
            .admit(context, &self.operation, request.input.observed_at)
        {
            Ok(admission) => {
                *progress = Some(FeedbackCycleProgress {
                    admission,
                    runtime: None,
                    completed_stages: Vec::new(),
                    baselines: Vec::new(),
                    diagnostics: Vec::new(),
                    provider_states: Vec::new(),
                    advisory_provider_states: Vec::new(),
                    baseline_states: Vec::new(),
                    impact: None,
                    impact_state: None,
                    findings: Vec::new(),
                    dedupe_key: None,
                });
                Ok(FeedbackCycleStep::continue_with(
                    FeedbackCycleStage::CheckInterruption,
                ))
            }
            Err(problem) => {
                let (termination, states) = terminal_for_problem(&problem);
                Ok(FeedbackCycleStep::terminal(FeedbackCycleTerminal {
                    termination,
                    provider_states: states,
                    baseline_states: Vec::new(),
                    impact: None,
                    impact_state: None,
                    findings: Vec::new(),
                    dedupe_key: None,
                    finish_path: FeedbackCycleFinishPath::Immediate,
                }))
            }
        }
    }

    fn handle_check_interruption(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        if let Some((termination, states)) = request_interruption(context, request) {
            return Ok(FeedbackCycleStep::terminal(FeedbackCycleTerminal {
                termination,
                provider_states: states,
                baseline_states: Vec::new(),
                impact: None,
                impact_state: None,
                findings: Vec::new(),
                dedupe_key: None,
                finish_path: FeedbackCycleFinishPath::AfterRuntime {
                    runtime: None,
                    stage_emission: FeedbackCycleStageEmission::FromProgress,
                },
            }));
        }
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::ResolveRuntime,
        ))
    }

    async fn handle_resolve_runtime(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let initial_runtime = match self.runtime.resolve(context, &request.input).await {
            Some(runtime) => runtime,
            None => {
                return Ok(FeedbackCycleStep::terminal(FeedbackCycleTerminal {
                    termination: FeedbackCycleTerminationV1::DaemonUnavailable,
                    provider_states: vec![ProviderEvaluationStateV1::Unavailable],
                    baseline_states: Vec::new(),
                    impact: None,
                    impact_state: None,
                    findings: Vec::new(),
                    dedupe_key: None,
                    finish_path: FeedbackCycleFinishPath::AfterRuntime {
                        runtime: None,
                        stage_emission: FeedbackCycleStageEmission::FromProgress,
                    },
                }));
            }
        };
        admitted_progress_mut(progress)?.runtime = Some(initial_runtime);
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::ValidateRuntime,
        ))
    }

    fn handle_validate_runtime(
        &self,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        let runtime = resolved_runtime(progress)?;
        if runtime.validate_for(&request.input).is_err() {
            return Ok(FeedbackCycleStep::terminal(after_runtime_terminal(
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
                progress.runtime.clone(),
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        if !runtime.has_same_root(&request.input) {
            return Ok(FeedbackCycleStep::terminal(after_runtime_terminal(
                FeedbackCycleTerminationV1::Blocked,
                Vec::new(),
                progress.runtime.clone(),
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        if !runtime.is_current_for(&request.input) {
            return Ok(FeedbackCycleStep::terminal(after_runtime_terminal(
                FeedbackCycleTerminationV1::StaleReplanRequired,
                vec![ProviderEvaluationStateV1::Stale],
                progress.runtime.clone(),
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::CheckUserStop,
        ))
    }

    fn handle_check_user_stop(
        &self,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        if request.control == FeedbackCycleControl::UserStop {
            let runtime = admitted_progress(progress)?.runtime.clone();
            return Ok(FeedbackCycleStep::terminal(after_runtime_terminal(
                FeedbackCycleTerminationV1::UserStop,
                Vec::new(),
                runtime,
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::CheckBudgetAndProviders,
        ))
    }

    fn handle_check_budget_and_providers(
        &self,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        progress.completed_stages = vec![FeedbackEvaluationStageV1::Admission];
        if request.usage.exceeds(&request.input) {
            return Ok(FeedbackCycleStep::terminal(after_runtime_terminal(
                FeedbackCycleTerminationV1::BudgetExceeded,
                vec![ProviderEvaluationStateV1::TimedOut],
                progress.runtime.clone(),
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        if request.providers.is_empty() {
            return Ok(FeedbackCycleStep::terminal(after_runtime_terminal(
                FeedbackCycleTerminationV1::Blocked,
                Vec::new(),
                progress.runtime.clone(),
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        progress
            .completed_stages
            .push(FeedbackEvaluationStageV1::Diagnostics);
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::LoadBaselines,
        ))
    }

    async fn handle_load_baselines(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        let runtime = resolved_runtime(progress)?;
        let diagnostics_request = FeedbackDiagnosticsRequest {
            input: request.input.clone(),
            providers: request.providers.clone(),
        };
        // Resolve authoritative history before asking providers for current
        // diagnostics. A known absence of prior history stays explicit and does
        // not manufacture a comparison horizon.
        progress.baselines = if request.input.request.durability() == FeedbackDurabilityV1::Durable
            && runtime.authoritative.baseline_horizon.is_some()
        {
            let baselines = self
                .diagnostics
                .diagnostic_history(context, &diagnostics_request, runtime)
                .await;
            if let Some((termination, states)) = self
                .runtime_override(context, request, progress.runtime.as_ref())
                .await
            {
                return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                    termination,
                    states,
                    Vec::new(),
                    progress.runtime.clone(),
                    FeedbackCycleStageEmission::Suppressed,
                )));
            }
            baselines
        } else {
            Vec::new()
        };
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::LoadDiagnostics,
        ))
    }

    async fn handle_load_diagnostics(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        let diagnostics_request = FeedbackDiagnosticsRequest {
            input: request.input.clone(),
            providers: request.providers.clone(),
        };
        progress.diagnostics = self
            .diagnostics
            .diagnostics(context, &diagnostics_request)
            .await;
        if let Some((termination, states)) = self
            .runtime_override(context, request, progress.runtime.as_ref())
            .await
        {
            return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                termination,
                states,
                Vec::new(),
                progress.runtime.clone(),
                FeedbackCycleStageEmission::Suppressed,
            )));
        }
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::ClassifyDiagnostics,
        ))
    }

    fn handle_classify_diagnostics(
        &self,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
        advisory: &FeedbackCycleAdvisoryV1,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        let runtime = resolved_runtime(progress)?;
        let resolved_baselines = resolve_baselines(request, runtime, &progress.baselines)?;
        progress.baseline_states = resolved_baselines
            .iter()
            .map(|resolved| resolved.state)
            .collect();
        let (mut provider_states, mut findings) =
            collect_diagnostics(request, &progress.diagnostics, &resolved_baselines)?;
        provider_states.extend(advisory.providers.iter().map(|provider| provider.state));
        findings.extend(advisory.findings.iter().cloned());
        if findings.iter().enumerate().any(|(index, finding)| {
            findings[index.saturating_add(1)..]
                .iter()
                .any(|other| other.finding_id == finding.finding_id)
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "feedback cycle finding",
            });
        }
        progress.provider_states = provider_states.clone();
        progress.advisory_provider_states = advisory.providers.clone();
        progress.findings = findings;
        if let Some(termination) =
            terminal_before_impact(&provider_states, &progress.baseline_states)
        {
            return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                termination,
                provider_states,
                progress.baseline_states.clone(),
                progress.runtime.clone(),
                FeedbackCycleStageEmission::FromProgress,
            )));
        }
        progress
            .completed_stages
            .push(FeedbackEvaluationStageV1::BaselineClassification);
        progress
            .completed_stages
            .push(FeedbackEvaluationStageV1::Impact);
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::ResolveImpact,
        ))
    }

    async fn handle_resolve_impact(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        match self.resolve_impact(context, &request.input).await {
            FeedbackImpactResolution::Evidence(impact, state) => {
                progress.impact = *impact;
                progress.impact_state = Some(state);
            }
            FeedbackImpactResolution::Cancelled => {
                return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                    FeedbackCycleTerminationV1::Cancelled,
                    vec![ProviderEvaluationStateV1::Cancelled],
                    Vec::new(),
                    progress.runtime.clone(),
                    FeedbackCycleStageEmission::FromProgress,
                )));
            }
            FeedbackImpactResolution::TimedOut => {
                return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                    FeedbackCycleTerminationV1::BudgetExceeded,
                    vec![ProviderEvaluationStateV1::TimedOut],
                    Vec::new(),
                    progress.runtime.clone(),
                    FeedbackCycleStageEmission::FromProgress,
                )));
            }
        }
        if let Some((termination, states)) = self
            .runtime_override(context, request, progress.runtime.as_ref())
            .await
        {
            return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                termination,
                states,
                Vec::new(),
                progress.runtime.clone(),
                FeedbackCycleStageEmission::Suppressed,
            )));
        }
        progress
            .completed_stages
            .push(FeedbackEvaluationStageV1::AffectedTests);
        progress
            .completed_stages
            .push(FeedbackEvaluationStageV1::ResultAssembly);
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::LookupDedupe,
        ))
    }

    async fn handle_lookup_dedupe(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
        advisory: &FeedbackCycleAdvisoryV1,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        let impact_state = resolved_impact_state(progress)?;
        progress.dedupe_key = if request.input.request.durability() == FeedbackDurabilityV1::Durable
        {
            let runtime = resolved_runtime(progress)?;
            let evidence_identity = canonical_sha256(&(
                "tracedecay.feedback.evidence-identity.v2",
                runtime,
                &progress.diagnostics,
                &progress.baselines,
                &progress.impact,
                impact_state,
                &advisory.providers,
                &advisory.findings,
            ))?;
            let key = request.input.dedupe_key(&evidence_identity)?;
            let dedupe_state = self.dedupe.lookup_completed(context, &key).await;
            if let Some((termination, states)) = self
                .runtime_override(context, request, progress.runtime.as_ref())
                .await
            {
                return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                    termination,
                    states,
                    Vec::new(),
                    progress.runtime.clone(),
                    FeedbackCycleStageEmission::Suppressed,
                )));
            }
            match dedupe_state {
                FeedbackCycleDedupeState::Duplicate => {
                    return Ok(FeedbackCycleStep::terminal(
                        after_checked_runtime_terminal_with_dedupe(
                            FeedbackCycleTerminationV1::DuplicateNoop,
                            Vec::new(),
                            Some(key),
                            progress.runtime.clone(),
                            FeedbackCycleStageEmission::FromProgress,
                        ),
                    ));
                }
                FeedbackCycleDedupeState::Unavailable => {
                    return Ok(FeedbackCycleStep::terminal(
                        after_checked_runtime_terminal_with_dedupe(
                            FeedbackCycleTerminationV1::DaemonUnavailable,
                            vec![ProviderEvaluationStateV1::Unavailable],
                            Some(key),
                            progress.runtime.clone(),
                            FeedbackCycleStageEmission::FromProgress,
                        ),
                    ));
                }
                FeedbackCycleDedupeState::Cancelled => {
                    return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                        FeedbackCycleTerminationV1::Cancelled,
                        vec![ProviderEvaluationStateV1::Cancelled],
                        Vec::new(),
                        progress.runtime.clone(),
                        FeedbackCycleStageEmission::FromProgress,
                    )));
                }
                FeedbackCycleDedupeState::TimedOut => {
                    return Ok(FeedbackCycleStep::terminal(after_checked_runtime_terminal(
                        FeedbackCycleTerminationV1::BudgetExceeded,
                        vec![ProviderEvaluationStateV1::TimedOut],
                        Vec::new(),
                        progress.runtime.clone(),
                        FeedbackCycleStageEmission::FromProgress,
                    )));
                }
                FeedbackCycleDedupeState::Unique => Some(key),
            }
        } else {
            None
        };
        Ok(FeedbackCycleStep::continue_with(
            FeedbackCycleStage::AssembleResult,
        ))
    }

    async fn handle_assemble_result(
        &self,
        context: &RequestContext,
        progress: &mut Option<FeedbackCycleProgress>,
        request: &FeedbackCycleExecutionRequest,
    ) -> Result<FeedbackCycleStep, ApplicationContractError> {
        let progress = admitted_progress_mut(progress)?;
        let impact_state = resolved_impact_state(progress)?;
        let affected_tests_state = progress
            .impact
            .as_ref()
            .map(|impact| impact.affected_tests_state)
            .unwrap_or(impact_state);
        let termination = determine_termination(
            &progress.provider_states,
            &progress.baseline_states,
            &progress.findings,
            impact_state,
            affected_tests_state,
            request.input.request.durability(),
        );
        let result = self
            .finish_after_checked_runtime(
                context,
                request,
                &progress.admission,
                progress.runtime.as_ref(),
                progress.dedupe_key.clone(),
                termination,
                progress.provider_states.clone(),
                progress.advisory_provider_states.clone(),
                progress.baseline_states.clone(),
                progress.impact.clone(),
                Some(impact_state),
                progress.findings.clone(),
                &progress.completed_stages,
            )
            .await?;
        Ok(FeedbackCycleStep::Complete(Box::new(result)))
    }

    async fn finish_terminal(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        progress: Option<&FeedbackCycleProgress>,
        terminal: FeedbackCycleTerminal,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        let completed_stages = match (&terminal.finish_path, progress) {
            (FeedbackCycleFinishPath::Immediate, _) => &[][..],
            (
                FeedbackCycleFinishPath::AfterRuntime {
                    stage_emission: FeedbackCycleStageEmission::Suppressed,
                    ..
                }
                | FeedbackCycleFinishPath::AfterCheckedRuntime {
                    stage_emission: FeedbackCycleStageEmission::Suppressed,
                    ..
                },
                _,
            ) => &[][..],
            (
                FeedbackCycleFinishPath::AfterRuntime {
                    stage_emission: FeedbackCycleStageEmission::FromProgress,
                    ..
                }
                | FeedbackCycleFinishPath::AfterCheckedRuntime {
                    stage_emission: FeedbackCycleStageEmission::FromProgress,
                    ..
                },
                Some(progress),
            ) => progress.completed_stages.as_slice(),
            (
                FeedbackCycleFinishPath::AfterRuntime { .. }
                | FeedbackCycleFinishPath::AfterCheckedRuntime { .. },
                None,
            ) => &[][..],
        };
        let advisory_provider_states = progress.map_or_else(Vec::new, |progress| {
            progress.advisory_provider_states.clone()
        });

        match terminal.finish_path {
            FeedbackCycleFinishPath::Immediate => self.finish(
                request,
                terminal.dedupe_key,
                terminal.termination,
                terminal.provider_states,
                advisory_provider_states,
                terminal.baseline_states,
                terminal.impact,
                terminal.impact_state,
                terminal.findings,
                None,
            ),
            FeedbackCycleFinishPath::AfterRuntime { runtime, .. } => {
                let admission = &progress
                    .ok_or(ApplicationContractError::Inconsistent {
                        field: "feedback cycle admission state",
                    })?
                    .admission;
                self.finish_after_runtime(
                    context,
                    request,
                    admission,
                    runtime.as_ref(),
                    terminal.dedupe_key,
                    terminal.termination,
                    terminal.provider_states,
                    advisory_provider_states,
                    terminal.baseline_states,
                    terminal.impact,
                    terminal.impact_state,
                    terminal.findings,
                    completed_stages,
                )
                .await
            }
            FeedbackCycleFinishPath::AfterCheckedRuntime { runtime, .. } => {
                let admission = &progress
                    .ok_or(ApplicationContractError::Inconsistent {
                        field: "feedback cycle admission state",
                    })?
                    .admission;
                self.finish_after_checked_runtime(
                    context,
                    request,
                    admission,
                    runtime.as_ref(),
                    terminal.dedupe_key,
                    terminal.termination,
                    terminal.provider_states,
                    advisory_provider_states,
                    terminal.baseline_states,
                    terminal.impact,
                    terminal.impact_state,
                    terminal.findings,
                    completed_stages,
                )
                .await
            }
        }
    }

    async fn resolve_impact(
        &self,
        context: &RequestContext,
        input: &FeedbackEvaluationInputV1,
    ) -> FeedbackImpactResolution {
        match self
            .impact
            .impact(
                context,
                &FeedbackImpactRequest {
                    input: input.clone(),
                },
            )
            .await
        {
            FeedbackImpactPortOutcome::Complete(impact)
                if impact.state == FeedbackImpactStateV1::Complete
                    && impact.target == input.target
                    && (input.request.durability() == FeedbackDurabilityV1::Durable
                        || impact.evidence_anchors.is_empty())
                    && impact.validate().is_ok() =>
            {
                FeedbackImpactResolution::Evidence(
                    Box::new(Some(impact)),
                    FeedbackImpactStateV1::Complete,
                )
            }
            FeedbackImpactPortOutcome::Partial(impact)
                if impact.state == FeedbackImpactStateV1::Partial
                    && impact.target == input.target
                    && (input.request.durability() == FeedbackDurabilityV1::Durable
                        || impact.evidence_anchors.is_empty())
                    && impact.validate().is_ok() =>
            {
                FeedbackImpactResolution::Evidence(
                    Box::new(Some(impact)),
                    FeedbackImpactStateV1::Partial,
                )
            }
            FeedbackImpactPortOutcome::Stale => {
                FeedbackImpactResolution::Evidence(Box::new(None), FeedbackImpactStateV1::Stale)
            }
            FeedbackImpactPortOutcome::Cancelled => FeedbackImpactResolution::Cancelled,
            FeedbackImpactPortOutcome::TimedOut => FeedbackImpactResolution::TimedOut,
            FeedbackImpactPortOutcome::Unavailable => FeedbackImpactResolution::Evidence(
                Box::new(None),
                FeedbackImpactStateV1::Unavailable,
            ),
            FeedbackImpactPortOutcome::Complete(_) | FeedbackImpactPortOutcome::Partial(_) => {
                FeedbackImpactResolution::Evidence(
                    Box::new(None),
                    FeedbackImpactStateV1::Unavailable,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_after_runtime(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        admission: &FeedbackRouteAdmission,
        initial_runtime: Option<&FeedbackRuntimeStateV1>,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        advisory_provider_states: Vec<FeedbackAdvisoryProviderStateV1>,
        baseline_states: Vec<FeedbackBaselineStateV1>,
        impact: Option<FeedbackImpactV1>,
        impact_state: Option<FeedbackImpactStateV1>,
        findings: Vec<FeedbackFindingV1>,
        completed_stages: &[FeedbackEvaluationStageV1],
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        self.finish_after_checked_runtime(
            context,
            request,
            admission,
            initial_runtime,
            dedupe_key,
            termination,
            provider_states,
            advisory_provider_states,
            baseline_states,
            impact,
            impact_state,
            findings,
            completed_stages,
        )
        .await
    }

    async fn runtime_override(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        initial_runtime: Option<&FeedbackRuntimeStateV1>,
    ) -> Option<(FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>)> {
        if let Some(interruption) = request_interruption(context, request) {
            return Some(interruption);
        }
        match self.runtime.resolve(context, &request.input).await {
            None => Some((
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
            )),
            Some(latest_runtime) if latest_runtime.validate_for(&request.input).is_err() => Some((
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
            )),
            Some(latest_runtime) if !latest_runtime.has_same_root(&request.input) => {
                Some((FeedbackCycleTerminationV1::Blocked, Vec::new()))
            }
            Some(latest_runtime)
                if !latest_runtime.is_current_for(&request.input)
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
    async fn finish_after_checked_runtime(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        admission: &FeedbackRouteAdmission,
        initial_runtime: Option<&FeedbackRuntimeStateV1>,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        advisory_provider_states: Vec<FeedbackAdvisoryProviderStateV1>,
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
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                    None,
                );
            }
        };
        self.emit_trigger(&request.input);
        for stage in completed_stages {
            self.emit_stage(&request.input, *stage);
        }
        let runtime_override = self
            .runtime_override(context, request, initial_runtime)
            .await;
        if let Some((termination, states)) = runtime_override {
            return self.finish(
                request,
                None,
                termination,
                states,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                Some(authority),
            );
        }

        let authority = Some(authority);
        let result = self.assemble(
            request,
            dedupe_key,
            termination,
            provider_states,
            advisory_provider_states,
            baseline_states,
            impact,
            impact_state,
            findings,
            authority.clone(),
        )?;
        let result = self
            .record_completed_publication(context, request, initial_runtime, result, authority)
            .await?;

        if result.cycle.termination == FeedbackCycleTerminationV1::DuplicateNoop
            && let Some(key) = result.dedupe_key.clone()
        {
            self.emit_dedupe(&request.input, key);
        }
        self.emit_completion(&request.input, &result);
        Ok(result)
    }

    async fn record_completed_publication(
        &self,
        context: &RequestContext,
        request: &FeedbackCycleExecutionRequest,
        initial_runtime: Option<&FeedbackRuntimeStateV1>,
        result: FeedbackCycleExecutionResult,
        authority: Option<AuthorityReceipt>,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        let Some(dedupe_key) = result.dedupe_key.clone() else {
            return Ok(result);
        };
        if !is_recordable_completed_publication(result.cycle.termination) {
            return Ok(result);
        }
        let Some(runtime) = initial_runtime else {
            return Ok(result);
        };
        let publication = FeedbackCompletedPublicationV1::new(
            request.input.clone(),
            dedupe_key.clone(),
            result.cycle.clone(),
            runtime.clone(),
            context.scope().clone(),
            authority
                .clone()
                .ok_or(ApplicationContractError::Inconsistent {
                    field: "feedback completed publication authority",
                })?,
        )?;
        match self.dedupe.record_completed(context, &publication).await {
            FeedbackCycleDedupePublicationState::Recorded => Ok(FeedbackCycleExecutionResult {
                publication: Some(publication),
                ..result
            }),
            FeedbackCycleDedupePublicationState::Duplicate => self.assemble(
                request,
                Some(dedupe_key),
                FeedbackCycleTerminationV1::DuplicateNoop,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                authority,
            ),
            FeedbackCycleDedupePublicationState::Cancelled => self.assemble(
                request,
                None,
                FeedbackCycleTerminationV1::Cancelled,
                vec![ProviderEvaluationStateV1::Cancelled],
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                authority,
            ),
            FeedbackCycleDedupePublicationState::TimedOut => self.assemble(
                request,
                None,
                FeedbackCycleTerminationV1::BudgetExceeded,
                vec![ProviderEvaluationStateV1::TimedOut],
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                authority,
            ),
            FeedbackCycleDedupePublicationState::Unavailable => self.assemble(
                request,
                Some(dedupe_key),
                FeedbackCycleTerminationV1::DaemonUnavailable,
                vec![ProviderEvaluationStateV1::Unavailable],
                Vec::new(),
                Vec::new(),
                None,
                None,
                Vec::new(),
                authority,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        &self,
        request: &FeedbackCycleExecutionRequest,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        advisory_provider_states: Vec<FeedbackAdvisoryProviderStateV1>,
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
        let cycle = FeedbackCycleResultV1::new_with_advisory_provider_states(
            &request.input.request,
            termination,
            provider_states,
            advisory_provider_states,
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
        Ok(FeedbackCycleExecutionResult {
            cycle,
            dedupe_key,
            authority,
            usage: request.usage,
            publication: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        request: &FeedbackCycleExecutionRequest,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        advisory_provider_states: Vec<FeedbackAdvisoryProviderStateV1>,
        baseline_states: Vec<FeedbackBaselineStateV1>,
        impact: Option<FeedbackImpactV1>,
        impact_state: Option<FeedbackImpactStateV1>,
        findings: Vec<FeedbackFindingV1>,
        authority: Option<AuthorityReceipt>,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        let result = self.assemble(
            request,
            dedupe_key,
            termination,
            provider_states,
            advisory_provider_states,
            baseline_states,
            impact,
            impact_state,
            findings,
            authority,
        )?;
        self.emit_completion(&request.input, &result);
        Ok(result)
    }

    fn emit_completion(
        &self,
        input: &FeedbackEvaluationInputV1,
        result: &FeedbackCycleExecutionResult,
    ) {
        if result.authority.is_some() {
            self.emit_terminal(input, result.cycle.termination);
            self.emit_latency(input, result.usage.elapsed_micros(input));
        }
    }

    fn emit_trigger(&self, input: &FeedbackEvaluationInputV1) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) = FeedbackCycleObservationV1::trigger(input)
        {
            self.observations.observe(input, observation);
        }
    }

    fn emit_stage(&self, input: &FeedbackEvaluationInputV1, stage: FeedbackEvaluationStageV1) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) = FeedbackCycleObservationV1::stage(input, stage)
        {
            self.observations.observe(input, observation);
        }
    }

    fn emit_dedupe(&self, input: &FeedbackEvaluationInputV1, dedupe_key: FeedbackDedupeKeyV1) {
        if input.request.durability() == FeedbackDurabilityV1::Durable
            && let Ok(observation) =
                FeedbackCycleObservationV1::dedupe_suppressed(input, dedupe_key)
        {
            self.observations.observe(input, observation);
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
            self.observations.observe(input, observation);
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
            self.observations.observe(input, observation);
        }
    }
}

enum FeedbackImpactResolution {
    Evidence(Box<Option<FeedbackImpactV1>>, FeedbackImpactStateV1),
    Cancelled,
    TimedOut,
}

fn request_interruption(
    context: &RequestContext,
    request: &FeedbackCycleExecutionRequest,
) -> Option<(FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>)> {
    match context.admission_at(request.usage.completed_at) {
        RequestAdmission::Admitted => None,
        RequestAdmission::Cancelled => Some((
            FeedbackCycleTerminationV1::Cancelled,
            vec![ProviderEvaluationStateV1::Cancelled],
        )),
        RequestAdmission::TimedOut => Some((
            FeedbackCycleTerminationV1::BudgetExceeded,
            vec![ProviderEvaluationStateV1::TimedOut],
        )),
    }
}

fn is_recordable_completed_publication(termination: FeedbackCycleTerminationV1) -> bool {
    matches!(
        termination,
        FeedbackCycleTerminationV1::Clean | FeedbackCycleTerminationV1::Blocked
    )
}

fn scope_matches(context: &RequestContext, input: &FeedbackEvaluationInputV1) -> bool {
    let scope = context.scope();
    scope.project_id == input.request.scope.project_id
        && scope.repository_id == input.request.scope.repository_id
        && scope.worktree_id == input.request.scope.worktree_id
        && scope
            .reference
            .as_ref()
            .is_some_and(|reference| reference.as_str() == input.request.scope.branch_ref)
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

fn resolve_baselines<'a>(
    request: &FeedbackCycleExecutionRequest,
    runtime: &FeedbackRuntimeStateV1,
    baselines: &'a [FeedbackDiagnosticBaselineV1],
) -> Result<Vec<ResolvedBaseline<'a>>, ApplicationContractError> {
    if request.input.request.durability() == FeedbackDurabilityV1::SessionOnly {
        return Ok(Vec::new());
    }
    if runtime.authoritative.baseline_horizon.is_none() {
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
        let expected = feedback_baseline_identity(&request.input, runtime, provider)?;
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
                            safe_bounded_preview: Some(truncate_at_char_boundary(
                                &diagnostic.message,
                                512,
                            )),
                            diagnostic_projection: None,
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
                            diagnostic_projection: None,
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

/// The shape every early terminal shares: the cycle stopped before it could
/// produce impact or findings, so only the termination, the provider states,
/// and how far the cycle got are known.
fn early_terminal(
    termination: FeedbackCycleTerminationV1,
    provider_states: Vec<ProviderEvaluationStateV1>,
    finish_path: FeedbackCycleFinishPath,
) -> FeedbackCycleTerminal {
    FeedbackCycleTerminal {
        termination,
        provider_states,
        baseline_states: Vec::new(),
        impact: None,
        impact_state: None,
        findings: Vec::new(),
        dedupe_key: None,
        finish_path,
    }
}

fn after_runtime_terminal(
    termination: FeedbackCycleTerminationV1,
    provider_states: Vec<ProviderEvaluationStateV1>,
    runtime: Option<FeedbackRuntimeStateV1>,
    stage_emission: FeedbackCycleStageEmission,
) -> FeedbackCycleTerminal {
    early_terminal(
        termination,
        provider_states,
        FeedbackCycleFinishPath::AfterRuntime {
            runtime,
            stage_emission,
        },
    )
}

fn after_checked_runtime_terminal(
    termination: FeedbackCycleTerminationV1,
    provider_states: Vec<ProviderEvaluationStateV1>,
    baseline_states: Vec<FeedbackBaselineStateV1>,
    runtime: Option<FeedbackRuntimeStateV1>,
    stage_emission: FeedbackCycleStageEmission,
) -> FeedbackCycleTerminal {
    FeedbackCycleTerminal {
        baseline_states,
        ..early_terminal(
            termination,
            provider_states,
            FeedbackCycleFinishPath::AfterCheckedRuntime {
                runtime,
                stage_emission,
            },
        )
    }
}

fn after_checked_runtime_terminal_with_dedupe(
    termination: FeedbackCycleTerminationV1,
    provider_states: Vec<ProviderEvaluationStateV1>,
    dedupe_key: Option<FeedbackDedupeKeyV1>,
    runtime: Option<FeedbackRuntimeStateV1>,
    stage_emission: FeedbackCycleStageEmission,
) -> FeedbackCycleTerminal {
    FeedbackCycleTerminal {
        dedupe_key,
        ..early_terminal(
            termination,
            provider_states,
            FeedbackCycleFinishPath::AfterCheckedRuntime {
                runtime,
                stage_emission,
            },
        )
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

    #[test]
    fn projectionless_advisory_findings_require_a_represented_provider_state() {
        assert!(FeedbackCycleAdvisoryV1::default().validate().is_ok());
        let projectionless_finding = FeedbackFindingV1 {
            finding_id: tracedecay_domain::feedback::FeedbackFindingId::new(
                "finding.advisory.projectionless",
            )
            .unwrap(),
            classification: FeedbackDiagnosticClassificationV1::Unknown,
            lifecycle: FeedbackFindingLifecycleV1::Active,
            retrieval_anchor_id: None,
            provider_state: ProviderEvaluationStateV1::Partial,
            safe_bounded_preview: None,
            diagnostic_projection: None,
        };
        assert!(
            FeedbackCycleAdvisoryV1 {
                providers: Vec::new(),
                findings: vec![projectionless_finding.clone()],
            }
            .validate()
            .is_err()
        );
        assert!(
            FeedbackCycleAdvisoryV1 {
                providers: vec![FeedbackAdvisoryProviderStateV1 {
                    producer:
                        tracedecay_domain::feedback::FeedbackDiagnosticProducerV1::GitHubReview,
                    state: ProviderEvaluationStateV1::Unavailable,
                }],
                findings: vec![projectionless_finding.clone()],
            }
            .validate()
            .is_err(),
            "a projection-less finding cannot claim an unrepresented provider state"
        );
        let mut ci_finding = projectionless_finding.clone();
        ci_finding.finding_id = tracedecay_domain::feedback::FeedbackFindingId::new(
            "finding.advisory.projectionless-ci",
        )
        .unwrap();
        ci_finding.provider_state = ProviderEvaluationStateV1::Unavailable;
        let mut proximity_finding = projectionless_finding.clone();
        proximity_finding.finding_id = tracedecay_domain::feedback::FeedbackFindingId::new(
            "finding.advisory.projectionless-proximity",
        )
        .unwrap();
        proximity_finding.provider_state = ProviderEvaluationStateV1::Failed;
        assert!(
            FeedbackCycleAdvisoryV1 {
                providers: vec![
                    FeedbackAdvisoryProviderStateV1 {
                        producer: tracedecay_domain::feedback::FeedbackDiagnosticProducerV1::GitHubReview,
                        state: ProviderEvaluationStateV1::Partial,
                    },
                    FeedbackAdvisoryProviderStateV1 {
                        producer: tracedecay_domain::feedback::FeedbackDiagnosticProducerV1::CiLocalization,
                        state: ProviderEvaluationStateV1::Unavailable,
                    },
                    FeedbackAdvisoryProviderStateV1 {
                        producer: tracedecay_domain::feedback::FeedbackDiagnosticProducerV1::Proximity,
                        state: ProviderEvaluationStateV1::Failed,
                    },
                ],
                findings: vec![projectionless_finding, ci_finding, proximity_finding],
            }
            .validate()
            .is_ok(),
            "resolved GitHub, symbol-less CI, and address-less proximity findings retain represented provider state without a diagnostic projection"
        );
    }
}
