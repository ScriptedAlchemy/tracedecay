//! Compatibility re-exports for the session-owned observation contracts.

pub use tracedecay_sessions::observation::*;

#[cfg(test)]
#[path = "observation_test.rs"]
mod tests;
