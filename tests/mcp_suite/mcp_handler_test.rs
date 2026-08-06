//! Integration tests for MCP tool handlers (`handle_tool_call`).
//!
//! Split into per-domain modules under `mcp_handler_test/`; shared
//! fixtures and helpers live in the suite-level `support` module.

mod admin_test;
mod bounded_analysis_test;
mod context_test;
#[cfg(feature = "test-transport")]
mod edit_test;
mod graph_analysis_test;
mod graph_query_test;
mod lcm_test;
mod memory_facts_test;
#[cfg(feature = "test-transport")]
mod move_symbol_test;
mod rename_symbol_test;
mod retrieve_truncation_test;
mod schema_test;
mod session_search_test;
mod skills_automation_test;
mod status_runtime_test;

// Backwards-compatible paths for sibling suite modules that import
// helpers via `crate::mcp_handler_test::…`.
pub(crate) use crate::support::{GLOBAL_DB_ENV_LOCK, setup_project};
