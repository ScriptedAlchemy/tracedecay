//! Seams between this crate and the subsystems that stay above it.
//!
//! The one-shot crate split moved `agents/` and `automation/` down but left
//! several of their collaborators in the root crate: the `TraceDecay` façade,
//! the daemon session registry, the registered global database, the MCP tool
//! catalog, the hook runtime, and the memory application. None of them can
//! become a dependency edge — they sit *above* this crate — so each is
//! expressed here instead.
//!
//! Three shapes appear below, and the choice between them is not stylistic:
//!
//! - **Canonical re-exports.** Value types already owned by a lower canonical
//!   crate remain available through this crate's historical port path.
//! - **Downward moves.** Pure value types and pure functions still owned here
//!   are re-exported by the root crate. This is the same shape as `agents` and
//!   `automation` themselves, which the root re-exports from this crate.
//! - **Registered ports.** Behaviour backed by root-owned runtime is a
//!   function pointer or trait object the root registers at startup, following
//!   `tracedecay_runtime_core::ports`. Every port degrades to a documented
//!   inert answer when the root never registers, so this crate's own unit
//!   tests stay runnable standalone.
//!
//! `SEAMS.md` next to this crate's manifest tracks which registration sites
//! the landing still owes.

pub mod codex_app_server;
pub mod configuration;
pub mod context;
pub mod hook_runtime;
pub mod mcp_tools;
pub mod pricing;
pub mod project_runtime;
pub mod session_evidence;
pub mod session_store;
