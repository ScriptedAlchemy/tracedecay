use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};

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
pub fn parse_cursor_human_timestamp(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    let (month_day, year, time_part) = match parts.as_slice() {
        [_, month_day, year, time] | [month_day, year, time] => (*month_day, *year, *time),
        _ => return None,
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
    let (hours, minutes) = magnitude.split_once(':').unwrap_or((magnitude, "0"));
    let hours = hours.parse::<i32>().ok()?;
    let minutes = minutes.parse::<i32>().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let seconds = hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?;
    FixedOffset::east_opt(sign.checked_mul(seconds)?)
}

#[must_use]
pub fn format_yyyy_mm_dd(days: i64) -> String {
    match days
        .checked_mul(86_400)
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
    {
        Some(timestamp) => timestamp.format("%Y-%m-%d").to_string(),
        None => format!("UTC day out of range: {days}"),
    }
}

#[must_use]
pub fn humanize_unix_secs(secs: i64) -> String {
    match DateTime::<Utc>::from_timestamp(secs, 0) {
        Some(timestamp) => timestamp.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        None => format!("UTC timestamp out of range: {secs}"),
    }
}

#[must_use]
pub fn now_iso_utc() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
