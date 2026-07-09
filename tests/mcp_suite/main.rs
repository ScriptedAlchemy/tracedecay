#![allow(clippy::collapsible_if)]
//! Consolidated MCP integration test suite.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates the test job. Folding the MCP suites into one binary removes
//! five extra link steps while keeping each former file as its own module,
//! so nextest test names stay prefixed with the original suite name (e.g.
//! `mcp_handler_test::…`) and `.config/nextest.toml` can keep the original
//! Windows test-group assignments per module.

#[path = "../common/mod.rs"]
mod common;

mod analytics_test;
mod context_relevance_eval_test;
mod fixture;
mod git_correlation_test;
mod mcp_cli_parity_test;
mod mcp_cli_serve_test;
mod mcp_dashboard_tool_test;
mod mcp_handler_test;
mod mcp_rendering_test;
#[cfg(feature = "test-transport")]
mod mcp_server_test;
mod mcp_test;
mod multi_mcp_coordination_test;
mod serve_degraded_mode_test;
mod serve_harness;
mod serve_template_path_test;
mod support;
mod workflow_query_test;
