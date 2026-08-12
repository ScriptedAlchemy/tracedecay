//! Integration tests for MCP tool handlers (`handle_tool_call`).
//!
//! Split into per-domain modules under `mcp_handler_test/`; shared
//! fixtures and helpers live in the suite-level `support` module.

mod admin_test;
mod bounded_analysis_test;
mod context_test;
mod dependency_hint_test;
#[cfg(feature = "test-transport")]
mod edit_test;
mod graph_analysis_test;
mod graph_query_test;
mod lcm_test;
#[cfg(feature = "test-transport")]
mod memory_curation_test;
mod memory_facts_test;
#[cfg(feature = "test-transport")]
mod move_symbol_test;
#[cfg(feature = "test-transport")]
mod rename_symbol_test;
mod retrieve_truncation_test;
mod schema_test;
mod session_search_test;
mod skills_automation_test;
mod status_runtime_test;

// Shared lock used by sibling transport suites.
#[cfg(feature = "test-transport")]
pub(crate) use crate::support::GLOBAL_DB_ENV_LOCK;
