//! Sanitized intake port (Plan 25, "Sanitized intake"): accept only
//! receipt-bound sanitized snapshots carrying repository, checkout, worktree,
//! ref, source revision, sanitizer revision, and content identity; reject
//! missing, stale, mixed-snapshot, or unsanitized input before parsing.
//!
//! Filesystem watching, repository reads, snapshot coalescing, and redaction
//! belong to capture, not this boundary (Plan 25, "Does not own").

use tracedecay_domain::{IntakeRejectionV1, SanitizedCodeSnapshotV1, ValidatedCodeSnapshotV1};

/// The intake validation contract (Plan 25 phase 2). This is the only legal
/// entry into the indexer: architecture tests construct the indexer through
/// `CodeIndexIntake` and the projection sink only.
pub trait CodeIndexIntake {
    /// Validate one sanitized snapshot, rejecting missing, stale,
    /// mixed-snapshot, or unsanitized input before any parsing occurs.
    fn validate(
        &self,
        snapshot: SanitizedCodeSnapshotV1,
    ) -> Result<ValidatedCodeSnapshotV1, IntakeRejectionV1>;
}
