//! Native Turso connections with actor-owned transaction control and execution
//! guards. Compiled on both engines while `tracedecay-runtime-core`'s
//! `engine::native` facade still drives it; the rusqlite-shaped `driver` over it
//! is the crate's public surface under `--config=native`.
mod connection;
#[cfg(td_engine = "native")]
pub(crate) mod driver;
mod error;
#[cfg(unix)]
mod pinned_io;
mod policy;
mod value;

pub use connection::{
    Access, Authority, Connection, Database, ExecutionGuard, InterruptHandle, TransactionBehavior,
};
pub use error::{Error, Result};
pub use value::{Rows, Value};

#[cfg(test)]
mod tests;
