//! Code-index generation retention, moved down from the root binary's
//! `src/retention/`. `code_index_generations.rs` has no `crate::` references
//! at all — it was root-owned only by accident of placement.

pub mod code_index_generations;
