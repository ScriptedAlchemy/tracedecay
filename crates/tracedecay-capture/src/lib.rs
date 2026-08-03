//! Zero-dependency timestamp parsing primitives used by transcript capture.

/// Parses a timezone-aware RFC3339 timestamp into non-negative Unix epoch
/// seconds.
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

/// Parses a `YYYY-MM-DD` UTC date to its Unix day start.
pub fn parse_yyyy_mm_dd_utc_start(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year = parse_fixed_i32(value, 0, 4)?;
    let month = parse_fixed_u32(value, 5, 7)?;
    let day = parse_fixed_u32(value, 8, 10)?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let timestamp = days_from_civil(year, month, day).checked_mul(86_400)?;
    (timestamp >= 0).then_some(timestamp)
}

/// Parses the human-readable timestamp Cursor injects into user prompts.
pub fn parse_cursor_human_timestamp(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
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

pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

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
