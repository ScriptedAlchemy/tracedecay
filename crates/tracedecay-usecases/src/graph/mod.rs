//! Code-graph use cases over daemon-resolved verified projections.

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
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead, application_graph_cancellation, map_code_graph_read_runtime_error,
    map_projection_error, request_graph_cancellation,
};
pub use queries::{FileAdjacencyScan, GraphQueryManager, NodeMetrics, VerifiedHealthFileAggregateV1};
pub use source_authority::{
    CodeGraphSourceAuthorityPort, CodeGraphSourceBindFuture, CodeGraphSourceBindRequest,
};
pub use verified_query::{
    VerifiedGraphQuery, VerifiedGraphQueryFuture, VerifiedGraphQueryPort, VerifiedGraphQueryRequest,
    open_verified_graph_query,
};

#[cfg(test)]
mod verified_query_deadline_tests;
#[cfg(test)]
mod verified_query_source_tests;
#[cfg(test)]
mod verified_query_test_support;
