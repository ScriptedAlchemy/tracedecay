//! Shared wall-clock readings for daemon invocation admission and expiry.

use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_application::clock::now_micros;
use tracedecay_domain::UtcMicros;

pub(super) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub(super) fn current_micros() -> UtcMicros {
    now_micros()
}
