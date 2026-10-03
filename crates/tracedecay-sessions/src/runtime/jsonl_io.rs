//! JSONL scan I/O accounting carried on every scan result.

/// How one JSONL scan classified the file relative to the stored cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JsonlChangeKind {
    #[default]
    Unchanged,
    Cold,
    Appended,
    Rewritten,
}

/// Byte-category accounting for one JSONL scan.
///
/// Categories are operation charges, not unique physical reads: a snapshot
/// hash of the whole file is charged here even when a prefix validation
/// already walked the same extent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonlIoAccounting {
    /// Logical first-line / head-window bytes hashed for file identity. This
    /// is not a count of physical bytes fetched by the buffered reader.
    pub identity_window_bytes: u64,
    /// Prefix bytes hashed to verify or seed the resume digest.
    pub prefix_validation_bytes: u64,
    /// Bytes hashed by the whole-extent snapshot fingerprint.
    pub snapshot_hash_bytes: u64,
    /// Frame bytes actually consumed past the resume offset.
    pub content_bytes: u64,
    /// Bytes returned by instrumented prefix, snapshot, framing, and boundary
    /// reads on the canonical handle. Identity-window reads remain separate.
    pub scan_payload_read_bytes: u64,
    pub change: JsonlChangeKind,
}

impl JsonlIoAccounting {
    /// Totals of two scans of one source where `next` decided the change.
    #[must_use]
    pub fn followed_by(self, next: Self) -> Self {
        Self {
            identity_window_bytes: self
                .identity_window_bytes
                .saturating_add(next.identity_window_bytes),
            prefix_validation_bytes: self
                .prefix_validation_bytes
                .saturating_add(next.prefix_validation_bytes),
            snapshot_hash_bytes: self
                .snapshot_hash_bytes
                .saturating_add(next.snapshot_hash_bytes),
            content_bytes: self.content_bytes.saturating_add(next.content_bytes),
            scan_payload_read_bytes: self
                .scan_payload_read_bytes
                .saturating_add(next.scan_payload_read_bytes),
            change: next.change,
        }
    }
}
