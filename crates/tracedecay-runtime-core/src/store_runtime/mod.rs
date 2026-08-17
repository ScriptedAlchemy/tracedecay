//! Store-runtime adapters and the canonical shard registry.
//!
//! This tree moved down from the root crate's `daemon::store_runtime` because
//! it is kernel code in every direction that matters: it opens `db::Database`
//! facades, holds `db::DatabaseAuthority`, and resolves `storage` layouts. The
//! root keeps a `daemon::store_runtime` shim so every historical path resolves,
//! and still owns `session_registry`, which could not follow (see this
//! crate's `lib.rs` module doc for why).
//!
//! The lifecycle publisher and registry are the canonical runtime authority for
//! shard attachment, maintenance, and retrieval.

pub mod profile_paths;
pub mod registry;
pub mod resolver;
pub mod shard;
pub mod telemetry;
mod verified_graph;

pub use verified_graph::VerifiedGraphRuntimePortV1;

/// Shared saturating wall clock for shard and registry stamps: a pre-epoch
/// clock reads as zero and an overflowing one as `i64::MAX`.
pub(crate) fn utc_now() -> tracedecay_domain::UtcMicros {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    tracedecay_domain::UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}
