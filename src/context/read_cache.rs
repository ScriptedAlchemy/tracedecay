//! Root shim for the cross-session read cache.
//!
//! The implementation lives in `tracedecay_usecases::context::read_cache`
//! (canonical copy; see SEAMS.md). This module keeps every historical
//! `crate::context::read_cache::…` path resolving from the root crate.

pub use tracedecay_usecases::context::read_cache::*;
