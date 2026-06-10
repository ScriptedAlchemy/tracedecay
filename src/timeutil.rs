//! Zero-dependency civil-date / RFC3339 timestamp parsing shared by the
//! accounting transcript parser and the MCP LCM session handlers.
//!
//! This is the stricter of the two parsers it consolidates: it requires an
//! explicit timezone (`Z` or `±HH:MM`), validates calendar ranges
//! (month/day/leap years) and rejects trailing garbage, while still
//! supporting fractional seconds (which are truncated).

/// Parses a timezone-aware RFC3339 timestamp (e.g. `2026-06-10T01:02:03Z`,
/// `2026-06-10 01:02:03.123+02:00`) into non-negative Unix epoch seconds.
///
/// Returns `None` for missing/invalid timezone suffixes, out-of-range
/// calendar or clock fields, or timestamps before the epoch.
pub fn parse_rfc3339_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }

    let year = parse_fixed_i32(value, 0, 4)?;
    let month = parse_fixed_u32(value, 5, 7)?;
    let day = parse_fixed_u32(value, 8, 10)?;
    let hour = parse_fixed_u32(value, 11, 13)?;
    let minute = parse_fixed_u32(value, 14, 16)?;
    let second = parse_fixed_u32(value, 17, 19)?;
    if !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }

    let mut zone_pos = 19;
    if bytes.get(zone_pos) == Some(&b'.') {
        zone_pos += 1;
        let fraction_start = zone_pos;
        while matches!(bytes.get(zone_pos), Some(b'0'..=b'9')) {
            zone_pos += 1;
        }
        if zone_pos == fraction_start {
            return None;
        }
    }

    let offset_seconds = match bytes.get(zone_pos)? {
        b'Z' | b'z' => {
            if zone_pos + 1 != bytes.len() {
                return None;
            }
            0
        }
        b'+' | b'-' => {
            if zone_pos + 6 != bytes.len() || bytes.get(zone_pos + 3) != Some(&b':') {
                return None;
            }
            let offset_hours = parse_fixed_i32(value, zone_pos + 1, zone_pos + 3)?;
            let offset_minutes = parse_fixed_i32(value, zone_pos + 4, zone_pos + 6)?;
            if offset_hours > 23 || offset_minutes > 59 {
                return None;
            }
            let offset = offset_hours * 3600 + offset_minutes * 60;
            if bytes[zone_pos] == b'+' {
                offset
            } else {
                -offset
            }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let local_seconds =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    let timestamp = local_seconds - i64::from(offset_seconds);
    (timestamp >= 0).then_some(timestamp)
}

fn parse_fixed_i32(value: &str, start: usize, end: usize) -> Option<i32> {
    value.get(start..end)?.parse().ok()
}

fn parse_fixed_u32(value: &str, start: usize, end: usize) -> Option<u32> {
    value.get(start..end)?.parse().ok()
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01 for a civil date.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
    fn applies_timezone_offsets() {
        assert_eq!(
            parse_rfc3339_timestamp("1970-01-01T02:00:00+02:00"),
            Some(0)
        );
        assert_eq!(
            parse_rfc3339_timestamp("1969-12-31T22:30:00-01:30"),
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
    }

    #[test]
    fn rejects_pre_epoch_and_garbage() {
        assert!(parse_rfc3339_timestamp("1969-12-31T23:59:59Z").is_none());
        assert!(parse_rfc3339_timestamp("bad").is_none());
        assert!(parse_rfc3339_timestamp("").is_none());
    }
}
