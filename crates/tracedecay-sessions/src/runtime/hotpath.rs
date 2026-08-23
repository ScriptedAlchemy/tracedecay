//! Bounded Hotpath gauges for session discovery, JSONL, LCM, and git backfill.
//!
//! Labels are static enumerated names only. Gauges compile to no-ops when the
//! `hotpath` feature is off. Counts stay exact; do not put paths, session IDs,
//! or query text in names or values.

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
#[cfg_attr(not(feature = "hotpath"), allow(unused_variables))]
pub(crate) fn add(name: &'static str, delta: u64) {
    if delta == 0 {
        return;
    }
    hotpath::gauge!(name).inc(delta);
}

#[inline]
fn add_usize(name: &'static str, delta: usize) {
    add(name, u64::try_from(delta).unwrap_or(u64::MAX));
}

pub(crate) fn record_jsonl_io(io: &JsonlIoAccounting) {
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

pub(crate) fn record_discovery_files(considered: u64, selected: u64, metadata_bytes: u64) {
    add("sessions.discovery.files.considered", considered);
    add("sessions.discovery.files.selected", selected);
    add("sessions.discovery.metadata_bytes", metadata_bytes);
}

pub(crate) fn record_file_opened() {
    add("sessions.discovery.files.opened", 1);
}

pub(crate) fn record_discovery_slice(recent_selected: u64, history_selected: u64) {
    add("sessions.discovery.slice.recent", recent_selected);
    add("sessions.discovery.slice.history", history_selected);
}

pub(crate) fn record_sweep_outcome(complete: bool) {
    if complete {
        add("sessions.discovery.sweep.complete", 1);
    } else {
        add("sessions.discovery.sweep.truncated", 1);
    }
}

pub(crate) fn record_admission_progress(
    frames_decoded: u64,
    frames_accepted: u64,
    frames_skipped: u64,
    frames_refused: u64,
    frames_persisted: u64,
    writer_txns: u64,
) {
    add("sessions.jsonl.frames.decoded", frames_decoded);
    add("sessions.jsonl.frames.accepted", frames_accepted);
    add("sessions.jsonl.frames.skipped", frames_skipped);
    add("sessions.jsonl.frames.refused", frames_refused);
    add("sessions.jsonl.frames.persisted", frames_persisted);
    add("sessions.jsonl.writer_txns", writer_txns);
}

pub(crate) fn record_lcm_compression(summary_nodes: usize, attempts: usize) {
    add_usize("sessions.lcm.compress.summary_nodes", summary_nodes);
    add_usize("sessions.lcm.compress.attempts", attempts);
}

pub(crate) fn record_lcm_gc(bytes: u64, files: usize) {
    add("sessions.lcm.gc.reclaimed_bytes", bytes);
    add_usize("sessions.lcm.gc.reclaimed_files", files);
}

pub(crate) fn record_lcm_retention(bytes: u64) {
    add("sessions.lcm.retention.reclaimed_bytes", bytes);
}

pub(crate) fn record_lcm_retrieval(matches: usize) {
    add_usize("sessions.lcm.retrieval.matches", matches);
}

pub(crate) fn record_git_backfill(sessions_scanned: usize, spans_written: usize) {
    add_usize("sessions.git.backfill.sessions_scanned", sessions_scanned);
    add_usize("sessions.git.backfill.spans_written", spans_written);
}

pub(crate) fn record_historical_ingest(complete: bool) {
    if complete {
        add("sessions.ingest.historical.complete", 1);
    } else {
        add("sessions.ingest.historical.truncated", 1);
    }
}
