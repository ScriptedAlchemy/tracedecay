//! Read-side query surface: search ranking plus thin delegation to the
//! graph query/traversal layers.

pub(crate) mod graph;
mod meta;
mod search;
mod source;
mod traits;
