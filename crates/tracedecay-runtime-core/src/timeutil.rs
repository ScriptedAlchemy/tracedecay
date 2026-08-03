//! Time formatting and search-filter parsing over the shared timestamp
//! primitives.
//!
//! The parsers themselves live in `tracedecay_capture::timestamp`, which
//! transcript ingest already reads: this module re-exports them so the
//! accounting parser and the MCP LCM session handlers cannot drift from what
//! ingest accepted, and adds the formatting and relative-filter parsing that
//! only the executable needs.

pub use tracedecay_capture::{
    format_yyyy_mm_dd, humanize_unix_secs, now_iso_utc, parse_cursor_human_timestamp,
    parse_rfc3339_timestamp, parse_yyyy_mm_dd_utc_start,
};

/// Returns the nearest-rank percentile from an ascending sample.
///
/// The caller owns sorting so repeated percentile reads can share one sort.
/// Empty samples and percentiles outside `1..=100` return `None`.
pub fn nearest_rank(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() || !(1..=100).contains(&percentile) {
        return None;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTimeBound {
    Start,
    End,
}

/// Parses search filter timestamps. Accepts Unix seconds, RFC3339, `YYYY-MM-DD`
/// UTC dates, `today`, `yesterday`, and relative forms like `last hour`, with
/// `bound` deciding which end of a whole-day value is returned.
pub fn parse_search_time_filter_bound(
    value: &str,
    now: i64,
    bound: SearchTimeBound,
) -> Option<i64> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(timestamp) = text.parse::<i64>() {
        return (timestamp >= 0).then_some(timestamp);
    }
    if let Some(timestamp) = parse_rfc3339_timestamp(text) {
        return Some(timestamp);
    }
    if let Some(day_start) = parse_yyyy_mm_dd_utc_start(text) {
        return Some(bound_day_timestamp(day_start, bound));
    }

    let normalized = text.to_ascii_lowercase();
    match normalized.as_str() {
        "today" => return Some(bound_day_timestamp(now.div_euclid(86_400) * 86_400, bound)),
        "yesterday" => {
            return Some(bound_day_timestamp(
                now.div_euclid(86_400) * 86_400 - 86_400,
                bound,
            ));
        }
        _ => {}
    }

    let words: Vec<&str> = normalized.split_whitespace().collect();
    let (count, unit) = match words.as_slice() {
        ["last", unit] => (1_i64, *unit),
        ["last", count, unit] | [count, unit, "ago"] => (count.parse::<i64>().ok()?, *unit),
        _ => return None,
    };
    let seconds = match unit.trim_end_matches('s') {
        "minute" | "min" => count.checked_mul(60)?,
        "hour" | "hr" => count.checked_mul(3_600)?,
        "day" => count.checked_mul(86_400)?,
        "week" => count.checked_mul(604_800)?,
        _ => return None,
    };
    if count <= 0 || seconds < 0 {
        return None;
    }
    Some(now.saturating_sub(seconds))
}

fn bound_day_timestamp(day_start: i64, bound: SearchTimeBound) -> i64 {
    match bound {
        SearchTimeBound::Start => day_start,
        SearchTimeBound::End => day_start + 86_399,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The start-of-day bound every assertion below defaults to; production
    /// callers always name the bound they want at the call site.
    fn parse_search_time_filter(value: &str, now: i64) -> Option<i64> {
        parse_search_time_filter_bound(value, now, SearchTimeBound::Start)
    }

    #[test]
    fn nearest_rank_uses_real_samples() {
        assert_eq!(nearest_rank(&[], 95), None);
        assert_eq!(nearest_rank(&[7], 95), Some(7));
        assert_eq!(nearest_rank(&[1, 2, 3, 4], 50), Some(2));
        assert_eq!(nearest_rank(&(1..=100).collect::<Vec<_>>(), 99), Some(99));
        assert_eq!(nearest_rank(&(1..=101).collect::<Vec<_>>(), 99), Some(100));
        assert_eq!(nearest_rank(&[1], 0), None);
        assert_eq!(nearest_rank(&[1], 101), None);
    }

    #[test]
    fn parses_utc_with_fractional_seconds() {
        assert_eq!(parse_rfc3339_timestamp("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_rfc3339_timestamp("2026-01-01T00:00:00.123456Z"),
            Some(1_767_225_600)
        );
    }

    #[test]
    fn parses_space_separator_and_lowercase_zone() {
        assert_eq!(parse_rfc3339_timestamp("1970-01-01 00:00:01z"), Some(1));
    }

    #[test]
    fn humanizes_unix_seconds_as_utc_calendar_time() {
        assert_eq!(humanize_unix_secs(0), "1970-01-01 00:00:00Z");
        assert_eq!(humanize_unix_secs(1_767_225_600), "2026-01-01 00:00:00Z");
        assert_eq!(humanize_unix_secs(1_767_225_661), "2026-01-01 00:01:01Z");
    }

    #[test]
    fn formats_epoch_boundaries_as_exact_utc_bytes() {
        assert_eq!(format_yyyy_mm_dd(-1), "1969-12-31");
        assert_eq!(humanize_unix_secs(-1), "1969-12-31 23:59:59Z");
    }

    #[test]
    fn applies_timezone_offsets() {
        assert_eq!(
            parse_rfc3339_timestamp("1970-01-01T02:00:00+02:00"),
            Some(0)
        );
        assert_eq!(
            parse_rfc3339_timestamp("1969-12-31T22:30:00-01:30"),
            Some(0)
        );
        assert_eq!(
            parse_rfc3339_timestamp("1970-01-01T23:59:00+23:59"),
            Some(0)
        );
        assert_eq!(
            parse_rfc3339_timestamp("1969-12-31T00:01:00-23:59"),
            Some(0)
        );
    }

    #[test]
    fn rejects_missing_or_malformed_timezone() {
        assert!(parse_rfc3339_timestamp("2026-01-01T00:00:00").is_none());
        assert!(parse_rfc3339_timestamp("2026-01-01T00:00:00+0200").is_none());
        assert!(parse_rfc3339_timestamp("2026-01-01T00:00:00Zjunk").is_none());
        assert!(parse_rfc3339_timestamp("2026-01-01T00:00:00.Z").is_none());
    }

    #[test]
    fn rejects_invalid_calendar_and_clock_fields() {
        assert!(parse_rfc3339_timestamp("2026-13-01T00:00:00Z").is_none());
        assert!(parse_rfc3339_timestamp("2026-02-29T00:00:00Z").is_none());
        assert_eq!(
            parse_rfc3339_timestamp("2024-02-29T00:00:00Z"),
            Some(1_709_164_800)
        );
        assert!(parse_rfc3339_timestamp("2026-01-00T00:00:00Z").is_none());
        assert!(parse_rfc3339_timestamp("2026-01-01T24:00:00Z").is_none());
        assert!(parse_rfc3339_timestamp("2026-01-01T00:60:00Z").is_none());
        assert!(parse_rfc3339_timestamp("2026-01-01T00:00:60Z").is_none());
    }

    #[test]
    fn rejects_pre_epoch_and_garbage() {
        assert!(parse_rfc3339_timestamp("1969-12-31T23:59:59Z").is_none());
        assert!(parse_rfc3339_timestamp("bad").is_none());
        assert!(parse_rfc3339_timestamp("").is_none());
    }

    #[test]
    fn parses_search_time_filters() {
        let now = 1_800_000_000;
        assert_eq!(parse_search_time_filter("123", now), Some(123));
        assert_eq!(
            parse_search_time_filter("1970-01-02T00:00:00Z", now),
            Some(86_400)
        );
        assert_eq!(parse_search_time_filter("1970-01-02", now), Some(86_400));
        assert_eq!(
            parse_search_time_filter_bound("1970-01-02", now, SearchTimeBound::End),
            Some(172_799)
        );
        assert_eq!(
            parse_search_time_filter("last hour", now),
            Some(now - 3_600)
        );
        assert_eq!(
            parse_search_time_filter("last 2 days", now),
            Some(now - 172_800)
        );
        assert_eq!(
            parse_search_time_filter("15 minutes ago", now),
            Some(now - 900)
        );
        assert_eq!(
            parse_search_time_filter("today", now),
            Some(now.div_euclid(86_400) * 86_400)
        );
        assert_eq!(
            parse_search_time_filter_bound("today", now, SearchTimeBound::End),
            Some(now.div_euclid(86_400) * 86_400 + 86_399)
        );
        assert!(parse_search_time_filter("last zero hours", now).is_none());
        assert!(parse_search_time_filter("tomorrow", now).is_none());
    }

    #[test]
    fn parses_cursor_human_timestamp() {
        // 2026-06-10 09:11 at UTC+2 == 2026-06-10T07:11:00Z.
        assert_eq!(
            parse_cursor_human_timestamp("Wednesday, Jun 10, 2026, 9:11 AM (UTC+2)"),
            parse_rfc3339_timestamp("2026-06-10T09:11:00+02:00"),
        );
        assert_eq!(
            parse_cursor_human_timestamp("Monday, Jun 8, 2026, 11:55 PM (UTC+2)"),
            parse_rfc3339_timestamp("2026-06-08T23:55:00+02:00"),
        );
    }

    #[test]
    fn cursor_human_timestamp_handles_midnight_noon_and_offsets() {
        assert_eq!(
            parse_cursor_human_timestamp("Thursday, Jan 1, 1970, 12:00 AM (UTC)"),
            Some(0)
        );
        assert_eq!(
            parse_cursor_human_timestamp("Thursday, Jan 1, 1970, 12:30 PM (UTC)"),
            Some(12 * 3_600 + 30 * 60)
        );
        assert_eq!(
            parse_cursor_human_timestamp("Friday, Jan 2, 1970, 5:30 AM (UTC+5:30)"),
            Some(86_400)
        );
        assert_eq!(
            parse_cursor_human_timestamp("Wednesday, Dec 31, 1969, 5:00 PM (UTC-7)"),
            Some(0)
        );
    }

    #[test]
    fn cursor_human_timestamp_tolerates_missing_weekday_and_24h_clock() {
        assert_eq!(
            parse_cursor_human_timestamp("Jun 10, 2026, 9:11 AM (UTC+2)"),
            parse_rfc3339_timestamp("2026-06-10T09:11:00+02:00"),
        );
        assert_eq!(
            parse_cursor_human_timestamp("Jun 10, 2026, 21:11 (UTC+2)"),
            parse_rfc3339_timestamp("2026-06-10T21:11:00+02:00"),
        );
    }

    #[test]
    fn formats_days_with_proleptic_gregorian_calendar() {
        assert_eq!(format_yyyy_mm_dd(20_588), "2026-05-15");
        assert_eq!(format_yyyy_mm_dd(-1), "1969-12-31");
    }

    #[test]
    fn cursor_human_timestamp_rejects_garbage() {
        assert!(parse_cursor_human_timestamp("").is_none());
        assert!(parse_cursor_human_timestamp("…").is_none());
        assert!(parse_cursor_human_timestamp("Jun 10, 2026").is_none());
        assert!(parse_cursor_human_timestamp("Foo 10, 2026, 9:11 AM (UTC+2)").is_none());
        assert!(parse_cursor_human_timestamp("Jun 32, 2026, 9:11 AM (UTC+2)").is_none());
        assert!(parse_cursor_human_timestamp("Jun 10, 2026, 13:11 PM (UTC+2)").is_none());
        assert!(parse_cursor_human_timestamp("Jun 10, 2026, 9:11 AM (GMT+2)").is_none());
    }
}
