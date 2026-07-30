//! Native Rust facade over TraceDecay's canonical public contracts.
//!
//! This crate intentionally owns no transport, parsing, authentication,
//! cursor, retry, streaming, or daemon behavior. Native consumers use the
//! canonical API, application, domain, and operation metadata authorities
//! directly through these stable namespaces.

#![forbid(unsafe_code)]

/// Canonical HTTP/SSE presentation contracts.
pub use tracedecay_api as api;
/// Canonical transport-neutral use-case contracts, ports, and results.
pub use tracedecay_application as application;
/// Canonical transport-neutral remote authority, protocol, and outcome contracts.
pub use tracedecay_application::remote;
/// Canonical cancellation observations, identity, and process-local signal.
pub use tracedecay_application::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId,
};
/// Canonical pure domain values and validation contracts.
pub use tracedecay_domain as domain;
/// Canonical capability, binding, schema, and operation metadata.
pub use tracedecay_tool_catalog as operation;
