//! Consolidated storage/migration/branch test suite.
//!
//! Each module was previously a standalone integration-test binary; merging
//! them into one binary removes ten separate link steps, which dominate
//! Windows CI time. Migration engine coverage now lives beside the private
//! runtime APIs in `db::migrations::tests`.

#[path = "../common/mod.rs"]
mod common;
mod support;

mod branch_db_safety_test;
mod branch_drift_test;
mod corruption_test;
mod db_query_test;
mod db_test;
mod fact_merge_hydration_test;
mod global_registry_test;
mod migrate_inventory_test;
mod multi_connection_test;
mod native_project_alias_test;
mod profile_storage_migration_test;
mod project_identity_collapse_test;
mod storage_resolver_test;
mod worktree_canonical_root_guard_test;
