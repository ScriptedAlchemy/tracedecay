//! Compatibility shim for the session runtime that now lives in
//! `tracedecay-sessions`.
//!
//! The whole former `src/sessions/` tree moved to
//! `crates/tracedecay-sessions/src/runtime/` during the one-shot crate split.
//! Every previously reachable `crate::sessions::…` path is re-exported here so
//! root modules, binaries, and integration tests keep compiling against the old
//! path while the aftermath campaign rewrites call sites.
//!
//! Unresolved root couplings that the move could not carry across the boundary
//! are catalogued in `crates/tracedecay-sessions/SEAMS.md`.

pub use tracedecay_sessions::runtime::*;

#[cfg(test)]
mod claude_observation_benchmark;
