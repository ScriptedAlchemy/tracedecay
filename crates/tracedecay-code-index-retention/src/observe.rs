//! Metrics gauges for code-index generation retention.
//!
//! Gauge keys stay the historical `usecases.retention.*` labels so dashboards
//! and comparisons remain continuous across the crate extraction. Never pass
//! model inputs, paths, or generation identifiers as labels. Every macro
//! is emitted through the `metrics` facade and drops with no recorder.

#[inline]
pub(crate) fn retention_plan(candidates: usize, bytes_planned: u64) {}

#[inline]
pub(crate) fn retention_inspected(bytes: u64) {}

#[inline]
pub(crate) fn retention_hashed(bytes: u64) {}

#[inline]
pub(crate) fn retention_quarantined(bytes: u64) {}

#[inline]
pub(crate) fn retention_reclaimed(bytes: u64) {}

#[inline]
pub(crate) fn retention_cancelled() {}

#[inline]
pub(crate) fn retention_recovery_pending() {}

#[inline]
pub(crate) fn retention_recovery_running() {}

#[inline]
pub(crate) fn retention_recovery_idle() {}

/// One non-blocking probe while collection waits for the graph-replay pool.
#[inline]
pub(crate) fn retention_replay_pool_acquire_wait() {}

/// Exclusive graph-replay pool lock taken by collection or recovery.
#[inline]
pub(crate) fn retention_replay_pool_acquired() {}

/// Collection deferred because the graph-replay pool stayed held through the
/// carried acquire budget.
#[inline]
pub(crate) fn retention_replay_pool_busy() {}

/// Collection abandoned the pool wait because the caller cancelled.
#[inline]
pub(crate) fn retention_replay_pool_acquire_cancelled() {}

/// Exclusive graph-replay pool lock released by collection or recovery.
#[inline]
pub(crate) fn retention_replay_pool_released() {}

/// Durable graph-replay release events written for retired generations.
#[inline]
pub(crate) fn retention_replay_releases_queued(count: usize) {}

/// Release events still awaiting graph-reconciler consumption after one
/// bounded queue page scan.
#[inline]
pub(crate) fn retention_replay_releases_pending(count: usize) {}

/// One release event consumed by the graph reconciler.
#[inline]
pub(crate) fn retention_replay_release_completed() {}

/// Stranded scope roots moved into the retention quarantine stage.
#[inline]
pub(crate) fn retention_scopes_quarantined(count: usize) {}

/// Quarantined scope roots restored by a reconciliation rollback.
#[inline]
pub(crate) fn retention_scopes_restored(count: usize) {}

/// Quarantined scope roots unlinked after a durable deletion receipt.
#[inline]
pub(crate) fn retention_scopes_deleted(count: usize) {}
