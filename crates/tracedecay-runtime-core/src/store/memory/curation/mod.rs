//! Compatibility curation apply, relations, entity merges, and fact merges.
//!
//! Re-exports below preserve every path used outside this module.

mod apply;
mod entities;
mod relations;
#[cfg(test)]
mod tests;

pub(super) use self::apply::*;
use self::entities::*;
pub(super) use self::relations::*;
