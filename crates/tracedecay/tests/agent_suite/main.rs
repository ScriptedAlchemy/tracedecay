//! Consolidated agent-integration and skill test binary.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates shard wall time. The formerly standalone binaries below are
//! compiled as modules of this single `agent_suite` binary instead; test
//! names gain a module prefix (e.g. `agent_test::test_get_all_integrations`)
//! but coverage is unchanged.

#[path = "../common/mod.rs"]
mod common;

mod agent_registry_test;
mod agent_targets_test;
mod claude_plugin_bundle_test;
mod claude_plugin_schema_test;
mod cli_args_contract_test;
mod managed_skill_archive_test;
mod managed_skills_test;
mod plugin_config_schema_test;
mod plugin_manifest_schema_test;
mod plugin_validation_support;
mod shared_skill_contract_test;
mod skill_lint_claude_test;
mod skill_lint_cursor_test;
mod skill_materialization_test;
mod skill_targets_test;
mod skill_usage_test;
mod tool_skill_coverage_test;
