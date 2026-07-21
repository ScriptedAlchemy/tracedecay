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
mod diagnostics;
mod dispatch;
mod endpoint;
mod gateway;
mod overlay;
mod protocol;
mod provider;
mod rpc;
mod session;

pub use capabilities::{
    CapabilityAvailability, CapabilityParseError, CapabilityUnavailable,
    CapabilityUnavailableReason, ClientCapabilities, EffectiveCapabilities, GatewayCapabilities,
    LSP_PROTOCOL_VERSION, PositionEncoding, SemanticCapability, TextDocumentSync,
    UpstreamCapabilities, negotiate_capabilities,
};
pub use diagnostics::{
    DiagnosticMerge, DiagnosticSeverity, DiagnosticSource, DocumentDiagnosticReport,
    GatewayDiagnostic, LspPosition, LspRange, MAX_DIAGNOSTIC_MESSAGE_BYTES,
    MAX_DOCUMENT_DIAGNOSTICS, PositionError, byte_offset_to_utf16_position, merge_diagnostics,
    utf16_position_to_byte_offset,
};
pub use endpoint::{
    AuthorizedLspSession, DaemonLspSessionEndpoint, LSP_SESSION_TTL_MS, LspEndpointError,
    LspSessionAccess, LspSessionAdmissionPort, LspSessionCredential, LspSessionId,
    LspSessionOpenRequest, LspSessionRegistry, MAX_LSP_SESSIONS,
};
pub use gateway::{
    AdmittedRoot, CallHierarchyItem, DaemonLspGateway, DiagnosticTrigger, DocumentSymbol,
    FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse, GatewayDocumentDiagnostics,
    GatewayMethod, GatewayResponse, Hover, IncomingCall, LspLocation, MethodUnavailable,
    MethodUnavailableReason, OutgoingCall, SemanticProviderOutcome, SemanticProviderPort,
    SignatureHelp, TypeHierarchyItem, UnavailableSemanticProvider, WorkspaceSymbol,
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
    AnalyzerSupervisor, AnalyzerTransitionError, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort,
    GenerationDiagnostics, MAX_ANALYZER_RESTARTS, UnavailableDiagnosticSnapshotProvider,
};
pub use session::{
    CancellationOutcome, CompletionDisposition, LifecycleError, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_PENDING_REQUESTS, MAX_PUBLICATION_BYTES, PublicationAdmission,
    PublicationDelivery, PublicationState, RequestAdmission, SessionLifecycle,
};
