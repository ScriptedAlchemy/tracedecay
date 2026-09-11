//! Consolidated code-index integration suite.
//!
//! Every code-index integration test is a module of this one binary so the
//! crate links its dependency closure once; a module that used to be its own
//! `tests/<module>.rs` binary keeps that name as its module prefix.

mod architecture_boundaries;
mod chunk_incremental;
mod deterministic_extraction;
mod diagnostic_generation;
mod generations;
mod git_joins;
mod git_topology_edge_cases;
mod git_topology_projection;
mod graph_generation_identity;
mod graph_projection_publication;
mod ignored_source_admissions;
mod impact_joins;
mod import_evidence;
mod language_registry;
mod lineage;
mod production_orchestration;
mod projection_receipts;
mod retained_parse;
mod retained_parse_canonical_identity;
mod sanitized_intake;
mod sealed_generation_restore;
mod search_chunks;
mod support;
mod symbol_span_digest;
mod test_attribution;
