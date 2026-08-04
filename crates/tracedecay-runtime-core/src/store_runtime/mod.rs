//! Store-runtime adapters and the canonical shard registry.
//!
//! This tree moved down from the root crate's `daemon::store_runtime` because
//! it is kernel code in every direction that matters: it opens `db::Database`
//! facades, holds `db::DatabaseAuthority`, and resolves `storage` layouts. The
//! root keeps a `daemon::store_runtime` shim so every historical path resolves,
//! and still owns `session_registry`, which could not follow (see `SEAMS.md`).
//!
//! The lifecycle publisher and registry are the canonical runtime authority for
//! shard attachment, maintenance, and retrieval.

mod graph_metadata;
pub mod profile_paths;
pub mod registry;
pub mod resolver;
pub mod rusqlite_parity;
pub mod shard;
pub mod telemetry;
