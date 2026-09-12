//! Compatibility import path for crates this lane cannot edit.
//!
//! Authority is `tracedecay-configuration`. `tracedecay-code-index-runtime`
//! and `tracedecay-agent-hosts` still import these two types through usecases.

pub use tracedecay_global_db::configuration::contracts::ports::{
    ConfigurationControlStore, ConfigurationCurrentStateV1,
};
