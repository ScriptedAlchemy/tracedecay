//! Consolidated agent-integration and skill test binary.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates shard wall time. The formerly standalone binaries below are
//! compiled as modules of this single `agent_suite` binary instead; test
//! names gain a module prefix (e.g. `agent_test::test_get_all_integrations`)
//! but coverage is unchanged.

#[path = "../common/mod.rs"]
mod common;

mod agent_test;
mod claude_agent_test;
mod copilot_agent_test;
mod kiro_agent_test;
mod managed_skills_test;
mod opencode_agent_test;
mod plugin_skill_contract_test;
mod prompt_rules_parity_test;
mod skill_targets_test;
mod skill_usage_test;
mod update_plugin_test;
