//! Global-db operation-family counters and Hotpath gauges.
//!
//! Named `hotpath_observe` so this crate-root module cannot shadow the
//! external `hotpath` profiler crate. Call `hotpath::measure`,
//! `hotpath::measure_block!`, `hotpath::gauge!`, and `hotpath::val!` at
//! operation sites. This module only holds static family names and the
//! process-wide counters tests use to prove scan/admission behavior.
//!
//! Rusqlite-runtime owns writer-queue and reader-pool spans. Labels are
//! static family names. SQL text and user payloads are never logged.
//! Profiling stays opt-in; these atomics remain available with the feature
//! off.

use std::sync::atomic::{AtomicU64, Ordering};

/// Whether pending discovery may admit a projection snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTemporalDiscoveryWake {
    /// Caller asked to discover sessions that need temporal projection.
    ExplicitProjectionRequest,
    /// History-only ingest made no projection progress. Must not admit a
    /// snapshot or scan observation effects.
    HistoryOnlyRetry,
}

impl SessionTemporalDiscoveryWake {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitProjectionRequest => "explicit_projection_request",
            Self::HistoryOnlyRetry => "history_only_retry",
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalDbOpCounters {
    pub snapshot_admissions: u64,
    pub rows_visited: u64,
    pub full_scan_work: u64,
    pub sort_work: u64,
    pub output_sessions: u64,
    pub transaction_rows: u64,
    pub transaction_bytes: u64,
}

struct CounterBank {
    snapshot_admissions: AtomicU64,
    rows_visited: AtomicU64,
    full_scan_work: AtomicU64,
    sort_work: AtomicU64,
    output_sessions: AtomicU64,
    transaction_rows: AtomicU64,
    transaction_bytes: AtomicU64,
}

impl CounterBank {
    const fn new() -> Self {
        Self {
            snapshot_admissions: AtomicU64::new(0),
            rows_visited: AtomicU64::new(0),
            full_scan_work: AtomicU64::new(0),
            sort_work: AtomicU64::new(0),
            output_sessions: AtomicU64::new(0),
            transaction_rows: AtomicU64::new(0),
            transaction_bytes: AtomicU64::new(0),
        }
    }
}

static COUNTERS: CounterBank = CounterBank::new();

#[inline]
fn add(target: &AtomicU64, delta: u64) {
    if delta != 0 {
        target.fetch_add(delta, Ordering::Relaxed);
    }
}

pub fn record_snapshot_admissions(count: u64) {
    add(&COUNTERS.snapshot_admissions, count);
    hotpath::gauge!("global_db.snapshot_admissions").inc(count);
}

pub fn record_rows_visited(count: u64) {
    add(&COUNTERS.rows_visited, count);
    hotpath::gauge!("global_db.rows_visited").inc(count);
}

pub fn record_full_scan_work(count: u64) {
    add(&COUNTERS.full_scan_work, count);
    hotpath::gauge!("global_db.full_scan_work").inc(count);
}

pub fn record_sort_work(count: u64) {
    add(&COUNTERS.sort_work, count);
    hotpath::gauge!("global_db.sort_work").inc(count);
}

pub fn record_output_sessions(count: u64) {
    add(&COUNTERS.output_sessions, count);
    hotpath::gauge!("global_db.output_sessions").inc(count);
}

pub fn record_transaction_rows(count: u64) {
    add(&COUNTERS.transaction_rows, count);
    hotpath::gauge!("global_db.transaction_rows").inc(count);
}

pub fn record_transaction_bytes(count: u64) {
    add(&COUNTERS.transaction_bytes, count);
    hotpath::gauge!("global_db.transaction_bytes").inc(count);
}

pub fn record_discovery_wake(wake: SessionTemporalDiscoveryWake) {
    hotpath::val!("session_temporal.discovery_wake").set(&wake.as_str());
}

pub fn snapshot_counters() -> GlobalDbOpCounters {
    GlobalDbOpCounters {
        snapshot_admissions: COUNTERS.snapshot_admissions.load(Ordering::Relaxed),
        rows_visited: COUNTERS.rows_visited.load(Ordering::Relaxed),
        full_scan_work: COUNTERS.full_scan_work.load(Ordering::Relaxed),
        sort_work: COUNTERS.sort_work.load(Ordering::Relaxed),
        output_sessions: COUNTERS.output_sessions.load(Ordering::Relaxed),
        transaction_rows: COUNTERS.transaction_rows.load(Ordering::Relaxed),
        transaction_bytes: COUNTERS.transaction_bytes.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
static TEST_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes counter assertions across parallel tests. The bank is process-wide.
#[cfg(test)]
pub fn lock_counters_for_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_COUNTER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub fn reset_counters_for_test() {
    COUNTERS.snapshot_admissions.store(0, Ordering::Relaxed);
    COUNTERS.rows_visited.store(0, Ordering::Relaxed);
    COUNTERS.full_scan_work.store(0, Ordering::Relaxed);
    COUNTERS.sort_work.store(0, Ordering::Relaxed);
    COUNTERS.output_sessions.store(0, Ordering::Relaxed);
    COUNTERS.transaction_rows.store(0, Ordering::Relaxed);
    COUNTERS.transaction_bytes.store(0, Ordering::Relaxed);
}
