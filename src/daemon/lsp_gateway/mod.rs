//! Daemon-owned LSP 3.17 gateway.
//!
//! The store-free session-facing contracts — protocol actor, session
//! lifecycle, capability negotiation, overlays, diagnostic merge, context
//! projection, and the analyzer broker ports — live in [`tracedecay_lsp`] and
//! are re-exported here so daemon callers keep one gateway façade.
//!
//! What stays daemon-owned is exactly what needs daemon authority: the
//! authenticated session registry ([`endpoint`]), analyzer/store composition
//! ([`factory`]), and the adapters that bind application semantic, context,
//! diagnostic, and feedback authorities to the crate's ports
//! ([`runtime_adapters`]). Those reach a session only as injected ports.

mod endpoint;
mod factory;
mod runtime_adapters;

pub use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationPort, AnalyzerEvent, AnalyzerSemanticAdapter, AnalyzerState,
    AnalyzerSupervisor, AnalyzerTransitionError, CallHierarchyItem, CancellationOutcome,
    CapabilityAvailability, CapabilityParseError, CapabilityUnavailable,
    CapabilityUnavailableReason, ClientCapabilities, CompletionDisposition, ContextCoverage,
    ContextExpansionEnvelope, ContextExpansionOutcome, ContextExpansionRequest,
    ContextExpansionScope, ContextFreshness, ContextProducerState, ContextProjectionChange,
    ContextProjectionEnvelope, ContextProjectionIdentity, ContextProjectionItem,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, ContextSubscribeRequest,
    DEFAULT_LSP_REQUEST_DEADLINE_MS, DaemonLspGateway, DaemonLspProtocolSession,
    DaemonLspProtocolTransport, DebouncedDiagnostic, DebouncedDiagnosticKind, DiagnosticMerge,
    DiagnosticRefreshAdmission, DiagnosticRefreshIdentity, DiagnosticSeverity,
    DiagnosticSnapshotOutcome, DiagnosticSnapshotPort, DiagnosticSource, DiagnosticTrigger,
    DocumentDiagnosticReport, DocumentSymbol, EffectiveCapabilities, FeedbackCyclePort,
    FeedbackCycleRequest, FeedbackCycleResponse, GatewayCapabilities, GatewayDiagnostic,
    GatewayDiagnosticCoverage, GatewayDiagnosticData, GatewayDiagnosticIdentity,
    GatewayDiagnosticLifecycle, GatewayDiagnosticProviderState,
    GatewayDiagnosticRelatedInformation, GatewayDocumentDiagnostics, GatewayMethod,
    GatewayResponse, GenerationDiagnostics, Hover, IncomingCall, LSP_PROTOCOL_VERSION,
    LifecycleError, LspLocation, LspPosition, LspRange, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_ANALYZER_RESTARTS, MAX_CONTEXT_CHANGES_PER_POLL,
    MAX_CONTEXT_PROJECTION_BYTES, MAX_CONTEXT_PROJECTION_ITEMS, MAX_CONTEXT_PROJECTION_KINDS,
    MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES, MAX_CONTEXT_SUMMARY_BYTES, MAX_DIAGNOSTIC_MESSAGE_BYTES,
    MAX_DIAGNOSTIC_OPERATION_ID_BYTES, MAX_DIAGNOSTIC_RELATED_INFORMATION,
    MAX_DIAGNOSTIC_RELATED_MESSAGE_BYTES, MAX_DIAGNOSTIC_URI_BYTES, MAX_DOCUMENT_DIAGNOSTICS,
    MAX_OPEN_DOCUMENTS, MAX_OVERLAY_BYTES, MAX_PENDING_OVERLAY_DIAGNOSTICS, MAX_PENDING_REQUESTS,
    MAX_PUBLICATION_BYTES, MAX_QUEUED_OUTBOUND_BYTES, MAX_QUEUED_OUTBOUND_MESSAGES,
    MethodUnavailable, MethodUnavailableReason, OVERLAY_DIAGNOSTIC_DEBOUNCE_MS,
    OVERLAY_DIAGNOSTIC_MAX_WAIT_MS, OutgoingCall, OverlayChange, OverlayDiagnosticDebouncer,
    OverlayError, OverlaySnapshot, OverlayStore, PositionEncoding, PositionError, ProtocolDispatch,
    PublicationAdmission, PublicationDelivery, PublicationState, RenameCandidate,
    RenameCandidateResult, RenameCandidateUnavailableReason, RequestAdmission, SemanticCapability,
    SemanticProviderOutcome, SemanticProviderPort, SemanticRequest, SemanticResponse,
    SessionLifecycle, SignatureHelp, TRACEDECAY_CONTEXT_CHANGED_METHOD,
    TRACEDECAY_CONTEXT_EXPAND_METHOD, TRACEDECAY_CONTEXT_METHOD, TRACEDECAY_CONTEXT_REVISION,
    TRACEDECAY_DIAGNOSTIC_DATA_REVISION, TRACEDECAY_SUBSCRIBE_METHOD, TextDocumentSync,
    TypeHierarchyItem, UnavailableDiagnosticSnapshotProvider, UnavailableSemanticProvider,
    UpstreamCapabilities, WorkspaceSymbol, byte_offset_to_utf16_position, merge_diagnostics,
    negotiate_capabilities, utf16_position_to_byte_offset,
};

pub use endpoint::{
    AuthorizedLspSession, DaemonLspSessionEndpoint, LSP_SESSION_TTL_MS, LspEndpointError,
    LspSessionAccess, LspSessionAdmissionPort, LspSessionCredential, LspSessionId,
    LspSessionOpenRequest, LspSessionRegistry, MAX_LSP_SESSIONS,
};
pub use factory::{
    DaemonLspProviderBundle, DaemonLspProviderFactory, DaemonLspRuntimeSession,
    Pr12LspSessionFactory,
};
pub use runtime_adapters::{
    BrokerDiagnosticSnapshotAuthority, CanonicalContextProjectionAuthority,
    CanonicalDiagnosticRefreshRequest, CanonicalDiagnosticSnapshotAuthority,
    FeedbackCycleRuntimePort, LspAnalyzerCancellationAuthority, LspDiagnosticDocumentPort,
    LspRuntimeFailure, LspRuntimeFuture, LspSemanticOperationOutcome, LspSemanticRequestAuthority,
    MAX_PR12_FEEDBACK_CYCLES, ManagedDiagnosticSnapshot, ManagedDiagnosticSnapshotPort,
    Pr12AnalyzerCancellationAdapter, Pr12ContextProjectionAdapter, Pr12DiagnosticSnapshotAdapter,
    Pr12FeedbackCycleAdapter, Pr12SemanticProviderAdapter,
};
