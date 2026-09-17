use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::Value;

/// Millisecond/second boundary for numeric provider timestamps: values at or
/// above this are unix milliseconds, values below are already unix seconds.
const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

/// Normalizes a numeric provider timestamp that may be unix seconds or unix
/// milliseconds into unix seconds.
pub const fn normalize_timestamp_secs(ts: i64) -> i64 {
    if ts >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
        ts / 1000
    } else {
        ts
    }
}

/// Extracts a unix-seconds timestamp from a JSON number or numeric string,
/// normalizing millisecond-scale values (see [`normalize_timestamp_secs`]).
pub fn timestamp_secs(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .map(normalize_timestamp_secs)
}

/// Parses RFC3339 timestamps into Unix seconds, rejecting pre-epoch values.
///
/// Chrono deliberately accepts RFC3339's space separator and mixed-case
/// literals, matching the provider timestamp forms capture accepts.
pub fn parse_rfc3339_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.get(17) == Some(&b'6') && bytes.get(18) == Some(&b'0') {
        return None;
    }
    let timestamp = DateTime::parse_from_rfc3339(value).ok()?.timestamp();
    (timestamp >= 0).then_some(timestamp)
}

/// Parses Cursor's human-readable timestamp format into Unix seconds.
///
/// The native grammar is `[Weekday, ]Mon D, YYYY, H:MM[ AM|PM][ (UTC[±H[H][:MM]])]`:
/// three comma-separated fields, or four with the leading weekday.
pub fn parse_cursor_human_timestamp(value: &str) -> Option<i64> {
    let mut fields = value.split(',').map(str::trim);
    let first = fields.next()?;
    let second = fields.next()?;
    let third = fields.next()?;
    let (month_day, year, time_part) = match fields.next() {
        None => (first, second, third),
        Some(fourth) => {
            if fields.next().is_some() {
                return None;
            }
            (second, third, fourth)
        }
    };

    let mut time_parts = time_part.split_whitespace();
    let clock = time_parts.next()?;
    let marker_or_zone = time_parts.next();
    let (date_time, format, zone) = match marker_or_zone {
        Some(marker) if marker.eq_ignore_ascii_case("AM") || marker.eq_ignore_ascii_case("PM") => {
            let marker = marker.to_ascii_uppercase();
            (
                format!("{month_day}, {year}, {clock} {marker}"),
                "%b %-d, %Y, %-I:%M %p",
                time_parts.next(),
            )
        }
        Some(zone) => (
            format!("{month_day}, {year}, {clock}"),
            "%b %-d, %Y, %-H:%M",
            Some(zone),
        ),
        None => (
            format!("{month_day}, {year}, {clock}"),
            "%b %-d, %Y, %-H:%M",
            None,
        ),
    };
    if time_parts.next().is_some() {
        return None;
    }

    let local = NaiveDateTime::parse_from_str(&date_time, format).ok()?;
    let offset = zone.map_or_else(|| FixedOffset::east_opt(0), parse_cursor_utc_offset)?;
    let timestamp = offset.from_local_datetime(&local).single()?.timestamp();
    (timestamp >= 0).then_some(timestamp)
}

/// Parses a `YYYY-MM-DD` value as the start of that UTC day.
pub fn parse_yyyy_mm_dd_utc_start(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let timestamp = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp();
    (timestamp >= 0).then_some(timestamp)
}

/// Parses Cursor's `(UTC)` / `(UTC±H[H][:MM])` zone suffix.
///
/// Exactly one sign is permitted, and it belongs to the whole offset: the
/// hour and minute components are unsigned digit strings, so `(UTC+-1)`,
/// `(UTC--1)`, and `(UTC+1:-30)` are malformed evidence rather than
/// alternative spellings of some other offset.
fn parse_cursor_utc_offset(zone: &str) -> Option<FixedOffset> {
    let inner = zone.strip_prefix("(UTC")?.strip_suffix(')')?;
    if inner.is_empty() {
        return FixedOffset::east_opt(0);
    }
    let (sign, magnitude) = match inner.as_bytes().first()? {
        b'+' => (1_i32, &inner[1..]),
        b'-' => (-1_i32, &inner[1..]),
        _ => return None,
    };
    let (hours, minutes) = match magnitude.split_once(':') {
        Some((hours, minutes)) => (
            parse_offset_component(hours, 1..=2, 23)?,
            parse_offset_component(minutes, 2..=2, 59)?,
        ),
        None => (parse_offset_component(magnitude, 1..=2, 23)?, 0),
    };
    // Bounded by the component ranges: at most 23 h 59 min.
    FixedOffset::east_opt(sign * (hours * 3_600 + minutes * 60))
}

/// An unsigned, digit-only offset component whose digit count lies in
/// `width` and whose value is at most `max_value`.
fn parse_offset_component(
    text: &str,
    width: std::ops::RangeInclusive<usize>,
    max_value: i32,
) -> Option<i32> {
    if !width.contains(&text.len()) || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<i32>().ok()?;
    (value <= max_value).then_some(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_cursor_human_timestamp, parse_cursor_utc_offset};

    const NOON_UTC: &str = "Jun 10, 2026, 12:00 PM";
    const NOON_UTC_UNIX: i64 = 1_781_092_800;

    fn with_zone(zone: &str) -> Option<i64> {
        parse_cursor_human_timestamp(&format!("{NOON_UTC} {zone}"))
    }

    #[test]
    fn valid_offsets_shift_local_noon_by_their_magnitude() {
        let table = [
            ("(UTC)", 0),
            ("(UTC+0)", 0),
            ("(UTC-0)", 0),
            ("(UTC+2)", 2 * 3_600),
            ("(UTC+02)", 2 * 3_600),
            ("(UTC-7)", -7 * 3_600),
            ("(UTC+5:30)", 5 * 3_600 + 30 * 60),
            ("(UTC-3:30)", -(3 * 3_600 + 30 * 60)),
            ("(UTC+12:45)", 12 * 3_600 + 45 * 60),
            ("(UTC+23:59)", 23 * 3_600 + 59 * 60),
            ("(UTC-23:59)", -(23 * 3_600 + 59 * 60)),
        ];
        for (zone, offset_seconds) in table {
            assert_eq!(
                with_zone(zone),
                Some(NOON_UTC_UNIX - offset_seconds),
                "{zone}"
            );
        }
    }

    #[test]
    fn malformed_offsets_are_rejected_instead_of_repaired() {
        let table = [
            "(UTC+-1)",
            "(UTC--1)",
            "(UTC-+1)",
            "(UTC++1)",
            "(UTC+1:-30)",
            "(UTC+1:+30)",
            "(UTC+-1:30)",
            "(UTC+)",
            "(UTC-)",
            "(UTC+:30)",
            "(UTC+1:)",
            "(UTC+1:3)",
            "(UTC+1:300)",
            "(UTC+123)",
            "(UTC+24)",
            "(UTC+1:60)",
            "(UTC+1a)",
            "(UTC+ 1)",
            "(UTC+1:30:00)",
            "(UTC1)",
            "(UTC+1",
            "UTC+1)",
            "(GMT+1)",
        ];
        for zone in table {
            assert_eq!(parse_cursor_utc_offset(zone), None, "{zone}");
            assert_eq!(with_zone(zone), None, "{zone}");
        }
    }

    #[test]
    fn weekday_prefix_is_optional_and_extra_fields_are_rejected() {
        assert_eq!(
            parse_cursor_human_timestamp("Wednesday, Jun 10, 2026, 12:00 PM (UTC)"),
            Some(NOON_UTC_UNIX)
        );
        assert_eq!(parse_cursor_human_timestamp(NOON_UTC), Some(NOON_UTC_UNIX));
        assert_eq!(
            parse_cursor_human_timestamp("Jun 10, 2026, 21:11 (UTC+2)"),
            Some(1_781_118_660)
        );
        assert_eq!(parse_cursor_human_timestamp("Jun 10, 2026"), None);
        assert_eq!(
            parse_cursor_human_timestamp("Wednesday, Jun 10, 2026, 12:00 PM, (UTC)"),
            None
        );
        assert_eq!(
            parse_cursor_human_timestamp("Wednesday, Dec 31, 1969, 5:00 PM (UTC+7)"),
            None,
            "pre-epoch instants stay rejected"
        );
    }

    /// A native Cursor record carries its time only inside the transcript
    /// text; a malformed zone must leave the record without a timestamp
    /// rather than dating it with a repaired offset.
    #[test]
    fn native_cursor_record_timestamp_follows_the_zone_grammar() {
        let record = |zone: &str| {
            json!({
                "type": "user",
                "message": {
                    "content": format!("<timestamp>{NOON_UTC} {zone}</timestamp>hello")
                }
            })
        };
        assert_eq!(
            crate::cursor::timestamp_tag_from_record(&record("(UTC+2)")),
            Some(NOON_UTC_UNIX - 2 * 3_600)
        );
        assert_eq!(
            crate::cursor::timestamp_tag_from_record(&record("(UTC+-2)")),
            None
        );
    }
}
