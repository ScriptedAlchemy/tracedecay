//! Calendar date type stored as days since Unix epoch.
//!
//! Uses Hinnant's civil date algorithms (public domain, no external deps).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A calendar date, stored as days since Unix epoch (1970-01-01).
///
/// Range: roughly year -5,879,610 to +5,879,610.
///
/// # Examples
///
/// ```
/// use grafeo_common::types::Date;
///
/// let d = Date::from_ymd(2024, 3, 15).unwrap();
/// assert_eq!(d.year(), 2024);
/// assert_eq!(d.month(), 3);
/// assert_eq!(d.day(), 15);
/// assert_eq!(d.to_string(), "2024-03-15");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[repr(transparent)]
pub struct Date(i32);

impl Date {
    /// Creates a date from year, month (1-12), and day (1-31).
    ///
    /// Returns `None` if the components are out of range.
    #[must_use]
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        // Validate day for the given month
        let max_day = days_in_month(year, month);
        if day > max_day {
            return None;
        }
        Some(Self(days_from_civil(year, month, day)))
    }

    /// Creates a date from days since Unix epoch.
    #[inline]
    #[must_use]
    pub const fn from_days(days: i32) -> Self {
        Self(days)
    }

    /// Returns the number of days since Unix epoch.
    #[inline]
    #[must_use]
    pub const fn as_days(self) -> i32 {
        self.0
    }

    /// Returns the year component.
    #[must_use]
    pub fn year(self) -> i32 {
        civil_from_days(self.0).0
    }

    /// Returns the month component (1-12).
    #[must_use]
    pub fn month(self) -> u32 {
        civil_from_days(self.0).1
    }

    /// Returns the day component (1-31).
    #[must_use]
    pub fn day(self) -> u32 {
        civil_from_days(self.0).2
    }

    /// Returns (year, month, day) components.
    #[must_use]
    pub fn to_ymd(self) -> (i32, u32, u32) {
        civil_from_days(self.0)
    }

    /// Parses a date from ISO 8601 format "YYYY-MM-DD".
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        // Handle optional leading minus for negative years
        let (negative, s) = if let Some(rest) = s.strip_prefix('-') {
            (true, rest)
        } else {
            (false, s)
        };

        let parts: Vec<&str> = s.splitn(3, '-').collect();
        if parts.len() != 3 {
            return None;
        }
        let year: i32 = parts[0].parse().ok()?;
        let month: u32 = parts[1].parse().ok()?;
        let day: u32 = parts[2].parse().ok()?;
        let year = if negative { -year } else { year };
        Self::from_ymd(year, month, day)
    }

    /// Returns today's date (UTC).
    #[must_use]
    pub fn today() -> Self {
        let ts = super::Timestamp::now();
        ts.to_date()
    }

    /// Converts this date to a timestamp at midnight UTC.
    #[must_use]
    pub fn to_timestamp(self) -> super::Timestamp {
        super::Timestamp::from_micros(self.0 as i64 * 86_400_000_000)
    }

    /// Adds a duration to this date.
    ///
    /// Month components are added first (clamping day to month's max),
    /// then day components.
    #[must_use]
    pub fn add_duration(self, dur: &super::Duration) -> Self {
        let (mut y, mut m, mut d) = self.to_ymd();

        // Add months
        if dur.months() != 0 {
            let total_months = y as i64 * 12 + (m as i64 - 1) + dur.months();
            y = i32::try_from(total_months.div_euclid(12)).unwrap_or(if total_months < 0 {
                i32::MIN
            } else {
                i32::MAX
            });
            // reason: rem_euclid(12) + 1 is in [1, 12], always fits u32
            #[allow(clippy::cast_possible_truncation)]
            {
                m = (total_months.rem_euclid(12) + 1) as u32;
            }
            // Clamp day to max for new month
            let max_d = days_in_month(y, m);
            if d > max_d {
                d = max_d;
            }
        }

        // Add days
        let days = days_from_civil(y, m, d) as i64 + dur.days();
        Self(i32::try_from(days).unwrap_or(if days < 0 { i32::MIN } else { i32::MAX }))
    }

    /// Subtracts a duration from this date.
    #[must_use]
    pub fn sub_duration(self, dur: &super::Duration) -> Self {
        self.add_duration(&dur.neg())
    }

    /// Truncates this date to the given unit.
    ///
    /// - `"year"`: sets month and day to 1 (first day of year)
    /// - `"month"`: sets day to 1 (first day of month)
    /// - `"day"`: no-op (already at day precision)
    #[must_use]
    pub fn truncate(self, unit: &str) -> Option<Self> {
        let (y, m, _d) = self.to_ymd();
        match unit {
            "year" => Self::from_ymd(y, 1, 1),
            "month" => Self::from_ymd(y, m, 1),
            "day" => Some(self),
            _ => None,
        }
    }
}

impl fmt::Debug for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Date({})", self)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = civil_from_days(self.0);
        if y < 0 {
            write!(f, "-{:04}-{:02}-{:02}", -y, m, d)
        } else {
            write!(f, "{:04}-{:02}-{:02}", y, m, d)
        }
    }
}

// ---------------------------------------------------------------------------
// Hinnant's civil date algorithms (public domain)
// See: https://howardhinnant.github.io/date_algorithms.html
// ---------------------------------------------------------------------------

/// Converts (year, month, day) to days since Unix epoch (1970-01-01).
pub(crate) fn days_from_civil(year: i32, month: u32, day: u32) -> i32 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = y.div_euclid(400);
    // reason: rem_euclid(400) is in [0, 399], always fits u32
    #[allow(clippy::cast_possible_truncation)]
    let yoe = y.rem_euclid(400) as u32; // year of era [0, 399]
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1; // day of year [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era [0, 146096]
    let days = era * 146097 + doe as i64 - 719468;
    i32::try_from(days).unwrap_or(if days < 0 { i32::MIN } else { i32::MAX })
}

/// Converts days since Unix epoch to (year, month, day).
pub(crate) fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    // reason: rem_euclid(146097) is in [0, 146096], always fits u32
    #[allow(clippy::cast_possible_truncation)]
    let doe = z.rem_euclid(146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month pseudo [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    // reason: year range from i32 input days is bounded within i32
    #[allow(clippy::cast_possible_truncation)]
    let year = y as i32;
    (year, m, d)
}

/// Returns the number of days in a given month.
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Returns true if the year is a leap year.
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch() {
        let d = Date::from_ymd(1970, 1, 1).unwrap();
        assert_eq!(d.as_days(), 0);
        assert_eq!(d.year(), 1970);
        assert_eq!(d.month(), 1);
        assert_eq!(d.day(), 1);
    }

    #[test]
    fn test_known_dates() {
        // 2024-01-01 is 19723 days after epoch
        let d = Date::from_ymd(2024, 1, 1).unwrap();
        assert_eq!(d.as_days(), 19723);
        assert_eq!(d.to_string(), "2024-01-01");

        // 2000-03-01 (leap year)
        let d = Date::from_ymd(2000, 3, 1).unwrap();
        assert_eq!(d.year(), 2000);
        assert_eq!(d.month(), 3);
        assert_eq!(d.day(), 1);
    }

    #[test]
    fn test_roundtrip() {
        for days in [-100000, -1, 0, 1, 10000, 19723, 50000] {
            let d = Date::from_days(days);
            let (y, m, day) = d.to_ymd();
            let d2 = Date::from_ymd(y, m, day).unwrap();
            assert_eq!(d, d2, "roundtrip failed for days={days}");
        }
    }

    #[test]
    fn test_parse() {
        let d = Date::parse("2024-03-15").unwrap();
        assert_eq!(d.year(), 2024);
        assert_eq!(d.month(), 3);
        assert_eq!(d.day(), 15);

        assert!(Date::parse("not-a-date").is_none());
        assert!(Date::parse("2024-13-01").is_none()); // invalid month
        assert!(Date::parse("2024-02-30").is_none()); // invalid day
    }

    #[test]
    fn test_display() {
        assert_eq!(
            Date::from_ymd(2024, 1, 5).unwrap().to_string(),
            "2024-01-05"
        );
        assert_eq!(
            Date::from_ymd(100, 12, 31).unwrap().to_string(),
            "0100-12-31"
        );
    }

    #[test]
    fn test_ordering() {
        let d1 = Date::from_ymd(2024, 1, 1).unwrap();
        let d2 = Date::from_ymd(2024, 6, 15).unwrap();
        assert!(d1 < d2);
    }

    #[test]
    fn test_leap_year() {
        assert!(Date::from_ymd(2000, 2, 29).is_some()); // leap
        assert!(Date::from_ymd(1900, 2, 29).is_none()); // not leap
        assert!(Date::from_ymd(2024, 2, 29).is_some()); // leap
        assert!(Date::from_ymd(2023, 2, 29).is_none()); // not leap
    }

    #[test]
    fn test_to_timestamp() {
        let d = Date::from_ymd(1970, 1, 2).unwrap();
        assert_eq!(d.to_timestamp().as_micros(), 86_400_000_000);
    }

    #[test]
    fn test_truncate() {
        let d = Date::from_ymd(2024, 6, 15).unwrap();

        let year = d.truncate("year").unwrap();
        assert_eq!(year.to_string(), "2024-01-01");

        let month = d.truncate("month").unwrap();
        assert_eq!(month.to_string(), "2024-06-01");

        let day = d.truncate("day").unwrap();
        assert_eq!(day, d);

        assert!(d.truncate("hour").is_none());
    }

    #[test]
    fn test_negative_year() {
        let d = Date::parse("-0001-01-01").unwrap();
        assert_eq!(d.year(), -1);
        assert_eq!(d.to_string(), "-0001-01-01");
    }

    #[test]
    fn test_add_duration_months_clamps_day() {
        use crate::types::Duration;
        // Jan 31 + 1 month should clamp to Feb 28 (non-leap year)
        let d = Date::from_ymd(2025, 1, 31).unwrap();
        let dur = Duration::from_months(1);
        let result = d.add_duration(&dur);
        assert_eq!(result.to_string(), "2025-02-28");
    }

    #[test]
    fn test_add_duration_months_clamps_leap_year() {
        use crate::types::Duration;
        // Jan 31 + 1 month in a leap year should clamp to Feb 29
        let d = Date::from_ymd(2024, 1, 31).unwrap();
        let dur = Duration::from_months(1);
        let result = d.add_duration(&dur);
        assert_eq!(result.to_string(), "2024-02-29");
    }

    #[test]
    fn test_add_duration_days() {
        use crate::types::Duration;
        let d = Date::from_ymd(2025, 3, 1).unwrap();
        let dur = Duration::from_days(10);
        let result = d.add_duration(&dur);
        assert_eq!(result.to_string(), "2025-03-11");
    }

    #[test]
    fn test_sub_duration() {
        use crate::types::Duration;
        let d = Date::from_ymd(2025, 3, 15).unwrap();
        let dur = Duration::from_months(2);
        let result = d.sub_duration(&dur);
        assert_eq!(result.to_string(), "2025-01-15");
    }
}
