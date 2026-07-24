//! Compatibility curation apply, relations, entity merges, and fact merges.
//!
//! Split into submodules; every path previously reachable via this module
//! is re-exported here so no external consumer changes.

mod apply;
mod entities;
mod relations;
#[cfg(test)]
mod tests;

pub(super) use self::apply::*;
use self::entities::*;
pub(super) use self::relations::*;
