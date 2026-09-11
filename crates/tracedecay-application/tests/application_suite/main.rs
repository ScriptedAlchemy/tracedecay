//! Consolidated application-layer integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix.

mod delivery_settlement_observability;
mod github_stack_anchor_authority;
mod github_stack_coordinator;
mod github_stack_drift_observability;
mod native_declared_topology_projection;
mod observability_empty_day_closure;
mod observability_producer_shutdown;
mod observability_rollup_convergence;
// `pub` keeps `work_rollup_harness` as reachable as it was at the old crate
// root: the work_rollup bench mounts it, and the suite itself uses a subset.
pub mod observability_runtime_contract;
mod registered_scope_route;
mod semantic_graph_deadline_authority;
mod work_service_composition;
mod work_topology_contract;
mod workflow_topology_contract;
