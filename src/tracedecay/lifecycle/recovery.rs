//! Corruption classification used by Doctor.

/// Whether a `PRAGMA quick_check` problem row describes damage confined to
/// the retired SQLite FTS index. Doctor uses this only to classify persisted
/// damage; project open never repairs or rebuilds the index inline.
pub(crate) fn is_fts_only_corruption(problem: &str) -> bool {
    problem.contains("malformed inverted index for FTS5 table main.nodes_fts")
        || problem.contains("malformed inverted index for FTS5 table nodes_fts")
        || (problem.contains("fts5: corruption found") && problem.contains("nodes_fts"))
}
