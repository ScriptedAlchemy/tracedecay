//! Canonical Work product application authority.
//!
//! This module coordinates typed Work reads and effects. It owns neither the
//! immutable event journal nor the verified graph projection and never opens a
//! database, dispatches execution, or treats runtime evidence as acceptance.

mod attempt_admission;
mod mutation;
mod query;
mod read;
mod types;

pub use attempt_admission::*;
pub use mutation::*;
pub use query::*;
pub use read::*;
pub use types::*;
