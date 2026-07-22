//! Concrete, one-shot PR12 feedback-cycle composition.
//!
//! The runtime only composes existing application ports and direct graph
//! queries. It owns no provider lifecycle, source write, or second feedback
//! store.

use std::sync::Arc;

use thiserror::Error;
use tokio::runtime::Handle;
use tracedecay_application::diagnostics::{
    AnalyzerAdmittedDiagnosticProviderV1, DiagnosticProviderIdentity,
};
use tracedecay_application::feedback::{
    FeedbackCycleAdvisoryV1, FeedbackCycleExecutionRequest, FeedbackCycleExecutionResult,
    FeedbackCycleService, FeedbackImpactPort, FeedbackImpactPortOutcome, FeedbackImpactRequest,
    FeedbackObservationPort, FeedbackPortFuture, FeedbackRuntimeStatePort, FeedbackRuntimeStateV1,
    GenerationBoundFeedbackDiagnosticsAdapter,
};
use tracedecay_application::retrieval::{
    AffectedTestsRequest, AffectedTestsResult, AffectedTestsRetrievalPort, PageRequest,
    ResultProjection, RetrievalOrder, RetrievalPortContext, RetrievalPortOutcome,
    RetrievalRequestMeta,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationOperation, CoverageCompleteness, FreshnessState,
    PolicyEvaluationV1, RequestAdmission, RequestContext,
};
use tracedecay_domain::feedback::{
    FeedbackDurabilityV1, FeedbackImpactStateV1, FeedbackImpactV1, FeedbackTriggerV1,
};
use tracedecay_domain::{FileOccurrenceId, SymbolOccurrenceId};
use tracedecay_policy::CapabilityRoutingDecisionV1;

use crate::daemon::lsp_gateway::{
    DiagnosticTrigger, FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleRuntimePort,
    LspRuntimeFailure, LspRuntimeFuture, Pr12FeedbackCycleAdapter,
};
use crate::db::Database;
use crate::tracedecay::TraceDecay;

use super::concrete::{
    Pr12FeedbackRuntime, ProjectFeedbackRouteAuthorization, ProjectFeedbackStore,
};
use super::diagnostics::{DatabaseDiagnosticStore, DiagnosticStoreFeedbackProvider};
use super::observations::{
    Plan26DeliveryRouteV1, Plan26FeedbackObservationEmitterV1, Plan26FeedbackOperationV1,
    Plan26FeedbackOutcomeV1, Plan26FeedbackSourceEventV1,
};

/// Resolves one LSP lifecycle request to the already-authorized, bounded
/// application input. The caller owns URI-to-identity resolution, cancellation,
/// deadline, and budget measurement.
pub type Pr12FeedbackCycleLspInput = Arc<
    dyn Fn(
            FeedbackCycleRequest,
        ) -> LspRuntimeFuture<Result<Pr12FeedbackCycleInvocation, LspRuntimeFailure>>
        + Send
        + Sync,
>;

/// Complete input for exactly one canonical feedback-cycle invocation.
#[derive(Clone)]
pub struct Pr12FeedbackCycleInvocation {
    pub context: RequestContext,
    pub request: FeedbackCycleExecutionRequest,
}

impl Pr12FeedbackCycleInvocation {
    pub fn new(
        context: RequestContext,
        request: FeedbackCycleExecutionRequest,
    ) -> Result<Self, Pr12FeedbackCycleRuntimeError> {
        let invocation = Self { context, request };
        invocation.validate()?;
        Ok(invocation)
    }

    pub fn validate(&self) -> Result<(), Pr12FeedbackCycleRuntimeError> {
        self.context.validate()?;
        self.request.validate()?;
        if !matches!(
            self.request.input.request.trigger,
            FeedbackTriggerV1::PostEditHook
                | FeedbackTriggerV1::DocumentSave
                | FeedbackTriggerV1::ExplicitDiagnostics
        ) {
            return Err(Pr12FeedbackCycleRuntimeError::UnsupportedTrigger);
        }
        if self.request.input.request.durability() != FeedbackDurabilityV1::Durable {
            return Err(Pr12FeedbackCycleRuntimeError::NonDurableRequest);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Pr12FeedbackCycleRuntimeError {
    #[error("feedback cycle contract is invalid")]
    Contract(#[from] ApplicationContractError),
    #[error("feedback cycle requires at least one managed diagnostic provider")]
    NoManagedDiagnosticProviders,
    #[error("feedback cycle request provider identities differ from its admission set")]
    ProviderSetMismatch,
    #[error("feedback cycle trigger is not supported by PR12")]
    UnsupportedTrigger,
    #[error("PR12 feedback cycles require durable saved content")]
    NonDurableRequest,
}

impl Pr12FeedbackCycleRuntimeError {
    fn lsp_failure_class(&self) -> &'static str {
        match self {
            Self::Contract(_) => "feedback-cycle-contract",
            Self::NoManagedDiagnosticProviders => "feedback-cycle-provider-missing",
            Self::ProviderSetMismatch => "feedback-cycle-provider-mismatch",
            Self::UnsupportedTrigger => "feedback-cycle-trigger-unsupported",
            Self::NonDurableRequest => "feedback-cycle-non-durable",
        }
    }
}

type Pr12FeedbackCycleService = FeedbackCycleService<
    SharedFeedbackRuntimeState,
    GenerationBoundFeedbackDiagnosticsAdapter<
        DiagnosticStoreFeedbackProvider<DatabaseDiagnosticStore>,
    >,
    DirectFeedbackImpactAdapter,
    ProjectFeedbackStore,
    SharedFeedbackObservations,
    ProjectFeedbackRouteAuthorization,
>;

/// Concrete Plan 09 runtime for PR12 saved-edit and explicit-diagnostic
/// cycles. Every invocation delegates once to [`FeedbackCycleService`], whose
/// existing durable compare-and-record boundary publishes the terminal result.
#[derive(Clone)]
pub struct Pr12FeedbackCycleRuntime {
    feedback: Arc<Pr12FeedbackRuntime>,
    publications: ProjectFeedbackStore,
    service: Arc<Pr12FeedbackCycleService>,
    lsp_input: Pr12FeedbackCycleLspInput,
    providers: Vec<DiagnosticProviderIdentity>,
    correlation_policy: PolicyEvaluationV1<CapabilityRoutingDecisionV1>,
    source_observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

/// Opens the one concrete PR12 feedback-cycle owner from already-open project
/// authorities. Diagnostics are bound directly to the project database,
/// graph/test queries retain their existing services, and publication reuses
/// the exact store and route authorization owned by `feedback`.
#[allow(clippy::too_many_arguments)]
pub fn open_pr12_feedback_cycle_runtime(
    database: Database,
    feedback: Arc<Pr12FeedbackRuntime>,
    runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
    correlation_policy: PolicyEvaluationV1<CapabilityRoutingDecisionV1>,
    provider_admissions: Vec<AnalyzerAdmittedDiagnosticProviderV1>,
    graph: Arc<TraceDecay>,
    affected_tests: Arc<dyn AffectedTestsRetrievalPort + Send + Sync>,
    observations: Arc<dyn FeedbackObservationPort + Send + Sync>,
    operation: ApplicationOperation,
    graph_operation: ApplicationOperation,
    tests_operation: ApplicationOperation,
    lsp_input: Pr12FeedbackCycleLspInput,
) -> Result<Arc<Pr12FeedbackCycleRuntime>, Pr12FeedbackCycleRuntimeError> {
    if provider_admissions.is_empty() {
        return Err(Pr12FeedbackCycleRuntimeError::NoManagedDiagnosticProviders);
    }

    let publications = feedback.publication_store();
    let source_observations = feedback.source_observation_port();
    let providers = provider_admissions
        .iter()
        .map(|provider| provider.identity().clone())
        .collect::<Vec<_>>();
    let diagnostics = GenerationBoundFeedbackDiagnosticsAdapter::new(
        DiagnosticStoreFeedbackProvider::new(DatabaseDiagnosticStore::new(database)),
        provider_admissions,
    )?;
    let route_authorization = feedback.route_authorization();
    let impact = DirectFeedbackImpactAdapter::new(
        graph,
        SharedAffectedTests(affected_tests),
        route_authorization.clone(),
        graph_operation,
        tests_operation,
    );
    let service = FeedbackCycleService::new(
        SharedFeedbackRuntimeState(runtime_state),
        diagnostics,
        impact,
        publications.clone(),
        SharedFeedbackObservations(observations),
        route_authorization,
        operation,
    );

    Ok(Arc::new(Pr12FeedbackCycleRuntime {
        feedback,
        publications,
        service: Arc::new(service),
        lsp_input,
        providers,
        correlation_policy,
        source_observations,
    }))
}

impl Pr12FeedbackCycleRuntime {
    pub fn feedback_runtime(&self) -> Arc<Pr12FeedbackRuntime> {
        Arc::clone(&self.feedback)
    }

    /// The same durable store used by the completed-publication dedupe port.
    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    pub fn provider_identities(&self) -> &[DiagnosticProviderIdentity] {
        &self.providers
    }

    pub fn correlation_policy(&self) -> &PolicyEvaluationV1<CapabilityRoutingDecisionV1> {
        &self.correlation_policy
    }

    pub fn source_observation_port(
        &self,
    ) -> Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync> {
        Arc::clone(&self.source_observations)
    }

    /// Input for `ConcretePr12FeedbackLspSource` to share this cycle with
    /// managed diagnostics and context projections.
    pub fn context_projection_input(self: &Arc<Self>) -> Arc<dyn FeedbackCycleRuntimePort> {
        self.clone()
    }

    /// Runs exactly one bounded feedback cycle and returns its terminal,
    /// canonical result. It never schedules retries or follow-up work.
    pub async fn run_once(
        &self,
        invocation: Pr12FeedbackCycleInvocation,
    ) -> Result<FeedbackCycleExecutionResult, Pr12FeedbackCycleRuntimeError> {
        invocation.validate()?;
        if invocation.request.providers.as_slice() != self.providers.as_slice() {
            return Err(Pr12FeedbackCycleRuntimeError::ProviderSetMismatch);
        }
        let Pr12FeedbackCycleInvocation { context, request } = invocation;
        Ok(self.service.execute(&context, request).await?)
    }

    /// Runs one canonical Plan 09 cycle with source-backed advisory findings.
    /// It reuses this runtime's authorization, diagnostics, impact, and single
    /// durable publication/dedupe path.
    pub async fn run_once_with_advisory(
        &self,
        context: &RequestContext,
        request: FeedbackCycleExecutionRequest,
        advisory: FeedbackCycleAdvisoryV1,
    ) -> Result<FeedbackCycleExecutionResult, ApplicationContractError> {
        if request.providers.as_slice() != self.providers.as_slice() {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback cycle provider set",
            });
        }
        self.service
            .execute_with_advisory(context, request, advisory)
            .await
    }

    /// Builds the LSP-facing trigger port and the runtime input used by the
    /// shared diagnostics/context projection source.
    pub fn lsp_registration(self: &Arc<Self>, runtime: Handle) -> Pr12FeedbackCycleLspRegistration {
        let context_projection_input = self.context_projection_input();
        let feedback_adapter = Arc::new(Pr12FeedbackCycleAdapter::new(
            runtime,
            context_projection_input.clone(),
        ));
        Pr12FeedbackCycleLspRegistration {
            feedback_adapter,
            context_projection_input,
        }
    }
}

impl FeedbackCycleRuntimePort for Pr12FeedbackCycleRuntime {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let runtime = self.clone();
        Box::pin(async move {
            let trigger = request.trigger;
            let invocation = (runtime.lsp_input)(request).await?;
            if !lsp_trigger_matches_invocation(trigger, &invocation) {
                runtime.source_observations.observe_source_event(
                    &invocation.request.input,
                    Plan26FeedbackSourceEventV1::ArgumentRejected {
                        operation: Plan26FeedbackOperationV1::LspSession,
                        outcome: Plan26FeedbackOutcomeV1::Rejected,
                    },
                );
                return Err(LspRuntimeFailure::new("feedback-cycle-trigger-mismatch"));
            }
            let input = invocation.request.input.clone();
            let result = runtime.run_once(invocation).await;
            runtime.source_observations.observe_source_event(
                &input,
                Plan26FeedbackSourceEventV1::Delivery {
                    operation: Plan26FeedbackOperationV1::FeedbackCycle,
                    route: Plan26DeliveryRouteV1::Lsp,
                    outcome: if result.is_ok() {
                        Plan26FeedbackOutcomeV1::Completed
                    } else {
                        Plan26FeedbackOutcomeV1::Failed
                    },
                    item_count: u32::from(result.is_ok()),
                    duration_micros: None,
                },
            );
            result
                .map(|_| ())
                .map_err(|error| LspRuntimeFailure::new(error.lsp_failure_class()))
        })
    }
}

/// Registration output for the LSP gateway and the shared context-projection
/// source. Both handles delegate to the same concrete runtime and store.
#[derive(Clone)]
pub struct Pr12FeedbackCycleLspRegistration {
    feedback_adapter: Arc<Pr12FeedbackCycleAdapter>,
    context_projection_input: Arc<dyn FeedbackCycleRuntimePort>,
}

impl Pr12FeedbackCycleLspRegistration {
    pub fn feedback_adapter(&self) -> Arc<Pr12FeedbackCycleAdapter> {
        Arc::clone(&self.feedback_adapter)
    }

    pub fn feedback_port(&self) -> Arc<dyn FeedbackCyclePort + Send + Sync> {
        let port: Arc<dyn FeedbackCyclePort + Send + Sync> = self.feedback_adapter();
        port
    }

    pub fn context_projection_input(&self) -> Arc<dyn FeedbackCycleRuntimePort> {
        Arc::clone(&self.context_projection_input)
    }
}

struct SharedFeedbackRuntimeState(Arc<dyn FeedbackRuntimeStatePort + Send + Sync>);

impl FeedbackRuntimeStatePort for SharedFeedbackRuntimeState {
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a tracedecay_domain::feedback::FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>> {
        self.0.resolve(context, input)
    }
}

struct DirectFeedbackImpactAdapter {
    graph: Arc<TraceDecay>,
    tests: SharedAffectedTests,
    authorization: ProjectFeedbackRouteAuthorization,
    graph_operation: ApplicationOperation,
    tests_operation: ApplicationOperation,
}

impl DirectFeedbackImpactAdapter {
    fn new(
        graph: Arc<TraceDecay>,
        tests: SharedAffectedTests,
        authorization: ProjectFeedbackRouteAuthorization,
        graph_operation: ApplicationOperation,
        tests_operation: ApplicationOperation,
    ) -> Self {
        Self {
            graph,
            tests,
            authorization,
            graph_operation,
            tests_operation,
        }
    }
}

impl FeedbackImpactPort for DirectFeedbackImpactAdapter {
    fn impact<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackImpactRequest,
    ) -> FeedbackPortFuture<'a, FeedbackImpactPortOutcome> {
        Box::pin(async move {
            if request.validate().is_err()
                || !self.authorization.allows(
                    context,
                    &self.graph_operation,
                    request.input.observed_at,
                )
            {
                return FeedbackImpactPortOutcome::Unavailable;
            }
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }
            let Some(symbol) = request.input.target.symbol.clone() else {
                return FeedbackImpactPortOutcome::Unavailable;
            };
            let subgraph = match self.graph.get_impact_radius(symbol.as_str(), 3).await {
                Ok(subgraph) => subgraph,
                Err(_) => return FeedbackImpactPortOutcome::Unavailable,
            };
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }

            let mut affected_files = subgraph
                .nodes
                .iter()
                .filter_map(|node| FileOccurrenceId::new(node.file_path.clone()).ok())
                .collect::<Vec<_>>();
            affected_files.sort();
            affected_files.dedup();
            let mut affected_callers = subgraph
                .nodes
                .iter()
                .filter(|node| node.id.as_str() != symbol.as_str())
                .filter_map(|node| SymbolOccurrenceId::new(node.id.clone()).ok())
                .collect::<Vec<_>>();
            affected_callers.sort();
            affected_callers.dedup();

            let meta = RetrievalRequestMeta::current(
                PageRequest::first(100).expect("static feedback page size is valid"),
                ResultProjection::ReferencesOnly,
                RetrievalOrder::StableIdentity,
            );
            if !self
                .authorization
                .allows(context, &self.tests_operation, request.input.observed_at)
            {
                return FeedbackImpactPortOutcome::Unavailable;
            }
            let tests = self.tests.affected_tests(
                &RetrievalPortContext {
                    request: context,
                    operation: &self.tests_operation,
                },
                &AffectedTestsRequest { symbol, meta },
            );
            let (affected_tests, affected_tests_state) = match affected_tests_outcome(tests) {
                DirectAffectedTestsOutcome::Evidence { tests, state } => (tests, state),
                DirectAffectedTestsOutcome::Cancelled => {
                    return FeedbackImpactPortOutcome::Cancelled;
                }
                DirectAffectedTestsOutcome::TimedOut => {
                    return FeedbackImpactPortOutcome::TimedOut;
                }
                DirectAffectedTestsOutcome::Stale => return FeedbackImpactPortOutcome::Stale,
            };

            let impact = FeedbackImpactV1 {
                target: request.input.target.clone(),
                affected_files,
                affected_callers,
                affected_tests,
                evidence_anchors: Vec::new(),
                state: FeedbackImpactStateV1::Partial,
                affected_tests_state,
            };
            if impact.validate().is_err() {
                FeedbackImpactPortOutcome::Unavailable
            } else {
                FeedbackImpactPortOutcome::Partial(impact)
            }
        })
    }
}

struct SharedAffectedTests(Arc<dyn AffectedTestsRetrievalPort + Send + Sync>);

impl AffectedTestsRetrievalPort for SharedAffectedTests {
    fn affected_tests(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &AffectedTestsRequest,
    ) -> RetrievalPortOutcome<AffectedTestsResult> {
        self.0.affected_tests(context, request)
    }
}

enum DirectAffectedTestsOutcome {
    Evidence {
        tests: Vec<SymbolOccurrenceId>,
        state: FeedbackImpactStateV1,
    },
    Cancelled,
    TimedOut,
    Stale,
}

fn affected_tests_outcome(
    outcome: RetrievalPortOutcome<AffectedTestsResult>,
) -> DirectAffectedTestsOutcome {
    match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            if evidence.temporal.freshness == FreshnessState::Stale {
                return DirectAffectedTestsOutcome::Stale;
            }
            let state = if evidence.coverage.completeness == CoverageCompleteness::Complete
                && evidence.payload.is_some()
            {
                FeedbackImpactStateV1::Complete
            } else if evidence.payload.is_some() {
                FeedbackImpactStateV1::Partial
            } else {
                FeedbackImpactStateV1::Unavailable
            };
            DirectAffectedTestsOutcome::Evidence {
                tests: evidence
                    .payload
                    .map_or_else(Vec::new, |result| result.tests),
                state,
            }
        }
        RetrievalPortOutcome::Partial(evidence) => {
            if evidence.temporal.freshness == FreshnessState::Stale {
                return DirectAffectedTestsOutcome::Stale;
            }
            DirectAffectedTestsOutcome::Evidence {
                tests: evidence
                    .payload
                    .map_or_else(Vec::new, |result| result.tests),
                state: FeedbackImpactStateV1::Partial,
            }
        }
        RetrievalPortOutcome::Cancelled(_) => DirectAffectedTestsOutcome::Cancelled,
        RetrievalPortOutcome::TimedOut(_) => DirectAffectedTestsOutcome::TimedOut,
        RetrievalPortOutcome::Failed(_) | RetrievalPortOutcome::Unavailable(_) => {
            DirectAffectedTestsOutcome::Evidence {
                tests: Vec::new(),
                state: FeedbackImpactStateV1::Unavailable,
            }
        }
    }
}

struct SharedFeedbackObservations(Arc<dyn FeedbackObservationPort + Send + Sync>);

impl FeedbackObservationPort for SharedFeedbackObservations {
    fn observe(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        observation: tracedecay_domain::feedback::FeedbackCycleObservationV1,
    ) {
        self.0.observe(input, observation);
    }
}

fn lsp_trigger_matches_invocation(
    trigger: DiagnosticTrigger,
    invocation: &Pr12FeedbackCycleInvocation,
) -> bool {
    matches!(
        (trigger, invocation.request.input.request.trigger),
        (
            DiagnosticTrigger::DocumentSave,
            FeedbackTriggerV1::DocumentSave
        ) | (
            DiagnosticTrigger::ExplicitDocumentDiagnostics,
            FeedbackTriggerV1::ExplicitDiagnostics
        )
    )
}
