//! Transport-neutral application contracts and direct use-case services.
//!
//! This crate owns no storage, transport, provider runtime, UI, model runtime,
//! Git mutation, scheduler, or root catalog composition.

#![forbid(unsafe_code)]

pub mod advisory;
pub mod authorization;
pub mod configuration;
pub mod context;
pub mod diagnostics;
pub mod external_source;
pub mod feedback;
pub mod framed_log;
pub mod git;
pub mod handlers;
pub mod policy;
pub mod result;
pub mod retrieval;

mod error;

pub use advisory::*;
pub use authorization::{
    AuthorizationAdmission, AuthorizationPhase, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, AuthorizationService, ConcealedResourceCause, NonDisclosureHooks,
    SourceAuthorizationSnapshot,
};
pub use configuration::{
    configuration_surface_catalog_contribution, configuration_surface_handler_descriptors,
    configuration_surface_operation,
};
pub use context::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId,
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestAdmission,
    RequestContext, RequestId, ResolvedScope,
};
pub use diagnostics::{
    AnalyzerAdmittedDiagnosticProviderV1, CurrentDiagnosticsRequest, DiagnosticProviderDescriptor,
    DiagnosticProviderFuture, DiagnosticProviderIdentity, DiagnosticProviderIdentityParts,
    DiagnosticProviderPort, DiagnosticProviderResult, DiagnosticProviderState,
    GenerationDiagnosticHistoryPort, GenerationDiagnosticHistoryRequest, ProviderCoverage,
    ProviderDocumentIdentity, ProviderFreshness, ProviderOrigin, ProviderProvenance,
    ProviderSourceIdentity, RevisionDigest,
};
pub use error::ApplicationContractError;
pub use external_source::{
    MAX_SOURCE_OBSERVATIONS_PER_ADMISSION_V1, SourceCaptureAdmissionErrorV1,
    SourceCaptureAdmissionV1,
};
pub use feedback::{feedback_surface_catalog_contribution, feedback_surface_handler_descriptors};
pub use framed_log::{
    DirectorySyncPolicy, append_durable, atomic_write, file_len, io_error, read_bounded,
    replace_via_rename, sync_directory, sync_parent_directory, tighten_existing_file,
    truncate_file, validate_regular_or_missing, with_owned_temp_publish,
};
pub use git::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, git_index_catalog_contribution,
    git_index_effect_class, git_index_handler_descriptors, git_surface_catalog_contribution,
    git_surface_handler_descriptors,
};
pub use handlers::{
    ApplicationHandlerDescriptor, ApplicationHandlerDescriptors, ApplicationOperation,
    application_handler_descriptors,
};
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
pub use retrieval::catalog::{APPLICATION_DEFAULT_PROFILE_ID, application_catalog_contributions};
pub use retrieval::{
    AffectedTestsRequest, AffectedTestsRetrievalPort, AffectedTestsService, AnchorExpandRequest,
    AnchorExpandResult, AnchorHydrationPort, CALLABLE_CODE_OPERATION_COUNT,
    CallableCodeOperationKind, CallableCodeOperations, CallableCodeQueryFuture,
    CallableCodeQueryPort, CallableCodeQueryService, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeOccurrenceRecord, CodeQueryPage, CodeQueryScope,
    CodeRelationRequest, CodeSignatureRequest, CodeSymbolSearchRequest, ExactOccurrenceRecord,
    ExactOccurrenceRequest, GraphCallersRequest, GraphCallersService, GraphImpactRequest,
    GraphImpactResult, GraphImpactRetrievalPort, GraphRetrievalPort, HealthReadRequest,
    LexicalOccurrenceRecord, ModuleApiRequest, OperationalRetrievalPort, PageRequest,
    PhraseSearchRequest, QualifiedNameRequest, ResultProjection, RetrievalOrder,
    RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta, SessionLookupRequest,
    SourceLinesRequest, SourceLinesResult, SourceLinesService, SourceMetadataRecord,
    SourceMetadataRequest, SourceRetrievalPort, SymbolRetrievalPort, SymbolSearchRequest,
    SymbolSearchResult, SymbolSearchService, TemporalRetrievalPort, TestRetrievalPort,
    callable_code_catalog_contribution, callable_code_handler_descriptors, callable_code_operation,
    callable_code_operations, callable_code_request_schema, callable_code_result_schema,
};
