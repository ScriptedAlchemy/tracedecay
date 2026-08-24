//! Seams between this crate and the subsystems that stay above it.
//!
//! The one-shot crate split moved `agents/` and `automation/` down but left
//! several of their collaborators in the root crate: the `TraceDecay` façade,
//! the daemon session registry, the registered global database, the MCP tool
//! catalog, the hook runtime, and the memory application. None of them can
//! become a dependency edge — they sit *above* this crate — so each is
//! expressed here instead.
//!
//! Two shapes appear below, and the choice between them is not stylistic:
//!
//! - **Registered ports.** Behaviour backed by root-owned runtime is a
//!   function pointer or trait object the root registers at startup, following
//!   `tracedecay_runtime_core::ports`. Every port degrades to a documented
//!   inert answer when the root never registers, so this crate's own unit
//!   tests stay runnable standalone.
//! - **Boundary contracts.** Values that cross a remaining upward boundary are
//!   owned here only when no lower canonical crate owns their identity.
//!
//! Lower-owned value types are imported directly from their canonical crate;
//! this module does not provide compatibility re-export paths for them.

pub mod codex_app_server;
pub mod hook_runtime;
pub mod mcp_tools;
pub mod pricing;
pub mod project_runtime;
pub mod session_evidence;
pub mod session_store;
