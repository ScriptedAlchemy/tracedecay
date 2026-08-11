//! Dashboard memory-service payloads: facts, graph, projection, similarity, curation, oplog.

mod curation;
mod facts;
mod graph;
mod oplog;
mod projection;
mod similarity;

pub use curation::build_delete_plan;
pub use facts::{
    fact_detail_payload, fetch_entities, fetch_facts, overview_payload, providers_payload,
};
pub use graph::graph_payload;
pub use oplog::oplog_payload;
pub use projection::{projection_payload, projection_point_cap};
pub use similarity::{coerce_similarity_score, similarity_computation, similarity_payload};
