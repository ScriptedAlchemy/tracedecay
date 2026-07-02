//! Consolidated storage/migration/branch test suite.
//!
//! Each module was previously a standalone integration-test binary; merging
//! them into one binary removes ten separate link steps, which dominate
//! Windows CI time. Test paths keep their old binary name as the module
//! prefix (e.g. `migration_test::test_migrate_from_v0`) so nextest filters
//! and test-group assignments stay readable.

#[path = "../common/mod.rs"]
mod common;
mod support;

mod branch_db_safety_test;
mod branch_drift_test;
mod corruption_test;
mod db_query_test;
mod db_test;
mod global_registry_test;
mod migrate_inventory_test;
mod migration_manifest_test;
mod migration_test;
mod profile_storage_migration_test;
mod storage_resolver_test;
