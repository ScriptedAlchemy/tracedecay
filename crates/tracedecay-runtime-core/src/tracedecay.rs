//! Kernel-owned slice of the root `tracedecay` orchestrator module.
//!
//! Wall-clock stamps for this crate share [`wall_clock_since_epoch`]: a
//! pre-epoch clock saturates to a zero duration. Microsecond stamps then
//! saturate overflow to `i64::MAX`; second stamps keep the prior `as i64`
//! conversion. This crate cannot depend on `tracedecay_application::clock`
//! (that crate is the ports/contracts layer; taking it would pull policy,
//! tool-catalog, and schemars into a kernel that currently has no
//! application edge).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;

/// Shared wall-clock duration since Unix epoch. Pre-epoch clocks yield zero.
fn wall_clock_since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Returns the current UNIX timestamp in seconds.
pub fn current_timestamp() -> i64 {
    wall_clock_since_epoch().as_secs() as i64
}

/// Shared saturating wall clock for shard, registry, and fact-runtime stamps.
///
/// A pre-epoch clock reads as zero and an overflowing microsecond count as
/// `i64::MAX`. This is the kernel-local equivalent of
/// `tracedecay_application::clock::now_micros`.
pub(crate) fn saturating_utc_now() -> UtcMicros {
    UtcMicros(i64::try_from(wall_clock_since_epoch().as_micros()).unwrap_or(i64::MAX))
}
