//! Daemon invocation wire contract, authenticated client, and leaf transport.
//!
//! This crate sits below the composition root. It owns request/response
//! envelopes, binding resolution, cancellation/ack frames, handshake identity,
//! and the socket framing used to carry those envelopes. It does not open
//! stores, mint authority, or assemble the daemon service.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::large_enum_variant)]

pub mod client;
pub mod client_identity;
pub mod connection;
pub mod contract;
pub mod handshake;
pub mod output_format;
pub mod surface;
pub mod transport;

pub use client::{
    AdapterInvocation, BindingResolution, BindingResolver, BoundInvocation, CanonicalInvocation,
    CatalogBindingResolver, DaemonInvocationClient, DaemonInvocationError,
    DaemonInvocationExecutor, DaemonInvocationExecutorFuture, DaemonLspSessionClient,
    DispatchError, DispatchInput, DispatchedInvocation, InvocationCancellationPolicy,
    InvocationControls, ResolvedBinding,
    SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
    SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS, ScopeSelector,
    SemanticEvaluationPublicationResultV1, SemanticEvaluationQualificationResultV1,
    application_delivery_route, application_response, deadline_remaining, invocation_now_micros,
    map_invocation_error, resolve_dispatch, wait_for_cancellation,
};
pub use client_identity::DaemonClientIdentity;
pub use output_format::{RequestedOutputFormat, requested_output_format};
pub use connection::{
    DAEMON_CONNECT_DOWN, DAEMON_CONNECT_SATURATED, DAEMON_RESPONSE_STALLED,
    DAEMON_TOOL_LIVENESS_POLL_INTERVAL, DAEMON_TOOL_RESPONSE_GRACE, DEFAULT_TOOL_REQUEST_DEADLINE,
    DaemonConnection, DaemonLivenessProbe, MAX_TOOL_REQUEST_DEADLINE, TOOL_REQUEST_DEADLINE_ENV,
    connect_to_daemon_connection, daemon_connect_failure, daemon_response_stalled,
    daemon_tool_response_bound, next_daemon_response_line, tool_request_deadline,
    write_daemon_preamble,
};
pub use contract::{
    CanonicalQualificationBlob, CanonicalQualificationBlobError, DAEMON_INVOCATION_PROTOCOL,
    DAEMON_INVOCATION_REVISION, DaemonFeedbackResult, DaemonGitEffectResult,
    DaemonGitPreviewResult, DaemonInvocationCancellationRequest,
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
pub use surface::{
    ApplicationSurfaceOperation, ContextScoutCancelSurfaceRequest, ContextScoutClaimSurfaceRequest,
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
