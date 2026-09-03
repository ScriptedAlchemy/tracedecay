//! Opt-in hotpath gauges for code-index generation retention.
//!
//! Gauge keys stay the historical `usecases.retention.*` labels so dashboards
//! and comparisons remain continuous across the crate extraction. Never pass
//! model inputs, paths, or generation identifiers as labels. Every macro
//! expands to a no-op unless this crate's `hotpath` feature is selected.

#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn retention_plan(candidates: usize, bytes_planned: u64) {
    hotpath::gauge!("usecases.retention.candidates_planned").set(candidates as f64);
    hotpath::gauge!("usecases.retention.bytes_planned").set(bytes_planned as f64);
}

#[inline]
pub(crate) fn retention_inspected(bytes: u64) {
    hotpath::gauge!("usecases.retention.bytes_inspected").inc(bytes as f64);
}

#[inline]
pub(crate) fn retention_hashed(bytes: u64) {
    hotpath::gauge!("usecases.retention.bytes_hashed").inc(bytes as f64);
}

#[inline]
pub(crate) fn retention_quarantined(bytes: u64) {
    hotpath::gauge!("usecases.retention.bytes_quarantined").set(bytes as f64);
}

#[inline]
pub(crate) fn retention_reclaimed(bytes: u64) {
    hotpath::gauge!("usecases.retention.bytes_reclaimed").set(bytes as f64);
}

#[inline]
pub(crate) fn retention_cancelled() {
    hotpath::gauge!("usecases.retention.cancellation_checkpoints").inc(1.0);
    hotpath::gauge!("usecases.retention.cancellation_state").set(1.0);
}

#[inline]
pub(crate) fn retention_recovery_pending() {
    hotpath::gauge!("usecases.retention.recovery_state").set(1.0);
}

#[inline]
pub(crate) fn retention_recovery_running() {
    hotpath::gauge!("usecases.retention.recovery_state").set(2.0);
}

#[inline]
pub(crate) fn retention_recovery_idle() {
    hotpath::gauge!("usecases.retention.recovery_state").set(0.0);
    hotpath::gauge!("usecases.retention.cancellation_state").set(0.0);
}

/// One non-blocking probe while collection waits for the graph-replay pool.
#[inline]
pub(crate) fn retention_replay_pool_acquire_wait() {
    hotpath::gauge!("usecases.retention.replay_pool_acquire_wait").inc(1.0);
}

/// Exclusive graph-replay pool lock taken by collection or recovery.
#[inline]
pub(crate) fn retention_replay_pool_acquired() {
    hotpath::gauge!("usecases.retention.replay_pool_acquired").inc(1.0);
}

/// Collection deferred because the graph-replay pool stayed held through the
/// carried acquire budget.
#[inline]
pub(crate) fn retention_replay_pool_busy() {
    hotpath::gauge!("usecases.retention.replay_pool_busy").inc(1.0);
}

/// Collection abandoned the pool wait because the caller cancelled.
#[inline]
pub(crate) fn retention_replay_pool_acquire_cancelled() {
    hotpath::gauge!("usecases.retention.replay_pool_acquire_cancelled").inc(1.0);
}

/// Exclusive graph-replay pool lock released by collection or recovery.
#[inline]
pub(crate) fn retention_replay_pool_released() {
    hotpath::gauge!("usecases.retention.replay_pool_released").inc(1.0);
}

/// Durable graph-replay release events written for retired generations.
#[inline]
pub(crate) fn retention_replay_releases_queued(count: usize) {
    hotpath::gauge!("usecases.retention.replay_releases_queued").inc(count as f64);
}

/// Release events still awaiting graph-reconciler consumption after one
/// bounded queue page scan.
#[inline]
pub(crate) fn retention_replay_releases_pending(count: usize) {
    hotpath::gauge!("usecases.retention.replay_releases_pending").set(count as f64);
}

/// One release event consumed by the graph reconciler.
#[inline]
pub(crate) fn retention_replay_release_completed() {
    hotpath::gauge!("usecases.retention.replay_releases_completed").inc(1.0);
}

/// Stranded scope roots moved into the retention quarantine stage.
#[inline]
pub(crate) fn retention_scopes_quarantined(count: usize) {
    hotpath::gauge!("usecases.retention.scopes_quarantined").inc(count as f64);
}

/// Quarantined scope roots restored by a reconciliation rollback.
#[inline]
pub(crate) fn retention_scopes_restored(count: usize) {
    hotpath::gauge!("usecases.retention.scopes_restored").inc(count as f64);
}

/// Quarantined scope roots unlinked after a durable deletion receipt.
#[inline]
pub(crate) fn retention_scopes_deleted(count: usize) {
    hotpath::gauge!("usecases.retention.scopes_deleted").inc(count as f64);
}
