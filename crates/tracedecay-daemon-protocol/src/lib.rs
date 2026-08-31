//! Daemon invocation wire contract, authenticated client, and leaf transport.
//!
//! This crate sits below the composition root. It owns request/response
//! envelopes, binding resolution, cancellation/ack frames, handshake identity,
//! and the socket transport used to carry those envelopes. Bounded frame
//! limits, readers, and oversized-frame errors live in `tracedecay-framing`.
//! It does not open stores, mint authority, or assemble the daemon service.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::if_not_else)]
#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]
#![allow(clippy::large_futures)]
#![allow(unreachable_pub)]
#![allow(clippy::large_enum_variant)]

pub mod client;
pub mod client_identity;
pub mod connection;
pub mod contract;
pub mod handshake;
pub mod lsp_wire;
pub mod output_format;
pub mod surface;
pub mod transport;

pub use client::{
    AdapterInvocation, BindingResolution, BindingResolver, BoundInvocation, CanonicalInvocation,
    CatalogBindingResolver, DaemonInvocationClient, DaemonInvocationError,
    DaemonInvocationExecutor, DaemonInvocationExecutorFuture, DaemonLspSessionClient,
    DispatchError, DispatchInput, DispatchedInvocation, InvocationCancellationPolicy,
    InvocationControls, ResolvedBinding, SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
    SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS, ScopeSelector,
    SemanticEvaluationPublicationResultV1, SemanticEvaluationQualificationResultV1,
    application_delivery_route, application_response, deadline_remaining, handshake_refusal_error,
    invocation_now_micros, map_invocation_error, resolve_dispatch, wait_for_cancellation,
};
pub use client_identity::DaemonClientIdentity;
pub use connection::{
    DAEMON_CONNECT_DOWN, DAEMON_CONNECT_SATURATED, DAEMON_RESPONSE_STALLED,
    DAEMON_TOOL_LIVENESS_POLL_INTERVAL, DAEMON_TOOL_RESPONSE_GRACE, DEFAULT_TOOL_REQUEST_DEADLINE,
    DaemonConnection, DaemonLivenessProbe, MAX_TOOL_REQUEST_DEADLINE, TOOL_REQUEST_DEADLINE_ENV,
    connect_to_daemon_connection, daemon_connect_failure, daemon_response_stalled,
    daemon_response_stalled_during, daemon_tool_response_bound, next_daemon_response_line,
    tool_request_deadline, write_daemon_preamble,
};
pub use contract::{
    CanonicalQualificationBlob, CanonicalQualificationBlobError, DAEMON_INVOCATION_PROTOCOL,
    DAEMON_INVOCATION_REVISION, DAEMON_SHUTDOWN_METHOD, DaemonFeedbackResult,
    DaemonGitEffectResult, DaemonGitPreviewResult, DaemonInvocationCancellationRequest,
    DaemonInvocationDeliveryAckRejectReason, DaemonInvocationDeliveryAckRequest,
    DaemonInvocationDeliveryAckResponse, DaemonInvocationDeliveryAckResponseOutcome,
    DaemonInvocationOperation, DaemonInvocationOutcome, DaemonInvocationPayload,
    DaemonInvocationProblem, DaemonInvocationRequest, DaemonInvocationResponse,
    DaemonLspSessionAccess, HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1,
    WorkApplicationInvocationV1, WorkApplicationOutcomeV1, WorkflowApplicationInvocation,
    WorkflowApplicationOutcome, parse_daemon_invocation_cancellation_request,
    parse_daemon_invocation_delivery_ack_request, parse_daemon_invocation_request,
};
pub use handshake::{
    DAEMON_HANDSHAKE_REFUSAL_PROTOCOL, DaemonHandshake, DaemonHandshakeRefusal,
    DaemonHandshakeRefusalReason, MovedStoreAdoption, client_version_skew, version_skew_action,
};
pub use lsp_wire::{
    ConnectionLocalRequestSequence, FramePoll, FrameSend, LspFrame, LspSessionAccess,
    LspSessionCredential, LspSessionId, LspSessionIdentityError, MAX_LSP_FRAME_BYTES,
    MAX_LSP_WORKSPACE_ROOTS, ProcessLocalRequestSequence, SequenceExhausted,
};
pub use output_format::{RequestedOutputFormat, requested_output_format};
pub use surface::{
    ContextScoutCancelSurfaceRequest, ContextScoutClaimSurfaceRequest,
    ContextScoutClaimWindowSurfaceV1, ContextScoutControlSurfaceRequest,
    ContextScoutDeliverySurfaceRequest, ContextScoutExactAddressSurfaceRequest,
    ContextScoutFeedbackSurfaceRequest, ContextScoutRecentSurfaceRequest,
    ContextScoutSurfaceRequest, GitReadSurfaceRequest,
};
pub use transport::{
    AUTH_PREFACE_PROTOCOL, BrokerListener, BrokerReadHalf, BrokerStream, BrokerWriteHalf,
    DaemonAuthPreface, DaemonEndpoint, SOCKET_ENV, default_loopback_endpoint,
};
#[cfg(unix)]
pub use transport::{
    MAX_UNIX_SOCKET_PATH_BYTES, ensure_private_socket_parent, unix_socket_path_within_limit,
};
