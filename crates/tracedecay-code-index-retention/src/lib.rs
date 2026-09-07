//! Liveness-based retention for immutable code-index generations.
//!
//! This crate sits below `tracedecay-usecases` so the daemon, maintenance,
//! and a future code-index-runtime crate can collect generations without
//! pulling the usecases spine.

pub mod code_index_generations;
mod hotpath_observe;
