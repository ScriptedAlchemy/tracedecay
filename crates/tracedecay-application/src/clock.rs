//! The one wall-clock reading shared by every TraceDecay runtime.
//!
//! `tracedecay-domain` deliberately holds values and validation only, so the
//! ambient clock lives at the lowest impure crate instead. Every consumer of
//! `now_micros` already depends on this crate.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;

/// Why [`try_now_micros`] refused to mint a timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockError {
    BeforeUnixEpoch,
    OverflowsI64Micros,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeUnixEpoch => formatter.write_str("wall clock is before the Unix epoch"),
            Self::OverflowsI64Micros => {
                formatter.write_str("wall clock overflows i64 microseconds")
            }
        }
    }
}

impl std::error::Error for ClockError {}

/// The current wall-clock instant as [`UtcMicros`], or a typed clock failure.
///
/// Callers that already return a clock-unavailable outcome should use this
/// instead of saturating. Stamp-now runtimes that must never fail closed use
/// [`now_micros`].
pub fn try_now_micros() -> Result<UtcMicros, ClockError> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ClockError::BeforeUnixEpoch)?;
    let micros =
        i64::try_from(since_epoch.as_micros()).map_err(|_| ClockError::OverflowsI64Micros)?;
    Ok(UtcMicros(micros))
}

/// The current wall-clock instant as [`UtcMicros`].
///
/// Saturating by construction: a clock that reads before the Unix epoch yields
/// `UtcMicros(0)`, and an instant beyond `i64::MAX` microseconds clamps to
/// `UtcMicros(i64::MAX)`. Runtimes that stamp "now" share this definition so
/// the clamp cannot differ by call site — a truncating `as i64` cast, which
/// two call sites previously used, wraps a far-future clock into a negative
/// timestamp that then compares as older than every stored record.
#[must_use]
pub fn now_micros() -> UtcMicros {
    match try_now_micros() {
        Ok(now) => now,
        Err(ClockError::BeforeUnixEpoch) => UtcMicros(0),
        Err(ClockError::OverflowsI64Micros) => UtcMicros(i64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::{now_micros, try_now_micros};

    #[test]
    fn reads_a_plausible_non_saturated_epoch_instant() {
        let first = now_micros();
        let second = now_micros();
        // 2020-01-01T00:00:00Z: any plausible clock is past this.
        assert!(first.0 > 1_577_836_800_000_000);
        assert!(first.0 < i64::MAX);
        assert!(second >= first);
    }

    #[test]
    fn try_now_micros_agrees_with_the_saturating_stamp_on_a_plausible_clock() {
        let attempted = try_now_micros().expect("plausible clock is representable");
        let stamped = now_micros();
        assert!(attempted.0 > 1_577_836_800_000_000);
        assert!(attempted.0 < i64::MAX);
        assert!(stamped >= attempted);
    }
}
