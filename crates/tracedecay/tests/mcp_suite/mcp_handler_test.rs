//! Integration tests for MCP tool handlers (`handle_tool_call`).
//!
//! Split into per-domain modules under `mcp_handler_test/`; shared
//! fixtures and helpers live in the suite-level `support` module.

mod admin_test;
#[cfg(feature = "test-transport")]
mod affected_tests_behavior_test;
#[cfg(feature = "test-transport")]
mod affected_tests_test;
#[cfg(feature = "test-transport")]
mod ast_grep_rewrite_behavior_test;
mod automation_run_list_behavior_test;
mod automation_runs_test;
mod bounded_analysis_test;
mod branch_diff_behavior_test;
#[cfg(feature = "test-transport")]
mod branch_search_test;
#[cfg(feature = "test-transport")]
mod branch_sensitivity_test;
mod circular_behavior_test;
#[cfg(feature = "test-transport")]
mod configuration_unset_test;
#[cfg(feature = "test-transport")]
mod constructors_behavior_test;
mod context_behavior_test;
mod context_test;
mod dependency_depth_test;
mod dependency_hint_test;
mod derives_test;
mod diagnose_test;
#[cfg(feature = "test-transport")]
mod diagnostics_test;
#[cfg(feature = "test-transport")]
mod edit_test;
#[cfg(feature = "test-transport")]
mod expand_query_behavior;
mod fact_store_remove_behavior_test;
#[cfg(feature = "test-transport")]
mod fact_store_update_behavior_test;
mod feedback_diagnostics_test;
mod feedback_expand_test;
mod feedback_get_test;
#[cfg(feature = "test-transport")]
mod feedback_list_test;
#[cfg(feature = "test-transport")]
mod files_behavior_test;
mod find_exact_symbol_test;
mod god_class_test;
mod graph_analysis_test;
mod graph_query_test;
mod grep_behavior_test;
mod health_behavior_test;
#[cfg(feature = "test-transport")]
mod hermes_skill_bridge_test;
mod impact_behavior_test;
mod implementations_test;
mod impls_behavior_test;
mod inheritance_depth_test;
#[cfg(feature = "test-transport")]
mod insert_at_test;
mod largest_test;
#[cfg(feature = "test-transport")]
mod lcm_describe_behavior;
mod lcm_doctor_test;
#[cfg(feature = "test-transport")]
mod lcm_expand_behavior_test;
#[cfg(feature = "test-transport")]
mod lcm_grep_behavior_test;
mod lcm_test;
#[cfg(feature = "test-transport")]
mod memory_contradiction_contract_test;
#[cfg(feature = "test-transport")]
mod memory_fact_assertions;
#[cfg(feature = "test-transport")]
mod memory_fact_supersede_test;
mod memory_facts_test;
mod memory_feedback_test;
#[cfg(feature = "test-transport")]
mod memory_status_test;
mod move_symbol_behavior_test;
#[cfg(feature = "test-transport")]
mod move_symbol_test;
#[cfg(feature = "test-transport")]
mod multi_str_replace_behavior_test;
mod mutate_graph_test;
mod node_behavior_test;
#[cfg(feature = "test-transport")]
mod port_order_test;
mod port_status_test;
#[cfg(feature = "test-transport")]
mod project_context_test;
#[cfg(feature = "test-transport")]
mod project_list_test;
#[cfg(feature = "test-transport")]
mod project_search_behavior_test;
mod rank_behavior_test;
#[cfg(all(feature = "test-transport", unix))]
mod release_placement_test;
mod rename_preview_test;
#[cfg(feature = "test-transport")]
mod rename_symbol_test;
#[cfg(feature = "test-transport")]
mod replace_symbol_test;
mod retrieve_truncation_test;
mod schema_test;
mod search_behavior_test;
mod session_refresh_begin_test;
mod session_refresh_cancel_test;
mod session_refresh_status_test;
mod session_search_test;
#[cfg(feature = "test-transport")]
mod shell_dead_code_test;
mod signature_behavior_test;
mod signature_search_test;
mod similar_test;
#[cfg(feature = "test-transport")]
mod skill_list_test;
#[cfg(feature = "test-transport")]
mod skill_view_behavior_test;
mod skills_automation_test;
#[cfg(feature = "test-transport")]
mod source_edit_reconcile_test;
#[cfg(feature = "test-transport")]
mod source_edit_rollback_test;
mod status_runtime_test;
#[cfg(feature = "test-transport")]
mod str_replace_behavior_test;
#[cfg(feature = "test-transport")]
mod test_map_test;
#[cfg(feature = "test-transport")]
mod test_risk_behavior_test;
mod todos_test;
mod type_hierarchy_test;
mod unmounted_files_test;
mod unsafe_patterns_test;
#[cfg(feature = "test-transport")]
mod work_resume_attempts_test;
#[cfg(feature = "test-transport")]
mod work_test;
#[cfg(feature = "test-transport")]
mod workflow_activate_definition_test;
#[cfg(all(feature = "test-transport", unix))]
mod workflow_register_definition_test;

// Shared lock used by sibling transport suites.
#[cfg(feature = "test-transport")]
pub(crate) use crate::support::GLOBAL_DB_ENV_LOCK;
