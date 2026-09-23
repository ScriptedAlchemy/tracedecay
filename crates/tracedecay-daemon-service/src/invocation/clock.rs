//! Shared wall-clock readings for daemon invocation admission and expiry.

pub fn now_millis() -> u64 {
    tracedecay_runtime_core::tracedecay::unix_millis()
}
