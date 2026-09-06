//! Seams between this crate and the subsystems that stay above it.
//!
//! The one-shot crate split moved `agents/` and `automation/` down but left
//! several of their collaborators in the root crate: the `TraceDecay` façade,
//! the daemon session registry, the registered global database, and the hook
//! runtime. Those sit *above* this crate and cannot become a dependency edge,
//! so they are expressed here instead.
//!
//! Three shapes appear below, and the choice between them is not stylistic:
//!
//! - **A required composition handle.** [`hook_runtime::HookRuntimeV1`] is one
//!   value carrying every root-owned capability a hook path needs. The root
//!   installs it whole; a process that never did is a bootstrap failure that
//!   every reader reports as such, rather than nine slots each answering with
//!   a plausible production value.
//! - **Direct reads of a lower owner.** [`mcp_tools`] and [`pricing`] name the
//!   crate that owns the data. Neither was ever a root-only capability once
//!   the split settled, and inverting them cost real safety: an unregistered
//!   tool catalog answered empty, which installers wrote as a permission
//!   allowlist.
//! - **Boundary contracts.** Values that cross a remaining upward boundary are
//!   owned here only when no lower canonical crate owns their identity.
//!
//! Lower-owned value types are imported directly from their canonical crate;
//! this module does not provide compatibility re-export paths for them.

pub mod hook_runtime;
pub mod mcp_tools;
pub mod pricing;
