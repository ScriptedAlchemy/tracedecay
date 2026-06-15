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

/// Parses the human-readable timestamp Cursor injects into user prompts as
/// `<timestamp>…</timestamp>` (e.g. `Wednesday, Jun 10, 2026, 9:11 AM (UTC+2)`)
/// into Unix epoch seconds.
///
/// Cursor transcript JSONL carries no structured per-message timestamps, so
/// this tag is the only per-message time signal available to ingest. The
/// parser is tolerant: the weekday is optional, the clock accepts 12-hour
/// (`AM`/`PM`) or 24-hour form, and the offset accepts `(UTC)`, `(UTC±H)`,
/// and `(UTC±H:MM)`.
pub fn parse_cursor_human_timestamp(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    // [weekday,] "Jun 10", "2026", "9:11 AM (UTC+2)"
    let (month_day, year_part, time_part) = match parts.as_slice() {
        [_, month_day, year, time] | [month_day, year, time] => (*month_day, *year, *time),
        _ => return None,
    };

    let mut md = month_day.split_whitespace();
    let month = month_number(md.next()?)?;
    let day: u32 = md.next()?.parse().ok()?;
    if md.next().is_some() {
        return None;
    }
    let year: i32 = year_part.parse().ok()?;
    if day == 0 || day > days_in_month(year, month) {
        return None;
    }

    let mut clock = time_part.split_whitespace();
    let hour_minute = clock.next()?;
    let (hour_text, minute_text) = hour_minute.split_once(':')?;
    let mut hour: u32 = hour_text.parse().ok()?;
    let minute: u32 = minute_text.parse().ok()?;
    let mut rest = clock.next();
    match rest.map(str::to_ascii_uppercase).as_deref() {
        Some("AM") => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour %= 12;
            rest = clock.next();
        }
        Some("PM") => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + 12;
            rest = clock.next();
        }
        _ => {}
    }
    if hour > 23 || minute > 59 {
        return None;
    }
    let offset_seconds = match rest {
        Some(zone) => parse_utc_offset(zone)?,
        None => 0,
    };
    if clock.next().is_some() {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let local_seconds = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60;
    let timestamp = local_seconds - offset_seconds;
    (timestamp >= 0).then_some(timestamp)
}

fn month_number(name: &str) -> Option<u32> {
    let abbrev = name.get(..3)?.to_ascii_lowercase();
    Some(match abbrev.as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

/// Parses `(UTC)`, `(UTC+2)`, `(UTC-7)`, or `(UTC+5:30)` into offset seconds.
fn parse_utc_offset(zone: &str) -> Option<i64> {
    let inner = zone.strip_prefix("(UTC")?.strip_suffix(')')?;
    if inner.is_empty() {
        return Some(0);
    }
    let (sign, magnitude) = match inner.as_bytes().first()? {
        b'+' => (1, &inner[1..]),
        b'-' => (-1, &inner[1..]),
        _ => return None,
    };
    let (hours_text, minutes_text) = magnitude.split_once(':').unwrap_or((magnitude, "0"));
    let hours: i64 = hours_text.parse().ok()?;
    let minutes: i64 = minutes_text.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
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

/// Howard Hinnant's `civil_from_days`, the inverse of [`days_from_civil`]:
/// a civil `(year, month, day)` for a count of days since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Formats "days since 1970-01-01 UTC" as `YYYY-MM-DD`.
pub fn format_yyyy_mm_dd(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
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
    fn civil_from_days_round_trips_days_from_civil() {
        for days in [0, 1, 59, 60, 20_588, 365 * 100, -1, -365] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(
                days_from_civil(y as i32, m, d),
                days,
                "round trip failed for {days} ({y:04}-{m:02}-{d:02})"
            );
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(format_yyyy_mm_dd(20_588), "2026-05-15");
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
