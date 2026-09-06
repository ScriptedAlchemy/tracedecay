//! Opt-in hotpath gauges for usecases scheduling, queries, and admission.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Every macro expands to a no-op unless
//! this crate's `hotpath` feature is selected.

/// Count one failed semantic activation/coordination outcome by its typed
/// class. Success is visible through the surrounding phase spans; failures
/// would otherwise vanish into mapped error returns.
#[inline]
pub(crate) fn semantic_coordination_error(
    error: &crate::semantic_runtime::SemanticActivationCoordinationErrorV1,
) {
    use crate::semantic_runtime::SemanticActivationCoordinationErrorV1;
    match error {
        SemanticActivationCoordinationErrorV1::Unavailable => {
            hotpath::gauge!("usecases.semantic.coordination.unavailable").inc(1.0);
        }
        SemanticActivationCoordinationErrorV1::Rejected
        | SemanticActivationCoordinationErrorV1::RejectedDetail(_) => {
            hotpath::gauge!("usecases.semantic.coordination.rejected").inc(1.0);
        }
        SemanticActivationCoordinationErrorV1::Conflict => {
            hotpath::gauge!("usecases.semantic.coordination.conflict").inc(1.0);
        }
        SemanticActivationCoordinationErrorV1::Runtime(_) => {
            hotpath::gauge!("usecases.semantic.coordination.runtime_failure").inc(1.0);
        }
    }
}

/// One completed redundancy generation read: vectors visited on the active
/// generation versus vectors admitted (symbol-grain, integrity-checked).
#[inline]
pub(crate) fn semantic_redundancy_scan(scanned: usize, admitted: usize) {
    hotpath::gauge!("usecases.semantic.redundancy.vectors_scanned").set(scanned as f64);
    hotpath::gauge!("usecases.semantic.redundancy.vectors_admitted").set(admitted as f64);
}

/// Outcome of one redundancy generation read. A skipped read (no committed
/// authority, disabled semantics, or a pin/integrity mismatch) is normal
/// bounded behavior, but its rate distinguishes "semantics off" from "reads
/// repeatedly failing verification".
#[inline]
pub(crate) fn semantic_redundancy_read(complete: bool) {
    if complete {
        hotpath::gauge!("usecases.semantic.redundancy.reads_complete").inc(1.0);
    } else {
        hotpath::gauge!("usecases.semantic.redundancy.reads_skipped").inc(1.0);
    }
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
pub(crate) fn semantic_stage_descriptor_failed() {
    hotpath::gauge!("usecases.semantic.schedule.stage_descriptor_failed_total").inc(1.0);
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
pub(crate) fn vector_cancelled() {
    hotpath::gauge!("usecases.vector.cancellation_checkpoints").inc(1.0);
}

/// One committed projector batch: chunk receipts carried by this batch and
/// the build's completed-batch progression after the commit.
#[inline]
pub(crate) fn vector_batch_committed(batch_chunks: usize, completed_batches: u64) {
    hotpath::gauge!("usecases.vector.batch_chunks").set(batch_chunks as f64);
    hotpath::gauge!("usecases.vector.completed_batches").set(completed_batches as f64);
}

/// A begin/rebuild request resolved to an already-published generation, so
/// projection work was skipped and the durable publication was replayed.
#[inline]
pub(crate) fn vector_publication_replayed() {
    hotpath::gauge!("usecases.vector.publication_replays").inc(1.0);
}

/// One staged vector build durably cancelled before publication.
#[inline]
pub(crate) fn vector_build_cancelled() {
    hotpath::gauge!("usecases.vector.builds_cancelled").inc(1.0);
}

/// Size of one hydrated published vector generation (catalog-verified rows
/// and vector payload bytes), recorded on every full generation-record read.
#[inline]
pub(crate) fn vector_generation_hydrated(rows: u64, vector_bytes: u64) {
    hotpath::gauge!("usecases.vector.hydrated_rows").set(rows as f64);
    hotpath::gauge!("usecases.vector.hydrated_bytes").set(vector_bytes as f64);
}

/// Depth of the published base-generation lineage walked during recovery.
/// Growth here means incremental generations are chaining instead of being
/// compacted onto a fresh base.
#[inline]
pub(crate) fn vector_lineage_depth(depth: usize) {
    hotpath::gauge!("usecases.vector.lineage_depth").set(depth as f64);
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
