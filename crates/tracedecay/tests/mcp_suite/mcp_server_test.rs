//! Integration tests for the MCP server (`McpServer`) exercising the full
//! JSON-RPC 2.0 protocol via `ChannelTransport`.
//!
//! Run with: `cargo test --features test-transport --test mcp_suite`
//!
//! Split into per-domain modules under `mcp_server_test/`; shared
//! helpers live in `mcp_server_test/support.rs`.

mod analytics_test;
mod hooks_branch_test;
mod protocol_test;
pub(crate) mod support;

// Backwards-compatible path for `crate::mcp_server_test::…` consumers.
pub(crate) use support::run_client_connection_with_messages;
