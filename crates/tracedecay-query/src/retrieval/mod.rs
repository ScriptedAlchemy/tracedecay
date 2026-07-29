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
pub mod pr9_authority;
pub mod request;
pub mod rerank;
pub mod semantic;
pub mod unavailable;

pub use self::ports::{
    ExactTermPostingReadPort, GraphEvidenceReadPort, LexicalPostingReadPort, RetrievalPortError,
};
pub use self::pr9_authority::{
    AuthorizedPr9FallbackV1, PR9_CURSOR_TTL_MICROS_V1, PR9_RANKING_REVISION_V1,
    Pr9QueryAuthorityErrorV1, Pr9QueryAuthorityV1,
};
pub use self::request::{RawRetrievalRequestV1, SanitizedRetrievalRequestV1};
pub use self::unavailable::{CapabilityReportedLane, UnavailableLaneReportV1};

pub const PR9_EXACT_RETRIEVER_REVISION_V1: &str = "retriever.exact.daemon.v1";
pub const PR9_LEXICAL_RETRIEVER_REVISION_V1: &str = "retriever.lexical.daemon.v1";
pub const PR9_GRAPH_RETRIEVER_REVISION_V1: &str = "retriever.graph.daemon.v1";
pub const PR9_QUERY_SANITIZER_REVISION_V1: &str = "query-sanitizer.daemon.v1";
pub const PR9_QUERY_NORMALIZATION_REVISION_V1: &str = "query-normalization.daemon.v1";
pub const PR9_EXACT_RULE_REVISION_V1: &str = "exact-rules.daemon.v1";
pub const PR9_LEXICAL_PROFILE_REVISION_V1: &str = "lexical-profile.daemon.v1";
pub const PR9_EXACT_SCORE_DOMAIN_V1: &str = "score.exact.daemon.v1";
pub const PR9_LEXICAL_SCORE_DOMAIN_V1: &str = "score.lexical.daemon.v1";
pub const PR9_GRAPH_SCORE_DOMAIN_V1: &str = "score.graph.daemon.v1";

#[cfg(test)]
mod tests;
