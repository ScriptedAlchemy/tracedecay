//! Agent host integrations and self-improvement automation for TraceDecay.
//!
//! The root package keeps process-level composition surfaces. This crate owns
//! host behavior, configuration transforms, generated host assets, and
//! automation policy while depending only on lower-layer crates.

#![allow(clippy::collapsible_if)]

pub mod agents;
pub mod analytics;
pub mod automation;
pub mod ports;

// Compatibility shims for modules extracted concurrently into the runtime
// kernel. They retain the historical paths inside the moved source without a
// dependency back to the root package.
pub(crate) use tracedecay_runtime_core::{
    config, db, errors, memory, serde_util, storage, timeutil, worktree,
};
pub(crate) use tracedecay_sessions as sessions;

pub(crate) mod tracedecay {
    pub(crate) use tracedecay_runtime_core::tracedecay::current_timestamp;
}
