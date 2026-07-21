//! Driver-neutral contracts for daemon-owned storage runtimes.
//!
//! These types describe identity, admission, consistency, operations, effects,
//! errors, and telemetry. They deliberately contain no physical paths,
//! database-driver values, executors, or connection-opening behavior.

mod consistency;
mod error;
mod identity;
mod lifecycle;
mod operation;
mod outbox;
mod ports;
mod telemetry;

pub use consistency::*;
pub use error::*;
pub use identity::*;
pub use lifecycle::*;
pub use operation::*;
pub use outbox::*;
pub use ports::*;
pub use telemetry::*;
