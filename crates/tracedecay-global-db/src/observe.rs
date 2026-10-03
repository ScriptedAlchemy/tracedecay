//! global-db metrics gauges.
//!
//! Rusqlite-runtime owns measured writer, reader, and SQLite VM counters. These
//! helpers only record operation-family counts that are already known at the
//! call site; they never fabricate scan, sort, or transaction work.
