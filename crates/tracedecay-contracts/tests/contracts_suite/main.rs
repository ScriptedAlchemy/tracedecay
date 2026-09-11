//! Consolidated contract-crate integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. `common` is compiled once here and reached through
//! `use crate::common;` from the modules that used to declare it.

mod common;

mod advisory_requests;
mod authorization_non_disclosure;
mod authorization_recheck;
mod callable_code_queries;
mod catalog_contributions;
mod diagnostic_provider_identity;
mod doctor_advisory_feedback;
mod doctor_report;
mod effect_receipts;
mod evidence_contract;
mod execution_topology_metrics;
mod execution_topology_producer_terminal;
mod execution_topology_rollup;
mod execution_topology_rollup_compaction;
mod feedback_advisory_cycle;
mod feedback_cycle;
mod git_read_contract;
mod git_sdk_catalog;
mod github_stack_signal_expand_catalog;
mod handoff_catalog;
mod handoff_open;
mod memory_use_cases;
mod multi_root_catalog;
mod multi_root_query;
mod multi_root_scope_set;
mod observability_share_contract;
mod policy_composition;
mod primitive_sdk_catalog;
mod source_edit_effect;
mod source_edit_sdk_catalog;
mod stream_contract;
mod surface_binding_parity;
mod work_artifact_hydration_service;
mod work_attempt_service;
mod work_authority;
mod work_placement_service;
mod work_product_application;
mod work_proposal_planner;
mod work_run_control_service;
mod work_synthesis_service;
mod work_topology_view;
mod workflow_coordination;
mod workflow_dag_execution;
mod workflow_fan_out_census;
mod workflow_provider_registry;
mod workflow_runtime;
