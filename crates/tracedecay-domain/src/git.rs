//! Dependency-neutral Git read models and index transaction contracts.

mod hunk;
mod index_preview;
mod index_transaction;
mod native_integration;
#[cfg(test)]
mod native_integration_tests;
mod read_model;
pub mod repository_state;

pub use hunk::*;
pub use index_preview::*;
pub use index_transaction::*;
pub use native_integration::*;
pub use read_model::*;
pub use repository_state::*;

use read_model::validate_path_label;
