//! Root shim for the native Git intelligence adapter.
//!
//! The implementation lives in `tracedecay_usecases::git_intelligence`
//! (canonical copy; see SEAMS.md). This module keeps every historical
//! `crate::git_intelligence::…` path resolving from the root crate.

pub use tracedecay_usecases::git_intelligence::*;
