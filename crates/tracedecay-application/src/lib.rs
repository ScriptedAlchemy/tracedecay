//! Transport-neutral application contracts and direct use-case services.
//!
//! This crate owns no storage, transport, provider runtime, UI, model runtime,
//! Git mutation, scheduler, or root catalog composition.

#![forbid(unsafe_code)]

pub mod authorization;
pub mod context;
pub mod diagnostics;
pub mod feedback;
pub mod git;
pub mod handlers;
pub mod result;
pub mod retrieval;

mod error;

pub use authorization::{
    AuthorizationAdmission, AuthorizationPhase, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, AuthorizationService, ConcealedResourceCause, NonDisclosureHooks,
    SourceAuthorizationSnapshot,
};
pub use context::{
    CancellationContext, CancellationState, CancellationTokenId, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestAdmission, RequestContext,
    RequestId, ResolvedScope,
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
pub use git::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, git_index_catalog_contribution,
    git_index_effect_class, git_index_handler_descriptors,
};
pub use handlers::{
    ApplicationHandlerDescriptor, ApplicationHandlerDescriptors, ApplicationOperation,
    application_handler_descriptors,
};
pub use result::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationProblemKind, ApplicationResult, AuthorityReceipt, BudgetClass,
    CancellationObservation, CancellationStage, CoverageCompleteness, CoverageDomainState,
    EffectId, EffectReceipt, EffectResult, EffectTermination, EvidenceAuthority, EvidenceCoverage,
    EvidenceDomain, EvidenceIdentity, EvidencePacket, EvidenceScore, EvidenceScoreKind,
    EvidenceScoreValue, FreshnessState, IdempotencyKey, LegalAction, Omission, OmissionReason,
    OpaqueCursor, OperationBudgetUsage, OperationReceipt, OperationTermination, PageState,
    PolicyDecisionRef, PreviewId, PreviewResult, ReconciliationState, ResultContractRef,
    ResumeToken, RetrievalEvidence, RetrieverContribution, RetrieverContributionState,
    RetryDirective, SafeDiagnostic, ScoreId, StreamEvent, StreamEventKind, StreamFrontier,
    StreamGap, StreamTermination, StreamValidationError, TemporalState, validate_stream,
};
pub use retrieval::catalog::{APPLICATION_DEFAULT_PROFILE_ID, application_catalog_contributions};
pub use retrieval::{
    AffectedTestsRequest, AffectedTestsRetrievalPort, AffectedTestsService, AnchorExpandRequest,
    AnchorHydrationPort, GraphCallersRequest, GraphCallersService, GraphImpactRequest,
    GraphImpactResult, GraphImpactRetrievalPort, GraphRetrievalPort, HealthReadRequest,
    OperationalRetrievalPort, PageRequest, ResultProjection, RetrievalOrder, RetrievalPortContext,
    RetrievalPortOutcome, RetrievalRequestMeta, SessionLookupRequest, SourceLinesRequest,
    SourceLinesResult, SourceLinesService, SourceRetrievalPort, SymbolRetrievalPort,
    SymbolSearchRequest, SymbolSearchResult, SymbolSearchService, TemporalRetrievalPort,
    TestRetrievalPort,
};
