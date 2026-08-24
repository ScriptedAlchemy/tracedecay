#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::single_match)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::format_push_string)]

//! Agent host integrations and self-improvement automation for `TraceDecay`.
//!
//! The root package keeps process-level composition surfaces. This crate owns
//! host behavior, configuration transforms, generated host assets, and
//! automation policy while depending only on lower-layer crates.

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
