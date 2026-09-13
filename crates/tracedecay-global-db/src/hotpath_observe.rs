//! Opt-in global-db Hotpath gauges.
//!
//! Rusqlite-runtime owns measured writer, reader, and SQLite VM counters. These
//! helpers only record operation-family counts that are already known at the
//! call site; they never fabricate scan, sort, or transaction work.

#[inline(always)]
pub(crate) fn record_snapshot_admissions(count: u64) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("global_db.snapshot_admissions").inc(count);
    #[cfg(not(feature = "hotpath"))]
    let _ = count;
}

#[inline(always)]
pub(crate) fn record_transaction_rows(count: u64) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("global_db.transaction_rows").inc(count);
    #[cfg(not(feature = "hotpath"))]
    let _ = count;
}
