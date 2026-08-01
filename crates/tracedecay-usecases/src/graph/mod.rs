//! Graph query and health helpers moved down from the root binary's
//! `src/graph/`. Both files' whole closure is the runtime kernel
//! (`tracedecay_runtime_core::{db, errors, types}`); nothing kept them at the
//! composition root. See SEAMS.md.

pub mod health;
pub mod queries;
pub mod scc;
