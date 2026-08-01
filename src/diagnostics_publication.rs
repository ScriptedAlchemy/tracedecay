//! Production write side for generation-bound diagnostics (Plan 35,
//! "Universal managed diagnostics").
//!
//! The canonical implementation now lives in
//! [`tracedecay_usecases::diagnostics_publication`]; this module is a thin
//! re-export shim so existing `crate::diagnostics_publication::…` paths keep
//! resolving during the crate split. See the canonical module for the full
//! contract documentation.

pub use tracedecay_usecases::diagnostics_publication::*;
