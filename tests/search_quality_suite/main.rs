//! Search-quality workload fixture integrity.
//!
//! The direct PR9 retrieval and composition regressions moved to
//! `crates/tracedecay-query/tests/search_quality_suite` so query edits iterate
//! without linking the full root test binary. What remains here is the
//! workload/fixture contract that is bound to the root-owned `search_eval`
//! module and the root-checked-in fixture corpus.

mod workload_fixture;
