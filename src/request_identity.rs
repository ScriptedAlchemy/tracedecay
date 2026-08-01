//! Root shim for request-identity primitives.
//!
//! The implementation lives in `tracedecay_usecases::request_identity`
//! (canonical copy; see SEAMS.md). This module keeps every historical
//! `crate::request_identity::…` path resolving from the root crate.

pub use tracedecay_usecases::request_identity::*;
