//! Owned, daemon-reachable dispatch for the closed PR12 primitive set.
//!
//! This module composes existing application ports. It never calls an MCP,
//! CLI, HTTP, or handler registry and it opens no store or policy authority.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracedecay_application::retrieval::grep_analysis::{
    AstGrepAuthorityV1, AstGrepRequestV1, ComplexityAuthorityV1, ComplexityRequestV1,
    DependencyDepthAuthorityV1, DependencyDepthRequestV1, GrepAnalysisProblemV1, GrepRequestV1,
    LexicalGrepAuthorityV1, PrimitiveCoverageV1, PrimitiveOutcomeV1, PrimitivePortContextV1,
    RedundancyAuthorityV1, RedundancyRequestV1,
};
use tracedecay_application::retrieval::{
    AffectedFileTestsPrimitiveRequest, AffectedFileTestsPrimitiveResultV1, ExactSymbolRequest,
    GraphImpactPrimitiveRequest, GraphRelationRequest, HealthDeltaRequest, HealthDeltaResult,
    HealthReadRequest, ImplementationsRequest, OperationalRetrievalPort, RetrievalPortContext,
    RetrievalPortOutcome, SessionLookupRequest, SignatureSearchRequest, SourceLinesRequest,
    SourceReadPortContext, SourceReadPortOutcome, SourceReadPrimitivePort,
    SourceReadPrimitiveRequest, SourceRetrievalPort, SymbolGraphPage, SymbolGraphPortContext,
    SymbolGraphPortOutcome, SymbolGraphPrimitivePort, SymbolSearchPrimitiveRequest,
    TemporalRetrievalPort, TestMapPrimitiveRequest, TestMapPrimitiveResultV1, TestPrimitivePort,
    TestPrimitivePortContext, TestPrimitivePortOutcome, TypeHierarchyRequest,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOperation, ApplicationOutcome,
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, AuthorityReceipt,
    CancellationContext, CancellationObservation, CancellationStage, CapabilityGrantId,
    CapabilityGrantSnapshot, CoverageCompleteness, CoverageDomainState, Deadline, DisclosureClass,
    EvidenceCoverage, EvidenceDomain, EvidencePacket, LegalAction, OpaqueCursor,
    OperationBudgetUsage, OperationReceipt, OperationTermination, PageRequest, PageState,
    PolicyDecisionRef, RequestAdmission, RequestContext, RequestId, ResolvedScope,
    RetrievalEvidence, RetryDirective, SafeDiagnostic, TemporalState,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ComponentVersion, GenerationDiagnosticV1, UtcMicros,
};
use tracedecay_tool_catalog::SortContractId;
use url::Url;

use super::concrete::Pr12SourceReadAdapter;
use super::grep_analysis::{
    TraceDecayAstGrepAuthorityV1, TraceDecayComplexityAuthorityV1,
    TraceDecayDependencyDepthAuthorityV1,
};
use super::symbol_graph::{CanonicalSymbolGraphAdapter, SymbolGraphCursorPort};
use crate::ProjectSourceAccessSnapshot;
use crate::operation_stream::{
    CanonicalManagedTestRunReader, ManagedTestRunCurrentScope, ManagedTestRunReadOutcome,
    ManagedTestRunStaleReason, OperationEventAuthority,
};
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::db::Database;

const MAX_OPERATION_PARAMETERS_BYTES: usize = 1_048_576;
const MAX_OPERATION_OUTPUT_BYTES: usize = 1_048_576;
const MAX_ADMITTED_ROOT_URI_BYTES: usize = 4_096;
const MAX_CONCURRENT_PR12_PRIMITIVES: usize = 32;

/// Validated once per process rather than on every paged primitive result.
static PR12_PRIMITIVE_SORT_CONTRACT: LazyLock<SortContractId> = LazyLock::new(|| {
    SortContractId::new("sort.application.retrieval.stable")
        .unwrap_or_else(|_| panic!("static primitive sort contract is valid"))
});

pub type Pr12PrimitiveDispatchFuture<'a> =
    Pin<Box<dyn Future<Output = ApplicationResult<Value>> + Send + 'a>>;

pub type Pr12PrimitiveTransportDispatchFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<ApplicationResult<Value>, ApplicationContractError>> + Send + 'a,
    >,
>;

pub type Pr12OperationalPrimitiveFuture<'a> =
    Pin<Box<dyn Future<Output = ApplicationResult<Value>> + Send + 'a>>;

pub type Pr12ExtendedPrimitiveFuture<'a, T> =
    Pin<Box<dyn Future<Output = RetrievalPortOutcome<T>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedTestRunCurrentIdentity {
    pub head_commit_id: CommitId,
    pub code_generation_id: CodeGenerationId,
}

pub type ManagedTestRunCurrentIdentityFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<ManagedTestRunCurrentIdentity, ApplicationContractError>>
            + Send
            + 'a,
    >,
>;

pub trait ManagedTestRunCurrentScopePort: Send + Sync {
    fn current_identity(&self) -> ManagedTestRunCurrentIdentityFuture<'_>;
}

/// Closed operational reads whose concrete owner remains Doctor,
/// configuration, diagnostics, project, or status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pr12OperationalPrimitive {
    Project,
    Status,
    Files,
    Configuration,
    RuntimeStatus,
}

/// Canonical bounded parameters for one operational read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pr12OperationalPrimitiveRequest {
    pub operation: Pr12OperationalPrimitive,
    pub parameters: Value,
    pub maximum_output_bytes: u32,
}

impl Pr12OperationalPrimitiveRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        if !self.parameters.is_object() {
            return Err(ApplicationContractError::Inconsistent {
                field: "PR12 operational parameter object",
            });
        }
        validate_no_scope_selector(&self.parameters)?;
        validate_bounds(&self.parameters, self.maximum_output_bytes as usize)
    }
}

/// Object-safe owner for the existing operational read families.
pub trait Pr12OperationalPrimitivePort: Send + Sync {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        request: &'a Pr12OperationalPrimitiveRequest,
        observed_at: UtcMicros,
    ) -> Pr12OperationalPrimitiveFuture<'a>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QualifiedNamePrimitiveRequest {
    pub qualified_name: String,
    pub page: tracedecay_application::PageRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualifiedNamePrimitiveResult {
    pub symbols: Vec<tracedecay_application::retrieval::SymbolPrimitiveRecord>,
    pub total: Option<u64>,
    pub next_cursor: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallChainPrimitiveRequest {
    #[serde(alias = "from_id")]
    pub from_node_id: String,
    #[serde(alias = "to_id")]
    pub to_node_id: String,
    #[serde(default = "default_call_chain_depth", alias = "max_depth")]
    pub maximum_depth: u32,
}

const fn default_call_chain_depth() -> u32 {
    8
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallChainPrimitiveResult {
    pub node_ids: Vec<String>,
    pub edge_kinds: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileDependentsPrimitiveRequest {
    pub file: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileDependentsPrimitiveResult {
    pub file: String,
    pub dependent_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceBodyPrimitiveRequest {
    pub node_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceBodyPrimitiveResult {
    pub node_id: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceOutlinePrimitiveRequest {
    pub file: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceOutlinePrimitiveResult {
    pub file: String,
    pub symbols: Vec<tracedecay_application::retrieval::SymbolPrimitiveRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModuleApiPrimitiveRequest {
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModuleApiPrimitiveResult {
    pub path: String,
    pub symbols: Vec<tracedecay_application::retrieval::SymbolPrimitiveRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileMetadataPrimitiveRequest {
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileMetadataRecord {
    pub file: String,
    pub language: Option<String>,
    pub indexed_at: Option<i64>,
    pub byte_size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileMetadataPrimitiveResult {
    pub files: Vec<FileMetadataRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageStatusPrimitiveRequest {
    #[serde(default)]
    pub include_details: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageStatusHistoryPointV1 {
    pub observed_at: i64,
    pub database_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageStatusPrimitiveResult {
    pub status: String,
    pub read_only: bool,
    pub database_bytes: Option<u64>,
    #[serde(default)]
    pub page_size_bytes: Option<u32>,
    #[serde(default)]
    pub page_count: Option<u64>,
    #[serde(default)]
    pub freelist_pages: Option<u64>,
    pub details: Vec<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub store_path: Option<String>,
    #[serde(default)]
    pub history: Vec<StorageStatusHistoryPointV1>,
    #[serde(default)]
    pub history_coverage: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsPrimitiveScope {
    Workspace,
    Package(String),
    File(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsPrimitiveRequest {
    pub scope: DiagnosticsPrimitiveScope,
    pub maximum_diagnostics: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticPrimitiveRecord {
    pub logical_path: String,
    pub diagnostic: GenerationDiagnosticV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsPrimitiveResult {
    pub generation_id: CodeGenerationId,
    pub clean_generation: bool,
    pub findings_cleared: bool,
    pub diagnostics: Vec<DiagnosticPrimitiveRecord>,
    pub next_cursor: Option<String>,
}

/// Typed extension over existing query/source/system services. Every method is
/// independently callable; no operation string or untyped parameter bag is
/// accepted.
pub trait Pr12ExtendedPrimitivePort: Send + Sync {
    fn qualified_name<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a QualifiedNamePrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, QualifiedNamePrimitiveResult>;

    fn call_chain<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CallChainPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, CallChainPrimitiveResult>;

    fn file_dependents<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a FileDependentsPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, FileDependentsPrimitiveResult>;

    fn source_body<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceBodyPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, SourceBodyPrimitiveResult>;

    fn source_outline<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceOutlinePrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, SourceOutlinePrimitiveResult>;

    fn module_api<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ModuleApiPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, ModuleApiPrimitiveResult>;

    fn file_metadata<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a FileMetadataPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, FileMetadataPrimitiveResult>;

    fn health_delta<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a HealthDeltaRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, HealthDeltaResult>;

    fn storage_status<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a StorageStatusPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, StorageStatusPrimitiveResult>;

    fn diagnostics<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a DiagnosticsPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, DiagnosticsPrimitiveResult>;
}

/// Closed typed request enum accepted by direct daemon invocation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "primitive", content = "request", rename_all = "snake_case")]
pub enum Pr12PrimitiveRequest {
    #[serde(skip)]
    SymbolSearch(SymbolSearchPrimitiveRequest),
    ExactSymbol(ExactSymbolRequest),
    SignatureSearch(SignatureSearchRequest),
    Implementations(ImplementationsRequest),
    TypeHierarchy(TypeHierarchyRequest),
    Callers(GraphRelationRequest),
    Callees(GraphRelationRequest),
    Impact(GraphImpactPrimitiveRequest),
    SourceRead(SourceReadPrimitiveRequest),
    TestMap(TestMapPrimitiveRequest),
    AffectedFileTests(AffectedFileTestsPrimitiveRequest),
    LexicalGrep(GrepRequestV1),
    AstGrep(AstGrepRequestV1),
    Complexity(ComplexityRequestV1),
    Redundancy(RedundancyRequestV1),
    DependencyDepth(DependencyDepthRequestV1),
    SessionLookup(SessionLookupRequest),
    QualifiedName(QualifiedNamePrimitiveRequest),
    CallChain(CallChainPrimitiveRequest),
    FileDependents(FileDependentsPrimitiveRequest),
    SourceLines(SourceLinesRequest),
    SourceBody(SourceBodyPrimitiveRequest),
    SourceOutline(SourceOutlinePrimitiveRequest),
    ModuleApi(ModuleApiPrimitiveRequest),
    FileMetadata(FileMetadataPrimitiveRequest),
    HealthRead(HealthReadRequest),
    HealthDelta(HealthDeltaRequest),
    StorageStatus(StorageStatusPrimitiveRequest),
    DiagnosticsRead(DiagnosticsPrimitiveRequest),
    Operational(Pr12OperationalPrimitiveRequest),
    RecentTestResults(PageRequest),
}

/// One catalog operation plus its closed typed primitive request.
#[derive(Debug)]
pub struct Pr12PrimitiveInvocation {
    pub operation: ApplicationOperation,
    pub request: Pr12PrimitiveRequest,
}

/// Object-safe asynchronous facade retained and called directly by the daemon.
pub trait Pr12PrimitiveDispatch: Send + Sync {
    fn dispatch(
        &self,
        invocation: Pr12PrimitiveInvocation,
        context: RequestContext,
        observed_at: UtcMicros,
    ) -> Pr12PrimitiveDispatchFuture<'_>;

    fn dispatch_transport(
        &self,
        request_id: RequestId,
        operation: ApplicationOperation,
        request: Pr12PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Pr12PrimitiveTransportDispatchFuture<'_>;
}

/// Owned production authorities supplied by the daemon project-open path.
///
/// The Arc fields are the existing graph/query/source/cursor/test/grep and
/// operational services. This is an ownership boundary, not a locator:
/// every dependency is explicit and no authority can be discovered at call
/// time.
#[derive(Clone)]
struct Pr12PrimitiveProjectServices {
    pub symbol_graph: Arc<dyn SymbolGraphPrimitivePort + Send + Sync>,
    pub source: Arc<dyn SourceReadPrimitivePort + Send + Sync>,
    pub tests: Arc<dyn TestPrimitivePort + Send + Sync>,
    pub lexical_grep: Arc<dyn LexicalGrepAuthorityV1 + Send + Sync>,
    pub ast_grep: Arc<dyn AstGrepAuthorityV1 + Send + Sync>,
    pub complexity: Arc<dyn ComplexityAuthorityV1 + Send + Sync>,
    pub redundancy: Arc<dyn RedundancyAuthorityV1 + Send + Sync>,
    pub dependency_depth: Arc<dyn DependencyDepthAuthorityV1 + Send + Sync>,
    pub temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
    pub source_lines: Arc<dyn SourceRetrievalPort + Send + Sync>,
    pub health: Arc<dyn OperationalRetrievalPort + Send + Sync>,
    pub extended: Arc<dyn Pr12ExtendedPrimitivePort>,
    pub operational: Arc<dyn Pr12OperationalPrimitivePort>,
}

impl Pr12PrimitiveProjectServices {
    #[allow(clippy::too_many_arguments)]
    fn new(
        symbol_graph: Arc<dyn SymbolGraphPrimitivePort + Send + Sync>,
        source: Arc<dyn SourceReadPrimitivePort + Send + Sync>,
        tests: Arc<dyn TestPrimitivePort + Send + Sync>,
        lexical_grep: Arc<dyn LexicalGrepAuthorityV1 + Send + Sync>,
        ast_grep: Arc<dyn AstGrepAuthorityV1 + Send + Sync>,
        complexity: Arc<dyn ComplexityAuthorityV1 + Send + Sync>,
        redundancy: Arc<dyn RedundancyAuthorityV1 + Send + Sync>,
        dependency_depth: Arc<dyn DependencyDepthAuthorityV1 + Send + Sync>,
        temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
        source_lines: Arc<dyn SourceRetrievalPort + Send + Sync>,
        health: Arc<dyn OperationalRetrievalPort + Send + Sync>,
        extended: Arc<dyn Pr12ExtendedPrimitivePort>,
        operational: Arc<dyn Pr12OperationalPrimitivePort>,
    ) -> Self {
        Self {
            symbol_graph,
            source,
            tests,
            lexical_grep,
            ast_grep,
            complexity,
            redundancy,
            dependency_depth,
            temporal,
            source_lines,
            health,
            extended,
            operational,
        }
    }
}

/// Cloneable owned runtime retained by the daemon for one admitted root.
#[derive(Clone)]
pub struct OwnedPr12PrimitiveRuntime {
    project_runtime: Pr12PrimitiveProjectServices,
    scope: ResolvedScope,
    access: ProjectSourceAccessSnapshot,
    admitted_root_uri: String,
    test_runs: CanonicalManagedTestRunReader,
    test_run_scope: Arc<dyn ManagedTestRunCurrentScopePort>,
    capacity: Pr12PrimitiveCapacity,
}

#[derive(Clone)]
struct Pr12PrimitiveCapacity {
    permits: Arc<Semaphore>,
}

impl Pr12PrimitiveCapacity {
    fn new(maximum_concurrent: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(maximum_concurrent)),
        }
    }

    fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }
}

/// Teardown owner retained by central project-open.
///
/// Dropping this value releases the dispatch facade and every Arc-backed
/// project primitive authority together. The database and graph fields keep
/// the exact project-open owners alive for the same lifetime as dispatch.
pub struct Pr12PrimitiveProjectRuntime {
    database: Database,
    graph: Arc<TraceDecay>,
    dispatch: Arc<dyn Pr12PrimitiveDispatch>,
}

/// Replace the mount-time authority carried by an owned primitive result with
/// the authority revalidated immediately before publication.
pub fn reauthorize_primitive_evidence(
    result: &mut ApplicationResult<Value>,
    authority: AuthorityReceipt,
) -> bool {
    let Ok(envelope) = result else {
        return true;
    };
    if authority.validate_for(&envelope.scope).is_err() {
        return false;
    }
    let ApplicationOutcome::Evidence(evidence) = &mut envelope.outcome else {
        return false;
    };
    evidence.authority = authority;
    true
}

impl Pr12PrimitiveProjectRuntime {
    pub fn dispatch(&self) -> Arc<dyn Pr12PrimitiveDispatch> {
        Arc::clone(&self.dispatch)
    }

    pub fn database(&self) -> &Database {
        &self.database
    }

    pub fn graph(&self) -> Arc<TraceDecay> {
        Arc::clone(&self.graph)
    }

    /// Releases the project database, graph, dispatch, and all Arc-backed
    /// primitive authorities as one teardown unit.
    pub fn teardown(self) {
        drop(self);
    }
}

impl Pr12PrimitiveDispatch for OwnedPr12PrimitiveRuntime {
    fn dispatch(
        &self,
        invocation: Pr12PrimitiveInvocation,
        context: RequestContext,
        observed_at: UtcMicros,
    ) -> Pr12PrimitiveDispatchFuture<'_> {
        self.dispatch_invocation(invocation, context, observed_at)
    }

    fn dispatch_transport(
        &self,
        request_id: RequestId,
        operation: ApplicationOperation,
        request: Pr12PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Pr12PrimitiveTransportDispatchFuture<'_> {
        Box::pin(async move {
            if let Some(problem) = pre_admission_problem(
                &request_id,
                &operation,
                observed_at,
                &deadline,
                &cancellation,
            ) {
                return Ok(Err(problem));
            }
            if observed_at >= self.access.grant_expires_at {
                return Ok(Err(ApplicationProblemEnvelope::new(
                    operation.result_contract().clone(),
                    request_id,
                    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
                )));
            }
            let context = transport_context(
                &self.scope,
                &self.access,
                request_id,
                &operation,
                observed_at,
                deadline,
                cancellation,
            )?;
            Ok(self
                .dispatch_invocation(
                    Pr12PrimitiveInvocation { operation, request },
                    context,
                    observed_at,
                )
                .await)
        })
    }
}

impl OwnedPr12PrimitiveRuntime {
    fn dispatch_invocation(
        &self,
        invocation: Pr12PrimitiveInvocation,
        context: RequestContext,
        observed_at: UtcMicros,
    ) -> Pr12PrimitiveDispatchFuture<'_> {
        Box::pin(async move {
            if let Some(problem) = admission_problem(
                &self.scope,
                &self.access,
                &context,
                &invocation.operation,
                observed_at,
            ) {
                return Err(problem);
            }
            let Some(_permit) = self.capacity.try_acquire() else {
                return saturated(&context, &invocation.operation);
            };
            dispatch_admitted(self, invocation, context, observed_at).await
        })
    }
}

fn pre_admission_problem(
    request_id: &RequestId,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
    deadline: &Deadline,
    cancellation: &CancellationContext,
) -> Option<ApplicationProblemEnvelope> {
    let problem = if cancellation.is_cancelled() {
        ApplicationProblem::cancelled_before_admission()
    } else if deadline.is_elapsed_at(observed_at) {
        ApplicationProblem::timed_out_before_admission()
    } else {
        return None;
    };
    Some(ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        problem,
    ))
}

#[allow(clippy::too_many_arguments)]
fn transport_context(
    scope: &ResolvedScope,
    access: &ProjectSourceAccessSnapshot,
    request_id: RequestId,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<RequestContext, ApplicationContractError> {
    let expires_at = UtcMicros(deadline.expires_at.0.min(access.grant_expires_at.0));
    if observed_at.0 <= 0 || expires_at.0 <= observed_at.0 {
        return Err(ApplicationContractError::InvalidRange {
            field: "PR12 primitive transport deadline",
        });
    }
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.primitive.{}", request_id.as_str()))?,
        1,
        access.configuration_digest.clone(),
        access.requester.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )?;
    RequestContext::new(
        access.requester.clone(),
        scope.clone(),
        grant,
        request_id,
        Deadline::new(expires_at)?,
        cancellation,
    )
}

/// Concrete project-open factory for the complete owned PR12 primitive
/// runtime.
///
/// Exact constructor signature:
///
/// `open_pr12_primitive_project_runtime(database, graph, symbol_graph_cursors,
/// tests, lexical_grep, redundancy, temporal, source_lines, health, extended,
/// operational, scope, access,
/// admitted_root_uri, operation_events, test_run_scope) ->
/// Result<Pr12PrimitiveProjectRuntime,
/// ApplicationContractError>`
#[allow(clippy::too_many_arguments)]
pub fn open_pr12_primitive_project_runtime(
    database: Database,
    graph: Arc<TraceDecay>,
    symbol_graph_cursors: Arc<dyn SymbolGraphCursorPort>,
    tests: Arc<dyn TestPrimitivePort + Send + Sync>,
    lexical_grep: Arc<dyn LexicalGrepAuthorityV1 + Send + Sync>,
    redundancy: Arc<dyn RedundancyAuthorityV1 + Send + Sync>,
    temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
    source_lines: Arc<dyn SourceRetrievalPort + Send + Sync>,
    health: Arc<dyn OperationalRetrievalPort + Send + Sync>,
    extended: Arc<dyn Pr12ExtendedPrimitivePort>,
    operational: Arc<dyn Pr12OperationalPrimitivePort>,
    scope: ResolvedScope,
    access: ProjectSourceAccessSnapshot,
    admitted_root_uri: String,
    operation_events: OperationEventAuthority,
    test_run_scope: Arc<dyn ManagedTestRunCurrentScopePort>,
) -> Result<Pr12PrimitiveProjectRuntime, ApplicationContractError> {
    scope.validate()?;
    validate_admitted_root_uri(&admitted_root_uri)?;
    if access.scope != scope {
        return Err(ApplicationContractError::Inconsistent {
            field: "PR12 primitive admitted project authority",
        });
    }
    let symbol_graph: Arc<dyn SymbolGraphPrimitivePort + Send + Sync> = Arc::new(
        CanonicalSymbolGraphAdapter::new(Arc::clone(&graph), symbol_graph_cursors),
    );
    let source: Arc<dyn SourceReadPrimitivePort + Send + Sync> = Arc::new(
        Pr12SourceReadAdapter::new(Arc::clone(&graph), scope.clone())?,
    );
    let services = Pr12PrimitiveProjectServices::new(
        symbol_graph,
        source,
        tests,
        lexical_grep,
        Arc::new(TraceDecayAstGrepAuthorityV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayComplexityAuthorityV1::new(Arc::clone(&graph))),
        redundancy,
        Arc::new(TraceDecayDependencyDepthAuthorityV1::new(Arc::clone(
            &graph,
        ))),
        temporal,
        source_lines,
        health,
        extended,
        operational,
    );
    let dispatch: Arc<dyn Pr12PrimitiveDispatch> = Arc::new(OwnedPr12PrimitiveRuntime {
        project_runtime: services,
        scope,
        access,
        admitted_root_uri,
        test_runs: CanonicalManagedTestRunReader::new(operation_events),
        test_run_scope,
        capacity: Pr12PrimitiveCapacity::new(MAX_CONCURRENT_PR12_PRIMITIVES),
    });
    Ok(Pr12PrimitiveProjectRuntime {
        database,
        graph,
        dispatch,
    })
}

fn validate_admitted_root_uri(admitted_root_uri: &str) -> Result<(), ApplicationContractError> {
    if admitted_root_uri.len() > MAX_ADMITTED_ROOT_URI_BYTES {
        return Err(ApplicationContractError::InvalidRange {
            field: "PR12 primitive admitted root URI",
        });
    }
    let uri =
        Url::parse(admitted_root_uri).map_err(|_| ApplicationContractError::Inconsistent {
            field: "PR12 primitive admitted root URI",
        })?;
    if uri.scheme() != "file"
        || !admitted_root_uri
            .strip_prefix("file:")
            .is_some_and(|path| path.starts_with('/'))
        || uri.cannot_be_a_base()
        || !uri.path().starts_with('/')
        || uri.query().is_some()
        || uri.fragment().is_some()
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "PR12 primitive admitted root URI",
        });
    }
    Ok(())
}

fn admission_problem(
    scope: &ResolvedScope,
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
) -> Option<ApplicationProblemEnvelope> {
    if context.validate().is_err() || context.scope() != scope {
        return Some(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            context.request_id().clone(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));
    }
    match context.admission_at(observed_at) {
        RequestAdmission::Cancelled => {
            return Some(ApplicationProblemEnvelope::new(
                operation.result_contract().clone(),
                context.request_id().clone(),
                ApplicationProblem::cancelled_before_admission(),
            ));
        }
        RequestAdmission::TimedOut => {
            return Some(ApplicationProblemEnvelope::new(
                operation.result_contract().clone(),
                context.request_id().clone(),
                ApplicationProblem::timed_out_before_admission(),
            ));
        }
        RequestAdmission::Admitted => {}
    }
    if !access.allows(context, operation, observed_at) {
        return Some(ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            context.request_id().clone(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));
    }
    None
}

async fn dispatch_admitted(
    runtime: &OwnedPr12PrimitiveRuntime,
    invocation: Pr12PrimitiveInvocation,
    context: RequestContext,
    observed_at: UtcMicros,
) -> ApplicationResult<Value> {
    let operation = invocation.operation;
    if !valid_owned_symbol_graph_request(&invocation.request) {
        return invalid_request(&context, &operation);
    }
    match invocation.request {
        Pr12PrimitiveRequest::SymbolSearch(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .symbol_search(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Symbol,
                outcome,
            )
        }
        Pr12PrimitiveRequest::ExactSymbol(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .exact_symbol(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Symbol,
                outcome,
            )
        }
        Pr12PrimitiveRequest::SignatureSearch(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .signature_search(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Symbol,
                outcome,
            )
        }
        Pr12PrimitiveRequest::Implementations(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .implementations(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Graph,
                outcome,
            )
        }
        Pr12PrimitiveRequest::TypeHierarchy(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .type_hierarchy(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Graph,
                outcome,
            )
        }
        Pr12PrimitiveRequest::Callers(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .callers(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Graph,
                outcome,
            )
        }
        Pr12PrimitiveRequest::Callees(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .callees(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Graph,
                outcome,
            )
        }
        Pr12PrimitiveRequest::Impact(request) => {
            let outcome = runtime
                .project_runtime
                .symbol_graph
                .impact(symbol_context(&context, &operation, observed_at), &request)
                .await;
            symbol_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Graph,
                outcome,
            )
        }
        Pr12PrimitiveRequest::SourceRead(request) => {
            let outcome = runtime
                .project_runtime
                .source
                .source_read(
                    SourceReadPortContext {
                        request: &context,
                        operation: &operation,
                        observed_at,
                    },
                    &request,
                )
                .await;
            source_outcome(&runtime.access, &context, &operation, outcome)
        }
        Pr12PrimitiveRequest::TestMap(request) => {
            let outcome = runtime
                .project_runtime
                .tests
                .test_map(test_context(&context, &operation, observed_at), &request)
                .await;
            test_map_outcome(&runtime.access, &context, &operation, outcome)
        }
        Pr12PrimitiveRequest::AffectedFileTests(request) => {
            let outcome = runtime
                .project_runtime
                .tests
                .affected_file_tests(test_context(&context, &operation, observed_at), &request)
                .await;
            affected_file_tests_outcome(&runtime.access, &context, &operation, outcome)
        }
        Pr12PrimitiveRequest::LexicalGrep(request) => {
            let port_context = grep_context(&context, &operation, observed_at);
            let outcome = runtime
                .project_runtime
                .lexical_grep
                .grep(&port_context, &request)
                .await;
            grep_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Source,
                outcome,
            )
        }
        Pr12PrimitiveRequest::AstGrep(request) => {
            let port_context = grep_context(&context, &operation, observed_at);
            let outcome = runtime
                .project_runtime
                .ast_grep
                .ast_grep(&port_context, &request)
                .await;
            grep_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Source,
                outcome,
            )
        }
        Pr12PrimitiveRequest::Complexity(request) => {
            let port_context = grep_context(&context, &operation, observed_at);
            let outcome = runtime
                .project_runtime
                .complexity
                .complexity(&port_context, &request)
                .await;
            grep_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Operational,
                outcome,
            )
        }
        Pr12PrimitiveRequest::Redundancy(request) => {
            let port_context = grep_context(&context, &operation, observed_at);
            let outcome = runtime
                .project_runtime
                .redundancy
                .redundancy(&port_context, &request)
                .await;
            grep_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Operational,
                outcome,
            )
        }
        Pr12PrimitiveRequest::DependencyDepth(request) => {
            let port_context = grep_context(&context, &operation, observed_at);
            let outcome = runtime
                .project_runtime
                .dependency_depth
                .dependency_depth(&port_context, &request)
                .await;
            grep_outcome(
                &runtime.access,
                &context,
                &operation,
                EvidenceDomain::Graph,
                outcome,
            )
        }
        Pr12PrimitiveRequest::SessionLookup(request) => {
            let outcome = runtime
                .project_runtime
                .temporal
                .session_lookup(&retrieval_context(&context, &operation), &request);
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::QualifiedName(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .qualified_name(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::CallChain(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .call_chain(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::FileDependents(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .file_dependents(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::SourceLines(request) => {
            let outcome = runtime
                .project_runtime
                .source_lines
                .source_lines(&retrieval_context(&context, &operation), &request);
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::SourceBody(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .source_body(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::SourceOutline(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .source_outline(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::ModuleApi(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .module_api(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::FileMetadata(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .file_metadata(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::HealthRead(request) => {
            let outcome = runtime
                .project_runtime
                .health
                .health_read(&retrieval_context(&context, &operation), &request);
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::HealthDelta(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .health_delta(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::StorageStatus(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .storage_status(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::DiagnosticsRead(request) => {
            let outcome = runtime
                .project_runtime
                .extended
                .diagnostics(retrieval_context(&context, &operation), &request)
                .await;
            retrieval_outcome(&runtime.access, &context, &operation, outcome, observed_at)
        }
        Pr12PrimitiveRequest::Operational(request) => {
            if request.validate().is_err() {
                return invalid_request(&context, &operation);
            }
            let maximum_output_bytes = request.maximum_output_bytes as usize;
            let result = runtime
                .project_runtime
                .operational
                .read(&context, &operation, &request, observed_at)
                .await;
            validate_operational_result(&context, &operation, maximum_output_bytes, result)
        }
        Pr12PrimitiveRequest::RecentTestResults(page) => {
            recent_test_results(runtime, &context, &operation, &page, observed_at).await
        }
    }
}

fn valid_owned_symbol_graph_request(request: &Pr12PrimitiveRequest) -> bool {
    match request {
        Pr12PrimitiveRequest::SymbolSearch(request) => request.validate().is_ok(),
        Pr12PrimitiveRequest::ExactSymbol(request) => request.validate().is_ok(),
        Pr12PrimitiveRequest::SignatureSearch(request) => request.validate().is_ok(),
        Pr12PrimitiveRequest::Implementations(request) => request.validate().is_ok(),
        Pr12PrimitiveRequest::TypeHierarchy(request) => request.validate().is_ok(),
        Pr12PrimitiveRequest::Callers(request) | Pr12PrimitiveRequest::Callees(request) => {
            request.validate().is_ok()
        }
        Pr12PrimitiveRequest::Impact(request) => request.validate().is_ok(),
        _ => true,
    }
}

fn retrieval_context<'a>(
    context: &'a RequestContext,
    operation: &'a ApplicationOperation,
) -> RetrievalPortContext<'a> {
    RetrievalPortContext {
        request: context,
        operation,
    }
}

fn retrieval_outcome<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    outcome: RetrievalPortOutcome<T>,
    started_at: UtcMicros,
) -> ApplicationResult<Value> {
    let (termination, mut evidence) = match outcome {
        RetrievalPortOutcome::Completed(evidence) => (OperationTermination::Completed, evidence),
        RetrievalPortOutcome::Partial(evidence) => (OperationTermination::Partial, evidence),
        RetrievalPortOutcome::Cancelled(evidence) => (OperationTermination::Cancelled, evidence),
        RetrievalPortOutcome::TimedOut(evidence) => (OperationTermination::TimedOut, evidence),
        RetrievalPortOutcome::Failed(evidence) => (OperationTermination::Failed, evidence),
        RetrievalPortOutcome::Unavailable(evidence) => {
            (OperationTermination::Unavailable, evidence)
        }
    };
    if evidence.cancellation.is_none()
        && matches!(
            termination,
            OperationTermination::Cancelled | OperationTermination::TimedOut
        )
    {
        evidence.cancellation = Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: evidence.finished_at,
        });
    }
    let evidence =
        erase_retrieval_evidence(evidence).map_err(|_| contract_problem(context, operation))?;
    let authority = authority_receipt(access, context, operation, evidence.finished_at)?;
    let execution = OperationReceipt {
        started_at,
        ended_at: evidence.finished_at,
        effective_deadline: context.deadline().clone(),
        cancellation: evidence.cancellation.clone(),
        budget: evidence.budget,
        termination,
    };
    let packet = EvidencePacket::from_retrieval(evidence, authority, execution)
        .map_err(|_| contract_problem(context, operation))?;
    Ok(ApplicationEnvelope::evidence(
        operation.result_contract().clone(),
        context.request_id().clone(),
        context.scope().clone(),
        packet,
    ))
}

fn erase_retrieval_evidence<T: Serialize>(
    evidence: RetrievalEvidence<T>,
) -> Result<RetrievalEvidence<Value>, serde_json::Error> {
    Ok(RetrievalEvidence {
        payload: evidence.payload.map(serde_json::to_value).transpose()?,
        temporal: evidence.temporal,
        evidence_authorities: evidence.evidence_authorities,
        coverage: evidence.coverage,
        omissions: evidence.omissions,
        scores: evidence.scores,
        contributions: evidence.contributions,
        page: evidence.page,
        finished_at: evidence.finished_at,
        budget: evidence.budget,
        cancellation: evidence.cancellation,
    })
}

fn validate_operational_result(
    context: &RequestContext,
    operation: &ApplicationOperation,
    maximum_output_bytes: usize,
    result: ApplicationResult<Value>,
) -> ApplicationResult<Value> {
    match result {
        Ok(envelope)
            if envelope.contract == *operation.result_contract()
                && envelope.request_id == *context.request_id()
                && envelope.scope == *context.scope() =>
        {
            let bounded = match &envelope.outcome {
                tracedecay_application::ApplicationOutcome::Evidence(packet) => {
                    packet.payload.as_ref().is_none_or(|payload| {
                        serde_json::to_vec(payload)
                            .is_ok_and(|bytes| bytes.len() <= maximum_output_bytes)
                    })
                }
                _ => false,
            };
            if bounded {
                Ok(envelope)
            } else {
                unavailable(context, operation)
            }
        }
        Err(problem)
            if problem.contract == *operation.result_contract()
                && problem.request_id == *context.request_id() =>
        {
            Err(problem)
        }
        Ok(_) | Err(_) => unavailable(context, operation),
    }
}

fn symbol_context<'a>(
    context: &'a RequestContext,
    operation: &'a ApplicationOperation,
    observed_at: UtcMicros,
) -> SymbolGraphPortContext<'a> {
    SymbolGraphPortContext {
        request: context,
        operation,
        observed_at,
    }
}

fn test_context<'a>(
    context: &'a RequestContext,
    operation: &'a ApplicationOperation,
    observed_at: UtcMicros,
) -> TestPrimitivePortContext<'a> {
    TestPrimitivePortContext {
        request: context,
        operation,
        observed_at,
    }
}

fn grep_context<'a>(
    context: &'a RequestContext,
    operation: &'a ApplicationOperation,
    observed_at: UtcMicros,
) -> PrimitivePortContextV1<'a> {
    PrimitivePortContextV1 {
        request: context,
        operation,
        scope_prefix: None,
        observed_at,
    }
}

fn symbol_outcome<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    domain: EvidenceDomain,
    outcome: SymbolGraphPortOutcome<T>,
) -> ApplicationResult<Value> {
    match outcome {
        SymbolGraphPortOutcome::Completed {
            page,
            finished_at,
            budget,
        } => symbol_page(
            access,
            context,
            operation,
            domain,
            page,
            finished_at,
            budget,
            false,
        ),
        SymbolGraphPortOutcome::Partial {
            page,
            finished_at,
            budget,
        } => symbol_page(
            access,
            context,
            operation,
            domain,
            page,
            finished_at,
            budget,
            true,
        ),
        SymbolGraphPortOutcome::Failed { failure, .. } => {
            primitive_failure(context, operation, failure)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn symbol_page<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    domain: EvidenceDomain,
    page: SymbolGraphPage<T>,
    finished_at: UtcMicros,
    budget: OperationBudgetUsage,
    partial: bool,
) -> ApplicationResult<Value> {
    let returned = page.items.len() as u64;
    let total = page.total;
    let continuation = page.next_cursor.clone();
    let payload = serde_json::to_value(page).map_err(|_| contract_problem(context, operation))?;
    evidence_result(
        access,
        context,
        operation,
        domain,
        payload,
        PrimitiveCoverageV1 {
            completeness: if partial {
                CoverageCompleteness::Partial
            } else {
                CoverageCompleteness::Complete
            },
            visited: total.or(Some(returned)),
            eligible: total.or(Some(returned)),
            returned,
            unsupported_languages: Vec::new(),
        },
        continuation,
        finished_at,
        budget,
        partial,
    )
}

fn source_outcome(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    outcome: SourceReadPortOutcome,
) -> ApplicationResult<Value> {
    match outcome {
        SourceReadPortOutcome::Completed {
            result,
            finished_at,
            budget,
        } => {
            let payload =
                serde_json::to_value(result).map_err(|_| contract_problem(context, operation))?;
            evidence_result(
                access,
                context,
                operation,
                EvidenceDomain::Source,
                payload,
                simple_coverage(false, 1),
                None,
                finished_at,
                budget,
                false,
            )
        }
        SourceReadPortOutcome::Partial {
            result,
            finished_at,
            budget,
        } => {
            let payload =
                serde_json::to_value(result).map_err(|_| contract_problem(context, operation))?;
            evidence_result(
                access,
                context,
                operation,
                EvidenceDomain::Source,
                payload,
                simple_coverage(true, 1),
                None,
                finished_at,
                budget,
                true,
            )
        }
        SourceReadPortOutcome::Failed { .. } => unavailable(context, operation),
    }
}

fn test_map_outcome(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    outcome: TestPrimitivePortOutcome<TestMapPrimitiveResultV1>,
) -> ApplicationResult<Value> {
    test_outcome(access, context, operation, outcome, |result| {
        (
            (result.coverage.len() + result.uncovered.len()) as u64,
            result.total,
            result.next_cursor.clone(),
        )
    })
}

fn affected_file_tests_outcome(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    outcome: TestPrimitivePortOutcome<AffectedFileTestsPrimitiveResultV1>,
) -> ApplicationResult<Value> {
    test_outcome(access, context, operation, outcome, |result| {
        (
            result.affected_tests.len() as u64,
            result.total,
            result.next_cursor.clone(),
        )
    })
}

fn test_outcome<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    outcome: TestPrimitivePortOutcome<T>,
    page: impl Fn(&T) -> (u64, Option<u64>, Option<OpaqueCursor>),
) -> ApplicationResult<Value> {
    match outcome {
        TestPrimitivePortOutcome::Completed {
            result,
            finished_at,
            budget,
        } => {
            let (returned, total, continuation) = page(&result);
            typed_result(
                access,
                context,
                operation,
                EvidenceDomain::Test,
                result,
                returned,
                total,
                continuation,
                finished_at,
                budget,
                false,
            )
        }
        TestPrimitivePortOutcome::Partial {
            result,
            finished_at,
            budget,
        } => {
            let (returned, total, continuation) = page(&result);
            typed_result(
                access,
                context,
                operation,
                EvidenceDomain::Test,
                result,
                returned,
                total,
                continuation,
                finished_at,
                budget,
                true,
            )
        }
        TestPrimitivePortOutcome::Failed { .. } => unavailable(context, operation),
    }
}

#[allow(clippy::too_many_arguments)]
fn typed_result<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    domain: EvidenceDomain,
    result: T,
    returned: u64,
    total: Option<u64>,
    continuation: Option<OpaqueCursor>,
    finished_at: UtcMicros,
    budget: OperationBudgetUsage,
    partial: bool,
) -> ApplicationResult<Value> {
    let payload = serde_json::to_value(result).map_err(|_| contract_problem(context, operation))?;
    let mut coverage = simple_coverage(partial, returned);
    coverage.visited = total;
    coverage.eligible = total;
    evidence_result(
        access,
        context,
        operation,
        domain,
        payload,
        coverage,
        continuation,
        finished_at,
        budget,
        partial,
    )
}

fn grep_outcome<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    domain: EvidenceDomain,
    outcome: PrimitiveOutcomeV1<T>,
) -> ApplicationResult<Value> {
    match outcome {
        PrimitiveOutcomeV1::Completed(page) => {
            grep_page(access, context, operation, domain, page, false)
        }
        PrimitiveOutcomeV1::Partial(page) => {
            grep_page(access, context, operation, domain, page, true)
        }
        PrimitiveOutcomeV1::Cancelled => interrupted(context, operation, true),
        PrimitiveOutcomeV1::TimedOut => interrupted(context, operation, false),
        PrimitiveOutcomeV1::Failed(problem) => grep_problem(context, operation, problem),
    }
}

fn grep_page<T: Serialize>(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    domain: EvidenceDomain,
    page: tracedecay_application::retrieval::grep_analysis::PrimitivePageV1<T>,
    partial: bool,
) -> ApplicationResult<Value> {
    let coverage = page.coverage.clone();
    let continuation = page.continuation.clone();
    let finished_at = page.finished_at;
    let payload = serde_json::to_value(page).map_err(|_| contract_problem(context, operation))?;
    evidence_result(
        access,
        context,
        operation,
        domain,
        payload,
        coverage,
        continuation,
        finished_at,
        OperationBudgetUsage::default(),
        partial,
    )
}

#[allow(clippy::too_many_arguments)]
fn evidence_result(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    domain: EvidenceDomain,
    payload: Value,
    coverage: PrimitiveCoverageV1,
    continuation: Option<OpaqueCursor>,
    finished_at: UtcMicros,
    budget: OperationBudgetUsage,
    partial: bool,
) -> ApplicationResult<Value> {
    let payload_bytes =
        serde_json::to_vec(&payload).map_err(|_| contract_problem(context, operation))?;
    if payload_bytes.len() > MAX_OPERATION_OUTPUT_BYTES {
        return unavailable(context, operation);
    }
    let authority = authority_receipt(access, context, operation, finished_at)?;
    let complete = coverage.completeness == CoverageCompleteness::Complete;
    let visited = coverage
        .visited
        .or_else(|| complete.then_some(coverage.returned));
    let eligible = coverage
        .eligible
        .or_else(|| complete.then_some(coverage.returned));
    let evidence_coverage = EvidenceCoverage {
        requested_domains: vec![domain],
        visited,
        eligible,
        returned: coverage.returned,
        completeness: coverage.completeness,
        domains: vec![CoverageDomainState {
            domain,
            completeness: coverage.completeness,
        }],
    };
    evidence_coverage
        .validate()
        .map_err(|_| contract_problem(context, operation))?;
    let mut page = PageState::first_page(
        PR12_PRIMITIVE_SORT_CONTRACT.clone(),
        1,
        eligible,
        coverage.returned,
    )
    .map_err(|_| contract_problem(context, operation))?;
    page.cursor = continuation;
    if page.cursor.is_some() {
        page.expires_at = Some(context.deadline().expires_at);
    }
    let execution = OperationReceipt {
        started_at: context.grant().issued_at,
        ended_at: finished_at,
        effective_deadline: context.deadline().clone(),
        cancellation: None,
        budget,
        termination: if partial {
            OperationTermination::Partial
        } else {
            OperationTermination::Completed
        },
    };
    execution
        .validate()
        .map_err(|_| contract_problem(context, operation))?;
    Ok(ApplicationEnvelope::evidence(
        operation.result_contract().clone(),
        context.request_id().clone(),
        context.scope().clone(),
        EvidencePacket {
            temporal: TemporalState::current(finished_at),
            authority,
            evidence_authorities: Vec::new(),
            coverage: evidence_coverage,
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page,
            execution,
            payload: Some(payload),
        },
    ))
}

async fn recent_test_results(
    runtime: &OwnedPr12PrimitiveRuntime,
    context: &RequestContext,
    operation: &ApplicationOperation,
    page: &PageRequest,
    observed_at: UtcMicros,
) -> ApplicationResult<Value> {
    let current = match runtime.test_run_scope.current_identity().await {
        Ok(identity) => ManagedTestRunCurrentScope {
            root_uri: runtime.admitted_root_uri.clone(),
            head_commit_id: Some(identity.head_commit_id),
            code_generation_id: Some(identity.code_generation_id),
            document_uri: None,
            document_content_digest: None,
        },
        Err(_) => return unavailable(context, operation),
    };
    let snapshot = match runtime.test_runs.latest_current_page(&current, page).await {
        ManagedTestRunReadOutcome::Current(snapshot) => snapshot,
        ManagedTestRunReadOutcome::Stale(
            ManagedTestRunStaleReason::SourceIdentity | ManagedTestRunStaleReason::DocumentContent,
        ) => {
            return problem(
                context,
                operation,
                ApplicationProblem::stale(
                    SafeDiagnostic::new(
                        "application.retrieval.test-results-stale",
                        "The retained managed test result does not match the current source identity.",
                    )
                    .unwrap_or_else(|_| panic!("static diagnostic is valid")),
                ),
            );
        }
        ManagedTestRunReadOutcome::Unavailable(_) => return unavailable(context, operation),
    };
    let returned = snapshot.results.len() as u64;
    let available_results = snapshot.available_results as u64;
    let termination = snapshot.termination;
    let next_cursor = snapshot.next_cursor;
    let partial = !matches!(termination, Some(OperationTermination::Completed))
        || next_cursor.is_some()
        || available_results < snapshot.completed;
    let payload = json!({
        "operation_id": snapshot.operation_id.to_string(),
        "generation": snapshot.generation,
        "head_commit_id": snapshot
            .head_commit_id
            .as_ref()
            .map(CommitId::as_str),
        "code_generation_id": snapshot
            .code_generation_id
            .as_ref()
            .map(CodeGenerationId::as_str),
        "results": snapshot.results.into_iter().map(|result| json!({
            "test": result.test,
            "passed": result.passed,
        })).collect::<Vec<_>>(),
        "completed": snapshot.completed,
        "total": snapshot.total,
        "termination": termination,
        "result_offset": snapshot.result_offset,
        "available_results": available_results,
    });
    evidence_result(
        &runtime.access,
        context,
        operation,
        EvidenceDomain::Test,
        payload,
        PrimitiveCoverageV1 {
            completeness: if partial {
                CoverageCompleteness::Partial
            } else {
                CoverageCompleteness::Complete
            },
            returned,
            visited: Some(available_results),
            eligible: Some(available_results),
            unsupported_languages: Vec::new(),
        },
        next_cursor,
        observed_at,
        OperationBudgetUsage::default(),
        partial,
    )
}

fn simple_coverage(partial: bool, returned: u64) -> PrimitiveCoverageV1 {
    PrimitiveCoverageV1 {
        completeness: if partial {
            CoverageCompleteness::Partial
        } else {
            CoverageCompleteness::Complete
        },
        visited: Some(returned),
        eligible: Some(returned),
        returned,
        unsupported_languages: Vec::new(),
    }
}

fn authority_receipt(
    access: &ProjectSourceAccessSnapshot,
    context: &RequestContext,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
) -> Result<AuthorityReceipt, ApplicationProblemEnvelope> {
    let policy = PolicyDecisionRef::new(
        format!(
            "route.application.retrieval.{}",
            access.binding.binding_id.as_str()
        ),
        1,
        access.configuration_provenance_digest.clone(),
        ComponentVersion::new("project-source-access.v1")
            .unwrap_or_else(|_| panic!("static component version is valid")),
    )
    .map_err(|_| contract_problem(context, operation))?;
    AuthorityReceipt::from_context(context, policy, observed_at)
        .map_err(|_| contract_problem(context, operation))
}

fn primitive_failure<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    failure: tracedecay_application::retrieval::PrimitiveFailure,
) -> ApplicationResult<T> {
    use tracedecay_application::retrieval::PrimitiveFailureKind;
    let application_problem = match failure.kind {
        PrimitiveFailureKind::InvalidRequest => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: failure.code,
                message: failure.message,
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        PrimitiveFailureKind::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        PrimitiveFailureKind::Stale => ApplicationProblem::stale(SafeDiagnostic {
            code: failure.code,
            message: failure.message,
        }),
        PrimitiveFailureKind::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: failure.code,
            message: failure.message,
        }),
    };
    problem(context, operation, application_problem)
}

fn grep_problem<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    failure: GrepAnalysisProblemV1,
) -> ApplicationResult<T> {
    match failure {
        GrepAnalysisProblemV1::Denied => problem(
            context,
            operation,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ),
        GrepAnalysisProblemV1::Cancelled => interrupted(context, operation, true),
        GrepAnalysisProblemV1::TimedOut => interrupted(context, operation, false),
        GrepAnalysisProblemV1::InvalidRequest(message) => problem(
            context,
            operation,
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "application.retrieval.invalid-request".to_owned(),
                    message,
                },
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            },
        ),
        GrepAnalysisProblemV1::AuthorityFailed(_) => unavailable(context, operation),
    }
}

fn interrupted<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    cancelled: bool,
) -> ApplicationResult<T> {
    let problem_value = if cancelled {
        ApplicationProblem::cancelled_before_admission()
    } else {
        ApplicationProblem::timed_out_before_admission()
    };
    problem(context, operation, problem_value)
}

fn invalid_request<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> ApplicationResult<T> {
    problem(
        context,
        operation,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "application.retrieval.invalid-request".to_owned(),
                message: "The primitive request is invalid.".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    )
}

fn unavailable<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> ApplicationResult<T> {
    problem(
        context,
        operation,
        ApplicationProblem::unavailable(
            SafeDiagnostic::new(
                "application.retrieval.unavailable",
                "The admitted primitive authority is unavailable.",
            )
            .unwrap_or_else(|_| panic!("static diagnostic is valid")),
        ),
    )
}

fn saturated<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> ApplicationResult<T> {
    problem(
        context,
        operation,
        ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic::new(
                "application.retrieval.saturated",
                "The admitted primitive authority has reached its bounded capacity.",
            )
            .unwrap_or_else(|_| panic!("static diagnostic is valid")),
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
    )
}

fn problem<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    problem: ApplicationProblem,
) -> ApplicationResult<T> {
    Err(ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        problem,
    ))
}

fn contract_problem(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> ApplicationProblemEnvelope {
    ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        ApplicationProblem::unavailable(
            SafeDiagnostic::new(
                "application.retrieval.contract",
                "The primitive authority returned an invalid result.",
            )
            .unwrap_or_else(|_| panic!("static diagnostic is valid")),
        ),
    )
}

fn validate_no_scope_selector(value: &Value) -> Result<(), ApplicationContractError> {
    let object = value
        .as_object()
        .ok_or(ApplicationContractError::Inconsistent {
            field: "PR12 primitive parameter object",
        })?;
    if object.contains_key("project_id")
        || object.contains_key("project_path")
        || object.contains_key("project_selector")
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "PR12 primitive request scope",
        });
    }
    Ok(())
}

fn validate_bounds(
    value: &Value,
    maximum_output_bytes: usize,
) -> Result<(), ApplicationContractError> {
    let parameter_bytes =
        serde_json::to_vec(value).map_err(|_| ApplicationContractError::Inconsistent {
            field: "PR12 primitive parameter serialization",
        })?;
    if parameter_bytes.len() > MAX_OPERATION_PARAMETERS_BYTES
        || maximum_output_bytes == 0
        || maximum_output_bytes > MAX_OPERATION_OUTPUT_BYTES
    {
        return Err(ApplicationContractError::InvalidRange {
            field: "PR12 primitive bounds",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Pr12ExtendedPrimitivePort, Pr12OperationalPrimitiveRequest, Pr12PrimitiveCapacity,
        Pr12PrimitiveDispatch, Pr12PrimitiveRequest, StorageStatusPrimitiveRequest,
        pre_admission_problem, valid_owned_symbol_graph_request, validate_admitted_root_uri,
    };
    use tracedecay_application::retrieval::{
        GraphRelationRequest, ImplementationSelector, ImplementationsRequest, ResultProjection,
        RetrievalOrder, RetrievalRequestMeta, SignatureSearchRequest, SymbolGraphScope,
        SymbolSearchPrimitiveRequest, TypeHierarchyRequest,
    };
    use tracedecay_application::{
        ApplicationProblemKind, CancellationContext, Deadline, PageRequest, RequestId,
    };
    use tracedecay_domain::{
        EphemeralSanitizedQueryViewV1, QueryNormalizationRevision, SanitizerRevision, UtcMicros,
    };

    fn assert_object_safe(_: &dyn Pr12PrimitiveDispatch) {}
    fn assert_extended_object_safe(_: &dyn Pr12ExtendedPrimitivePort) {}

    #[test]
    fn primitive_dispatch_is_object_safe() {
        let _ = assert_object_safe;
        let _ = assert_extended_object_safe;
    }

    #[test]
    fn transport_pre_admission_problems_are_canonical() {
        let operation =
            tracedecay_application::retrieval::catalog::primitive_read_operation("storage_status")
                .expect("operation contract")
                .expect("storage status operation");
        let request_id = RequestId::new("request.primitive.pre-admission").expect("request id");
        let deadline = Deadline::new(UtcMicros(200)).expect("deadline");
        let cancelled =
            CancellationContext::cancelled("cancel.primitive", UtcMicros(90)).expect("cancelled");

        let problem = pre_admission_problem(
            &request_id,
            &operation,
            UtcMicros(100),
            &deadline,
            &cancelled,
        )
        .expect("cancelled problem");
        assert_eq!(problem.problem.kind(), ApplicationProblemKind::Cancelled);

        let active = CancellationContext::active("cancel.primitive").expect("active");
        let problem =
            pre_admission_problem(&request_id, &operation, UtcMicros(200), &deadline, &active)
                .expect("timeout problem");
        assert_eq!(problem.problem.kind(), ApplicationProblemKind::TimedOut);
        assert!(
            pre_admission_problem(&request_id, &operation, UtcMicros(100), &deadline, &active)
                .is_none()
        );
    }

    #[test]
    fn primitive_dispatch_capacity_fails_closed_and_recovers() {
        let capacity = Pr12PrimitiveCapacity::new(1);
        let permit = capacity.try_acquire().expect("first permit");
        assert!(capacity.try_acquire().is_none());
        drop(permit);
        assert!(capacity.try_acquire().is_some());
    }

    #[test]
    fn admitted_root_uri_must_be_an_absolute_file_url() {
        assert!(validate_admitted_root_uri("file:///workspace/project").is_ok());
        for invalid in [
            "file:relative",
            "https://example.com/project",
            "file:///workspace/project?other=true",
            "file:///workspace/project#other",
        ] {
            assert!(validate_admitted_root_uri(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn operational_parameters_reject_cross_project_selectors() {
        let request = Pr12OperationalPrimitiveRequest {
            operation: super::Pr12OperationalPrimitive::Project,
            parameters: serde_json::json!({"project_path": "/other"}),
            maximum_output_bytes: 1024,
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn closed_request_round_trips_for_direct_daemon_invocation() {
        let request = Pr12PrimitiveRequest::Operational(Pr12OperationalPrimitiveRequest {
            operation: super::Pr12OperationalPrimitive::Status,
            parameters: serde_json::json!({"include_runtime": true}),
            maximum_output_bytes: 4096,
        });
        let encoded = serde_json::to_value(request).expect("encode closed request");
        assert!(matches!(
            serde_json::from_value(encoded).expect("decode closed request"),
            Pr12PrimitiveRequest::Operational(_)
        ));
    }

    #[test]
    fn typed_system_request_round_trips_without_value_parameters() {
        let request = Pr12PrimitiveRequest::StorageStatus(StorageStatusPrimitiveRequest {
            include_details: true,
        });
        let encoded = serde_json::to_value(request).expect("encode typed request");
        assert!(matches!(
            serde_json::from_value(encoded).expect("decode typed request"),
            Pr12PrimitiveRequest::StorageStatus(_)
        ));
    }

    #[test]
    fn invalid_owned_symbol_request_is_rejected_before_port_dispatch() {
        let meta = || {
            RetrievalRequestMeta::current(
                PageRequest::first(10).expect("page"),
                ResultProjection::Evidence,
                RetrievalOrder::StableIdentity,
            )
        };
        let query = EphemeralSanitizedQueryViewV1::sanitize(
            "query".to_owned(),
            SanitizerRevision::new("sanitizer.test").expect("sanitizer"),
            QueryNormalizationRevision::new("normalization.test").expect("normalization"),
        )
        .expect("query");
        let requests = [
            Pr12PrimitiveRequest::SymbolSearch(SymbolSearchPrimitiveRequest {
                query,
                scope: SymbolGraphScope {
                    path_prefix: Some("../other".to_owned()),
                },
                lazy_index_ignored_dependencies: false,
                meta: meta(),
            }),
            Pr12PrimitiveRequest::SignatureSearch(SignatureSearchRequest {
                returns: None,
                params: Vec::new(),
                is_async: None,
                scope: SymbolGraphScope::default(),
                meta: meta(),
            }),
            Pr12PrimitiveRequest::Implementations(ImplementationsRequest {
                selector: ImplementationSelector::Method {
                    name: String::new(),
                },
                scope: SymbolGraphScope::default(),
                meta: meta(),
            }),
            Pr12PrimitiveRequest::TypeHierarchy(TypeHierarchyRequest {
                node_id: "node".to_owned(),
                maximum_depth: 0,
                scope: SymbolGraphScope::default(),
                meta: meta(),
            }),
            Pr12PrimitiveRequest::Callers(GraphRelationRequest {
                node_id: "node".to_owned(),
                maximum_depth: 0,
                resolve_trait_dispatch: false,
                scope: SymbolGraphScope::default(),
                meta: meta(),
            }),
        ];

        assert!(
            requests
                .iter()
                .all(|request| !valid_owned_symbol_graph_request(request))
        );
    }
}
