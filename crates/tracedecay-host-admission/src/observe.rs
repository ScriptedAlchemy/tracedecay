//! Metrics gauges for host-event admission.
//!
//! Gauge keys stay the historical `usecases.admission.*` labels so dashboards
//! remain continuous across the crate extraction. Never pass model inputs,
//! paths, or session identifiers as labels.

#[inline]
pub(crate) fn admission_capture_frames(frames: usize) {}

#[inline]
pub(crate) fn admission_persist_frames(frames: usize) {}
