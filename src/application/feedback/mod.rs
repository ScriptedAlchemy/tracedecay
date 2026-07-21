//! Root-owned concrete adapters for the transport-neutral PR11 feedback core.
//!
//! This module composes existing store, query, and observation boundaries. It
//! deliberately does not register a daemon trigger, transport handler, CI or
//! GitHub source, proximity source, or persistence schema.

pub mod dedupe;
pub mod diagnostics;
pub mod observations;
pub mod runtime;
