mod callable_code;
mod callable_code_catalog;
mod callable_code_service;
pub mod catalog;
mod git_topology_anchor;
pub mod grep_analysis;
mod ports;
mod primitive_surface;
mod requests;
mod service;
mod source_read;
mod symbol_graph;
mod test_attribution;

use crate::error::ApplicationContractError;

/// Shared bounded-string validator for the retrieval leaf modules.
/// Delegates to [`crate::identity::validate_identifier`] so a single
/// implementation defines what counts as a valid identifier or bounded query
/// string (non-empty, trimmed, control-character-free, within
/// `maximum_bytes`). Pass `usize::MAX` for fields that intentionally allow
/// unbounded free text (e.g. a support-gap explanation) while still
/// rejecting empty, untrimmed, or control-character input.
fn validate_bounded_text(
    value: &str,
    field: &'static str,
    maximum_bytes: usize,
) -> Result<(), ApplicationContractError> {
    crate::identity::validate_identifier(value, field, maximum_bytes)
}

/// Shared node-id + traversal-depth validator for the graph primitive
/// surfaces (symbol graph and callable code). `node_field`/`node_max_bytes`
/// bound the node id text via [`validate_bounded_text`]; `depth_field`/
/// `max_depth` bound the requested traversal depth.
fn validate_node_depth(
    node_id: &str,
    node_field: &'static str,
    node_max_bytes: usize,
    maximum_depth: u32,
    depth_field: &'static str,
    max_depth: u32,
) -> Result<(), ApplicationContractError> {
    validate_bounded_text(node_id, node_field, node_max_bytes)?;
    if maximum_depth == 0 || maximum_depth > max_depth {
        return Err(ApplicationContractError::InvalidRange { field: depth_field });
    }
    Ok(())
}

/// Shared "current temporal mode + valid page request" check used by every
/// retrieval request whose `meta` only supports
/// [`tracedecay_domain::TemporalModeV1::Current`].
fn validate_current_temporal_meta(
    meta: &RetrievalRequestMeta,
    field: &'static str,
) -> Result<(), ApplicationContractError> {
    if meta.temporal != tracedecay_domain::TemporalModeV1::Current {
        return Err(ApplicationContractError::Inconsistent { field });
    }
    PageRequest::new(meta.page.page_size, meta.page.cursor.clone()).map(|_| ())
}

pub use callable_code::{
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeOperationKind, CallableCodeOperations,
    CodeFacetDimension, CodeFacetRecord, CodeFacetRequest, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeLexicalField, CodeLexicalFieldFilter, CodeNavigationRequest,
    CodeOccurrenceRecord, CodeQueryPage, CodeQueryScope, CodeRelationRequest, CodeSignatureRequest,
    CodeSymbolSearchRequest, CodeTimelineRecord, CodeTimelineRequest, ExactOccurrenceRecord,
    ExactOccurrenceRequest, LexicalOccurrenceRecord, MAX_CALLABLE_CODE_DEPTH,
    MAX_CALLABLE_CODE_FILTERS, MAX_CALLABLE_CODE_FUZZY_EXPANSIONS, MAX_CALLABLE_CODE_QUERY_BYTES,
    MAX_SOURCE_METADATA_FILES, ModuleApiRequest, PhraseSearchRequest, PhraseSearchSurfaceRequest,
    QualifiedNameRequest, SourceMetadataRecord, SourceMetadataRequest,
};
pub use callable_code_catalog::{
    callable_code_catalog_contribution, callable_code_handler_descriptors, callable_code_operation,
    callable_code_operations, callable_code_request_schema, callable_code_result_schema,
};
pub use callable_code_service::{
    CallableCodeAuthorizationAdmission, CallableCodeAuthorizationFuture,
    CallableCodeAuthorizationPort, CallableCodeQueryFuture, CallableCodeQueryPort,
    CallableCodeQueryService, UNPINNED_LATEST_GENERATION_SENTINEL,
};
pub use git_topology_anchor::{
    GitTopologyAnchorAuthorityErrorV2, GitTopologyAnchorAuthorityV2, GitTopologyAnchorFutureV2,
    GitTopologyAnchorPublicationOutcomeV2, GitTopologyAnchorPublicationV2,
    GitTopologyAnchorResolutionOutcomeV2, GitTopologyAnchorResolutionV2,
    MAX_GIT_TOPOLOGY_ANCHORS_PER_PUBLICATION_V2,
};
pub use grep_analysis::RedundancyResultV1;
pub use ports::{
    AffectedTestsRetrievalPort, AnchorHydrationPort, GraphImpactRetrievalPort, GraphRetrievalPort,
    OperationalRetrievalPort, RetrievalPortContext, RetrievalPortOutcome, SourceRetrievalPort,
    SymbolRetrievalPort, TemporalRetrievalFailure, TemporalRetrievalFuture, TemporalRetrievalPort,
};
pub use primitive_surface::{
    CalleeV1, CalleesResultV1, CalleesSurfaceRequestV1, ContextCodeBlockV1, ContextModeV1,
    ContextResultV1, ContextSurfaceRequestV1, ImpactNodeV1, ImpactResultV1, ImpactSurfaceRequestV1,
    NodeDepthSurfaceRequestV1, NodeDetailsV1, NodeExpansionCostV1, NodeResultV1,
    NodeSurfaceRequestV1, PortCycleAnchorV1, PortCycleFileV1, PortCycleSymbolV1, PortCycleV1,
    PortMatchedSymbolV1, PortOrderLevelV1, PortOrderResultV1, PortOrderSurfaceRequestV1,
    PortOrderSymbolV1, PortStatusResultV1, PortStatusSurfaceRequestV1, PortTargetOnlySymbolV1,
    PortUnmatchedSymbolV1, PrimitiveLaneCompleteV1, PrimitiveLaneStateV1, PrimitiveLaneStatusV1,
    PrimitiveNotFoundV1, PrimitiveRecallV1, PrimitiveSearchCoverageV1, PrimitiveSemanticModeV1,
    PrimitiveSymbolLocationV1, RedundancySurfaceRequestV1, RenamePreviewNodeV1,
    RenamePreviewPrimitiveOutcomeV1, RenamePreviewPrimitiveRequestV1,
    RenamePreviewPrimitiveResultV1, RenamePreviewReferenceV1, RenamePreviewTextOnlyMatchV1,
    SimilarResultV1, SimilarSurfaceRequestV1, SimilarSymbolV1, TodoMarkerV1, TodosResultV1,
    TodosSurfaceRequestV1,
};
pub use requests::{
    AffectedTestAttributionV1, AffectedTestsRequest, AffectedTestsResult, AnchorExpandRequest,
    AnchorExpandResult, CallChainPrimitiveRequest, CallChainPrimitiveResult,
    DiagnosticPrimitiveRecord, DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult,
    DiagnosticsPrimitiveScope, FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult,
    FileMetadataPrimitiveRequest, FileMetadataPrimitiveResult, FileMetadataRecord,
    GraphCallersRequest, GraphCallersResult, GraphImpactRequest, GraphImpactResult,
    HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1, HealthDeltaRequest,
    HealthDeltaResult, HealthDeltaScopeV1, HealthDimensionDeltaV1, HealthDimensionPointV1,
    HealthReadRequest, HealthReadResult, MAX_APPLICATION_PAGE_SIZE, ModuleApiPrimitiveRequest,
    ModuleApiPrimitiveResult, PageRequest, QualifiedNamePrimitiveRequest,
    QualifiedNamePrimitiveResult, ResultProjection, RetrievalOrder, RetrievalRequestMeta,
    SessionLookupRequest, SessionLookupResult, SourceBodyPrimitiveRequest,
    SourceBodyPrimitiveResult, SourceLinesRequest, SourceLinesResult,
    SourceOutlinePrimitiveRequest, SourceOutlinePrimitiveResult, SourceReference,
    StorageStatusHistoryPointV1, StorageStatusPrimitiveRequest, StorageStatusPrimitiveResult,
    SymbolSearchRequest, SymbolSearchResult,
};
pub use source_read::{
    MAX_SOURCE_READ_PATH_BYTES, SourceReadModeV1, SourceReadPortContext, SourceReadPortFuture,
    SourceReadPortOutcome, SourceReadPrimitivePort, SourceReadPrimitiveRequest, SourceReadResultV1,
};
pub use symbol_graph::{
    CallableCodeSurfaceMetaV1, CodeSymbolSearchSurfaceRequestV1, ExactSymbolRequest,
    GraphImpactPrimitiveRequest, GraphRelationRequest, ImplementationSelector,
    ImplementationsRequest, MAX_SYMBOL_GRAPH_DEPTH, MAX_SYMBOL_GRAPH_FILTERS,
    MAX_SYMBOL_GRAPH_QUERY_BYTES, PrimitiveFailure, PrimitiveFailureKind, PrimitiveSupportGap,
    SignatureSearchRequest, SymbolGraphPage, SymbolGraphPortContext, SymbolGraphPortFuture,
    SymbolGraphPortOutcome, SymbolGraphPrimitivePort, SymbolGraphScope, SymbolPrimitiveRecord,
    SymbolRelationRecord, SymbolSearchPrimitiveRequest, TypeHierarchyRecord, TypeHierarchyRequest,
};
pub use test_attribution::{
    AffectedFileTestsPrimitiveRequest, AffectedFileTestsPrimitiveResultV1, MAX_TEST_FILTER_BYTES,
    MAX_TEST_PRIMITIVE_DEPTH, MAX_TEST_PRIMITIVE_FILES, RankedAffectedTestV1, TestMapCoverageV1,
    TestMapPrimitiveRequest, TestMapPrimitiveResultV1, TestPrimitivePort, TestPrimitivePortContext,
    TestPrimitivePortFuture, TestPrimitivePortOutcome, TestReferenceV1, UncoveredSourceV1,
};
