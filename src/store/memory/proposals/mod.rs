//! Compatibility fact-proposal lifecycle, transitions, and legacy imports.
//!
//! Split into submodules; every path previously reachable via this module
//! is re-exported here so no external consumer changes.

mod lifecycle;
mod records;

pub(super) use self::lifecycle::*;
pub(super) use self::records::*;
