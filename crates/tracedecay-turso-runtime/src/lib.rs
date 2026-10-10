//! Native Turso connections with actor-owned transaction control and execution guards.
mod connection;
mod error;
#[cfg(unix)]
mod pinned_io;
mod policy;
mod value;

pub use connection::{
    Access, Authority, Connection, Database, ExecutionGuard, TransactionBehavior,
};
pub use error::{Error, Result};
pub use value::{Rows, Value};

#[cfg(test)]
mod tests;

pub mod ledger;
pub mod writer;

pub mod diagnostics;
