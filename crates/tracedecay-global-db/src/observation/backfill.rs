//! Completion markers for the historical observation backfills.
//!
//! The backfill passes themselves are gone: stores are created at the final
//! observation schema, so there is no historical row set to attach derived
//! provenance or retrieval anchors to. The markers survive because the
//! consolidator still records them on a destination store it assembles, and
//! clears the provenance marker when it merges a source tail.

/// Completion marker for the repository-provenance backfill. Public so a
/// writer that appends observations the backfill has already passed -- the
/// consolidator merges a source tail above the target frontier -- can clear it
/// and re-arm convergence.
pub const OBSERVATION_PROVENANCE_SCHEMA_MIGRATION: &str = "observation-repository-provenance-v1";
