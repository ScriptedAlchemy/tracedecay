//! Crate-local helpers that previously lived as `pub(crate)` global-db internals.

/// Millisecond-scale Unix timestamps are at least 13 digits.
pub(crate) const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

#[inline(always)]
pub(crate) fn record_snapshot_admissions(count: u64) {}

#[inline(always)]
pub(crate) fn record_output_sessions(count: u64) {}

/// Payload bytes whose integrity proof passed, whether or not they were later
/// emitted. Compared with [`record_hydration_emitted_bytes`], the gap is proof
/// work discarded by a revocation, refusal, or interruption before emission.
#[inline(always)]
pub(crate) fn record_hydration_verified_bytes(count: usize) {}

/// Payload bytes handed to a hydration sink, charged per chunk so an
/// interrupted emission still reports the partial work it did.
#[inline(always)]
pub(crate) fn record_hydration_emitted_bytes(count: usize) {}
