//! Shard-runtime kernel: the per-shard `SQLite` runtime and its registry.
//!
//! `db::Database` holds a client lease on a published [`shard::ShardRuntime`].
//! The registry opens, attaches, retires, and closes those runtimes.
//! `telemetry` reports their inventory. Every store depends on this module.
//!
//! `tracedecay-store-runtime` owns project-store and session lifecycle. It
//! selects the shards a daemon opens, their order and profile authority, and
//! how they converge, retire, and shut down. It uses this registry through
//! `registry::StoreRuntimeResolver` and `registry::ShardRuntimePublisher`.

pub mod registry;
pub mod shard;
pub mod telemetry;
mod verified_graph;

pub use verified_graph::{VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1};

pub(crate) use crate::tracedecay::saturating_utc_now as utc_now;
