//! MCP message-search translation and rendering adapter.
//!
//! Canonical temporal retrieval, authorization, pagination, and payload
//! hydration are owned by `crate::mcp::server::session_retrieval`.

#[path = "message_search/adapter.rs"]
mod adapter;
#[path = "message_search/contract.rs"]
mod contract;

pub(crate) use adapter::*;
pub(crate) use contract::*;

#[cfg(test)]
#[path = "message_search/tests.rs"]
mod tests;
