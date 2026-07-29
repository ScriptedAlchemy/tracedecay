//! Daemon store-runtime adapters and the canonical shard registry.
//!
//! Production call sites still land behind these modules during S8 cutover, so
//! the registry surfaces remain intentionally constructed from tests and the
//! lifecycle publisher until every live open is routed here.

#![allow(dead_code)] // S8 lands before all daemon call sites route through this registry.

mod graph_metadata;
pub(crate) mod registry;
pub(crate) mod resolver;
pub(crate) mod rusqlite_parity;
pub(crate) mod session_registry;
pub(crate) mod shard;
pub(crate) mod telemetry;
