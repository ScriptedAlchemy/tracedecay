//! Generation-pinned verified code-graph queries over daemon-resolved
//! projections, plus the code-index-backed source readers
//! (`context::{read_modes, source_read, markdown_sections}`) that hydrate
//! source evidence for those queries.
//!
//! This crate sits below the transport adapters (`tracedecay-mcp`, the root
//! composition crate) and above the projection/store kernels
//! (`tracedecay-code-index`, `tracedecay-graph-db`,
//! `tracedecay-runtime-core`). It owns the [`VerifiedGraphQuery`] authority:
//! admission, source binding, and every analytical read run through the one
//! generation-pinned reader opened by [`open_verified_graph_query`].

use std::path::Path;

use tracedecay_runtime_core::db::Database;

pub mod context;
pub mod health;
mod projection;
pub mod queries;
pub mod scc;
mod source_authority;
mod verified_query;

pub use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
pub use tracedecay_code_index::graph_projection::{
    CodeGraphImpactBatchV1, CodeGraphSemanticEdgeV1, CodeGraphSymbolPageV1,
    CodeGraphSymbolSummaryV1,
};
pub use tracedecay_code_index::lineage::LineageSymbolRecordV1;

pub use projection::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionFuture, CodeGraphReadAdmissionPort,
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadFreshnessV1,
    CodeGraphReadFuture, CodeGraphReadRequest, VerifiedCodeGraphRead,
    application_graph_cancellation, map_code_graph_read_runtime_error, map_projection_error,
    request_graph_cancellation,
};
pub use queries::{
    FileAdjacencyScan, GraphQueryManager, NodeMetrics, VerifiedHealthFileAggregateV1,
};
pub use source_authority::{
    CodeGraphSourceAuthorityPort, CodeGraphSourceBindFuture, CodeGraphSourceBindRequest,
};
pub use verified_query::{
    VerifiedGraphQuery, VerifiedGraphQueryFuture, VerifiedGraphQueryPort,
    VerifiedGraphQueryRequest, open_verified_graph_query,
};

/// Narrow root-owned filesystem authority used by source retrieval.
///
/// Generation-pinned symbol evidence is supplied independently through the
/// code-graph projection port. This boundary intentionally exposes only the
/// source decoder's real filesystem and cache dependencies.
pub trait SourceReadRuntimePort: Send + Sync {
    fn project_root(&self) -> &Path;
    fn db(&self) -> &Database;
    fn is_read_only(&self) -> bool;
    /// Exact registered project identity this runtime may read.
    fn project_id(&self) -> &str;
}

pub type SourceReadRuntime = dyn SourceReadRuntimePort;

/// Installs the registered global/session schema into the kernel's
/// fail-closed port for this crate's test process. The real schema is owned
/// by `tracedecay-global-db`; the port keeps the first registration, so every
/// fixture entry point can call this unconditionally.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_test_schema_installer();
}

#[cfg(test)]
mod verified_query_deadline_tests;
#[cfg(test)]
mod verified_query_source_tests;
#[cfg(test)]
mod verified_query_test_support;
