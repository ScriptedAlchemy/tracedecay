//! Host transcript adapters.
//!
//! Each submodule owns one agent-host's on-disk transcript shape and the
//! projection into provider-neutral session rows. Ingest dispatches through
//! these adapters; they are re-exported at `crate::runtime::{claude, …}` so
//! existing public paths stay stable.

pub mod claude;
pub mod claude_observation;
pub mod cline_like;
pub mod codex;
pub mod codex_app_server;
pub mod cursor;
pub mod cursor_composer;
pub mod hermes;
pub mod kimi;
pub mod kiro;
pub mod opencode;
pub(in crate::runtime) mod opencode_frontier;
pub(in crate::runtime) mod opencode_part_scan;
pub(in crate::runtime) mod opencode_snapshot;
pub mod vibe;
