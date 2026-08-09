//! Code-graph use cases over daemon-resolved verified projections.

pub mod health;
pub mod health_delta;
mod projection;
pub mod queries;
pub mod scc;

pub use projection::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionFuture, CodeGraphReadAdmissionPort,
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead, application_graph_cancellation, map_code_graph_read_runtime_error,
    map_projection_error, request_graph_cancellation,
};
