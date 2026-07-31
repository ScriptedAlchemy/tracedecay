pub mod code_search;
pub mod retrieval;

/// Temporal session-query contracts, re-exported so historical
/// `crate::query::temporal::*` paths keep resolving through this kernel.
pub use tracedecay_temporal_query as temporal;
