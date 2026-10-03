//! Metrics gauges for usecases scheduling, queries, and admission.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Gauges are emitted through the `metrics`
//! facade and drop when no recorder is installed.
