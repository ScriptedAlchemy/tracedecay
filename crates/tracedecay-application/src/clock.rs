//! The one wall-clock reading shared by every TraceDecay runtime.
//!
//! `tracedecay-domain` deliberately holds values and validation only, so the
//! ambient clock lives at the lowest impure crate instead. Every consumer of
//! `now_micros` already depends on this crate.

use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;

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
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| {
            i64::try_from(since_epoch.as_micros()).unwrap_or(i64::MAX)
        });
    UtcMicros(micros)
}

#[cfg(test)]
mod tests {
    use super::now_micros;

    #[test]
    fn reads_a_plausible_non_saturated_epoch_instant() {
        let first = now_micros();
        let second = now_micros();
        // 2020-01-01T00:00:00Z: any plausible clock is past this.
        assert!(first.0 > 1_577_836_800_000_000);
        assert!(first.0 < i64::MAX);
        assert!(second >= first);
    }
}
