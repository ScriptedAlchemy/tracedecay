//! Bounded Hotpath gauges for session discovery, JSONL, LCM, and git backfill.
//!
//! Labels are static enumerated names only. Gauges compile to no-ops when the
//! `hotpath` feature is off. Counts stay exact; do not put paths, session IDs,
//! or query text in names or values.

use tracedecay_store::observation::ObservationCoverageReason;

/// How one JSONL scan classified the file relative to the stored cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JsonlChangeKind {
    #[default]
    Unchanged,
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
    /// First-line / head-window bytes hashed for file identity.
    pub identity_window_bytes: u64,
    /// Prefix bytes hashed to verify or seed the resume digest.
    pub prefix_validation_bytes: u64,
    /// Bytes hashed by the whole-extent snapshot fingerprint.
    pub snapshot_hash_bytes: u64,
    /// Frame bytes actually consumed past the resume offset.
    pub content_bytes: u64,
    pub change: JsonlChangeKind,
}

#[inline]
pub(crate) fn add(name: &'static str, delta: u64) {
    #[cfg(feature = "hotpath")]
    {
        if delta == 0 {
            return;
        }
        hotpath::gauge!(name).inc(delta);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = (name, delta);
}

#[inline(always)]
fn add_usize(name: &'static str, delta: usize) {
    #[cfg(feature = "hotpath")]
    add(name, u64::try_from(delta).unwrap_or(u64::MAX));
    #[cfg(not(feature = "hotpath"))]
    let _ = (name, delta);
}

#[inline(always)]
pub(crate) fn record_jsonl_io(io: &JsonlIoAccounting) {
    #[cfg(feature = "hotpath")]
    {
        add(
            "sessions.jsonl.identity_window_bytes",
            io.identity_window_bytes,
        );
        add(
            "sessions.jsonl.prefix_validation_bytes",
            io.prefix_validation_bytes,
        );
        add("sessions.jsonl.snapshot_hash_bytes", io.snapshot_hash_bytes);
        add("sessions.jsonl.content_bytes", io.content_bytes);
        match io.change {
            JsonlChangeKind::Unchanged => add("sessions.jsonl.files.unchanged", 1),
            JsonlChangeKind::Appended => add("sessions.jsonl.files.appended", 1),
            JsonlChangeKind::Rewritten => add("sessions.jsonl.files.rewritten", 1),
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = io;
}

#[inline(always)]
#[cfg(feature = "hotpath")]
pub(crate) fn record_discovery_files(considered: u64, selected: u64, metadata_bytes: u64) {
    add("sessions.discovery.files.considered", considered);
    add("sessions.discovery.files.selected", selected);
    add("sessions.discovery.metadata_bytes", metadata_bytes);
}

pub(crate) fn record_file_opened() {
    add("sessions.discovery.files.opened", 1);
}

/// One `read_dir` of a transcript directory.
///
/// Enumerations are the discovery cost that file counters cannot see: the same
/// bucket listed three times to answer three questions charges three times here
/// while `files.considered` reports one tree. Divergence between this and the
/// bucket count is the signal that a pass is re-walking what it already knows.
pub(crate) fn record_dir_enumerated() {
    add("sessions.discovery.dirs.enumerated", 1);
}

#[inline(always)]
#[cfg(feature = "hotpath")]
pub(crate) fn record_discovery_slice(recent_selected: u64, history_selected: u64) {
    add("sessions.discovery.slice.recent", recent_selected);
    add("sessions.discovery.slice.history", history_selected);
}

#[inline(always)]
pub(crate) fn record_sweep_outcome(complete: bool) {
    #[cfg(feature = "hotpath")]
    if complete {
        add("sessions.discovery.sweep.complete", 1);
    } else {
        add("sessions.discovery.sweep.truncated", 1);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = complete;
}

/// One scan discarded because the file changed under it.
///
/// Revalidation decides this from identity, size, and mtime rather than by
/// hashing the extent twice, so the rate is the guard on that trade: a rewrite
/// storm and a rule that mistakes ordinary appends for rewrites look identical
/// in latency and nowhere else. Rejected scans are re-read from scratch on the
/// next pass, so a persistently non-zero rate is wasted I/O, not just noise.
pub(crate) fn record_scan_generation_changed() {
    add("sessions.jsonl.scan.generation_changed", 1);
}

/// One frame decoded and then dropped, split by why.
///
/// The aggregate says three quarters of decoded frames are thrown away but not
/// which are cheap to avoid: a blank line costs only a decode, while an
/// out-of-scope frame was read, decoded, and parsed before anything consulted
/// its scope. Only the split says whether the scope test belongs earlier —
/// which is why this takes the reason rather than counting skips as one number.
pub(crate) fn record_frame_skipped(reason: ObservationCoverageReason) {
    add(
        match reason {
            ObservationCoverageReason::BlankFrame => "sessions.jsonl.frames.skipped.blank",
            ObservationCoverageReason::OutOfScope => "sessions.jsonl.frames.skipped.out_of_scope",
            ObservationCoverageReason::MalformedFrame => "sessions.jsonl.frames.skipped.malformed",
            ObservationCoverageReason::OversizedFrame => "sessions.jsonl.frames.skipped.oversized",
            ObservationCoverageReason::DuplicateObservation => {
                "sessions.jsonl.frames.skipped.duplicate"
            }
            ObservationCoverageReason::SanitizerRejected
            | ObservationCoverageReason::SanitizerQuarantined => {
                "sessions.jsonl.frames.skipped.sanitizer"
            }
            ObservationCoverageReason::AdmissionRefused => {
                "sessions.jsonl.frames.skipped.admission_refused"
            }
            // `ObservationCoverageReason` is `non_exhaustive`, so the residual
            // arm is required. A new reason lands in `other` and stays counted
            // rather than vanishing from the split.
            ObservationCoverageReason::UnknownVersion
            | ObservationCoverageReason::UnsupportedFact
            | ObservationCoverageReason::CanonicalPayloadRevision
            | _ => "sessions.jsonl.frames.skipped.other",
        },
        1,
    );
}

#[inline(always)]
pub(crate) fn record_admission_progress(
    frames_decoded: u64,
    frames_accepted: u64,
    frames_skipped: u64,
    frames_refused: u64,
    frames_persisted: u64,
) {
    #[cfg(feature = "hotpath")]
    {
        add("sessions.jsonl.frames.decoded", frames_decoded);
        add("sessions.jsonl.frames.accepted", frames_accepted);
        add("sessions.jsonl.frames.skipped", frames_skipped);
        add("sessions.jsonl.frames.refused", frames_refused);
        add("sessions.jsonl.frames.persisted", frames_persisted);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = (
        frames_decoded,
        frames_accepted,
        frames_skipped,
        frames_refused,
        frames_persisted,
    );
}

#[inline(always)]
pub(crate) fn record_lcm_compression(summary_nodes: usize, attempts: usize) {
    add_usize("sessions.lcm.compress.summary_nodes", summary_nodes);
    add_usize("sessions.lcm.compress.attempts", attempts);
}

#[inline(always)]
pub(crate) fn record_lcm_gc(bytes: u64, files: usize) {
    add("sessions.lcm.gc.reclaimed_bytes", bytes);
    add_usize("sessions.lcm.gc.reclaimed_files", files);
}

#[inline(always)]
pub(crate) fn record_lcm_retention(bytes: u64) {
    add("sessions.lcm.retention.reclaimed_bytes", bytes);
}

#[inline(always)]
pub(crate) fn record_lcm_retrieval(matches: usize) {
    add_usize("sessions.lcm.retrieval.matches", matches);
}

#[inline(always)]
pub(crate) fn record_git_backfill(sessions_scanned: usize, spans_written: usize) {
    add_usize("sessions.git.backfill.sessions_scanned", sessions_scanned);
    add_usize("sessions.git.backfill.spans_written", spans_written);
}

#[inline(always)]
pub(crate) fn record_historical_ingest(complete: bool) {
    #[cfg(feature = "hotpath")]
    if complete {
        add("sessions.ingest.historical.complete", 1);
    } else {
        add("sessions.ingest.historical.truncated", 1);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = complete;
}
