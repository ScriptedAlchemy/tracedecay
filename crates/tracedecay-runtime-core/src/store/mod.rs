//! Kernel-owned persistence stores.
//!
//! Only the fact store lives here. The root crate keeps the `store` adapters
//! that borrow an already-open `global_db` (`git_correlation`, `global_db`,
//! `observation`, `session`, `vector_generations`, `workflow`) because
//! `global_db`, `sessions`, and `semantic_code` all sit above this kernel; the
//! root `store` module re-exports [`memory`] so `crate::store::memory::…`
//! keeps resolving from both sides.

pub mod memory;

pub use memory::DatabaseFactStore;
