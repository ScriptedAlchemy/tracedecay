//! Native Turso connections: guarded statement execution over one confined
//! engine instance per store. Compiled on both engines while
//! `tracedecay-runtime-core`'s `engine::native` facade still drives the
//! guarded `Connection` directly; the rusqlite-shaped `driver` over it is the
//! crate's public surface under `--config=native`.
mod confined_io;
// The driver-only half of the guarded connection is unused on the default
// engine, where only the facade runs.
#[cfg_attr(not(td_engine = "native"), allow(dead_code))]
mod connection;
#[cfg(all(unix, feature = "test-helpers"))]
pub mod crash_io;
#[cfg(td_engine = "native")]
pub(crate) mod driver;
mod error;
mod policy;
mod value;

pub use connection::{
    Access, Authority, Connection, Database, ExecutionGuard, InterruptHandle, TransactionBehavior,
};
pub use error::{Error, Result};
pub use value::{Rows, Value};

/// Page-cache entries the engine's spill-check recount has walked on the
/// calling thread, a cycle-free measure of that O(n) work.
#[cfg(feature = "test-helpers")]
pub fn page_cache_entries_walked() -> u64 {
    turso_core::page_cache_entries_walked()
}

#[cfg(test)]
mod tests;
