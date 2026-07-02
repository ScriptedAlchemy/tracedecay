//! Consolidated transcript-ingest integration suite.
//!
//! One test binary for every per-agent transcript ingestion source (Claude,
//! Codex, Cursor, Hermes, Kiro, Cline-like, Vibe) instead of seven separate
//! binaries: each integration test binary links the full `tracedecay` crate
//! separately, and link time dominates Windows CI.

#[path = "../common/mod.rs"]
mod common;

mod support;

mod claude;
mod cline_like;
mod codex;
mod cursor;
mod hermes;
mod kiro;
mod vibe;
