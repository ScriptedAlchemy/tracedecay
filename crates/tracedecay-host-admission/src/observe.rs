//! Metrics gauges for host-event admission.
//!
//! Gauge keys stay the historical `usecases.admission.*` labels so dashboards
//! remain continuous across the crate extraction. Never pass model inputs,
//! paths, or session identifiers as labels.
