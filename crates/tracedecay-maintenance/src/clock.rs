//! Wall-clock helpers for retention cutoffs.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix time in seconds, or a typed clock failure.
pub fn now_secs_i64() -> Result<i64, &'static str> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system_clock_before_unix_epoch")?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system_clock_out_of_range")
}
