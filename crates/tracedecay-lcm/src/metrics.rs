//! Bounded LCM metrics backed by Hotpath.
//!
//! Labels are static enumerated names only. Gauges compile to no-ops when the
//! `hotpath` feature is off. Counts stay exact; do not put paths, session IDs,
//! or query text in names or values.

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

/// One raw-message row decode + integrity verification.
///
/// `Some(bytes)` charges the verified content bytes; `None` counts a failed
/// verification. The bytes gauge is the corpus-scale decode measure that the
/// per-row `sessions.lcm.raw.verify_row` span cannot express, and failures
/// stay visible because a rejected row aborts its whole page — decode work
/// paid and then discarded.
#[inline(always)]
pub(crate) fn record_lcm_raw_row_verified(content_bytes: Option<usize>) {
    match content_bytes {
        Some(bytes) => add_usize("sessions.lcm.raw.verified_bytes", bytes),
        None => add("sessions.lcm.raw.verify_failed", 1),
    }
}

/// One completed LCM grep page.
///
/// `pages` counts every completed page so zero-hit pages stay visible, and
/// `like_fallback` marks pages that bypassed FTS for the LIKE table scan —
/// the two query plans have very different costs, and without the split a
/// slow-grep profile cannot say which plan the workload is actually on.
#[inline(always)]
pub(crate) fn record_lcm_grep(hits: usize, like_fallback: bool) {
    add("sessions.lcm.grep.pages", 1);
    add_usize("sessions.lcm.grep.hits", hits);
    if like_fallback {
        add("sessions.lcm.grep.like_fallback", 1);
    }
}

/// One compression call that failed before producing a response. The
/// assembled backlog, ingest writes, and summary drafts behind it are all
/// discarded work that success-only counters would hide.
#[inline(always)]
pub(crate) fn record_lcm_compress_failed() {
    add("sessions.lcm.compress.failed", 1);
}

#[inline(always)]
pub(crate) fn record_lcm_compression(summary_nodes: usize, attempts: usize, replay_tokens: i64) {
    add_usize("sessions.lcm.compress.summary_nodes", summary_nodes);
    add_usize("sessions.lcm.compress.attempts", attempts);
    add(
        "sessions.lcm.compress.replay_tokens",
        u64::try_from(replay_tokens).unwrap_or(0),
    );
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
