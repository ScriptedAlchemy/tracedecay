//! Integration tests for MCP tool handlers (`handle_tool_call`).
//!
//! Split into per-domain modules under `mcp_handler_test/`; shared
//! fixtures and helpers live in the suite-level `support` module.

mod admin_test;
#[cfg(feature = "test-transport")]
mod affected_tests_behavior_test;
#[cfg(feature = "test-transport")]
mod affected_tests_test;
mod automation_runs_test;
mod bounded_analysis_test;
#[cfg(feature = "test-transport")]
mod branch_search_test;
#[cfg(feature = "test-transport")]
mod branch_sensitivity_test;
#[cfg(feature = "test-transport")]
mod configuration_unset_test;
mod context_behavior_test;
mod context_test;
mod dependency_depth_test;
mod dependency_hint_test;
mod derives_test;
#[cfg(feature = "test-transport")]
mod edit_test;
#[cfg(feature = "test-transport")]
mod fact_store_update_behavior_test;
#[cfg(feature = "test-transport")]
mod feedback_list_test;
mod find_exact_symbol_test;
mod graph_analysis_test;
mod graph_query_test;
mod grep_behavior_test;
#[cfg(feature = "test-transport")]
mod hermes_skill_bridge_test;
mod inheritance_depth_test;
mod lcm_test;
#[cfg(feature = "test-transport")]
mod project_list_test;
#[cfg(feature = "test-transport")]
mod memory_contradiction_contract_test;
#[cfg(feature = "test-transport")]
mod memory_fact_assertions;
mod memory_facts_test;
mod memory_feedback_test;
#[cfg(feature = "test-transport")]
mod move_symbol_test;
#[cfg(feature = "test-transport")]
mod project_context_test;
#[cfg(feature = "test-transport")]
mod project_search_behavior_test;
#[cfg(feature = "test-transport")]
mod rename_symbol_test;
mod retrieve_truncation_test;
mod schema_test;
mod session_search_test;
#[cfg(feature = "test-transport")]
mod shell_dead_code_test;
mod skills_automation_test;
#[cfg(feature = "test-transport")]
mod source_edit_rollback_test;
mod status_runtime_test;
#[cfg(feature = "test-transport")]
mod test_map_test;
mod unsafe_patterns_test;
#[cfg(feature = "test-transport")]
mod work_test;

// Shared lock used by sibling transport suites.
#[cfg(feature = "test-transport")]
pub(crate) use crate::support::GLOBAL_DB_ENV_LOCK;
