//! Dependency-neutral Git read models and index transaction contracts.

mod hunk;
mod index_preview;
mod index_transaction;
mod read_model;
pub mod repository_state;

pub use hunk::*;
pub use index_preview::*;
pub use index_transaction::*;
pub use read_model::*;
pub use repository_state::*;

use read_model::validate_path_label;
