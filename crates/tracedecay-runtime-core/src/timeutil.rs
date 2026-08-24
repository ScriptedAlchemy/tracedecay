//! Zero-dependency civil-date / RFC3339 timestamp parsing shared by the
//! accounting transcript parser and the MCP LCM session handlers.
//!
//! This is the stricter of the two parsers it consolidates: it requires an
//! explicit timezone (`Z` or `±HH:MM`), validates calendar ranges
//! (month/day/leap years) and rejects trailing garbage, while still
//! supporting fractional seconds (which are truncated).

use tracedecay_capture::parse_yyyy_mm_dd_utc_start;
pub use tracedecay_capture::{
    civil_from_days, parse_cursor_human_timestamp, parse_rfc3339_timestamp,
};

/// Parses search filter timestamps. Accepts Unix seconds, RFC3339, `YYYY-MM-DD`
/// UTC dates, `today`, `yesterday`, and relative forms like `last hour`.
pub fn parse_search_time_filter(value: &str, now: i64) -> Option<i64> {
    parse_search_time_filter_bound(value, now, SearchTimeBound::Start)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTimeBound {
    Start,
    End,
}

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

/// Formats "days since 1970-01-01 UTC" as `YYYY-MM-DD`.
pub fn format_yyyy_mm_dd(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Formats a Unix-seconds instant as a human-readable UTC
/// `YYYY-MM-DD HH:MM:SSZ` string. Used to render session activity windows
/// and commit times as calendar timestamps instead of raw epoch seconds.
pub fn humanize_unix_secs(secs: i64) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3_600, (rem / 60) % 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}Z")
}

/// The current UTC time as an ISO 8601 `yyyy-mm-ddThh:mm:ssZ` string.
pub fn now_iso_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3_600, (rem / 60) % 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn humanizes_unix_seconds_as_utc_calendar_time() {
        assert_eq!(humanize_unix_secs(0), "1970-01-01 00:00:00Z");
        assert_eq!(humanize_unix_secs(1_767_225_600), "2026-01-01 00:00:00Z");
        assert_eq!(humanize_unix_secs(1_767_225_661), "2026-01-01 00:01:01Z");
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
    fn formats_civil_days_as_yyyy_mm_dd() {
        assert_eq!(format_yyyy_mm_dd(20_588), "2026-05-15");
    }
}
