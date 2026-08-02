//! Transport-neutral application contracts and direct use-case services.
//!
//! This crate owns no storage, transport, provider runtime, UI, model runtime,
//! Git mutation, scheduler, or root catalog composition.

#![forbid(unsafe_code)]

pub mod advisory;
pub mod api_migration;
pub mod authorization;
pub mod clock;
pub mod configuration;
pub mod context;
pub mod context_scout;
pub mod diagnostics;
pub mod doctor;
pub mod external_source;
pub mod feedback;
/// Compatibility re-export: the framed-log primitives moved down into
/// `tracedecay-domain` so the dependency-free kernel can use them without an
/// edge back up into this contract crate. Every historical
/// `tracedecay_application::framed_log::…` path still resolves here.
pub use tracedecay_domain::framed_log;
pub mod git;
pub mod handlers;
pub mod historical_query;
mod identity;
pub mod invocation;
pub mod lsp_context_catalog;
pub mod memory;
pub mod multi_root;
pub mod observability;
pub mod policy;
pub mod remote;
pub mod result;
pub mod retained_surfaces;
pub mod retrieval;
pub mod settings_preview;
pub mod source_edit;
pub mod storage;
pub mod work;
pub mod work_catalog;
pub mod work_dispatch;
pub mod work_execution;
pub mod work_read;
pub mod workflow_catalog;
pub mod workflow_coordination;
pub mod workflow_runtime;

mod error;
mod surface_binding;

pub(crate) use surface_binding::{current_bindings, current_bindings_with_slug, surface_name};

pub use advisory::*;
pub use api_migration::*;
pub use authorization::{
    AuthorizationAdmission, AuthorizationPhase, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, AuthorizationService, ConcealedResourceCause, NonDisclosureHooks,
    SourceAuthorizationSnapshot,
};
pub use clock::now_micros;
pub use configuration::{
    ConfigurationGetRequestV1, ConfigurationSetRequestV1,
    configuration_surface_catalog_contribution, configuration_surface_handler_descriptors,
    configuration_surface_operation,
};
pub use context::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId,
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestAdmission,
    RequestContext, RequestId, ResolvedScope,
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
    ConfigurationDriftV1, DoctorConfirmationRequirementV1, DoctorCoverageCompletenessV1,
    DoctorCoverageStatementV1, DoctorEvidenceRefV1, DoctorEvidenceReferenceV1,
    DoctorEvidenceStateV1, DoctorFamilyConsultationV1, DoctorFamilyCoverageV1,
    DoctorFamilyUnavailableReasonV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorOwningOperationRefV1, DoctorOwningSurfaceV1, DoctorRemediationDescriptorV1,
    DoctorRemediationKindV1, DoctorRemediationRefV1, DoctorRemediationRegistryV1,
    DoctorRemediationResolutionErrorV1, DoctorReportComposerV1, DoctorReportCoverageV1,
    DoctorReportEntryV1, DoctorReportV1, DoctorSourceFuture, DoctorStorageFamilyReadV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1, HostConformanceV1,
    HostIntegrationDoctorPort, HostIntegrationReadV1, LanguageServerDoctorPort,
    LanguageServerReadV1, LanguageServerStateV1, ObservabilityDoctorPort, ObservabilityReadV1,
    ObservabilityStateV1, OperationalAuditDoctorPort, OperationalAuditReadV1,
    ProfileAuthorityReadV1, RemoteAuthorityReadV1, RemoteListenerReadV1, RemoteOperationalReadV1,
    RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1, StorageDoctorPort,
    advisory_feedback_findings, code_index_finding, configuration_finding,
    host_integration_finding, language_server_finding, observability_finding,
    operational_audit_findings, runtime_health_finding,
};
pub use error::ApplicationContractError;
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
#[cfg(feature = "native-git")]
pub use git::NativeHistoricalBlobReaderV1;
pub use git::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GIT_HISTORY_MAX_COUNT_LIMIT, GitBlameRequest,
    GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1, GitHistoricalBlobV1, GitHistoryRequest,
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, GitIntelligenceError, GitReadPort,
    git_index_catalog_contribution, git_index_effect_class, git_index_handler_descriptors,
    git_surface_catalog_contribution, git_surface_handler_descriptors,
    is_canonical_repository_relative_path,
};
pub use handlers::{
    ApplicationHandlerDescriptor, ApplicationHandlerDescriptors, ApplicationOperation,
    application_handler_descriptors,
};
pub use invocation::{
    ApplicationInvocation, ApplicationInvocationBinding, ApplicationInvocationContext,
    ApplicationInvocationExecutor, ApplicationInvocationFuture, ApplicationRequest,
    ApplicationResponse, ApplicationStream, ApplicationStreamResponse, InvocationCancellation,
    InvocationError, InvocationTarget,
};
pub use lsp_context_catalog::{lsp_context_catalog_contribution, lsp_context_handler_descriptors};
pub use memory::{
    DerivedMemoryConvergenceReportV1, DerivedMemoryConvergenceStateV1,
    DerivedMemoryFeedbackHistoryRepairV1, DerivedMemoryRepairPort, DerivedMemoryRepairStatsV1,
    converge_derived_memory,
};
pub use multi_root::{
    AuthorizedMultiRootQueryService, AuthorizedScopeSet, AuthorizedScopeSetAuthority,
    AuthorizedScopeSetError, MultiRootContinuationV1, MultiRootExecuteRequestV1,
    MultiRootOperationV1, MultiRootQueryError, MultiRootQueryPageV1, MultiRootQueryPort,
    MultiRootQueryRequestV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1,
    MultiRootScopeSetCasStatusV1, MultiRootScopeSetReadRequestV1,
};
pub use observability::*;
pub use policy::{
    PolicyConsumerV1, PolicyEvaluationContextV1, PolicyEvaluationV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceAgreementV1, PolicyEvidenceFrontierV1, PolicyEvidenceHorizonV1,
    RegisteredPolicyCapabilityV1,
};
pub use result::{
    APPLICATION_PROBLEM_REVISION, ApplicationEnvelope, ApplicationOutcome, ApplicationProblem,
    ApplicationProblemEnvelope, ApplicationProblemKind, ApplicationProblemRecord,
    ApplicationResult, AuthorityReceipt, BudgetClass, CancellationObservation, CancellationStage,
    CoverageCompleteness, CoverageDomainState, EffectId, EffectReceipt, EffectResult,
    EffectTermination, EvidenceAuthority, EvidenceCoverage, EvidenceDomain, EvidenceIdentity,
    EvidencePacket, EvidenceScore, EvidenceScoreKind, EvidenceScoreValue, FreshnessState,
    IdempotencyKey, LegalAction, Omission, OmissionReason, OpaqueCursor, OperationBudgetUsage,
    OperationReceipt, OperationTermination, PageState, PolicyDecisionRef, PreviewId, PreviewResult,
    ProblemOwningLayer, ProblemTerminality, ReconciliationState, ResultContractRef, ResumeToken,
    RetrievalEvidence, RetrieverContribution, RetrieverContributionState, RetryDirective,
    RetryScope, SafeDiagnostic, ScoreId, StreamEvent, StreamEventKind, StreamFrontier, StreamGap,
    StreamTermination, StreamValidationError, TemporalState, validate_stream,
};
pub use retained_surfaces::{
    RetainedSurfaceOperation, retained_surface_application_operation,
    retained_surface_catalog_contribution, retained_surface_handler_descriptors,
};
pub use retrieval::catalog::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_COMPACT_PROFILE_ID,
    APPLICATION_DEFAULT_PROFILE_ID, APPLICATION_HOST_LIMITED_PROFILE_ID,
    application_catalog_contributions,
};
pub use retrieval::{
    AffectedTestsRequest, AffectedTestsRetrievalPort, AnchorExpandRequest, AnchorExpandResult,
    AnchorHydrationPort, CALLABLE_CODE_OPERATION_COUNT, CallableCodeAuthorizationAdmission,
    CallableCodeAuthorizationFuture, CallableCodeAuthorizationPort, CallableCodeOperationKind,
    CallableCodeOperations, CallableCodeQueryFuture, CallableCodeQueryPort,
    CallableCodeQueryService, CodeFacetDimension, CodeFacetRecord, CodeFacetRequest,
    CodeHierarchyRequest, CodeImpactRequest, CodeImplementationsRequest, CodeLexicalField,
    CodeLexicalFieldFilter, CodeNavigationRequest, CodeOccurrenceRecord, CodeQueryPage,
    CodeQueryScope, CodeRelationRequest, CodeSignatureRequest, CodeSymbolSearchRequest,
    CodeTimelineRecord, CodeTimelineRequest, ExactOccurrenceRecord, ExactOccurrenceRequest,
    GraphCallersRequest, GraphImpactRequest, GraphImpactResult, GraphImpactRetrievalPort,
    GraphRetrievalPort, HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1,
    HealthDeltaRequest, HealthDeltaResult, HealthDeltaScopeV1, HealthDimensionDeltaV1,
    HealthDimensionPointV1, HealthReadRequest, LexicalOccurrenceRecord, MAX_APPLICATION_PAGE_SIZE,
    ModuleApiRequest, OperationalRetrievalPort, PageRequest, PhraseSearchRequest,
    QualifiedNameRequest, ResultProjection, RetrievalOrder, RetrievalPortContext,
    RetrievalPortOutcome, RetrievalRequestMeta, SessionLookupRequest, SourceLinesRequest,
    SourceLinesResult, SourceMetadataRecord, SourceMetadataRequest, SourceRetrievalPort,
    SymbolRetrievalPort, SymbolSearchRequest, SymbolSearchResult, TemporalRetrievalPort,
    TestRetrievalPort, UNPINNED_LATEST_GENERATION_SENTINEL,
    callable_code_catalog_contribution, callable_code_handler_descriptors,
    callable_code_operation, callable_code_operations, callable_code_request_schema,
    callable_code_result_schema,
};
pub use settings_preview::{
    MIN_AUTO_TRACK_PR_POLL_SECS_V1, ProjectSettingsPatchInputV1, SettingsValidationIssueV1,
    UserSettingsPatchInputV1, UserSettingsPreviewErrorV1, UserSettingsPreviewV1,
    UserSettingsValuesV1, parse_duration_label, prepare_user_settings_preview,
    validate_project_settings_patch, validate_user_settings_values,
};
pub use source_edit::{
    SourceEditAuthorizationAdmissionV1, SourceEditAuthorizationFuture, SourceEditAuthorizationPort,
    SourceEditDiagnosticV1, SourceEditEffectProofV1, SourceEditEffectRequestV1, SourceEditKind,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1, SourceEditRequest,
    SourceEditVerificationStateV1, SourceEditVerificationV1, source_edit_catalog_contribution,
    source_edit_handler_descriptors, source_edit_operation, source_edit_reconciliation_operation,
};
pub use storage::{
    BranchRefV1, CompactionDecisionV1, CompactionPlacementV1, CompactionTriggerPolicyV1,
    FreePageRatioV1, IncidentDebrisArtifactV1, IncidentDebrisKindV1, IncidentDebrisScanV1,
    OrphanStoreRecordV1, QuarantineContractV1, QuarantineLocationV1, QuarantinedArtifactV1,
    RelativeArtifactPathV1, RetentionBacklogRecordV1, StaleBranchDbRecordV1, StorageByteSizeV1,
    StorageTelemetryFuture, StorageTelemetryReadV1, StoreBudgetEvaluationV1, StoreKeyV1,
    StoreSizeBudgetV1, StoreSizeSampleV1, StoreSizeTelemetryPort, TableGrowthSampleV1, TableNameV1,
    incident_debris_finding, orphan_store_finding, over_budget_finding, retention_backlog_finding,
    stale_branch_dbs_finding,
};
pub use tracedecay_domain::framed_log::{
    DirectorySyncPolicy, append_durable, atomic_write, atomic_write_prepared, file_len,
    read_bounded, replace_via_rename, sync_directory, sync_parent_directory, tighten_existing_file,
    truncate_file, validate_regular_or_missing, with_owned_temp_publish,
};
pub use work::*;
pub use work_catalog::*;
pub use work_dispatch::*;
pub use work_execution::*;
pub use work_read::*;
pub use workflow_catalog::*;
pub use workflow_coordination::*;
pub use workflow_runtime::*;
