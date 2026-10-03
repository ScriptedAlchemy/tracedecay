//! Metrics gauges for usecases scheduling, queries, and admission.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Gauges are emitted through the `metrics`
//! facade and drop when no recorder is installed.

#[inline]
pub(crate) fn diagnostics_query(records: usize, total: usize) {}

#[inline]
pub(crate) fn feedback_query(findings: usize) {}
