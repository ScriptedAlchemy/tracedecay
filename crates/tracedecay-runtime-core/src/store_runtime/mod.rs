//! Store-runtime adapters and the canonical shard registry.
//!
//! This tree moved down from the root crate's `daemon::store_runtime` because
//! it is kernel code in every direction that matters: it opens `db::Database`
//! facades, holds `db::DatabaseAuthority`, and resolves `storage` layouts. The
//! root keeps a `daemon::store_runtime` shim so every historical path resolves,
//! and still owns `session_registry`, which could not follow (see `SEAMS.md`).
//!
//! Production call sites land behind these modules through the lifecycle
//! publisher and daemon store-runtime facade.

pub mod profile_paths;
#[allow(dead_code)]
pub mod registry;
#[allow(dead_code)]
pub mod resolver;
#[allow(dead_code)]
pub mod shard;
#[allow(dead_code)]
pub mod telemetry;
