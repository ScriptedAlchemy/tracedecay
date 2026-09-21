//! Shared wall-clock readings for daemon invocation admission and expiry.

pub use tracedecay_contracts::clock::now_micros;
pub use tracedecay_contracts::clock::now_micros as current_micros;

pub fn now_millis() -> u64 {
    tracedecay_runtime_core::tracedecay::unix_millis()
}
