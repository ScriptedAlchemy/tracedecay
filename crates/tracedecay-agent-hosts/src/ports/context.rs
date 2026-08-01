//! Cooperative cancellation and deadline primitives.
//!
//! Keep the agent-hosts port path as a compatibility shim while sharing the
//! runtime kernel's cancellation authority and monotonic deadline types.

pub use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
