//! Driver-neutral contracts for daemon-owned storage runtimes.
//!
//! These types describe identity, admission, consistency, operations, effects,
//! errors, and telemetry. They deliberately contain no physical paths,
//! database-driver values, executors, or connection-opening behavior.
//!
//! Canonical domain identities are re-exported instead of copied. Types whose
//! names begin with `Store` or `Runtime` carry storage-only invariants or
//! ownership. Application-layer IDs cross this lower-level dependency boundary
//! through validated lossless representations, never aliases.

mod consistency;
mod error;
mod graph_publication;
mod identity;
mod lifecycle;
mod operation;
mod outbox;
mod ports;
mod repository_read;
mod scope_set;
mod semantic_vector_staging;
mod telemetry;

pub use consistency::*;
pub use error::*;
pub use graph_publication::*;
pub use identity::*;
pub use lifecycle::*;
pub use operation::*;
pub use outbox::*;
pub use ports::*;
pub use repository_read::*;
pub use scope_set::*;
pub use semantic_vector_staging::*;
pub use telemetry::*;
