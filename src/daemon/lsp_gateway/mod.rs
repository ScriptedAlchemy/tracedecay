//! Daemon-owned LSP 3.17 gateway.
//!
//! This module deliberately contains session-facing protocol contracts only.
//! The daemon remains the authority for admitted-root resolution, diagnostics,
//! analyzer supervision, and application operations. The bridge in
//! [`crate::lsp_bridge`] is transport-only and must not duplicate those roles.
//!
//! The retained daemon owns the authenticated session registry; the bridge
//! only frames and forwards one typed protocol actor per admitted session.

mod capabilities;
mod context;
mod diagnostics;
mod dispatch;
mod endpoint;
mod factory;
mod gateway;
mod overlay;
mod protocol;
mod provider;
mod rpc;
mod runtime_adapters;
mod session;

pub use capabilities::{
    CapabilityAvailability, CapabilityParseError, CapabilityUnavailable,
    CapabilityUnavailableReason, ClientCapabilities, EffectiveCapabilities, GatewayCapabilities,
    LSP_PROTOCOL_VERSION, PositionEncoding, SemanticCapability, TextDocumentSync,
    UpstreamCapabilities, negotiate_capabilities,
};
pub use context::{
    ContextCoverage, ContextExpansionEnvelope, ContextExpansionOutcome, ContextExpansionRequest,
    ContextExpansionScope, ContextFreshness, ContextProducerState, ContextProjectionChange,
    ContextProjectionEnvelope, ContextProjectionIdentity, ContextProjectionItem,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, ContextSubscribeRequest,
    MAX_CONTEXT_CHANGES_PER_POLL, MAX_CONTEXT_PROJECTION_BYTES, MAX_CONTEXT_PROJECTION_ITEMS,
    MAX_CONTEXT_PROJECTION_KINDS, MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES, MAX_CONTEXT_SUMMARY_BYTES,
    TRACEDECAY_CONTEXT_CHANGED_METHOD, TRACEDECAY_CONTEXT_EXPAND_METHOD, TRACEDECAY_CONTEXT_METHOD,
    TRACEDECAY_CONTEXT_REVISION, TRACEDECAY_SUBSCRIBE_METHOD,
};
pub use diagnostics::{
    DiagnosticMerge, DiagnosticSeverity, DiagnosticSource, DocumentDiagnosticReport,
    GatewayDiagnostic, GatewayDiagnosticCoverage, GatewayDiagnosticData, GatewayDiagnosticIdentity,
    GatewayDiagnosticLifecycle, GatewayDiagnosticProviderState,
    GatewayDiagnosticRelatedInformation, LspPosition, LspRange, MAX_DIAGNOSTIC_MESSAGE_BYTES,
    MAX_DIAGNOSTIC_RELATED_INFORMATION, MAX_DIAGNOSTIC_RELATED_MESSAGE_BYTES,
    MAX_DIAGNOSTIC_URI_BYTES, MAX_DOCUMENT_DIAGNOSTICS, PositionError,
    TRACEDECAY_DIAGNOSTIC_DATA_REVISION, byte_offset_to_utf16_position, merge_diagnostics,
    utf16_position_to_byte_offset,
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
pub use gateway::{
    AdmittedRoot, CallHierarchyItem, DaemonLspGateway, DiagnosticTrigger, DocumentSymbol,
    FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse, GatewayDocumentDiagnostics,
    GatewayMethod, GatewayResponse, Hover, IncomingCall, LspLocation, MethodUnavailable,
    MethodUnavailableReason, OutgoingCall, RenameCandidate, RenameCandidateResult,
    RenameCandidateUnavailableReason, SemanticProviderOutcome, SemanticProviderPort,
    SemanticRequest, SemanticResponse, SignatureHelp, TypeHierarchyItem,
    UnavailableSemanticProvider, WorkspaceSymbol,
};
pub use overlay::{
    DebouncedDiagnostic, DebouncedDiagnosticKind, MAX_OPEN_DOCUMENTS, MAX_OVERLAY_BYTES,
    MAX_PENDING_OVERLAY_DIAGNOSTICS, OVERLAY_DIAGNOSTIC_DEBOUNCE_MS,
    OVERLAY_DIAGNOSTIC_MAX_WAIT_MS, OverlayChange, OverlayDiagnosticDebouncer, OverlayError,
    OverlaySnapshot, OverlayStore,
};
pub use protocol::{
    DEFAULT_LSP_REQUEST_DEADLINE_MS, DaemonLspProtocolSession, DaemonLspProtocolTransport,
    MAX_QUEUED_OUTBOUND_BYTES, MAX_QUEUED_OUTBOUND_MESSAGES, ProtocolDispatch,
};
pub use provider::{
    AnalyzerCancellationPort, AnalyzerEvent, AnalyzerSemanticAdapter, AnalyzerState,
    AnalyzerSupervisor, AnalyzerTransitionError, DiagnosticRefreshAdmission,
    DiagnosticRefreshIdentity, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort,
    GenerationDiagnostics, MAX_ANALYZER_RESTARTS, MAX_DIAGNOSTIC_OPERATION_ID_BYTES,
    UnavailableDiagnosticSnapshotProvider,
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
pub use session::{
    CancellationOutcome, CompletionDisposition, LifecycleError, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_PENDING_REQUESTS, MAX_PUBLICATION_BYTES, PublicationAdmission,
    PublicationDelivery, PublicationState, RequestAdmission, SessionLifecycle,
};
