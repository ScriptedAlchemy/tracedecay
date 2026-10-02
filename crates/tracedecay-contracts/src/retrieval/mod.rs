mod admin_cli_surface;
mod admin_project_surface;
mod analysis_report_surface;
mod analytics_surface;
mod automation_surface;
mod callable_code;
mod callable_code_catalog;
mod callable_code_service;
pub mod catalog;
mod git_context_surface;
mod git_topology_anchor;
mod graph_lookup_surface;
mod graph_report_surface;
pub mod grep_analysis;
mod hook_runtime_surface;
mod owner_effect_surface;
mod ports;
mod primitive_surface;
mod project_file_surface;
mod project_info_surface;
mod requests;
mod response_handle_surface;
mod search_surface;
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

/// The Remote Brain read is `tracedecay_remote_status`'s catalog result.
pub use crate::remote::status::RemoteOperationalStatusReadV1;
pub use admin_cli_surface::{
    AdminCliAnalyticsImportV1, AdminCliCostSummaryV1, AdminCliCostTodayV1, AdminCliCostTotalsV1,
    AdminCliGainDayV1, AdminCliGainHistoryV1, AdminCliGainTotalV1, AdminCliProjectTokenTotalV1,
    AdminCliProjectTokensV1, AdminCliRegistryContextV1, AdminCliRegistryEmptyV1,
    AdminCliRegistryGcV1, AdminCliRegistryListV1, AdminCliRegistryUpdateV1, AdminCliResultV1,
    AdminCliScopeV1, AdminCliSessionSyncV1, AdminCliStorageReportV1, AdminCliSurfaceRequestV1,
    AdminCliUnfinishedSessionsV1,
};
pub use admin_project_surface::{
    AdminProjectCounterResetV1, AdminProjectCounterV1, AdminProjectGitignoreStatusV1,
    AdminProjectResultV1, AdminProjectStatusAccountingV1, AdminProjectSurfaceRequestV1,
    AutomaticFactAddRequestV1, AutomaticFactEvidenceV1, AutomaticFactReceiptAvailabilityV1,
    AutomaticFactReceiptListV1, AutomaticFactReceiptStateV1, AutomaticFactReceiptV1,
    AutomaticFactReceiptViewV1, AutomationReconcileScope, AutomationSchedulerOwnerReconcileOutcome,
    AutomationSchedulerReconcileOutcome, ProfileAutomationReconcileReport,
    ProjectAutomationReconcileReport, UncachedProjectReconcileOutcome,
};
pub use analysis_report_surface::{
    CircularCycleV1, CircularResultV1, CircularSurfaceRequestV1, ComplexityReportEntryV1,
    ComplexityReportV1, ComplexitySurfaceRequestV1, ConstructorFieldCoverageV1,
    ConstructorResolutionReasonV1, ConstructorResolutionStatusV1, ConstructorSiteV1,
    ConstructorsNotFoundV1, ConstructorsReportV1, ConstructorsResultV1,
    ConstructorsSurfaceRequestV1, CouplingDirectionV1, CouplingEntryV1, CouplingResultV1,
    CouplingSurfaceRequestV1, DeadCodeResultV1, DeadCodeSurfaceRequestV1, DeadCodeSymbolV1,
    DistributionFileV1, DistributionKindCountV1, DistributionResultV1,
    DistributionSurfaceRequestV1, DistributionViewV1, DocCoverageFileV1, DocCoverageResultV1,
    DocCoverageSurfaceRequestV1, DocCoverageSymbolV1, FieldSiteV1, FieldSitesResultV1,
    FieldSitesSurfaceRequestV1, GodClassEntryV1, GodClassResultV1, GodClassSurfaceRequestV1,
    HotspotV1, HotspotsResultV1, HotspotsSurfaceRequestV1, InheritanceDepthEntryV1,
    InheritanceDepthResultV1, InheritanceDepthSurfaceRequestV1, LargestEntryV1, LargestResultV1,
    LargestSurfaceRequestV1, RankDirectionV1, RankEdgeKindV1, RankEntryV1, RankResultV1,
    RankSurfaceRequestV1, RecursionCycleV1, RecursionResultV1, RecursionSurfaceRequestV1,
    RecursionSymbolV1, UnmountedEcosystemStatusV1, UnmountedEcosystemV1, UnmountedFileV1,
    UnmountedFilesResultV1, UnmountedFilesSurfaceRequestV1, UnsafePatternFileOmissionV1,
    UnsafePatternKindV1, UnsafePatternMatchV1, UnsafePatternOmissionCauseV1,
    UnsafePatternOmissionReasonV1, UnsafePatternsResultV1, UnsafePatternsSurfaceRequestV1,
};
pub use analytics_surface::{
    ANALYTICS_DEFAULT_WINDOW_DAYS, ANALYTICS_MAX_WINDOW_DAYS, ANALYTICS_MIN_WINDOW_DAYS,
    AnalyticsAutomationJobOutcomesV1, AnalyticsAutomationOutcomesV1, AnalyticsAutomationSectionV1,
    AnalyticsCanonicalCallNameV1, AnalyticsEventCallNameV1, AnalyticsFactFunnelV1,
    AnalyticsFactsSectionV1, AnalyticsHintCategoryV1, AnalyticsHintsPayloadV1,
    AnalyticsLedgerUnavailableV1, AnalyticsProjectSectionUnavailableV1, AnalyticsReportStatusV1,
    AnalyticsResultV1, AnalyticsScopeV1, AnalyticsSectionUnavailableV1, AnalyticsSectionV1,
    AnalyticsSurfaceRequestV1, AnalyticsTierCountsV1, AnalyticsToolsSectionV1, AnalyticsTopToolV1,
    AnalyticsZeroCallToolsV1,
};
pub use automation_surface::{
    AUTOMATION_RUN_LIST_DEFAULT_LIMIT, AUTOMATION_RUN_LIST_MAX_LIMIT, AutomationReadStatusV1,
    AutomationRunArtifactViewResultV1, AutomationRunArtifactViewSurfaceRequestV1,
    AutomationRunListEntryV1, AutomationRunListResultV1, AutomationRunListSurfaceRequestV1,
    AutomationRunPageCompletenessV1, AutomationRunScopeV1, AutomationRunViewResultV1,
    AutomationRunViewSurfaceRequestV1, HermesSkillBridgeResultV1,
    HermesSkillBridgeSurfaceRequestV1, SkillListEntryV1, SkillListResultV1,
    SkillListSurfaceRequestV1, SkillSupportFileSummaryV1, SkillViewResultV1,
    SkillViewSurfaceRequestV1,
};
pub use callable_code::{
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeOperationKind, CallableCodeOperations,
    CodeFacetDimension, CodeFacetRecord, CodeFacetRequest, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeLexicalField, CodeLexicalFieldFilter, CodeNavigationRequest,
    CodeOccurrenceRecord, CodeQueryPage, CodeQueryRow, CodeQueryScope, CodeRelationRequest,
    CodeSignatureRequest, CodeSymbolSearchRequest, CodeTimelineRecord, CodeTimelineRequest,
    ExactOccurrenceRecord, ExactOccurrenceRequest, LexicalOccurrenceRecord,
    MAX_CALLABLE_CODE_DEPTH, MAX_CALLABLE_CODE_FILTERS, MAX_CALLABLE_CODE_FUZZY_EXPANSIONS,
    MAX_CALLABLE_CODE_QUERY_BYTES, MAX_SOURCE_METADATA_FILES, ModuleApiRequest,
    PhraseSearchRequest, QualifiedNameRequest, SourceMetadataRecord, SourceMetadataRequest,
};
pub(crate) use callable_code_catalog::callable_code_catalog_contribution_with;
pub use callable_code_catalog::{
    callable_code_catalog_contribution, callable_code_handler_descriptors, callable_code_operation,
    callable_code_operations, callable_code_request_schema, callable_code_result_schema,
};
pub use callable_code_service::{
    CallableCodeAuthorizationFuture, CallableCodeAuthorizationPort, CallableCodeQueryFuture,
    CallableCodeQueryPort, CallableCodeQueryService, UNPINNED_LATEST_GENERATION_SENTINEL,
};
pub use git_context_surface::{
    AffectedRankingMetadataV1, AffectedResultV1, AffectedSurfaceRequestV1, BranchDiffCompleteV1,
    BranchDiffPartialV1, BranchDiffReferenceUnavailableV1, BranchDiffResultV1, BranchDiffSummaryV1,
    BranchDiffSurfaceRequestV1, BranchDiffUnavailableV1, BranchListPageV1, BranchListResultV1,
    BranchListSurfaceRequestV1, BranchReadUnavailableV1, BranchReferenceUnavailableV1,
    BranchSearchHitV1, BranchSearchPageV1, BranchSearchResultV1, BranchSearchSurfaceRequestV1,
    BranchSearchUnavailableV1, BranchSnapshotEntryV1, BranchSymbolChangeV1, BranchSymbolV1,
    ChangelogCompleteV1, ChangelogPartialV1, ChangelogResultV1, ChangelogSurfaceRequestV1,
    CommitCategoryV1, CommitContextResultV1, CommitContextSummaryV1, CommitContextSurfaceRequestV1,
    CommitFileRoleV1, CommitSymbolEntryV1, CommitSymbolV1, ConfigSummaryKindV1, ConfigSummaryV1,
    DiffContextResultV1, DiffContextSurfaceRequestV1, GitCommitSubjectV1, GitComparedSymbolV1,
    GitContextSymbolV1, GitFileChangeStatusV1, GitFileChangeV1, GitFileRoleV1, GitPageStatusV1,
    GitReadCompleteV1, GitReadPartialV1, GitReadUnavailableV1, GitReferenceLimitV1,
    GitResultLimitV1, GitToolErrorKindV1, GitToolErrorV1, GitToolFailureV1, GitToolOperationV1,
    PrAnalysisCoverageV1, PrContextCompleteV1, PrContextGraphPendingV1, PrContextResultV1,
    PrContextSurfaceRequestV1, PrContextSymbolsUnavailableV1, PrCoverageSelectionV1,
    PrSelectionCoverageV1, PrSymbolChangesCompleteV1, PrSymbolEntryV1, PrSymbolPageV1,
    PrSymbolSelectionV1, SymbolChangesCompleteV1, SymbolChangesUnavailableV1,
};
pub use git_topology_anchor::{
    GitTopologyAnchorAuthority, GitTopologyAnchorAuthorityError, GitTopologyAnchorFuture,
    GitTopologyAnchorPublication, GitTopologyAnchorPublicationOutcome, GitTopologyAnchorResolution,
    GitTopologyAnchorResolutionOutcome, MAX_GIT_TOPOLOGY_ANCHORS_PER_PUBLICATION,
};
pub use graph_lookup_surface::{
    AstGrepSearchMatchV1, AstGrepSearchResultV1, AstGrepSearchSurfaceRequestV1,
    ByQualifiedNameResultV1, ByQualifiedNameSurfaceRequestV1, DeriveAnnotationV1,
    DeriveEvidenceClassV1, DerivesResultV1, DerivesSymbolV1, FindExactSymbolMatchV1,
    FindExactSymbolResultV1, FindExactSymbolSurfaceRequestV1, GrepGraphEnrichmentV1, GrepMatchV1,
    GrepScanOmissionsV1, GrepSearchResultV1, GrepSurfaceRequestV1, SignatureResultV1,
    SymbolSelectorSurfaceRequestV1, SymbolSignatureV1,
};
pub use graph_report_surface::{
    DependencyDepthSurfaceRequestV1, DiagnoseItemV1, DiagnosePublicationV1, DiagnoseResultV1,
    DiagnoseSeverityFilterV1, DiagnoseSeverityV1, DiagnoseSurfaceRequestV1, DiagnoseSymbolV1,
    DsmClusterV1, DsmMatrixV1, DsmResultV1, DsmShapeV1, DsmStatsV1, DsmSurfaceRequestV1,
    GiniMetricV1, GiniOutlierV1, GiniResultV1, GiniScopeV1, GiniSurfaceRequestV1,
    HealthAcyclicityV1, HealthCoverageDisciplineV1, HealthDepthV1, HealthDimensionsV1,
    HealthEqualityV1, HealthModularityV1, HealthRedundancyV1, HealthResultV1,
    HealthSurfaceRequestV1, HealthWeightsV1, TestAttributionMethodV1, TestMapResultV1,
    TestMapSourceCoverageV1, TestMapSurfaceRequestV1, TestMapTestV1, TestMapUncoveredV1,
    TestRiskAttributionSummaryV1, TestRiskBucketSummaryV1, TestRiskConfidenceV1, TestRiskEntryV1,
    TestRiskResultV1, TestRiskSummaryV1, TestRiskSurfaceRequestV1,
};
pub use grep_analysis::{DependencyDepthChainV1, DependencyDepthResultV1};
pub use hook_runtime_surface::{
    ContextScoutStoreStatusV1, HermesReceiptStatusV1, HookCompactionResultV1,
    HookIngestAdmissionV1, HookIngestTranscriptRequestV1, HookIngestTranscriptResultV1,
    HookRuntimeDispositionV1, HookRuntimeResultV1, HookRuntimeSurfaceRequestV1,
    HookV2AdmissionResultV1, HookV2AdmitRequestV1, HookV2NoticeDeliveryResultV1,
    HookV2ProfileAdmissionResultV1, HookV2RejectionReasonV1, hook_runtime_needs_session_stores,
};
pub use owner_effect_surface::{
    AdminSyncAdmissionV1, AdminSyncReconcileScopeV1, AdminSyncResultV1, AdminSyncSurfaceRequestV1,
    AffectedTestErrorV1, AffectedTestOutcomeV1, AffectedTestRunV1, AffectedTestsNotRunV1,
    DashboardActionV1, DashboardBoundV1, DashboardResultV1, DashboardSurfaceRequestV1,
    ManagedTestTerminalV1, RunAffectedTestsResultV1, RunAffectedTestsSurfaceRequestV1,
    TestProfileV1,
};
pub use ports::{
    AffectedTestsRetrievalPort, OperationalRetrievalPort, RetrievalPortContext,
    RetrievalPortOutcome, SessionRetrievalBudgetAccountingV1, SessionRetrievalBudgetObservationV1,
    SessionRetrievalBudgetStageV1, SessionRetrievalStructuralRefusalV1, SourceRetrievalPort,
    TemporalRetrievalFailure, TemporalRetrievalFuture, TemporalRetrievalPort,
};
pub use primitive_surface::{
    ContextCodeBlockV1, ContextExtensionPointV1, ContextLexicalAnchorV1, ContextModeV1,
    ContextPlanV1, ContextRelatedOmissionV1, ContextResultV1, ContextRetrievalPlanV1,
    ContextSearchMatchV1, ContextStageV1, ContextSurfaceRequestV1, ImpactNodeV1, ImpactResultV1,
    LexicalAnchorDropReasonV1, LexicalAnchorDropV1, MAX_REDUNDANCY_FAMILIES_V1,
    MAX_REDUNDANCY_PULL_REQUEST_PATHS_V1, MAX_REDUNDANCY_WORK_V1, NodeDepthSurfaceRequestV1,
    NodeDetailsV1, NodeExpansionCostV1, NodeResultV1, NodeSurfaceRequestV1, PortCycleAnchorV1,
    PortCycleFileV1, PortCycleSymbolV1, PortCycleV1, PortMatchedSymbolV1, PortOrderLevelV1,
    PortOrderResultV1, PortOrderSurfaceRequestV1, PortOrderSymbolV1, PortStatusResultV1,
    PortStatusSurfaceRequestV1, PortTargetOnlySymbolV1, PortUnmatchedSymbolV1,
    PrimitiveFreshnessStateV1, PrimitiveIndexingStateV1, PrimitiveLaneCompleteV1,
    PrimitiveLaneStateV1, PrimitiveLaneStatusV1, PrimitiveNotFoundV1, PrimitiveRecallV1,
    PrimitiveSearchCoverageV1, PrimitiveSearchFreshnessV1, PrimitiveSymbolLocationV1,
    PrimitiveUnavailableEvidenceV1, PrimitiveUnavailableStatusV1, RedundancyCoverageV1,
    RedundancyFamilyV1, RedundancyPartialReasonV1, RedundancyRankingV1, RedundancyResultV1,
    RedundancyScopeV1, RedundancySurfaceRequestV1, RenamePreviewNodeV1,
    RenamePreviewPrimitiveOutcomeV1, RenamePreviewPrimitiveRequestV1,
    RenamePreviewPrimitiveResultV1, RenamePreviewReferenceV1, RenamePreviewTextOnlyMatchV1,
    SimilarCoverageV1, SimilarFamilyV1, SimilarMatchClassV1, SimilarOccurrenceV1, SimilarResultV1,
    SimilarSurfaceRequestV1, SimilarTargetV1, TodoMarkerV1, TodosResultV1, TodosSurfaceRequestV1,
};
pub use project_file_surface::{
    ConfigKeyFoundV1, ConfigKeyMissingV1, ConfigMatchV1, ConfigParseErrorV1, ConfigResultV1,
    ConfigSurfaceRequestV1, FilesLayoutV1, FilesResultV1, FilesSurfaceRequestV1, IndexedFileV1,
};
pub use project_info_surface::{
    ActiveProjectBranchV1, ActiveProjectResolutionSourceV1, ActiveProjectResultV1,
    ActiveProjectStorageV1, ActiveProjectSurfaceRequestV1, ProjectContextResultV1,
    ProjectContextSurfaceRequestV1, ProjectListSurfaceRequestV1, ProjectRegistryListingResultV1,
    ProjectSearchSurfaceRequestV1, ProjectStatusV1, RegisteredProjectIdSelectorV1,
    RemoteStatusSurfaceRequestV1, RuntimeDoctorReportV1, RuntimeResultV1, RuntimeSurfaceRequestV1,
    StatusAdmissionV1, StatusBranchMismatchV1, StatusCodeIndexFreshnessV1,
    StatusGitStalenessUnavailableV1, StatusGitStalenessV1, StatusMemoryOwnerV1,
    StatusMemoryPressureV1, StatusMemoryV1, StatusResultV1, StatusRetrievalServingV1,
    StatusSchemaConvergenceStateV1, StatusSchemaConvergenceV1, StatusServingConditionV1,
    StatusServingFreshnessV1, StatusSessionGitEvidenceUnavailableV1, StatusSessionGitEvidenceV1,
    StatusSurfaceRequestV1,
};
pub use requests::{
    AffectedTestAttributionV1, AffectedTestsRequest, AffectedTestsResult, AnchorExpandRequest,
    AnchorExpandResult, CallChainPrimitiveRequest, CallChainPrimitiveResult,
    DiagnosticPrimitiveRecord, DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult,
    DiagnosticsPrimitiveScope, FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult,
    GraphImpactResult, HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1,
    HealthDeltaRequest, HealthDeltaResult, HealthDeltaScopeV1, HealthDimensionDeltaV1,
    HealthDimensionPointV1, HealthReadRequest, HealthReadResult, MAX_APPLICATION_PAGE_SIZE,
    ModuleApiPrimitiveRequest, ModuleApiPrimitiveResult, PageRequest, PrimitiveInvocation,
    PrimitiveRequest, QualifiedNamePrimitiveRequest, QualifiedNamePrimitiveResult,
    ResultProjection, RetrievalOrder, RetrievalRequestMeta, SessionLookupRequest,
    SessionLookupResult, SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceLinesRequest,
    SourceLinesResult, SourceOutlinePrimitiveRequest, SourceOutlinePrimitiveResult,
    SourceReference, StorageStatusHistoryPointV1, StorageStatusPrimitiveRequest,
    StorageStatusPrimitiveResult,
};
pub use response_handle_surface::{
    RetrieveHandleExpiredV1, RetrieveHandleMissingV1, RetrieveResultV1, RetrieveSurfaceRequestV1,
    RetrievedPageV1,
};
pub use search_surface::{
    SEARCH_MAX_LEXICAL_ALIASES, SEARCH_MAX_LEXICAL_ANCHOR_BYTES, SEARCH_MAX_LEXICAL_ANCHORS,
    SEARCH_MAX_LEXICAL_PHRASES, SEARCH_MAX_LEXICAL_PROXIMITIES, SEARCH_MAX_LEXICAL_PROXIMITY_GAP,
    SEARCH_MAX_LEXICAL_PROXIMITY_TERMS, SearchCompleteV1, SearchCoverageV1,
    SearchDisplayUnavailableV1, SearchExternalImportCandidatesV1, SearchExternalImportHintV1,
    SearchExternalImportV1, SearchLaneStateV1, SearchLaneStatusV1, SearchLexicalAliasV1,
    SearchLexicalAlternativeReasonV1, SearchLexicalFieldFilterV1, SearchLexicalFieldV1,
    SearchLexicalProximityV1, SearchLexicalRouteV1, SearchResultDisplayV1, SearchResultRowV1,
    SearchResultV1, SearchRouteMatchV1, SearchSpellingVariantV1, SearchSurfaceRequestV1,
    SearchUnavailableV1,
};
pub use source_read::{
    MAX_SOURCE_READ_PATH_BYTES, SourceReadModeV1, SourceReadPortContext, SourceReadPortFuture,
    SourceReadPortOutcome, SourceReadPrimitivePort, SourceReadPrimitiveRequest, SourceReadResultV1,
};
pub use symbol_graph::{
    CodeGraphReadFreshnessV1, ExactSymbolRequest, GraphImpactPrimitiveRequest,
    GraphRelationRequest, ImplementationRecord, ImplementationSelector, ImplementationsRequest,
    MAX_SYMBOL_GRAPH_DEPTH, MAX_SYMBOL_GRAPH_FILTERS, MAX_SYMBOL_GRAPH_QUERY_BYTES,
    PrimitiveFailure, PrimitiveFailureKind, PrimitiveSupportGap, ServedCodeGraphGenerationV1,
    SignatureSearchRequest, SymbolGraphItem, SymbolGraphPage, SymbolGraphPortContext,
    SymbolGraphPortFuture, SymbolGraphPortOutcome, SymbolGraphPrimitivePort, SymbolGraphScope,
    SymbolPrimitiveRecord, SymbolRelationRecord, SymbolSearchPrimitiveRequest, TypeHierarchyRecord,
    TypeHierarchyRequest,
};
pub use test_attribution::{
    AffectedFileTestsPrimitiveRequest, AffectedFileTestsPrimitiveResultV1, MAX_TEST_FILTER_BYTES,
    MAX_TEST_PRIMITIVE_DEPTH, MAX_TEST_PRIMITIVE_FILES, RankedAffectedTestV1, TestMapCoverageV1,
    TestMapPrimitiveRequest, TestMapPrimitiveResultV1, TestPrimitivePort, TestPrimitivePortContext,
    TestPrimitivePortFuture, TestPrimitivePortOutcome, TestReferenceV1, UncoveredSourceV1,
};
