//! Consolidated default-feature product-surface integration suite.
//!
//! Former standalone singles that a default-feature `cargo test -p tracedecay`
//! still runs, compiled as modules of one binary so a root-crate edit no
//! longer relinks a dozen ~700 MiB test binaries.

#![recursion_limit = "256"]

mod api_application_parity;
mod catalog_composition_contract;
mod git_intelligence_regression;
mod host_bundle_acceptance;
mod native_integration_surface_mount;
mod packaged_semantic_evaluator;
mod profile_backup_rehearsal_test;
#[allow(clippy::cloned_ref_to_slice_refs, clippy::drop_non_drop)] // test builders and explicit early drops
#[forbid(unsafe_code)]
mod semantic_vector_generation_prep_test;
mod verified_profile_backup;
mod work_views_route;
