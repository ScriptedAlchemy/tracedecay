//! Search-quality evaluation harness skeleton (pr9/13-search-eval-fixtures).
//!
//! Plan 15 (docs/plans/tracedecay-v2/15-search-quality-evaluation-and-retrieval-research.md)
//! owns the evaluation contract; the PR9 contract spine
//! (docs/plans/tracedecay-v2/pr9/00-contract-spine.md) assigns this packet the
//! sanitized corpus, contamination partitions, sealed holdout metadata, and
//! run/evidence schemas. This suite:
//!
//! - loads and integrity-verifies the committed fixtures under
//!   `tests/fixtures/search_quality/` (manifest, corpus snapshots, query
//!   workload, development labels, sealed holdout locator);
//! - executes the development workload against the indexed corpus through
//!   the existing `tracedecay_search` / `tracedecay_grep` tool behavior via
//!   the public lib dispatch path (`tracedecay::mcp::handle_tool_call`);
//! - emits typed `EvidenceBatchV1` batches whose digests self-verify.
//!
//! It deliberately asserts NO quality thresholds, tunes NO labels, and
//! changes NO constants: this packet produces fixtures + harness + schemas
//! only. Locked-scope runs and the holdout reveal capability land with the
//! locked-comparison packet.

#[path = "../common/mod.rs"]
mod common;

// INTERIM(pr9/13): the domain crate root registration (`pub mod evaluation;`
// in crates/tracedecay-domain/src/lib.rs) lands with the coordinator's
// compose step, and this packet must not edit that file. Until then the
// The domain `evaluation` module is registered in tracedecay-domain; the
// suite exercises the real schema types through it.
#[allow(dead_code)]
pub(crate) use tracedecay_domain::evaluation;

mod candidate_producers;
mod evaluator_test;
mod fixtures;
mod fixtures_test;
mod harness_test;
mod holdout;
mod retrieval;
mod single_root;
mod support;
