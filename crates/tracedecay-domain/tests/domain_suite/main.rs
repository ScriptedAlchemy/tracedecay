//! Consolidated domain-contract integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. `tests/work_projection_fold_allocations.rs` stays its own
//! binary because its counting `#[global_allocator]` is binary-global, and
//! `tests/session_contract.rs` stays put because the root crate's
//! runtime_acceptance_suite mounts it by path.

mod branch_stack_contract;
mod canonical_identity_wire_stability;
mod code_search_contract;
mod configuration_contract;
mod external_source_foundation_contract;
mod feedback_contract;
mod git_contract;
mod git_index_transaction_contract;
mod git_topology_anchor_contract;
mod host_descriptor_contract;
mod integration_catalog_contract;
mod multi_root_contract;
mod observability_execution_contract;
mod observation_contract;
mod repository_scope_contract;
mod repository_state_contract;
mod resource_policy_contract;
mod sanitization_schema_contract;
mod session_source_freshness_contract;
mod work_contract;
mod work_duplicate_adjudication_contract;
mod work_execution_snapshot_contract;
mod work_product_contract;
mod work_read_contract;
mod work_runtime_contract;
mod workflow_definition_contract;
