//! Root shim for user-level `TraceDecay` configuration.
//!
//! The implementation lives in `tracedecay_usecases::user_config` (canonical
//! copy; see SEAMS.md). This module keeps every historical
//! `crate::user_config::…` path resolving from the root crate.

pub use tracedecay_usecases::user_config::*;
