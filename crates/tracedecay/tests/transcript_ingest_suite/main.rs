//! Consolidated transcript-ingest integration suite.
//!
//! One test binary for every per-agent transcript ingestion source (Claude,
//! Codex, Cursor, Hermes, Kiro, Cline-like, Vibe) instead of seven separate
//! binaries: each integration test binary links the full `tracedecay` crate
//! separately, and link time dominates Windows CI.

// Full-journey Hotpath builds compose measured provider-ingest futures in each
// test body; keep the expanded query budget local to this test crate.
#![recursion_limit = "256"]

#[path = "../common/mod.rs"]
mod common;

mod support;

mod claude;
mod cline_like;
mod codex;
mod codex_compaction;
mod codex_goals;
mod codex_ingest;
mod codex_response_items;
mod codex_usage;
mod cursor;
mod cursor_composer;
mod hermes;
mod kiro;
mod provider_contract;
mod restart_atomicity;
mod source_identity;
mod vibe;
