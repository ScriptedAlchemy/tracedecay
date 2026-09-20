//! Kernel-owned slice of the root `tracedecay` orchestrator module.
//!
//! Wall-clock stamps for this crate share one read. A pre-epoch clock saturates
//! to a zero duration, except [`utc_now_or_one`], which reads as `1` so a failed
//! stamp stays distinct from an absent `UtcMicros(0)`. Microsecond overflow
//! saturates to `i64::MAX`; second stamps keep the prior `as i64` conversion.
//! This crate cannot depend on `tracedecay_contracts::clock` (that crate is the
//! ports/contracts layer; taking it would pull policy, tool-catalog, and
//! schemars into a kernel that currently has no application edge).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;

/// Shared wall-clock duration since Unix epoch. Pre-epoch clocks yield zero.
fn wall_clock_since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Unix seconds since the epoch. A pre-epoch clock is `0`.
pub fn unix_secs() -> u64 {
    wall_clock_since_epoch().as_secs()
}

/// Returns the current UNIX timestamp in seconds.
///
/// Overflow keeps the historical wrapping `as i64` cast. Callers that must
/// saturate use [`saturating_unix_secs`].
pub fn current_timestamp() -> i64 {
    unix_secs() as i64
}

/// Unix seconds as `i64`. A pre-epoch clock is `0`; overflow is `i64::MAX`.
pub fn saturating_unix_secs() -> i64 {
    i64::try_from(wall_clock_since_epoch().as_secs()).unwrap_or(i64::MAX)
}

/// Shared saturating wall clock for shard, registry, and fact-runtime stamps.
///
/// A pre-epoch clock reads as zero and an overflowing microsecond count as
/// `i64::MAX`. This is the kernel-local equivalent of
/// `tracedecay_contracts::clock::now_micros`.
pub fn saturating_utc_now() -> UtcMicros {
    UtcMicros(unix_micros_saturating(0))
}

/// Saturating microseconds since the epoch. A pre-epoch clock reads as `1`.
pub fn utc_now_or_one() -> UtcMicros {
    UtcMicros(unix_micros_saturating(1))
}

fn unix_micros_saturating(pre_epoch: i64) -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_micros()).unwrap_or(i64::MAX),
        Err(_) => pre_epoch,
    }
}
