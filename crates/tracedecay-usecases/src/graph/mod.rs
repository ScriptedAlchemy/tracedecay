//! Code-graph use cases over daemon-resolved verified projections.

pub mod health;
mod projection;
pub mod queries;
pub mod scc;

pub use projection::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead, map_code_graph_read_runtime_error, map_projection_error,
    request_graph_cancellation,
};
