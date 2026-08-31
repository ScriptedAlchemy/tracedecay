//! Retrieval-profile activation authority.
//!
//! Control-plane pin surfaces and helpers live in `tracedecay-configuration`.
//! This module keeps the production-load-bearing retrieval evaluation surface
//! that depends on search-eval. `PinnedRuntimeConfiguration` and
//! `RuntimeConfigurationTarget` are re-exported so parallel lanes that still
//! import through this path (`tracedecay-code-index-runtime`) compile.

pub mod retrieval;

pub use retrieval::*;
pub use tracedecay_configuration::{PinnedRuntimeConfiguration, RuntimeConfigurationTarget};

#[cfg(test)]
pub use tracedecay_runtime_core::config::PinnedUserDataDir;
