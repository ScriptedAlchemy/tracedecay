//! Transport-neutral ports and contracts for TraceDecay.
//!
//! This bottom layer defines request and result types, service contracts, and
//! the traits implemented by storage and runtime crates, including
//! [`WorkStoragePort`], [`WorkflowDefinitionAuthorityPort`],
//! [`StoreSizeTelemetryPort`], and [`SemanticActivationCoordinationPort`].
//! `tracedecay-application` depends on these contracts to orchestrate product
//! workflows; this crate never depends on that orchestration layer.
//!
//! Dependencies stay limited to `tracedecay-domain`, `tracedecay-policy`, and
//! `tracedecay-tool-catalog`. This crate owns no storage, transport, provider
//! runtime, UI, model runtime, Git mutation, scheduler, or process composition.
//! [`catalog_composition`] assembles the capability catalog from the operation
//! descriptors declared here, which is metadata assembly rather than
//! dependency construction.

#![forbid(unsafe_code)]

pub mod advisory;
mod application_catalog_projection;
pub mod authorization;
mod bearer_token;
pub mod branch_snapshots;
mod capability_manifest;
pub mod catalog_composition;
pub mod clock;
pub mod code_index_freshness;
pub mod configuration;
pub mod context;
pub mod context_scout;
pub mod diagnostics;
pub mod doctor;
pub mod execution_topology_metrics;
pub mod external_source;
pub mod feedback;
pub mod git;
pub mod handlers;
pub mod handoff;
pub mod handoff_catalog;
pub mod hint_outcomes;
pub mod historical_query;
mod hook_orchestration;
mod identity;
pub mod invocation;
pub mod lsp_context_catalog;
mod mcp_catalog;
pub mod memory;
pub mod multi_root;
pub mod observability;
pub mod observatory_surface;
pub mod policy;
mod profile_identity;
pub mod project_registry;
pub mod remote;
pub mod request_identity;
pub mod result;
pub mod retained_receipts;
pub mod retained_surfaces;
pub mod retrieval;
pub mod sdk_catalog;
pub mod semantic_activation;
pub mod session_sync;
mod session_temporal_refresh;
pub mod settings_preview;
pub mod source_edit;
mod source_edit_rollback;
pub mod storage;
pub mod work;
pub mod work_artifact_hydration;
pub mod work_attempt;
pub mod work_attempt_effect;
pub mod work_catalog;
pub mod work_duplicate_adjudication;
pub mod work_evidence;
pub mod work_execution_history;
pub mod work_handoff_frontier;
pub mod work_intelligence;
pub mod work_leak_adjudication;
pub mod work_owner_observation;
pub mod work_placement;
pub mod work_product;
pub mod work_read;
pub mod work_retry;
pub mod work_run_control;
pub mod work_synthesis;
pub mod work_topology_view;
pub mod workflow_admission;
pub mod workflow_catalog;
pub mod workflow_coordination;
pub mod workflow_effect;
pub mod workflow_fan_out_census;
pub mod workflow_provider;
pub mod workflow_run;
pub mod workflow_runtime;
pub mod workflow_synthesis;

mod error;
mod surface_binding;
pub mod surface_contracts;

pub(crate) use surface_binding::{
    current_application_bindings, current_bindings, current_bindings_with_slug, surface_name,
};

pub use advisory::{
    AdvisoryFindingContributionBatchV1, AdvisoryFindingContributorV1,
    AdvisoryFindingValidityWindowV1, CiCallerRelationV1, CiFailureBranchEvidenceV1,
    CiFailureCallerEvidenceV1, CiFailureCoverageV1, CiFailureGenerationEvidenceV1, CiFailureKindV1,
    CiFailureLocalizationResultV1, CiFailureLocalizationStateV1, CiFailureParserIdentityV1,
    CiFailureRunIdentityV1, CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, CiInertRerunHintV1,
    CiInertRerunTargetV1, GitHubPullRequestIdV1, GitHubReviewAuthorClassV1,
    GitHubReviewCommentIdV1, GitHubReviewCoverageV1, GitHubReviewCurrentBranchRemapV1,
    GitHubReviewCursorV1, GitHubReviewEtagV1, GitHubReviewIdV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitHubReviewLifecycleV1, GitHubReviewRateLimitCheckpointV1, GitHubReviewReadCheckpointV1,
    GitHubReviewReadOperationV1, GitHubReviewRemapStateV1, GitHubReviewStateV1,
    GitHubReviewThreadIdV1, MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_RERUN_HINTS_V1,
    MAX_CI_FAILURE_TEST_EVIDENCE_V1, PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1, ProximityAddressV1,
    ProximityBranchWorktreeIncompatibilityV1, ProximityContributionIdV1, ProximityContributionV1,
    ProximityCoverageV1, ProximityInclusionV1, ProximityObservationIdV1,
    ProximityRelationPathKindV1, ProximityRelationPathV1, ProximityRelationStrengthV1,
    ProximityRiskInputsV1, ProximityTierV1, ProximityWarningClassV1, ProximityWarningIdV1,
};
pub use authorization::{
    AuthorizationAdmission, AuthorizationPhase, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, AuthorizationService, ConcealedResourceCause, NonDisclosureHooks,
    SourceAuthorizationSnapshot,
};
pub use clock::{ClockError, now_micros, try_now_micros};
pub use configuration::{
    ActivationDriftV1, ComponentConfigurationState, ConfigurationAuditPage,
    ConfigurationAuditRequestV1, ConfigurationBatchRequestV1, ConfigurationDirectMutationRequestV1,
    ConfigurationGetRequestV1, ConfigurationListRequestV1, ConfigurationMutationReceipt,
    ConfigurationObservedStateRequestV1, ConfigurationProtectedApplyRequestV1,
    ConfigurationProtectedPreviewRequestV1, ConfigurationRollbackApplyRequestV1,
    ConfigurationRollbackPreviewRequestV1, ConfigurationSetRequestV1, ConfigurationUnsetRequestV1,
    ConfigurationWireRequestV1, ResolvedSetting, SettingSummary,
    configuration_surface_catalog_contribution, configuration_surface_handler_descriptors,
    configuration_surface_operation, configuration_surface_request_schema,
    configuration_surface_result_schema, configuration_wire_request_from_invocation_payload,
};
pub use context::{
    APPLICATION_REQUEST_ID_HEADER, ApplicationRequestControlV1, CancellationContext,
    CancellationSignal, CancellationState, CancellationTokenId, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestAdmission, RequestContext,
    RequestId, ResolvedScope,
};
pub use context_scout::{
    context_scout_surface_catalog_contribution, context_scout_surface_handler_descriptors,
    context_scout_surface_operation,
};
pub use diagnostics::{
    AnalyzerAdmittedDiagnosticProviderV1, CurrentDiagnosticsRequest, DiagnosticProviderDescriptor,
    DiagnosticProviderFuture, DiagnosticProviderIdentity, DiagnosticProviderIdentityParts,
    DiagnosticProviderPort, DiagnosticProviderResult, DiagnosticProviderState,
    GenerationDiagnosticHistoryPort, GenerationDiagnosticHistoryRequest, ProviderCoverage,
    ProviderDocumentIdentity, ProviderFreshness, ProviderOrigin, ProviderProvenance,
    ProviderSourceIdentity, RevisionDigest,
};
pub use doctor::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, CodeIndexMountDoctorPort, CodeIndexMountReadV1,
    CodeIndexMountStateV1, ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1,
    ConfigurationDriftV1, DOCTOR_FINDING_FAMILIES, DaemonRuntimeHealthSignalV1,
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFamilyConsultationV1,
    DoctorFamilyCoverageV1, DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1,
    DoctorFindingV1, DoctorKernelInputsV1, DoctorReportComposerV1, DoctorReportCoverageV1,
    DoctorReportEntryV1, DoctorReportV1, DoctorSourceFuture, DoctorStorageFamilyReadV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1, HostConformanceV1,
    HostIntegrationDoctorPort, HostIntegrationReadV1, IngestRefusalCensusReadV1,
    IngestRefusalCountV1, LanguageServerDoctorPort, LanguageServerReadV1, LanguageServerStateV1,
    ObservabilityDoctorPort, ObservabilityReadV1, ObservabilityStateV1, OperationalAuditDoctorPort,
    OperationalAuditReadV1, ProfileAuthorityReadV1, RemoteAuthorityReadV1, RemoteListenerReadV1,
    RemoteOperationalReadV1, RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1,
    StorageDoctorPort, advisory_feedback_findings, advisory_feedback_read_from_publication,
    code_index_finding, configuration_finding, doctor_finding_family_label,
    host_integration_finding, ingest_refusal_finding, language_server_finding, merge_storage_reads,
    observability_finding, operational_audit_findings, runtime_health_finding, runtime_health_read,
    storage_family_read,
};
pub use error::ApplicationContractError;
pub use execution_topology_metrics::{
    CONFLICT_MIN_ADJUDICATED_CASES_V1, EXECUTION_TOPOLOGY_CAPABILITY_ID_V1,
    EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1, EXECUTION_TOPOLOGY_EVENT_KINDS_V1,
    EXECUTION_TOPOLOGY_METRIC_DESCRIPTORS_V1, EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1,
    EXECUTION_TOPOLOGY_USE_CASE_ID_V1, ExecutionBlockedCauseV1, ExecutionConcurrencyPhaseV1,
    ExecutionConflictKindV1, ExecutionConflictOutcomeV1, ExecutionDeliveryOutcomeV1,
    ExecutionDuplicateKindV1, ExecutionDuplicateOutcomeV1, ExecutionDurationBucketV1,
    ExecutionFanoutPhaseV1, ExecutionGitHubStackCapabilityReadingV1,
    ExecutionGitHubStackCapabilityV1, ExecutionIntegrationKindV1, ExecutionIntegrationOutcomeV1,
    ExecutionIntervalStateV1, ExecutionLeakKindV1, ExecutionLeakOutcomeV1,
    ExecutionMetricUnavailableV1, ExecutionQuantityUnitV1, ExecutionRerunCauseV1,
    ExecutionRerunSourceV1, ExecutionStackDriftKindV1, ExecutionSurfaceFamilyV1,
    ExecutionTopologyBoundaryFragmentV1, ExecutionTopologyDimensionV1,
    ExecutionTopologyDrillAnchorV1, ExecutionTopologyEmissionCoverageV1,
    ExecutionTopologyMeasurementV1, ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    ExecutionTopologyRollupBuildErrorV1, ExecutionTopologyRollupBuildV1,
    ExecutionTopologyRollupErrorV1, ExecutionTopologyRollupFragmentPageV1,
    ExecutionTopologyRollupFragmentQueryV1, ExecutionTopologyRollupFragmentV1,
    ExecutionTopologyRollupQueryPort, ExecutionTopologyRollupRetentionV1, ExecutionWidthBucketV1,
    MAX_CENSORING_RATIO_V1, MAX_EXECUTION_TOPOLOGY_CELLS_V1,
    MAX_EXECUTION_TOPOLOGY_DRILL_ANCHORS_V1, MAX_EXECUTION_TOPOLOGY_EVENTS_V1,
    MAX_EXECUTION_TOPOLOGY_ROLLUP_DAYS_V1, MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1,
    MAX_EXECUTION_TOPOLOGY_ROLLUP_READ_BYTES_V1, MAX_METRIC_DIMENSIONS_V1, MIN_COVERAGE_RATIO_V1,
    MIN_EXECUTION_TOPOLOGY_LOCAL_CELL_SUPPORT_V1, RATE_MIN_ELIGIBLE_CASES_V1,
    build_empty_execution_topology_daily_rollup, build_execution_topology_boundary_fragment,
    build_execution_topology_daily_rollup, build_execution_topology_rollup_fragment,
    canonical_execution_topology_rollup_fragment_bytes,
    check_execution_topology_rollup_retention_json, duration_bucket,
    execution_topology_rollup_metrics, project_execution_topology_fragments,
    project_execution_topology_fragments_with_boundaries, width_bucket,
};
pub use external_source::{
    MAX_SOURCE_OBSERVATIONS_PER_ADMISSION_V1, SourceAdmissionAuthorityV1, SourceAuthorityContextV1,
    SourceCanonicalRefetchAuthorityV1, SourceCaptureAdmissionErrorV1, SourceCaptureAdmissionV1,
    SourceCaptureApplicationV1, SourceEventAdmissionContextV1, SourceEventAdmissionV1,
    SourceSanitizationAuthorityV1,
};
pub use feedback::{
    FeedbackExpandRequestV1, FeedbackExpandResultV1, FeedbackGetRequestV1, FeedbackGetResultV1,
    FeedbackHandleRequestV1, FeedbackListRequestV1, FeedbackListResultV1, FeedbackObservationPort,
    FeedbackReadService, feedback_surface_catalog_contribution,
    feedback_surface_handler_descriptors, feedback_surface_operation,
};
pub use git::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GIT_HISTORY_MAX_COUNT_LIMIT, GIT_QUERY_DEFAULT_MAX_BYTES,
    GIT_QUERY_DEFAULT_MAX_ENTRIES, GitBlameRequest, GitHistoricalBlobReadPort,
    GitHistoricalBlobRequestV1, GitHistoricalBlobV1, GitHistoryRequest, GitIndexApplyPortResultV1,
    GitIndexApplyRequestV1, GitIndexEffectProofV1, GitIndexOperationBindingV1,
    GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1,
    GitIndexTransactionApplicationError, GitIndexTransactionPort, GitIndexTransactionPortError,
    GitIndexTransactionService, GitIntelligenceError, GitReadPort,
    NATIVE_INTEGRATION_APPLY_OPERATION, NATIVE_INTEGRATION_CANCEL_OPERATION,
    NATIVE_INTEGRATION_PREFLIGHT_OPERATION, NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
    NATIVE_INTEGRATION_STATUS_OPERATION, NativeIntegrationApplyRequestV1,
    NativeIntegrationApplySurfaceRequest, NativeIntegrationCancelDispositionV1,
    NativeIntegrationCancelRequestV1, NativeIntegrationCancelSurfaceRequest,
    NativeIntegrationCancellationProjectionV1, NativeIntegrationContractError,
    NativeIntegrationPort, NativeIntegrationPortError, NativeIntegrationPreflightOutcomeV1,
    NativeIntegrationPreflightRequestV1, NativeIntegrationPreflightSurfaceRequest,
    NativeIntegrationPreviewProjectionV1, NativeIntegrationReceiptProjectionV1,
    NativeIntegrationRecoveryRequestV1, NativeIntegrationSealedStackSnapshotProjectionV1,
    NativeIntegrationSealedStackSnapshotV1, NativeIntegrationSelectionBindingV1,
    NativeIntegrationSelectionDeclarationV1, NativeIntegrationService,
    NativeIntegrationSnapshotProjectionV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStackResolutionRequestV1,
    NativeIntegrationStackSnapshotService, NativeIntegrationStackSnapshotSurfaceRequest,
    NativeIntegrationStatusProjectionV1, NativeIntegrationStatusRequestV1,
    NativeIntegrationStatusSurfaceRequest, NativeIntegrationSurfaceResultV1,
    NativeIntegrationSurfaceUnavailableV1, NativeWorktreeService, NativeWorktreeSurfaceRequest,
    NativeWorktreeSurfaceResultV1, WorktreeContractError, git_index_catalog_contribution,
    git_index_effect_class, git_index_handler_descriptors, git_surface_catalog_contribution,
    git_surface_handler_descriptors, is_canonical_repository_relative_path,
    native_integration_surface_catalog_contribution,
    native_integration_surface_handler_descriptors, native_integration_surface_operation,
};
pub use handlers::{
    ApplicationHandlerDescriptor, ApplicationHandlerDescriptors, ApplicationOperation,
    application_handler_descriptors,
};
pub use handoff::{
    HANDOFF_ISSUE_CAPABILITY_ID_V1, HANDOFF_ISSUE_USE_CASE_ID_V1, HandoffAuthoritySnapshotV1,
    HandoffOpenAuthorityError, HandoffOpenAuthorityPort, HandoffOpenBindingV1,
    HandoffOpenConsumeOutcomeV1, HandoffOpenConsumptionV1, HandoffOpenContextV1, HandoffOpenError,
    HandoffOpenExpectationV1, HandoffOpenGrantV1, HandoffOpenKindV1, HandoffOpenListFilterV1,
    HandoffOpenListingV1, HandoffOpenReceiptV1, HandoffOpenService, HandoffOpenTargetError,
    HandoffOpenTargetFuture, HandoffOpenTargetPort, HandoffOpenTargetV1, HandoffOpenToken,
    HandoffSessionId, InvestigationHandoffSurfaceV1, IssueTaskHandoffRequestV1,
    IssueTaskHandoffResultV1, LIST_TASK_HANDOFFS_CAPABILITY_ID_V1,
    LIST_TASK_HANDOFFS_USE_CASE_ID_V1, ListTaskHandoffsRequestV1, ListTaskHandoffsResultV1,
    ListedTaskHandoffV1, MAX_HANDOFF_LIST_RESULTS_V1, MAX_HANDOFF_OPEN_LIFETIME_MICROS,
    OPEN_INVESTIGATION_HANDOFF_CAPABILITY_ID_V1, OPEN_INVESTIGATION_HANDOFF_USE_CASE_ID_V1,
    OPEN_TASK_HANDOFF_CAPABILITY_ID_V1, OPEN_TASK_HANDOFF_USE_CASE_ID_V1,
    OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1, OpenTaskHandoffRequestV1,
    OpenTaskHandoffResultV1, TaskHandoffSurfaceV1, TaskHandoffTokenStateV1,
    handoff_open_consumption_input_digest, handoff_open_receipt_digest,
    investigation_owner_version_digest,
};
pub use handoff_catalog::{
    HANDOFF_APPLICATION_OPERATION_IDS_V1, handoff_executable_binding_registry,
};
pub use hint_outcomes::{
    HintEmission, HintOutcomeCorrelationPort, HintOutcomeObservation, HintOutcomePortError,
    HintOutcomePortFuture, HintOutcomePortOperation, HintOutcomeResolution, HintToolActivity,
};
pub use hook_orchestration::HookOrchestrationAdmissionV1;
pub use invocation::{
    ApplicationInvocation, ApplicationInvocationBinding, ApplicationInvocationContext,
    ApplicationInvocationExecutor, ApplicationInvocationFuture, ApplicationRequest,
    ApplicationResponse, ApplicationStream, ApplicationStreamResponse, InvocationCancellation,
    InvocationError, InvocationTarget,
};
pub use lsp_context_catalog::{lsp_context_catalog_contribution, lsp_context_handler_descriptors};
pub use mcp_catalog::mcp_executable_binding_registry;
pub use multi_root::{
    AuthorizedMultiRootQueryService, AuthorizedRoot, AuthorizedRootAdmission, AuthorizedScopeSet,
    AuthorizedScopeSetAuthority, AuthorizedScopeSetError, MultiRootCollectionResolutionV1,
    MultiRootCollectionSelectorV1, MultiRootCollectionUnavailableV1, MultiRootContinuationV1,
    MultiRootExecuteRequestV1, MultiRootOperationV1, MultiRootQueryError, MultiRootQueryPageV1,
    MultiRootQueryPort, MultiRootQueryRequestV1, MultiRootScopeSetCasRequestV1,
    MultiRootScopeSetCasResultV1, MultiRootScopeSetCasStatusV1, MultiRootScopeSetReadRequestV1,
    RegisteredRootLocatorV1, RegisteredRootSelectorV1, SharedProfileStoreLocatorV1,
};
pub use observability::{
    AGGREGATE_SHARE_MAX_CELLS_V1, AGGREGATE_SHARE_MAX_DIMENSIONS_V1,
    AGGREGATE_SHARE_MIN_CONTRIBUTION_WINDOWS_V1, AggregateCapabilityV1, AggregateOsFamilyV1,
    AggregateOutcomeV1, AggregateShareCellV1, AggregateShareDimensionV1,
    AggregateShareExportRequestV1, AggregateShareMetricV1, AggregateSharePacketV1,
    AggregateShareUnitV1, AnalyticsModeReadModelV1, ComparisonDispositionV1, CostsReadModelV1,
    LatencyDistributionReadModelV1, MetricCalibrationV1, MetricCohortV1, MetricCoverageV1,
    MetricEvidenceClassV1, MetricProvenanceV1, MetricSourceV1, MetricTemporalV1,
    MetricUncertaintyV1, MetricValueV1, ObservabilityAggregateExportApplicationV1,
    ObservabilityAggregateExportPort, ObservabilityApplicationV1, ObservabilityFuture,
    ObservabilityHorizonV1, ObservabilityPageV1, ObservabilityQueryPort, ObservabilityQueryV1,
    ObservabilityRecordPort, ObservatoryReadModelV1, PerformanceComparisonReadModelV1,
    ProviderLatencyReadModelV1, RejectedArgumentAnalyticsV1, RejectedArgumentGroupV1,
};
pub use observatory_surface::{
    OBSERVATORY_READ_OPERATION, ObservatoryReadRequestV1, ObservatoryReadResultV1,
    observatory_read_catalog_contribution, observatory_read_handler_descriptor,
    observatory_read_operation, observatory_read_request_schema, observatory_read_result_schema,
};
pub use policy::{
    PolicyConsumerV1, PolicyEvaluationContextV1, PolicyEvaluationV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceAgreementV1, PolicyEvidenceFrontierV1, PolicyEvidenceHorizonV1,
    RegisteredPolicyCapabilityV1,
};
pub use profile_identity::ProfileIdentityReadPort;
pub use project_registry::{
    ProjectRegistryContextCommand, ProjectRegistryContextFuture, ProjectRegistryContextOutcome,
    ProjectRegistryContextView, ProjectRegistryEntry, ProjectRegistryListingCommand,
    ProjectRegistryListingFuture, ProjectRegistryListingOutcome, ProjectRegistryListingScope,
    ProjectRegistryListingView, ProjectRegistryReadPort, ProjectRegistrySelector,
    ProjectRegistrySummary, ProjectRegistryView, ProjectRepoGroup, PublicCodeProject,
    list_registered_projects, read_registered_project_context, render_project_registry_view,
};
pub use remote::status::RemoteOperationalStatusReaderV1;
pub use result::{
    APPLICATION_PROBLEM_REVISION, ApplicationEnvelope, ApplicationExecutionFailureClassV1,
    ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind,
    ApplicationProblemRecord, ApplicationResult, ApplicationUnavailableClassV1, AuthorityReceipt,
    BudgetClass, CancellationObservation, CancellationStage, CoverageCompleteness,
    CoverageDomainState, EffectId, EffectReceipt, EffectResult, EffectTermination,
    EvidenceAuthority, EvidenceCoverage, EvidenceDomain, EvidenceIdentity, EvidencePacket,
    EvidenceScore, EvidenceScoreKind, EvidenceScoreValue, FreshnessState, IdempotencyKey,
    LegalAction, Omission, OmissionReason, OpaqueCursor, OperationBudgetUsage, OperationReceipt,
    OperationTermination, PageCursor, PageState, PolicyDecisionRef, PreviewId, PreviewResult,
    ProblemOwningLayer, ProblemTerminality, ReconciliationState, ResultContractRef, ResumeToken,
    RetrievalEvidence, RetrieverContribution, RetrieverContributionState, RetryDirective,
    RetryScope, SafeDiagnostic, ScoreId, StreamEvent, StreamEventKind, StreamFrontier, StreamGap,
    StreamTermination, StreamValidationError, TemporalState, validate_stream,
};
pub use retained_receipts::{
    PreparedRetainedEffect, authority_receipt, effective_memory_deadline, evidence_outcome,
    measured_budget, memory_expiry_partial, prepare_retained_effect,
    session_refresh_effect_outcome,
};
pub use retained_surfaces::{
    RetainedLcmExecutionPortV1, RetainedLcmRequestV1, RetainedMemoryExecutionPortV1,
    RetainedMemoryRequestV1, RetainedSessionExecutionPortV1, RetainedSessionRequestV1,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceExecutionFutureV1, RetainedSurfaceOperation, RetainedSurfacePortsV1,
    RetainedSurfaceServiceV1, retained_surface_application_operation,
    retained_surface_catalog_contribution, retained_surface_executable_binding_registry,
    retained_surface_execution_problem, retained_surface_handler_descriptors,
    retained_surface_operation_is_effect, retained_surface_outcome_matches_terminal,
    retained_surface_problem_matches_terminal,
};
pub use retrieval::catalog::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_COMPACT_PROFILE_ID,
    APPLICATION_DEFAULT_PROFILE_ID, APPLICATION_HOST_LIMITED_PROFILE_ID,
    application_catalog_contributions, application_operation_default_page_size,
};
pub use retrieval::{
    AffectedTestsRequest, AffectedTestsRetrievalPort, AnchorExpandRequest, AnchorExpandResult,
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeAuthorizationAdmission,
    CallableCodeAuthorizationFuture, CallableCodeAuthorizationPort, CallableCodeOperationKind,
    CallableCodeOperations, CallableCodeQueryFuture, CallableCodeQueryPort,
    CallableCodeQueryService, CodeFacetDimension, CodeFacetRecord, CodeFacetRequest,
    CodeHierarchyRequest, CodeImpactRequest, CodeImplementationsRequest, CodeLexicalField,
    CodeLexicalFieldFilter, CodeNavigationRequest, CodeOccurrenceRecord, CodeQueryPage,
    CodeQueryScope, CodeRelationRequest, CodeSignatureRequest, CodeSymbolSearchRequest,
    CodeTimelineRecord, CodeTimelineRequest, ExactOccurrenceRecord, ExactOccurrenceRequest,
    GraphImpactResult, HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1,
    HealthDeltaRequest, HealthDeltaResult, HealthDeltaScopeV1, HealthDimensionDeltaV1,
    HealthDimensionPointV1, HealthReadRequest, LexicalOccurrenceRecord, MAX_APPLICATION_PAGE_SIZE,
    ModuleApiRequest, OperationalRetrievalPort, PageRequest, PhraseSearchRequest,
    QualifiedNameRequest, ResultProjection, RetrievalOrder, RetrievalPortContext,
    RetrievalPortOutcome, RetrievalRequestMeta, SessionLookupRequest, SourceLinesRequest,
    SourceLinesResult, SourceMetadataRecord, SourceMetadataRequest, SourceRetrievalPort,
    TemporalRetrievalPort, UNPINNED_LATEST_GENERATION_SENTINEL, callable_code_catalog_contribution,
    callable_code_handler_descriptors, callable_code_operation, callable_code_operations,
    callable_code_request_schema, callable_code_result_schema,
};
pub use sdk_catalog::{
    application_http_executable_binding_registry, application_http_route_path,
    sdk_executable_binding_registry,
};
pub use semantic_activation::{
    SemanticActivationCoordinationErrorV1, SemanticActivationCoordinationPort,
};
pub use session_temporal_refresh::{
    SessionTemporalRefreshWakeFuture, SessionTemporalRefreshWakePort,
    UnavailableSessionTemporalRefreshWake,
};
pub use settings_preview::{
    MIN_AUTO_TRACK_PR_POLL_SECS_V1, ProjectSettingsPatchInputV1, SettingsValidationIssueV1,
    validate_project_settings_patch,
};
pub use source_edit::{
    RenameDispositionCountsV1, RenameFileEditV1, RenameHazardKindV1, RenameHazardV1,
    RenameImpactV1, RenamePreviewAcceptanceV1, RenamePreviewNodeV1, RenamePreviewResultV1,
    RenamePreviewSurfaceRequestV1, RenameProtectedValueCategoryV1, RenameProtectedValueV1,
    RenameResult, RenameSiteDispositionV1, RenameSiteKindV1, RenameSiteV1, RenameSymbolBindingV1,
    RenameSymbolSurfaceRequestV1, SourceEditAuthorizationAdmissionV1,
    SourceEditAuthorizationFuture, SourceEditAuthorizationPort, SourceEditDiagnosticV1,
    SourceEditEffectProofV1, SourceEditEffectRequestV1, SourceEditInvocationV1, SourceEditKind,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationInvocationV1,
    SourceEditReconciliationRequestV1, SourceEditRequest, SourceEditRollbackInvocationV1,
    SourceEditVerificationStateV1, SourceEditVerificationV1, source_edit_catalog_contribution,
    source_edit_handler_descriptors, source_edit_operation, source_edit_reconciliation_operation,
};
pub use source_edit_rollback::{SourceEditRollbackRequestV1, source_edit_rollback_operation};
pub use storage::{
    CompactionDecisionV1, CompactionPlacementV1, CompactionTriggerPolicyV1, FreePageRatioV1,
    IncidentDebrisArtifactV1, IncidentDebrisKindV1, IncidentDebrisScanV1, OrphanStoreRecordV1,
    QuarantineContractV1, QuarantineLocationV1, QuarantinedArtifactV1, RelativeArtifactPathV1,
    RetentionBacklogRecordV1, SemanticVectorRetentionRecordV1, StorageByteSizeV1,
    StorageTelemetryFuture, StorageTelemetryReadV1, StoreBudgetEvaluationV1, StoreKeyV1,
    StoreSizeBudgetV1, StoreSizeSampleV1, StoreSizeTelemetryPort, TableGrowthSampleV1, TableNameV1,
    incident_debris_finding, orphan_store_finding, over_budget_finding, retention_backlog_finding,
    semantic_vector_retention_finding,
};
pub use surface_contracts::{
    CallableCodeSurfaceMeta, CallableCodeSurfaceRequest, CodeCalleesSurfaceRequest,
    CodeCallersSurfaceRequest, CodeExactOccurrenceSurfaceRequest, CodeFacetSurfaceRequest,
    CodeImplementationsSurfaceRequest, CodeNavigationSurfaceRequest,
    CodePhraseSearchSurfaceRequest, CodeSignatureSearchSurfaceRequest,
    CodeSymbolSearchSurfaceRequest, CodeTimelineSurfaceRequest, CodeTypeHierarchySurfaceRequest,
    NativeIntegrationSurfaceRequest, PrimitiveCodeSurfaceRequest, primitive_code_into_primitive,
};
pub use work::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, CreateWorkCommand,
    ReplanDependenciesCommand, ReviewProposalCommand, ReviewProposalDispositionV1,
    ReviewProposalRequestV1, WorkAppendOutcome, WorkAppendRequest, WorkReadiness,
    WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1, WorkRoutingSnapshotV1, WorkService,
    WorkStorageError, WorkStoragePort,
};
pub use work_artifact_hydration::{
    WorkArtifactHydrationRequestV1, WorkArtifactHydrationService, WorkArtifactHydrationV1,
    WorkAttemptArtifactsV1, WorkAttemptEvidencePageV1, WorkAttemptEvidenceReadPort,
    WorkAttemptEvidenceRowV1, WorkAttemptEvidenceStateV1,
};
pub use work_attempt::{
    CancelWorkAttemptCommand, MAX_WORK_ATTEMPT_CAPACITY_TASKS, MAX_WORK_ATTEMPT_LIST_PAGE_SIZE,
    ResumeWorkAttemptsCommand, StartWorkAttemptCommand, WorkAttemptAdmissionKind,
    WorkAttemptCapacityScopeV1, WorkAttemptCapacityV1, WorkAttemptCapacityVerdictV1,
    WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome, WorkAttemptListCoverageV1,
    WorkAttemptListCursorV1, WorkAttemptListPageV1, WorkAttemptListRequestV1, WorkAttemptListV1,
    WorkAttemptProviderOutcomeV1, WorkAttemptRecoveryReportV1, WorkAttemptService,
    WorkAttemptStatusRequestV1, WorkAttemptStorageError, WorkAttemptStoragePort,
    WorkAttemptStreamChannelV1, WorkAttemptStreamSummaryV1, WorkAttemptTopologyBindingV1,
    WorkAttemptTopologyStateV1, WorkProductAttemptServiceV1, WorkProductSynthesisAttemptServiceV1,
    WorkProviderAvailabilityV1, WorkProviderFallbackRecordV1, WorkSynthesisAdmissionStoragePort,
    WorkSynthesisInsertOutcome, require_registered_work_topology,
};
pub use work_attempt_effect::{
    WorkAttemptEffectDispatchOutcomeV1, WorkAttemptEffectHolderErrorV1, WorkAttemptEffectHolderV1,
    WorkAttemptEffectResolutionV1, WorkAttemptEffectServiceV1, WorkAttemptEffectStorageErrorV1,
    WorkAttemptEffectStoragePortV1,
};
pub use work_catalog::{
    WORK_APPLICATION_OPERATION_IDS_V1, work_executable_binding, work_executable_binding_registry,
    work_executable_catalog_digest,
};
pub use work_duplicate_adjudication::{
    MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1, PrepareWorkDuplicateAdjudicationRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkDuplicateAdjudicationPortV1,
    WorkDuplicateAdjudicationServiceV1, WorkDuplicateAdjudicationStorageErrorV1,
    WorkDuplicateAdjudicationWriteV1, WorkDuplicateAttemptClassificationReadV1,
    WorkDuplicateAttemptClassificationRequestV1, WorkDuplicateAttemptClassificationV1,
    WorkDuplicateClassificationUnavailableReasonV1, prepare_work_duplicate_adjudication,
    work_duplicate_adjudication_input_digest,
};
pub use work_evidence::{
    MAX_WORK_ROOTED_EVIDENCE_SOURCES_V1, VerifiedWorkEvidenceRootV1, WorkAnchorHydrationFuture,
    WorkAnchorHydrationPortV1, WorkAnchorHydrationRequestV1, WorkAnchorHydrationV1,
    WorkAttemptReceiptReadErrorV1, WorkAttemptReceiptReadPortV1, WorkAttemptReceiptV1,
    WorkEvidenceContinuationV1, WorkEvidenceCoverageStateV1, WorkEvidenceCoverageV1,
    WorkEvidenceExpansionSelectorV1, WorkEvidenceFreshnessV1, WorkEvidenceHydrationErrorV1,
    WorkEvidenceOmissionReasonV1, WorkEvidenceOmissionV1, WorkEvidenceRetrievalPortV1,
    WorkEvidenceRetrievalServiceV1, WorkEvidenceRetrievalV1, WorkEvidenceRetrieveRequestV1,
    WorkEvidenceRootReadErrorV1, WorkEvidenceRootReadPortV1, WorkEvidenceSourceV1,
    WorkTaskSessionContinuationV1, WorkTaskSessionCoverageV1, WorkTaskSessionEvidenceV1,
    WorkTaskSessionFuture, WorkTaskSessionHydrationStateV1, WorkTaskSessionHydrationV1,
    WorkTaskSessionPortV1, WorkTaskSessionRankContributionV1, WorkTaskSessionRankedAnchorV1,
    WorkTaskSessionReauthorizationErrorV1, WorkTaskSessionReauthorizationPortV1,
    WorkTaskSessionRequestV1,
};
pub use work_execution_history::{
    WorkExecutionHistoryV1, WorkExecutionSpanV1, WorkExecutionTimingCoverageV1,
    WorkObservedExecutionOrderBasisV1, WorkObservedExecutionV1, project_work_execution_history,
};
pub use work_handoff_frontier::{
    MAX_WORK_HANDOFF_ENTRIES, MAX_WORK_HANDOFF_ENTRY_BYTES, WorkHandoffAttemptFrontierV1,
    WorkHandoffFrontierError, WorkHandoffFrontierV1, WorkHandoffLineageV1,
};
pub use work_intelligence::{
    GenerateProposalRequest, GeneratedWorkProposal, MAX_WORK_EXPERIENCE_CANDIDATES_V1,
    WorkCalibrationEvidenceV1, WorkCalibrationProvenanceV1, WorkCalibrationUncertaintyV1,
    WorkExperienceApplicabilityV1, WorkExperienceCandidateV1, WorkExperienceCoverageV1,
    WorkExperienceRequestV1, WorkExperienceV1, WorkExpertiseAuthorizationV1,
    WorkExpertiseConsentPinV1, WorkExpertiseConsentSnapshotV1, WorkExpertiseContextDurabilityV1,
    WorkExpertiseLegalActionV1, WorkExpertiseUnavailableReasonV1, WorkIntelligenceServiceV1,
    WorkProposalComparisonEffectV1, WorkProposalComparisonRequestV1, WorkProposalComparisonV1,
};
pub use work_leak_adjudication::{
    AdjudicateWorkLeakCommandV1, MAX_WORK_LEAK_EVIDENCE_REFS_V1, MAX_WORK_LEAK_HORIZON_MICROS_V1,
    MAX_WORK_LEAK_SCAN_MICROS_V1, VerifiedWorkLeakEvidenceV1, WorkLeakAdjudicationOutcomeV1,
    WorkLeakAdjudicationReceiptV1, WorkLeakAdjudicationServiceV1,
    WorkLeakAdjudicationStorageErrorV1, WorkLeakAdjudicationStoragePortV1,
    WorkLeakAdjudicationWriteV1, WorkLeakEvidenceErrorV1, WorkLeakEvidencePortV1,
};
pub use work_owner_observation::{
    PendingWorkOwnerObservationV1, WorkOwnerObservationKindV1, WorkOwnerObservationMarkOutcomeV1,
    WorkOwnerObservationMarkerV1, WorkOwnerObservationReceiptV1, WorkOwnerObservationScanCursorV1,
    WorkOwnerObservationStorageErrorV1, WorkOwnerObservationStoragePortV1,
};
pub use work_placement::{
    AdmitWorkPlacementCommand, ReleaseWorkPlacementCommand, WorkPlacementPreflightRequestV1,
    WorkPlacementReadingV1, WorkPlacementService, WorkPlacementStatusRequestV1,
    WorkPlacementStorageError, WorkPlacementStoragePort,
};
pub use work_product::{
    AcceptWorkTaskRequestV1, AddWorkTaskRequestV1, AdmitWorkExecutionRequestV1,
    AuthorizedWorkProductScopeV1, CreateWorkProductRequestV1, CreateWorkTaskRequestV1,
    DecideWorkProposalRequestV1, MAX_WORK_EVIDENCE_SELECTION_V1,
    MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1, MAX_WORK_HISTORY_EVENTS_V1,
    PrepareWorkProductMutationRequestV1, SelectedWorkEvidenceV1, VerifiedWorkEvidenceExpansionV1,
    VerifiedWorkGraphVersionV1, WorkEvidenceExpandRequestV1, WorkEvidenceExpansionV1,
    WorkEvidenceReadPortErrorV1, WorkEvidenceReadPortV1, WorkEvidenceSelectRequestV1,
    WorkGraphReadModeV1, WorkGraphReadPortErrorV1, WorkGraphReadPortV1, WorkGraphReadRequestV1,
    WorkGraphReadV1, WorkGraphSelectionCoverageV1, WorkGraphTimelineCoverageV1,
    WorkGraphTimelineV1, WorkGraphVersionEntryV1, WorkHistoryCoverageV1, WorkHistoryReadPortV1,
    WorkHistoryRequestV1, WorkHistoryServiceV1, WorkHistoryV1, WorkProductApplicationErrorV1,
    WorkProductAttemptAdmissionErrorV1, WorkProductAttemptAdmissionOutcomeV1,
    WorkProductAttemptAdmissionPortV1, WorkProductAttemptAdmissionV1, WorkProductBindingV1,
    WorkProductChangeDraftV1, WorkProductEventCommitOutcomeV1, WorkProductEventCommitV1,
    WorkProductEventDraftV1, WorkProductEventPortErrorV1, WorkProductEventPortV1,
    WorkProductEvidenceServiceV1, WorkProductExpectedAuthorityV1, WorkProductMutationIdentityV1,
    WorkProductMutationReceiptV1, WorkProductMutationRequestV1, WorkProductMutationServiceV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductReadServiceV1, WorkProductRetryAdmissionV1,
    WorkProductRevisionPinsV1, WorkProductSelectionScopeV1, WorkProductSynthesisAdmissionV1,
    WorkRelationScopeV1, work_product_projection_generation,
};
pub use work_read::{
    MAX_WORK_PROJECTION_PAGE_SIZE, WorkProjectionApplicationError, WorkProjectionPortError,
    WorkProjectionReadPort, WorkProjectionReadService,
};
pub use work_retry::{
    RetryWorkAttemptCommandV1, RuntimeWorkRetryEvidenceV1, VerifiedWorkRetryFailureV1,
    WorkProductRetryServiceV1, WorkRetryAttemptOutcomeV1, WorkRetryCauseV1,
    WorkRetryEvidenceErrorV1, WorkRetryEvidencePortV1, WorkRetryFailureSelectorV1,
    WorkRetryReceiptV1, WorkRetrySourceV1, WorkRetryStoragePortV1, WorkRetryWriteV1,
    WorkflowFanOutRetryRebindV1,
};
pub use work_run_control::{
    PauseWorkRunCommand, ResumeWorkRunCommand, WorkRunAdmissionV1, WorkRunControlFrontierV1,
    WorkRunControlReadingV1, WorkRunControlRequestV1, WorkRunControlService,
    WorkRunControlStorageError, WorkRunControlStoragePort, WorkRunControlTransitionReceiptV1,
    WorkRunLiveAttemptV1,
};
pub use work_synthesis::{
    AdmitWorkSynthesisCommand, WorkSynthesisAdmissionRecordV1, WorkSynthesisAdmissionV1,
    WorkSynthesisAttemptV1, WorkSynthesisEvidenceGroupV1, WorkSynthesisRefusalV1,
    WorkSynthesisSourceEnvelopeV1, WorkSynthesisSourceOutcomeV1, WorkSynthesisSourceSetV1,
    admit_work_synthesis_against_registered_topology,
};
pub use work_topology_view::{
    ExecutionTopologyViewV1, WorkTopologyExecutionPlacementV1, WorkTopologyIntegrationStrategyV1,
    WorkTopologyPlacementLaneV1, WorkTopologyViewRequestV1, execution_topology_view,
};
pub use workflow_admission::WorkflowCatalogAdmissionError;
pub use workflow_catalog::{
    WORKFLOW_APPLICATION_OPERATION_IDS, workflow_executable_binding_registry,
};
pub use workflow_coordination::{
    TASK_HANDOFF_LIFETIME_MICROS, TaskHandoffAuthorityError, TaskHandoffAuthorityPort,
    TaskHandoffConsumeOutcome, TaskHandoffError, TaskHandoffGrant, TaskHandoffIssueRequest,
    TaskHandoffRedeemRequest, TaskHandoffRedeemed, TaskHandoffScope, TaskHandoffService,
    TaskHandoffToken, WorkflowCoordinationError, WorkflowDefinitionActivateRequest,
    WorkflowDefinitionAuthorityError, WorkflowDefinitionAuthorityPort, WorkflowDefinitionDiff,
    WorkflowDefinitionDiffRequest, WorkflowDefinitionDisposition, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionLifecycleCommand,
    WorkflowDefinitionLifecycleState, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionService, WorkflowDefinitionTransitionEntry,
    WorkflowDefinitionTransitionOutcome, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation, WorkflowLifecycleOperation, prepare_task_handoff_issue,
    prepare_task_handoff_redeem, prepare_workflow_definition_registration,
};
pub use workflow_effect::{
    WorkflowEffectAuthorityErrorV1, WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1,
    WorkflowEffectJournalRecordV1, WorkflowEffectJournalStateV1, WorkflowEffectMutationV1,
    WorkflowEffectOperationV1, WorkflowEffectOutcomeV1, WorkflowEffectPreparedV1,
    WorkflowEffectProblemV1, WorkflowEffectReceiptContextV1, WorkflowEffectSuccessV1,
    WorkflowEffectTerminalV1,
};
pub use workflow_fan_out_census::{
    WorkflowFanOutCensusBackfillPageV1, WorkflowFanOutCensusError, WorkflowFanOutCensusEvidenceV1,
    WorkflowFanOutCensusObservationV1, WorkflowFanOutCensusPersistOutcomeV1,
    WorkflowFanOutCensusStoragePort, WorkflowNonDuplicateAttemptsEvidenceV1,
    derive_workflow_fan_out_census,
};
pub use workflow_provider::{
    WorkflowProviderPlacementError, WorkflowProviderPlacementService, WorkflowProviderRegistration,
    WorkflowProviderRegistry, WorkflowTopologyPlacementRequest,
};
pub use workflow_run::{
    MAX_WORKFLOW_ARTIFACT_PAYLOAD_BYTES, WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1,
    WorkflowActiveRunRecoveryCursorV1, WorkflowActiveRunRecoveryPageV1, WorkflowAdmissionSnapshot,
    WorkflowArtifactPayload, WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError,
    WorkflowArtifactStorePort, WorkflowFanOutAttemptBindingV1, WorkflowRunAppendOutcome,
    WorkflowRunAppendRequest, WorkflowRunCancelRequest, WorkflowRunGetRequest,
    WorkflowRunPauseRequest, WorkflowRunResumeRequest, WorkflowRunService, WorkflowRunServiceError,
    WorkflowRunStartRequest, WorkflowRunStorageError, WorkflowRunStoragePort,
    workflow_artifact_payload_digest,
};
pub use workflow_runtime::{
    WorkflowExecutionFence, WorkflowExecutionIdentity, WorkflowFailurePolicy, WorkflowFanOutInput,
    WorkflowFanOutPlan, WorkflowFanOutRequest, WorkflowFanOutRuntimeError, WorkflowFanOutStartV1,
    WorkflowPlannedChild, WorkflowProviderAdmission, durable_workflow_fan_out_plan,
    prepare_workflow_fan_out,
};
pub use workflow_synthesis::{
    WorkflowSynthesisDraft, WorkflowSynthesisRefusal, verify_workflow_synthesis_draft,
};
