//! Opt-in hotpath gauges for usecases scheduling, queries, and admission.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Every macro expands to a no-op unless
//! this crate's `hotpath` feature is selected.

#[inline]
pub(crate) fn diagnostics_query(records: usize, total: usize) {
    hotpath::gauge!("usecases.diagnostics.records").set(records as f64);
    hotpath::gauge!("usecases.diagnostics.total").set(total as f64);
}

#[inline]
pub(crate) fn feedback_query(findings: usize) {
    hotpath::gauge!("usecases.feedback.findings").set(findings as f64);
}
