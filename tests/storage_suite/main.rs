//! Consolidated storage/reset test suite.
//!
//! Each module was previously a standalone integration-test binary; merging
//! them into one binary removes ten separate link steps, which dominate
//! Windows CI time. Migration engine coverage now lives beside the private
//! runtime APIs in `db::schema::tests`.

#[path = "../common/mod.rs"]
mod common;
mod support;

mod corruption_test;
mod db_query_test;
mod db_test;
mod fact_merge_hydration_test;
mod global_registry_test;
mod multi_connection_test;
mod native_project_alias_test;
mod profile_storage_reset_test;
mod project_identity_collapse_test;
mod storage_resolver_test;
mod worktree_canonical_root_guard_test;
