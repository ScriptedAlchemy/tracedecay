//! PR9 retrieval query port contracts (Plan 05 query crate, Plan 15
//! federated retrieval, Plan 25 code-intelligence lanes).
//!
//! This module tree composes the generic retrieval kernel owned by
//! `tracedecay_domain::retrieval`. It contains typed port traits and
//! lane-local request/evidence contracts only: no storage, no transport, no
//! policy, no ranking implementation. Root store/projector adapters implement
//! the read ports; lane adapters implement the lane retrievers; the
//! composition stages implement fusion, dedupe, diversity, and late
//! hydration.
//!
//! PR9 is explicitly single-root. The exact lane is independent of the
//! fielded lexical/BM25 lane. Semantic is an optional independently admitted
//! lane; temporal, task/session, and diagnostic lanes remain unavailable
//! until their delivery PRs.

pub mod dedupe;
pub mod diversity;
pub mod exact;
pub mod fusion;
pub mod graph;
pub mod hydrate;
pub mod lexical;
pub mod ports;
pub mod request;
pub mod semantic;
pub mod unavailable;

pub use self::ports::{
    ExactTermPostingReadPort, GraphEvidenceReadPort, LexicalPostingReadPort, RetrievalPortError,
};
pub use self::request::{RawRetrievalRequestV1, SanitizedRetrievalRequestV1};
pub use self::unavailable::{CapabilityReportedLane, UnavailableLaneReportV1};

#[cfg(test)]
mod tests;
