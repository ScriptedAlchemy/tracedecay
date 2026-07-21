//! Daemon-owned LSP 3.17 gateway scaffolding.
//!
//! This module deliberately contains session-facing protocol contracts only.
//! The daemon remains the authority for admitted-root resolution, diagnostics,
//! analyzer supervision, and application operations. The bridge in
//! [`crate::lsp_bridge`] is transport-only and must not duplicate those roles.
//!
//! Wiring this module into `crate::daemon` is intentionally deferred to the
//! daemon-gateway integration slice.

mod capabilities;
mod diagnostics;
mod gateway;
mod session;

pub use capabilities::{
    CapabilityAvailability, CapabilityUnavailable, CapabilityUnavailableReason, ClientCapabilities,
    EffectiveCapabilities, GatewayCapabilities, LSP_PROTOCOL_VERSION, PositionEncoding,
    SemanticCapability, TextDocumentSync, UpstreamCapabilities, negotiate_capabilities,
};
pub use diagnostics::{
    DiagnosticMerge, DiagnosticSeverity, DiagnosticSource, DocumentDiagnosticReport,
    GatewayDiagnostic, LspPosition, LspRange, MAX_DIAGNOSTIC_MESSAGE_BYTES,
    MAX_DOCUMENT_DIAGNOSTICS, PositionError, byte_offset_to_utf16_position, merge_diagnostics,
    utf16_position_to_byte_offset,
};
pub use gateway::{
    AdmittedRoot, CallHierarchyItem, DaemonLspGateway, DiagnosticTrigger, DocumentSymbol,
    FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse, GatewayDocumentDiagnostics,
    GatewayMethod, GatewayResponse, Hover, IncomingCall, LspLocation, MethodUnavailable,
    MethodUnavailableReason, OutgoingCall, SemanticProviderOutcome, SemanticProviderPort,
    SignatureHelp, TypeHierarchyItem, UnavailableSemanticProvider, WorkspaceSymbol,
};
pub use session::{
    CancellationOutcome, CompletionDisposition, LifecycleError, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_PENDING_REQUESTS, PublicationAdmission, PublicationDelivery,
    PublicationState, RequestAdmission, SessionLifecycle,
};
