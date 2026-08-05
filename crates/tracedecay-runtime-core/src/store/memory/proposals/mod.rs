//! Compatibility fact-proposal lifecycle and transitions.
//!
//! Re-exports below preserve every path used outside this module.

mod lifecycle;
mod records;

pub(super) use self::lifecycle::*;
pub(super) use self::records::*;
