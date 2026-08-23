//! Opt-in hotpath gauges for usecases retention, scheduling, and queries.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Every macro expands to a no-op unless
//! this crate's `hotpath` feature is selected.

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

#[inline]
pub(crate) fn semantic_queue(
    queued_batches: usize,
    queued_bytes: u64,
    reserved_session_memory_bytes: u64,
    active_publications: usize,
) {
    hotpath::gauge!("usecases.semantic.queued_batches").set(queued_batches as f64);
    hotpath::gauge!("usecases.semantic.queued_bytes").set(queued_bytes as f64);
    hotpath::gauge!("usecases.semantic.reserved_session_memory_bytes")
        .set(reserved_session_memory_bytes as f64);
    hotpath::gauge!("usecases.semantic.active_publications").set(active_publications as f64);
}

#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn semantic_queue_wait_ns(wait_ns: u64) {
    hotpath::gauge!("usecases.semantic.queue_wait_ns").set(wait_ns as f64);
}

#[inline]
pub(crate) fn semantic_cancelled(checkpoints: usize) {
    if checkpoints == 0 {
        return;
    }
    hotpath::gauge!("usecases.semantic.cancellation_checkpoints").inc(checkpoints as f64);
}

#[inline]
pub(crate) fn semantic_candidate_chunks(chunks: usize) {
    hotpath::gauge!("usecases.semantic.candidate_chunks").set(chunks as f64);
}

#[inline]
pub(crate) fn semantic_model_resident_bytes(bytes: u64) {
    hotpath::gauge!("usecases.semantic.model_resident_bytes").set(bytes as f64);
}

#[inline]
pub(crate) fn vector_candidates(candidates: usize) {
    hotpath::gauge!("usecases.vector.candidates").set(candidates as f64);
}

#[inline]
pub(crate) fn vector_resident_reservation(retained_bytes: u64, hydration_peak_bytes: u64) {
    hotpath::gauge!("usecases.vector.resident_retained_bytes").set(retained_bytes as f64);
    hotpath::gauge!("usecases.vector.resident_hydration_peak_bytes")
        .set(hydration_peak_bytes as f64);
}

#[inline]
pub(crate) fn vector_cancelled() {
    hotpath::gauge!("usecases.vector.cancellation_checkpoints").inc(1.0);
}

#[inline]
pub(crate) fn diagnostics_query(records: usize, total: usize) {
    hotpath::gauge!("usecases.diagnostics.records").set(records as f64);
    hotpath::gauge!("usecases.diagnostics.total").set(total as f64);
}

#[inline]
pub(crate) fn feedback_query(findings: usize) {
    hotpath::gauge!("usecases.feedback.findings").set(findings as f64);
}
