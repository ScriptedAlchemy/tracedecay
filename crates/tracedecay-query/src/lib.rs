pub mod code_search;
mod hotpath_metrics;
pub mod retrieval;
pub mod search_quality;

/// Temporal session-query contracts, re-exported so historical
/// `crate::query::temporal::*` paths keep resolving through this kernel.
pub use tracedecay_temporal_query as temporal;
