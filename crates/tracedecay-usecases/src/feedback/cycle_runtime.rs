//! Concrete, one-shot feedback-cycle composition.
//!
//! The runtime only composes existing application ports and direct graph
//! queries. It owns no provider lifecycle, source write, or second feedback
//! store.

use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;
use tracedecay_application::diagnostics::{
    AnalyzerAdmittedDiagnosticProviderV1, DiagnosticProviderIdentity,
};
use tracedecay_application::feedback::{
    FeedbackCycleAdvisoryV1, FeedbackCycleExecutionRequest, FeedbackCycleExecutionResult,
    FeedbackCycleService, FeedbackExpandRequestV1, FeedbackImpactPort, FeedbackImpactPortOutcome,
    FeedbackImpactRequest, FeedbackObservationPort, FeedbackPortFuture, FeedbackRuntimeStatePort,
    FeedbackRuntimeStateV1, GenerationBoundFeedbackDiagnosticsAdapter,
};
use tracedecay_application::retrieval::{
    AffectedTestsRequest, AffectedTestsResult, AffectedTestsRetrievalPort, AnchorExpandRequest,
    PageRequest, ResultProjection, RetrievalOrder, RetrievalPortContext, RetrievalPortOutcome,
    RetrievalRequestMeta,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationOperation, CoverageCompleteness, FreshnessState,
    PolicyEvaluationV1, RequestAdmission, RequestContext,
};
use tracedecay_domain::feedback::{
    FeedbackDurabilityV1, FeedbackFindingId, FeedbackFindingV1, FeedbackImpactStateV1,
    FeedbackImpactV1, FeedbackTriggerV1,
};
use tracedecay_domain::{
    FileOccurrenceId, RetrievalAnchorId, SymbolOccurrenceId, canonical_sha256,
};
use tracedecay_lsp::{
    DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort, LspRuntimeFailure,
    LspRuntimeFuture,
};
use tracedecay_policy::CapabilityRoutingDecisionV1;

use crate::diagnostics_publication::{CodeIndexPublicationIdentityPortV1, code_index_logical_path};
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::db::Database;

use super::concrete::{FeedbackRuntime, ProjectFeedbackRouteAuthorization, ProjectFeedbackStore};
use super::diagnostics::{DatabaseDiagnosticStore, DiagnosticStoreFeedbackProvider};
use super::observations::{
    FeedbackDeliveryRouteV1, FeedbackLspMethodClassV1, FeedbackLspStateV1,
    FeedbackObservationEmitterV1, FeedbackOperationV1, FeedbackOutcomeV1, FeedbackSourceEventV1,
};

/// Resolves one LSP lifecycle request to the already-authorized, bounded
/// application input. The caller owns URI-to-identity resolution, cancellation,
/// deadline, and budget measurement.
pub type FeedbackCycleLspInput = Arc<
    dyn Fn(
            FeedbackCycleRequest,
        ) -> LspRuntimeFuture<Result<FeedbackCycleInvocation, LspRuntimeFailure>>
        + Send
        + Sync,
>;

/// Complete input for exactly one canonical feedback-cycle invocation.
#[derive(Clone)]
pub struct FeedbackCycleInvocation {
    pub context: RequestContext,
    pub request: FeedbackCycleExecutionRequest,
}

impl FeedbackCycleInvocation {
    pub fn new(
        context: RequestContext,
        request: FeedbackCycleExecutionRequest,
    ) -> Result<Self, FeedbackCycleRuntimeError> {
        let invocation = Self { context, request };
        invocation.validate()?;
        Ok(invocation)
    }

    pub fn validate(&self) -> Result<(), FeedbackCycleRuntimeError> {
        self.context.validate()?;
        self.request.validate()?;
        if !matches!(
            self.request.input.request.trigger,
            FeedbackTriggerV1::PostEditHook
                | FeedbackTriggerV1::DocumentSave
                | FeedbackTriggerV1::ExplicitDiagnostics
                | FeedbackTriggerV1::AgentStopGate
        ) {
            return Err(FeedbackCycleRuntimeError::UnsupportedTrigger);
        }
        Ok(())
    }
}

/// Short-lived canonical-read handles for one stable finding identity.
///
/// The handles grant no authority by possession. Resolving either handle
/// re-enters the existing feedback read owner, which rechecks route authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackFindingHandlesV1 {
    pub finding_id: FeedbackFindingId,
    pub retrieval_anchor_id: Option<RetrievalAnchorId>,
    pub get_handle: String,
    pub expansion_handle: Option<String>,
}

/// One transport-neutral Plan 09 result shared by Hook, MCP, HTTP, LSP, CLI,
/// and later dashboard projections. Evidence remains reference-only in the
/// underlying result; this layer adds only authorized, short-lived handles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalFeedbackResultV1 {
    pub execution: FeedbackCycleExecutionResult,
    pub finding_handles: Vec<FeedbackFindingHandlesV1>,
}

impl CanonicalFeedbackResultV1 {
    fn new(
        execution: FeedbackCycleExecutionResult,
        finding_handles: Vec<FeedbackFindingHandlesV1>,
    ) -> Result<Self, ApplicationContractError> {
        let result = Self {
            execution,
            finding_handles,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.execution.cycle.validate()?;
        if let Some(publication) = &self.execution.publication {
            publication.validate()?;
            if publication.result != self.execution.cycle
                || self.execution.dedupe_key.as_ref() != Some(&publication.dedupe_key)
                || self.execution.authority.as_ref() != Some(&publication.authority)
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "canonical feedback publication",
                });
            }
        }
        let durable = self.execution.cycle.durability == FeedbackDurabilityV1::Durable;
        if !durable
            && (self.execution.dedupe_key.is_some()
                || self.execution.authority.is_some()
                || self.execution.publication.is_some()
                || !self.finding_handles.is_empty())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "overlay feedback durable output",
            });
        }
        if self.execution.publication.is_none() && !self.finding_handles.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "unpublished feedback expansion handles",
            });
        }
        if self.execution.publication.is_some()
            && (self.finding_handles.len() != self.execution.cycle.findings.len()
                || self
                    .finding_handles
                    .iter()
                    .zip(&self.execution.cycle.findings)
                    .any(|(handles, finding)| {
                        handles.finding_id != finding.finding_id
                            || handles.retrieval_anchor_id != finding.retrieval_anchor_id
                            || handles.get_handle.is_empty()
                            || handles.expansion_handle.is_some()
                                != finding.retrieval_anchor_id.is_some()
                    }))
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback expansion handles",
            });
        }
        Ok(())
    }
}

impl Deref for CanonicalFeedbackResultV1 {
    type Target = FeedbackCycleExecutionResult;

    fn deref(&self) -> &Self::Target {
        &self.execution
    }
}

#[derive(Debug, Error)]
pub enum FeedbackCycleRuntimeError {
    #[error("feedback cycle contract is invalid")]
    Contract(#[from] ApplicationContractError),
    #[error("feedback cycle requires at least one managed diagnostic provider")]
    NoManagedDiagnosticProviders,
    #[error("feedback cycle request provider identities differ from its admission set")]
    ProviderSetMismatch,
    #[error("feedback cycle trigger is not supported by the feedback cycle runtime")]
    UnsupportedTrigger,
    /// Retained for compatibility with callers that classify older rejection
    /// results. Session-only overlays now execute through the isolated path.
    #[error("feedback cycles require durable saved content")]
    NonDurableRequest,
}

impl FeedbackCycleRuntimeError {
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

type ProductionFeedbackCycleService = FeedbackCycleService<
    SharedFeedbackRuntimeState,
    GenerationBoundFeedbackDiagnosticsAdapter<
        DiagnosticStoreFeedbackProvider<DatabaseDiagnosticStore>,
    >,
    DirectFeedbackImpactAdapter,
    ProjectFeedbackStore,
    SharedFeedbackObservations,
    ProjectFeedbackRouteAuthorization,
>;

/// Concrete Plan 09 runtime for saved-edit, explicit-diagnostic, stop-gate,
/// and session-overlay cycles. Every invocation delegates once to
/// [`FeedbackCycleService`]; only its durable compare-and-record boundary can
/// publish a terminal result.
#[derive(Clone)]
pub struct FeedbackCycleRuntime {
    feedback: Arc<FeedbackRuntime>,
    publications: ProjectFeedbackStore,
    service: Arc<ProductionFeedbackCycleService>,
    lsp_input: FeedbackCycleLspInput,
    provider_admissions: Vec<AnalyzerAdmittedDiagnosticProviderV1>,
    correlation_policy: PolicyEvaluationV1<CapabilityRoutingDecisionV1>,
    source_observations: Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>,
}

/// Opens the one concrete feedback-cycle owner from already-open project
/// authorities. Diagnostics are bound directly to the project database,
/// graph/test queries retain their existing services, and publication reuses
/// the exact store and route authorization owned by `feedback`.
#[allow(clippy::too_many_arguments)]
pub fn open_feedback_cycle_runtime(
    database: Database,
    feedback: Arc<FeedbackRuntime>,
    runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
    correlation_policy: PolicyEvaluationV1<CapabilityRoutingDecisionV1>,
    provider_admissions: Vec<AnalyzerAdmittedDiagnosticProviderV1>,
    graph: Arc<TraceDecay>,
    affected_tests: Arc<dyn AffectedTestsRetrievalPort + Send + Sync>,
    observations: Arc<dyn FeedbackObservationPort + Send + Sync>,
    operation: ApplicationOperation,
    graph_operation: ApplicationOperation,
    tests_operation: ApplicationOperation,
    lsp_input: FeedbackCycleLspInput,
    code_index_identity: Option<Arc<dyn CodeIndexPublicationIdentityPortV1>>,
) -> Result<Arc<FeedbackCycleRuntime>, FeedbackCycleRuntimeError> {
    if provider_admissions.is_empty() {
        return Err(FeedbackCycleRuntimeError::NoManagedDiagnosticProviders);
    }

    let publications = feedback.publication_store();
    let source_observations = feedback.source_observation_port();
    let diagnostics = GenerationBoundFeedbackDiagnosticsAdapter::new(
        DiagnosticStoreFeedbackProvider::new(DatabaseDiagnosticStore::new(database)),
        provider_admissions.clone(),
    )?;
    let route_authorization = feedback.route_authorization();
    let impact = DirectFeedbackImpactAdapter::new(
        graph,
        SharedAffectedTests(affected_tests),
        route_authorization.clone(),
        graph_operation,
        tests_operation,
        code_index_identity,
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

    Ok(Arc::new(FeedbackCycleRuntime {
        feedback,
        publications,
        service: Arc::new(service),
        lsp_input,
        provider_admissions,
        correlation_policy,
        source_observations,
    }))
}

impl FeedbackCycleRuntime {
    pub fn feedback_runtime(&self) -> Arc<FeedbackRuntime> {
        Arc::clone(&self.feedback)
    }

    /// The same durable store used by the completed-publication dedupe port.
    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    pub fn correlation_policy(&self) -> &PolicyEvaluationV1<CapabilityRoutingDecisionV1> {
        &self.correlation_policy
    }

    pub fn source_observation_port(&self) -> Arc<dyn FeedbackObservationEmitterV1 + Send + Sync> {
        Arc::clone(&self.source_observations)
    }

    /// Input for `ConcreteFeedbackLspSource` to share this cycle with
    /// managed diagnostics and context projections.
    pub fn context_projection_input(self: &Arc<Self>) -> Arc<dyn FeedbackCycleRuntimePort> {
        self.clone()
    }

    /// Runs exactly one bounded feedback cycle and returns its terminal,
    /// canonical result. It never schedules retries or follow-up work.
    pub async fn run_once(
        &self,
        invocation: FeedbackCycleInvocation,
    ) -> Result<CanonicalFeedbackResultV1, FeedbackCycleRuntimeError> {
        invocation.validate()?;
        if !self.admits_provider_set(&invocation.request.providers) {
            return Err(FeedbackCycleRuntimeError::ProviderSetMismatch);
        }
        let FeedbackCycleInvocation { context, request } = invocation;
        let requested_durability = request.input.request.durability();
        let execution = self.service.execute(&context, request).await?;
        Ok(self.compose_canonical_result(execution, requested_durability)?)
    }

    /// Runs one canonical Plan 09 cycle with source-backed advisory findings.
    /// It reuses this runtime's authorization, diagnostics, impact, and single
    /// durable publication/dedupe path.
    pub async fn run_once_with_advisory(
        &self,
        context: &RequestContext,
        request: FeedbackCycleExecutionRequest,
        advisory: FeedbackCycleAdvisoryV1,
    ) -> Result<CanonicalFeedbackResultV1, ApplicationContractError> {
        if !self.admits_provider_set(&request.providers) {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback cycle provider set",
            });
        }
        let requested_durability = request.input.request.durability();
        let execution = self
            .service
            .execute_with_advisory(context, request, advisory)
            .await?;
        self.compose_canonical_result(execution, requested_durability)
    }

    fn compose_canonical_result(
        &self,
        execution: FeedbackCycleExecutionResult,
        requested_durability: FeedbackDurabilityV1,
    ) -> Result<CanonicalFeedbackResultV1, ApplicationContractError> {
        if execution.cycle.durability != requested_durability {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback result durability",
            });
        }
        if execution.cycle.durability != FeedbackDurabilityV1::Durable
            || execution.publication.is_none()
        {
            return CanonicalFeedbackResultV1::new(execution, Vec::new());
        }

        let observed_at = execution.usage.completed_at;
        let mut finding_handles = Vec::with_capacity(execution.cycle.findings.len());
        for finding in &execution.cycle.findings {
            let get_handle = self
                .feedback
                .mint_get(
                    feedback_handle_request_id("get", &execution, finding)?,
                    finding.finding_id.clone(),
                    observed_at,
                )
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "feedback get handle authority",
                })?;
            let expansion_handle = if let Some(request) = feedback_expansion_request(finding)? {
                Some(
                    self.feedback
                        .mint_expand(
                            feedback_handle_request_id("expand", &execution, finding)?,
                            request,
                            observed_at,
                        )
                        .map_err(|_| ApplicationContractError::Inconsistent {
                            field: "feedback expansion handle authority",
                        })?,
                )
            } else {
                None
            };
            finding_handles.push(FeedbackFindingHandlesV1 {
                finding_id: finding.finding_id.clone(),
                retrieval_anchor_id: finding.retrieval_anchor_id.clone(),
                get_handle,
                expansion_handle,
            });
        }
        CanonicalFeedbackResultV1::new(execution, finding_handles)
    }

    fn admits_provider_set(&self, providers: &[DiagnosticProviderIdentity]) -> bool {
        providers.len() == self.provider_admissions.len()
            && providers.iter().all(|identity| {
                self.provider_admissions
                    .iter()
                    .filter(|admission| admission.admits_identity(identity))
                    .count()
                    == 1
            })
    }
}

fn feedback_handle_request_id(
    operation: &'static str,
    execution: &FeedbackCycleExecutionResult,
    finding: &FeedbackFindingV1,
) -> Result<String, ApplicationContractError> {
    let digest = canonical_sha256(&(
        "tracedecay.feedback.canonical-handle-request.v1",
        operation,
        &execution.cycle.result_id,
        &finding.finding_id,
    ))?;
    Ok(format!("feedback.{operation}.{}", digest.as_str()))
}

fn feedback_expansion_request(
    finding: &FeedbackFindingV1,
) -> Result<Option<FeedbackExpandRequestV1>, ApplicationContractError> {
    let Some(anchor) = finding.retrieval_anchor_id.clone() else {
        return Ok(None);
    };
    Ok(Some(FeedbackExpandRequestV1 {
        finding_id: finding.finding_id.clone(),
        expansion: AnchorExpandRequest {
            anchor,
            meta: RetrievalRequestMeta::current(
                PageRequest::first(100)?,
                ResultProjection::ReferencesOnly,
                RetrievalOrder::StableIdentity,
            ),
        },
    }))
}

impl FeedbackCycleRuntimePort for FeedbackCycleRuntime {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let runtime = self.clone();
        Box::pin(async move {
            let started_at = Instant::now();
            let trigger = request.trigger;
            let invocation = (runtime.lsp_input)(request).await?;
            if !lsp_trigger_matches_invocation(trigger, &invocation) {
                let duration_micros =
                    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
                runtime.source_observations.observe_source_event(
                    &invocation.request.input,
                    FeedbackSourceEventV1::ArgumentRejected {
                        operation: FeedbackOperationV1::LspSession,
                        outcome: FeedbackOutcomeV1::Rejected,
                    },
                );
                runtime.source_observations.observe_source_event(
                    &invocation.request.input,
                    lsp_method_state_event(
                        FeedbackLspStateV1::MethodRejected,
                        FeedbackOutcomeV1::Rejected,
                        0,
                        duration_micros,
                    ),
                );
                return Err(LspRuntimeFailure::new("feedback-cycle-trigger-mismatch"));
            }
            let input = invocation.request.input.clone();
            let admission_duration_micros =
                u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
            runtime.source_observations.observe_source_event(
                &input,
                lsp_method_state_event(
                    FeedbackLspStateV1::MethodAdmitted,
                    FeedbackOutcomeV1::Admitted,
                    1,
                    admission_duration_micros,
                ),
            );
            let result = Box::pin(runtime.run_once(invocation)).await;
            let duration_micros =
                u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
            let outcome = if result.is_ok() {
                FeedbackOutcomeV1::Completed
            } else {
                FeedbackOutcomeV1::Failed
            };
            runtime.source_observations.observe_source_event(
                &input,
                lsp_method_state_event(
                    FeedbackLspStateV1::MethodCompleted,
                    outcome,
                    u32::from(result.is_ok()),
                    duration_micros,
                ),
            );
            runtime.source_observations.observe_source_event(
                &input,
                FeedbackSourceEventV1::Delivery {
                    operation: FeedbackOperationV1::FeedbackCycle,
                    route: FeedbackDeliveryRouteV1::Lsp,
                    outcome,
                    item_count: u32::from(result.is_ok()),
                    duration_micros: Some(duration_micros),
                },
            );
            result
                .map(|_| ())
                .map_err(|error| LspRuntimeFailure::new(error.lsp_failure_class()))
        })
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
    /// The code-index generation authority — the single mint for
    /// `file.daemon.<digest>` file identity. Absent for runtimes opened outside
    /// the daemon, where the adapter reports no affected files rather than
    /// minting raw-path identities the rest of the system cannot match.
    code_index_identity: Option<Arc<dyn CodeIndexPublicationIdentityPortV1>>,
}

impl DirectFeedbackImpactAdapter {
    fn new(
        graph: Arc<TraceDecay>,
        tests: SharedAffectedTests,
        authorization: ProjectFeedbackRouteAuthorization,
        graph_operation: ApplicationOperation,
        tests_operation: ApplicationOperation,
        code_index_identity: Option<Arc<dyn CodeIndexPublicationIdentityPortV1>>,
    ) -> Self {
        Self {
            graph,
            tests,
            authorization,
            graph_operation,
            tests_operation,
            code_index_identity,
        }
    }

    /// Resolves graph-node file paths onto code-index file identity.
    ///
    /// This adapter used to mint `FileOccurrenceId::new(node.file_path)` — a
    /// raw repository-relative path — which disagreed with every other file
    /// identity in the system. Identity is now resolved from the same authority
    /// that mints the cycle's impact target, and the outcome distinguishes the
    /// three reasons a node can contribute nothing: no resolver is bound, the
    /// published identity belongs to another generation, or the generation
    /// simply does not contain that path. Each maps onto its own coverage state
    /// rather than collapsing into one indistinguishable empty vector.
    async fn resolved_affected_files(
        &self,
        generation: &tracedecay_domain::CodeGenerationId,
        file_paths: &[String],
    ) -> ResolvedAffectedFiles {
        resolve_affected_files_for_published_generation(
            self.code_index_identity.as_deref(),
            self.graph.project_root(),
            generation,
            file_paths,
        )
        .await
    }
}

async fn resolve_affected_files_for_published_generation(
    resolver: Option<&dyn CodeIndexPublicationIdentityPortV1>,
    root: &Path,
    generation: &tracedecay_domain::CodeGenerationId,
    file_paths: &[String],
) -> ResolvedAffectedFiles {
    let Some(resolver) = resolver else {
        return ResolvedAffectedFiles::IdentityUnavailable;
    };
    let Some(identity) = resolver.resolve(root.to_path_buf()).await else {
        return ResolvedAffectedFiles::IdentityUnavailable;
    };
    if identity.generation_id() != generation {
        return ResolvedAffectedFiles::GenerationMismatch;
    }
    let mut resolved_every_path = true;
    let mut files = Vec::with_capacity(file_paths.len());
    for path in file_paths {
        let file = code_index_logical_path(root, path)
            .and_then(|logical| identity.file(&logical).map(|(file, _)| file.clone()));
        match file {
            Some(file) => files.push(file),
            None => resolved_every_path = false,
        }
    }
    files.sort();
    files.dedup();
    if resolved_every_path {
        ResolvedAffectedFiles::Complete(files)
    } else {
        ResolvedAffectedFiles::Partial(files)
    }
}

/// Typed outcome of resolving graph-node paths onto code-index file identity.
///
/// The empty-vector cases used to be indistinguishable from "this generation
/// genuinely has no affected files"; each now carries its own coverage meaning.
enum ResolvedAffectedFiles {
    /// Every graph-node path resolved against the requested generation.
    Complete(Vec<FileOccurrenceId>),
    /// The generation matched, but at least one graph-node path has no file
    /// identity in it, so the resolved set is a subset of the impact radius.
    Partial(Vec<FileOccurrenceId>),
    /// No code-index identity authority is bound to this runtime — the case for
    /// runtimes opened outside the daemon. The adapter reports no affected files
    /// rather than minting raw-path identities the rest of the system cannot
    /// match.
    IdentityUnavailable,
    /// The published identity belongs to a different generation than the one the
    /// request targets, so nothing it contains describes this impact.
    GenerationMismatch,
}

impl FeedbackImpactPort for DirectFeedbackImpactAdapter {
    fn impact<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackImpactRequest,
    ) -> FeedbackPortFuture<'a, FeedbackImpactPortOutcome> {
        Box::pin(async move {
            if request.validate().is_err() {
                return FeedbackImpactPortOutcome::Unavailable;
            }
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }
            if !self
                .authorization
                .allows(context, &self.graph_operation, request.input.observed_at)
            {
                return FeedbackImpactPortOutcome::Unavailable;
            }
            let Some(symbol) = request.input.target.symbol.clone() else {
                return FeedbackImpactPortOutcome::Unavailable;
            };
            let Some(generation) = request.input.target.generation_id.clone() else {
                return FeedbackImpactPortOutcome::Unavailable;
            };
            let Ok(subgraph) = self.graph.get_impact_radius(symbol.as_str(), 3).await else {
                return FeedbackImpactPortOutcome::Unavailable;
            };
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }
            let Ok(callers) = self.graph.get_callers(symbol.as_str(), 3).await else {
                return FeedbackImpactPortOutcome::Unavailable;
            };
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }

            let node_file_paths = subgraph
                .nodes
                .iter()
                .map(|node| node.file_path.clone())
                .collect::<Vec<_>>();
            let (affected_files, graph_state) = match self
                .resolved_affected_files(&generation, &node_file_paths)
                .await
            {
                ResolvedAffectedFiles::Complete(files) => (files, FeedbackImpactStateV1::Complete),
                ResolvedAffectedFiles::Partial(files) => (files, FeedbackImpactStateV1::Partial),
                ResolvedAffectedFiles::IdentityUnavailable => {
                    (Vec::new(), FeedbackImpactStateV1::Partial)
                }
                ResolvedAffectedFiles::GenerationMismatch => {
                    return FeedbackImpactPortOutcome::Stale;
                }
            };
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }
            let mut affected_callers = callers
                .into_iter()
                .map(|(node, _)| node)
                .filter_map(|node| SymbolOccurrenceId::new(node.id.clone()).ok())
                .collect::<Vec<_>>();
            affected_callers.sort();
            affected_callers.dedup();

            let Ok(page) = PageRequest::first(100) else {
                return FeedbackImpactPortOutcome::Unavailable;
            };
            let meta = RetrievalRequestMeta::current(
                page,
                ResultProjection::ReferencesOnly,
                RetrievalOrder::StableIdentity,
            );
            match context.admission_at(request.input.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return FeedbackImpactPortOutcome::Cancelled,
                RequestAdmission::TimedOut => return FeedbackImpactPortOutcome::TimedOut,
            }
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
                &AffectedTestsRequest {
                    symbol,
                    generation,
                    meta,
                },
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

            // The impact is complete only when both the graph and the
            // affected-test evidence report complete coverage.
            // `evidence_anchors` stays empty because this runtime binds no
            // anchor authority — the graph traversal yields nodes, not
            // retrieval anchors — and an invented anchor would be worse than
            // none.
            let state = if graph_state == FeedbackImpactStateV1::Complete
                && affected_tests_state == FeedbackImpactStateV1::Complete
            {
                FeedbackImpactStateV1::Complete
            } else {
                FeedbackImpactStateV1::Partial
            };
            let impact = FeedbackImpactV1 {
                target: request.input.target.clone(),
                affected_files,
                affected_callers,
                affected_tests,
                evidence_anchors: Vec::new(),
                state,
                affected_tests_state,
            };
            if impact.validate().is_err() {
                return FeedbackImpactPortOutcome::Unavailable;
            }
            match state {
                FeedbackImpactStateV1::Complete => FeedbackImpactPortOutcome::Complete(impact),
                FeedbackImpactStateV1::Partial => FeedbackImpactPortOutcome::Partial(impact),
                FeedbackImpactStateV1::Stale | FeedbackImpactStateV1::Unavailable => {
                    FeedbackImpactPortOutcome::Unavailable
                }
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
    invocation: &FeedbackCycleInvocation,
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

fn lsp_method_state_event(
    state: FeedbackLspStateV1,
    outcome: FeedbackOutcomeV1,
    item_count: u32,
    duration_micros: u64,
) -> FeedbackSourceEventV1 {
    FeedbackSourceEventV1::LspState {
        state,
        method: Some(FeedbackLspMethodClassV1::Diagnostics),
        outcome,
        item_count,
        duration_micros: Some(duration_micros),
    }
}

#[cfg(test)]
#[path = "cycle_runtime/tests.rs"]
mod tests;
