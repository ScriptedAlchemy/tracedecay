#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]
// Retained from the root crate's posture so extraction does not churn protocol
// signatures and control flow: these are stylistic findings whose "fixes"
// ripple across daemon call sites.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::similar_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::too_many_lines)]

//! Store-free LSP transport and session-protocol contracts.
//!
//! This crate owns the stdio framing bridge plus the LSP 3.17 protocol,
//! session lifecycle, capability negotiation, overlay, diagnostic-merge, and
//! analyzer-broker contracts that need no database, socket, analyzer process,
//! or filesystem authority.
//!
//! The daemon retains everything this crate deliberately excludes: the
//! authenticated session registry, admitted-root resolution, analyzer process
//! and store composition, and the application feedback/semantic/context
//! authorities. Those reach the session actor only as injected port
//! implementations.

pub mod analyzer;

mod bridge;
mod capabilities;
mod catalog;
mod context;
mod diagnostics;
mod dispatch;
mod gateway;
mod native_integration;
mod overlay;
mod protocol;
mod provider;
mod request_sequence;
mod rpc;
mod session;
mod workspace;
mod workspace_diagnostics;

pub use bridge::{
    AsyncContentLengthError, BridgeDirection, BridgePumpOutcome, ContentLengthCodec,
    ContentLengthCodecError, ContentLengthStdioError, ContentLengthStdioTransport,
    DaemonLspSessionTransport, FramePoll, FrameSend, LspFrame, MAX_LSP_FRAME_BYTES,
    MAX_LSP_HEADER_BYTES, StdioFrameTransport, StdioLspBridge, StdioLspBridgeError,
    read_content_length_frame_until,
};
pub use capabilities::{
    CapabilityAvailability, CapabilityParseError, CapabilityUnavailable,
    CapabilityUnavailableReason, ClientCapabilities, EffectiveCapabilities, GatewayCapabilities,
    LSP_PROTOCOL_VERSION, PositionEncoding, SemanticCapability, TextDocumentSync,
    UpstreamCapabilities, negotiate_capabilities,
};
pub use context::{
    CanonicalContextProjectionAuthority, ContextCoverage, ContextExpansionEnvelope,
    ContextExpansionOutcome, ContextExpansionRequest, ContextExpansionScope, ContextFreshness,
    ContextProducerState, ContextProjectionAdapter, ContextProjectionChange,
    ContextProjectionEnvelope, ContextProjectionIdentity, ContextProjectionItem,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, ContextSubscribeRequest,
    MAX_CONTEXT_CHANGES_PER_POLL, MAX_CONTEXT_OPERATIONS, MAX_CONTEXT_PROJECTION_BYTES,
    MAX_CONTEXT_PROJECTION_ITEMS, MAX_CONTEXT_PROJECTION_KINDS, MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES,
    MAX_CONTEXT_SUMMARY_BYTES, TRACEDECAY_CONTEXT_CHANGED_METHOD, TRACEDECAY_CONTEXT_EXPAND_METHOD,
    TRACEDECAY_CONTEXT_METHOD, TRACEDECAY_CONTEXT_REVISION, TRACEDECAY_SUBSCRIBE_METHOD,
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
pub use gateway::{
    AdmittedRoot, AnalyzerCancellationAdapter, CallHierarchyItem, DaemonLspGateway,
    DaemonLspProviderBundle, DaemonLspProviderFactory, DaemonLspRuntimeSession, DiagnosticTrigger,
    DocumentSymbol, FeedbackCycleAdapter, FeedbackCyclePort, FeedbackCycleRequest,
    FeedbackCycleResponse, FeedbackCycleRuntimePort, GatewayMethod, GatewayResponse, Hover,
    IncomingCall, LspAnalyzerCancellationAuthority, LspLocation, LspRuntimeFailure,
    LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask, LspSemanticOperationOutcome,
    LspSemanticRequest, LspSemanticRequestAuthority, MAX_FEEDBACK_CYCLES, MAX_SEMANTIC_OPERATIONS,
    MethodUnavailable, MethodUnavailableReason, OutgoingCall, RenameCandidate,
    RenameCandidateResult, RenameCandidateUnavailableReason, SemanticProviderAdapter,
    SemanticProviderOutcome, SemanticProviderPort, SemanticRequest, SemanticResponse,
    SignatureHelp, TypeHierarchyItem, UnavailableSemanticProvider, WorkspaceSymbol,
    decode_uri_segment, lsp_semantic_request, percent_hex_nibble, project_semantic_outcome,
    strict_file_uri_path, strict_file_url, valid_raw_uri_path,
};
pub use native_integration::{
    NativeIntegrationStatusPort, TRACEDECAY_NATIVE_INTEGRATION_STATUS_METHOD,
};
pub use overlay::{
    CanonicalDiagnosticRefreshRequest, CanonicalDiagnosticSnapshotAuthority, DebouncedDiagnostic,
    DebouncedDiagnosticKind, DiagnosticSnapshotAdapter, MAX_DIAGNOSTIC_OPERATIONS,
    MAX_OPEN_DOCUMENTS, MAX_OVERLAY_BYTES, MAX_PENDING_OVERLAY_DIAGNOSTICS,
    MAX_TOTAL_OVERLAY_BYTES, ManagedDiagnosticSnapshot, ManagedDiagnosticSnapshotPort,
    OVERLAY_DIAGNOSTIC_DEBOUNCE_MS, OVERLAY_DIAGNOSTIC_MAX_WAIT_MS, OverlayChange,
    OverlayDiagnosticDebouncer, OverlayError, OverlayExtractionState, OverlayLimits,
    OverlayParseState, OverlayParseUnavailable, OverlaySnapshot, OverlayStore,
};
pub use protocol::{
    ClientFrameAdmission, DEFAULT_LSP_REQUEST_DEADLINE_MS, DaemonLspProtocolSession,
    DaemonLspProtocolTransport, MAX_QUEUED_OUTBOUND_BYTES, MAX_QUEUED_OUTBOUND_MESSAGES,
    ProtocolDispatch,
};
pub use provider::{
    AnalyzerCancellationPort, AnalyzerEvent, AnalyzerSemanticAdapter, AnalyzerState,
    AnalyzerSupervisor, AnalyzerTransitionError, DiagnosticRefreshAdmission,
    DiagnosticRefreshIdentity, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort,
    GenerationDiagnostics, MAX_ANALYZER_RESTARTS, MAX_DIAGNOSTIC_OPERATION_ID_BYTES,
    UnavailableDiagnosticSnapshotProvider,
};
pub use request_sequence::{
    ConnectionLocalRequestSequence, ProcessLocalRequestSequence, SequenceExhausted,
};
pub use session::{
    AuthorizedLspSession, AuthorizedLspWorkspace, CancellationOutcome, CompletionDisposition,
    DaemonLspSessionEndpoint, LSP_SESSION_TTL_MS, LifecycleError, LspEndpointError,
    LspRequestFailure, LspRequestId, LspSessionAccess, LspSessionAdmissionPort, LspSessionControl,
    LspSessionCredential, LspSessionId, LspSessionOpenRequest, LspSessionRegistry,
    LspWorkspaceRouteError, MAX_LSP_SESSIONS, MAX_LSP_WORKSPACE_ROOTS, MAX_PENDING_REQUESTS,
    MAX_PUBLICATION_BYTES, PublicationAdmission, PublicationDelivery, PublicationState,
    RequestAdmission, SessionLifecycle,
};
pub use workspace::{WorkspaceFolderMutation, WorkspaceFolderMutationApplyError};
pub use workspace_diagnostics::{
    CanonicalWorkspaceDiagnosticRefreshRequest, IndexedWorkspaceDocument,
    IndexedWorkspaceDocuments, MAX_WORKSPACE_DIAGNOSTIC_BYTES, MAX_WORKSPACE_DIAGNOSTIC_FANOUT,
    MAX_WORKSPACE_DIAGNOSTIC_RESULT_ID_BYTES, MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
    WorkspaceDiagnosticRootFailure, WorkspaceDiagnosticSnapshotOutcome,
    WorkspaceDocumentDiagnostics, WorkspaceGenerationDiagnostics,
};
