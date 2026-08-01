//! Immutable semantic vector-generation storage.
//!
//! The canonical definitions now live in
//! `tracedecay_usecases::store::vector_generations`. This module re-exports
//! them so existing `crate::store::vector_generations::…` paths keep working
//! without duplicating the state machine, the database-backed store, or the
//! legacy-migration adapters.

pub use tracedecay_usecases::store::vector_generations::*;
