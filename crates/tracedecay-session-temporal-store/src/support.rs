//! Crate-local helpers that previously lived as `pub(crate)` global-db internals.

/// Millisecond-scale Unix timestamps are at least 13 digits.
pub(crate) const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;
