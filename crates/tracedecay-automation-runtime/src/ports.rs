//! Seams between this crate and collaborators that sit beside or above it.
//!
//! Two shapes:
//!
//! - **Registered ports.** Behaviour backed by a sibling or root-owned runtime
//!   is a function pointer the composition root registers at startup. Every
//!   port degrades to a documented inert answer when unregistered.
//! - **Boundary contracts.** Values that cross an upward boundary are owned
//!   here only when no lower canonical crate owns their identity.

pub mod codex_app_server;
pub mod project_runtime;
pub mod session_evidence;
pub mod session_store;
