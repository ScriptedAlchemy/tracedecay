//! End-to-end wire allocation bounds for host stdin versus MCP/daemon
//! JSON-RPC frames.
//!
//! The canonical definitions live in [`tracedecay_sessions::admission::wire`];
//! this module re-exports them so the use-case layer shares one implementation
//! of the host-event and MCP/daemon frame caps, the bounded line/record
//! readers, and the oversized-outcome helpers.

pub use tracedecay_sessions::admission::wire::*;
