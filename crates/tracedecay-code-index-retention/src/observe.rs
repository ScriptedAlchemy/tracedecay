//! Metrics gauges for code-index generation retention.
//!
//! Gauge keys stay the historical `usecases.retention.*` labels so dashboards
//! and comparisons remain continuous across the crate extraction. Never pass
//! model inputs, paths, or generation identifiers as labels. Every macro
//! is emitted through the `metrics` facade and drops with no recorder.
