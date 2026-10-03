//! Metrics gauges for usecases scheduling, queries, and admission.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Gauges are emitted through the `metrics`
//! facade and drop when no recorder is installed.

#[inline]
pub(crate) fn diagnostics_query(records: usize, total: usize) {
    metrics::gauge!("usecases.diagnostics.records").set(records as f64);
    metrics::gauge!("usecases.diagnostics.total").set(total as f64);
}

#[inline]
pub(crate) fn feedback_query(findings: usize) {
    metrics::gauge!("usecases.feedback.findings").set(findings as f64);
}
