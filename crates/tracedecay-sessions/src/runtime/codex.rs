//! Codex CLI transcript source.
//!
//! Codex appends one JSON object per line to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (sessions archived from the
//! picker move to a flat `~/.codex/archived_sessions/rollout-*.jsonl`). Each
//! line is `{"timestamp": "<iso8601>", "type": "<kind>", "payload": {…}}`. The
//! relevant kinds for conversation text are:
//!
//! * `session_meta` — first line; `payload.cwd`, session `id`. Real rollouts
//!   carry no `model` here (only `model_provider`); the active model is on
//!   `turn_context` lines and can change mid-session.
//! * `event_msg` with `payload.type == "user_message"` — a real user prompt
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "agent_message"` — a real assistant reply
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "token_count"` — provider usage captured
//!   by the canonical observation path, not conversational message metadata.
//! * `event_msg` with `payload.type == "thread_goal_updated"` — the structured
//!   session goal and its lifecycle (`payload.goal.{objective,status,tokensUsed,
//!   timeUsedSeconds,createdAt,updatedAt}`). `TraceDecay` records each state as a
//!   compact `goal` row (objective as text, the rest in `metadata_json`) so the
//!   session's goal and whether it is still active is searchable. `status` is
//!   stored verbatim — real rollouts emit `active`/`paused`, but any future
//!   value (e.g. `completed`) is carried through unchanged rather than mapped to
//!   a fixed enum. Consecutive events that repeat the same `(objective, status)`
//!   within one parse pass are deduped; each genuine transition keeps its row.
//! * `compacted` — Codex context-compression boundary. The rollout stores the
//!   replacement history and an encrypted compaction body, so `TraceDecay` records
//!   the boundary/provenance as a summary record without claiming plaintext
//!   access to Codex's private summary.
//! * `response_item` goal context — Codex replays active thread goals as
//!   synthetic user context. `TraceDecay` indexes those as compact goal-context
//!   records so LCM can catalog the objective and budget without treating the
//!   instruction boilerplate as normal conversation.
//! * subagent rollouts — separate `rollout-*.jsonl` files whose leading
//!   `session_meta` has `thread_source == "subagent"` and parent ids in
//!   `forked_from_id` / `source.subagent.thread_spawn.parent_thread_id`.
//!
//! `response_item` entries are intentionally skipped except for Codex goal
//! context blocks: they usually carry auto-injected synthetic context and
//! duplicate the `agent_message`/`user_message` turns, so ingesting them would
//! double-count the conversation. Goal context blocks are cataloged as compact
//! `goal_context` rows because real rollouts often record them only in
//! `response_item` form. This append-only JSONL is read with the shared
//! byte-offset machinery and scoped per turn by the latest Codex cwd context.

mod context;
mod events;
mod goals;
mod meta;
mod observation;
mod records;
mod scope_probe;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use context::CodexContextState;
use goals::{codex_goal_event_from_line, goal_context_from_line, goal_event_message};
use meta::session_meta;
use records::{
    compacted_summary_from_line, message_from_line, response_item_goal_context_from_line,
    response_item_tool_event_from_line, timestamp_from_record,
};

use crate::runtime::jsonl_observation_admission::{
    namespace_replacement_message_ids, preflight_and_parse_new,
};
use crate::runtime::shared::{
    ProjectMembership, ProjectRootMatcherCache, StoredCursor, TranscriptScopeMatcher,
    title_from_messages,
};
use crate::runtime::source::{
    FileDiscoveryReport, ParsedTranscript, SessionDraft, TranscriptDiscoveryBounds,
    TranscriptIngestResult, TranscriptSource, collect_files_with_ext_bounded, stream_new_jsonl,
};
pub use meta::{CodexMeta, session_meta_from_record, turn_context_from_record};
pub use observation::{
    CodexJsonlAdmissionProgress, try_admit_codex_jsonl_observations_for_profile,
    try_admit_codex_jsonl_observations_for_profile_with_admission,
    try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation,
    try_admit_codex_jsonl_observations_for_project,
    try_admit_codex_jsonl_observations_for_project_with_admission,
    try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation,
};

const PROVIDER: &str = "codex";
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` → date dirs add depth.
const MAX_SCAN_DEPTH: u8 = 6;
/// Slice of one discovery pass reserved for rotating historical catch-up.
///
/// A backlog larger than the discovery file cap must not starve the present:
/// most of the pass serves the newest transcripts, while this slice walks the
/// historical remainder under a durable rotation frontier so every older
/// bucket is revisited on a bounded cadence — tracked pending work, never a
/// skipped-and-forgotten range.
const HISTORY_CATCH_UP_UNITS: usize = 512;
/// Bound on enumerated transcript directories (a decade of daily Codex
/// directories is ~3700; this cap only guards against pathological trees).
const MAX_BUCKET_DIRS: usize = 8192;

pub struct CodexSource {
    sessions_dir: PathBuf,
    archived_sessions_dir: PathBuf,
    user_scope: Option<UserCodexScope>,
    /// Source-lifetime cache of project-root matchers and cwd worktree
    /// resolutions, so one scan pass runs git identity discovery once per
    /// root/cwd instead of once per transcript record.
    project_matchers: ProjectRootMatcherCache,
}

struct UserCodexScope {
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
}

impl CodexSource {
    /// Source rooted at the real `~/.codex`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.codex` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        let codex_home = home.join(".codex");
        Self {
            sessions_dir: codex_home.join("sessions"),
            archived_sessions_dir: codex_home.join("archived_sessions"),
            user_scope: None,
            project_matchers: ProjectRootMatcherCache::default(),
        }
    }

    /// Restricts ingestion to sessions that cannot be attributed to a registered project.
    #[must_use]
    pub fn for_user_scope(
        mut self,
        session_id: Option<String>,
        registered_roots: Vec<PathBuf>,
    ) -> Self {
        self.user_scope = Some(UserCodexScope {
            session_id,
            registered_roots,
        });
        self
    }

    /// One recent-first discovery pass with rotating historical coverage.
    ///
    /// Transcript directories (the dated `sessions/YYYY/MM/DD` leaves plus the
    /// archive tree) are enumerated newest-first and most of the pass budget
    /// fills from the newest buckets, so today's sessions are always
    /// discovered and ingested before any backlog. The remainder of the
    /// budget rotates through the older buckets under the packed
    /// `history_rotation` frontier (bucket rotation in the high bits, an
    /// intra-bucket file offset in the low bits — see [`HISTORY_INTRA_BITS`]),
    /// so a bucket larger than one catch-up slice converges file-by-file
    /// instead of starving its tail or pinning the rotation. The caller
    /// persists [`CodexDiscoveryPass::next_history_rotation`] through the
    /// durable ingest frontier so consecutive passes cover the whole backlog
    /// as bounded background catch-up. After a full wrap the packed frontier
    /// carries a sweep-complete watermark so idle polls stay complete instead
    /// of restarting as truncated-from-zero.
    #[hotpath::measure]
    pub fn discover_transcript_paths_with_rotation(
        &self,
        bounds: TranscriptDiscoveryBounds,
        history_rotation: u64,
    ) -> CodexDiscoveryPass {
        let mut buckets = collect_bucket_dirs(&self.sessions_dir, MAX_SCAN_DEPTH, MAX_BUCKET_DIRS);
        // Dated paths are zero-padded, so descending lexicographic order is
        // reverse-chronological. The flat archive holds picker-archived (old)
        // rollouts and is appended as the oldest bucket.
        buckets.sort_unstable_by(|a, b| b.cmp(a));
        // Archiving a session moves its rollout out of the dated tree; both
        // locations are real transcripts and must be ingested. The archive is
        // picker-archived (old) material, so its buckets trail every dated one.
        let mut archive_buckets = collect_bucket_dirs(
            &self.archived_sessions_dir,
            MAX_SCAN_DEPTH,
            MAX_BUCKET_DIRS.saturating_sub(buckets.len()),
        );
        archive_buckets.sort_unstable_by(|a, b| b.cmp(a));
        buckets.extend(archive_buckets);

        let history_units = (bounds.max_files / 8).min(HISTORY_CATCH_UP_UNITS);
        let recent_units = bounds.max_files.saturating_sub(history_units);
        let counts = JsonlBucketCounts::measure(&buckets);
        let total_files = counts.total;
        let incoming_complete = history_rotation_sweep_complete(history_rotation);
        let incoming_count = history_rotation_file_count(history_rotation).unwrap_or(0);
        // A Complete watermark stays valid only while the tree has not grown.
        // New files clear it so history rotation starts over — at-least-once.
        let idle_complete = incoming_complete && total_files <= incoming_count;
        let (mut bucket_rotation, mut intra_offset) = if incoming_complete && !idle_complete {
            (0, 0)
        } else {
            (
                history_rotation_bucket(history_rotation),
                history_rotation_intra(history_rotation),
            )
        };

        let mut pass = BucketScanState::new(bounds);
        // Phase 1 — the present: fill newest-first up to the recent budget.
        let mut first_unfinished_bucket = buckets.len();
        for (index, bucket) in buckets.iter().enumerate() {
            if !pass.scan_bucket(bucket, recent_units) {
                first_unfinished_bucket = index;
                break;
            }
        }

        // Phase 2 — bounded background history: rotate through the buckets
        // the recent fill did not finish, starting at the durable frontier.
        let mut next_history_rotation = if idle_complete {
            pack_history_rotation(bucket_rotation, intra_offset, Some(total_files))
        } else {
            pack_history_rotation(bucket_rotation, intra_offset, None)
        };
        let older = &buckets[first_unfinished_bucket..];
        #[cfg(feature = "hotpath")]
        let recent_selected = u64::try_from(pass.paths.len()).unwrap_or(u64::MAX);
        let mut sweep_complete = idle_complete || older.is_empty();
        if !idle_complete
            && !older.is_empty()
            && history_units > 0
            && pass.truncated_by_recent_budget()
        {
            pass.enter_history_phase();
            let start = usize::try_from(bucket_rotation % older.len() as u64).unwrap_or(0);
            let mut completed_buckets = 0u64;
            let mut partial_intra = 0u64;
            for offset in 0..older.len() {
                if pass.is_full(bounds.max_files) {
                    break;
                }
                let bucket = &older[(start + offset) % older.len()];
                // The intra-bucket offset resumes the bucket the previous pass
                // left partially covered. Bucket lists shift as new days
                // arrive, so the offset can land on a different bucket; the
                // files it then skips are recovered on a later wrap-around —
                // at-least-once coverage, never a dropped range.
                let skip = if offset == 0 {
                    usize::try_from(intra_offset).unwrap_or(usize::MAX)
                } else {
                    0
                };
                let outcome = pass.scan_bucket_skipping(bucket, bounds.max_files, skip);
                if outcome.complete {
                    completed_buckets = completed_buckets.saturating_add(1);
                } else {
                    // A partial bucket keeps the rotation on itself and moves
                    // only the intra-bucket offset, so an oversized bucket
                    // converges file-by-file instead of starving its tail.
                    partial_intra = (skip as u64)
                        .saturating_add(outcome.inspected as u64)
                        .min(HISTORY_INTRA_MASK);
                    break;
                }
            }
            if completed_buckets > 0 || partial_intra > intra_offset {
                bucket_rotation = bucket_rotation.saturating_add(completed_buckets);
                intra_offset = if completed_buckets == 0 {
                    partial_intra
                } else {
                    // Completed buckets reset the intra offset; a trailing
                    // partial bucket after completions restarts at zero on
                    // the next pass (its head files dedupe cheaply).
                    0
                };
                // Empty parent dirs (sessions/YYYY, …) still occupy older
                // slots. A file-cap stop can leave those unwalked after the
                // last jsonl is visited; complete when no jsonl remains in
                // the unvisited remainder of this wrap, not when every
                // directory slot has been counted.
                let remaining_jsonl = remaining_older_jsonl(
                    &counts.per_bucket[first_unfinished_bucket..],
                    start,
                    completed_buckets,
                    intra_offset,
                );
                let wrapped = pass.truncated.is_none()
                    && (bucket_rotation >= older.len() as u64 || remaining_jsonl == 0);
                sweep_complete = wrapped;
                next_history_rotation = pack_history_rotation(
                    bucket_rotation,
                    intra_offset,
                    wrapped.then_some(total_files),
                );
            }
        }

        let report = pass.into_report(sweep_complete, total_files);
        #[cfg(feature = "hotpath")]
        {
            let history_selected = u64::try_from(report.paths.len())
                .unwrap_or(u64::MAX)
                .saturating_sub(recent_selected);
            crate::runtime::pipeline_metrics::record_discovery_slice(
                recent_selected,
                history_selected,
            );
        }
        crate::runtime::pipeline_metrics::record_sweep_outcome(!report.is_truncated());
        CodexDiscoveryPass {
            report,
            next_history_rotation,
        }
    }
}

/// Low bits of the packed history-rotation frontier carrying the file offset
/// inside the bucket the rotation currently points at; the high bits count
/// fully covered buckets. Packing keeps the durable frontier a single
/// monotonically increasing counter (the ingest frontier store only advances).
///
/// Bit 63 is the sweep-complete watermark: after history rotation has visited
/// every backlog bucket, subsequent idle polls keep this bit and do not
/// restart as truncated-from-zero. Bits 43–62 then store the jsonl file count
/// so tree growth can clear the watermark. Existing frontiers never have bit
/// 63 set, so they unpack as in-progress rotations.
pub const HISTORY_INTRA_BITS: u32 = 20;
const HISTORY_INTRA_MASK: u64 = (1 << HISTORY_INTRA_BITS) - 1;
const SWEEP_COMPLETE_BIT: u64 = 1 << 63;
const HISTORY_FILE_COUNT_SHIFT: u32 = 43;
const HISTORY_FILE_COUNT_BITS: u32 = 20;
const HISTORY_FILE_COUNT_MASK: u64 = (1 << HISTORY_FILE_COUNT_BITS) - 1;
const HISTORY_COMPLETE_BUCKET_BITS: u32 = HISTORY_FILE_COUNT_SHIFT - HISTORY_INTRA_BITS;
const HISTORY_COMPLETE_BUCKET_MASK: u64 = (1 << HISTORY_COMPLETE_BUCKET_BITS) - 1;

pub(crate) fn history_rotation_sweep_complete(rotation: u64) -> bool {
    rotation & SWEEP_COMPLETE_BIT != 0
}

pub(crate) fn history_rotation_intra(rotation: u64) -> u64 {
    rotation & HISTORY_INTRA_MASK
}

pub(crate) fn history_rotation_bucket(rotation: u64) -> u64 {
    if history_rotation_sweep_complete(rotation) {
        (rotation >> HISTORY_INTRA_BITS) & HISTORY_COMPLETE_BUCKET_MASK
    } else {
        rotation >> HISTORY_INTRA_BITS
    }
}

pub(crate) fn history_rotation_file_count(rotation: u64) -> Option<u64> {
    history_rotation_sweep_complete(rotation)
        .then_some((rotation >> HISTORY_FILE_COUNT_SHIFT) & HISTORY_FILE_COUNT_MASK)
}

pub(crate) fn pack_history_rotation(bucket: u64, intra: u64, complete_files: Option<u64>) -> u64 {
    let intra = intra & HISTORY_INTRA_MASK;
    match complete_files {
        Some(count) => {
            SWEEP_COMPLETE_BIT
                | ((count & HISTORY_FILE_COUNT_MASK) << HISTORY_FILE_COUNT_SHIFT)
                | ((bucket & HISTORY_COMPLETE_BUCKET_MASK) << HISTORY_INTRA_BITS)
                | intra
        }
        None => bucket
            .saturating_mul(1 << HISTORY_INTRA_BITS)
            .saturating_add(intra),
    }
}

/// Drop the sweep-complete bit while keeping bucket/intra, so a Partial
/// coverage watermark can keep walking history instead of idling.
pub(crate) fn strip_sweep_complete(rotation: u64) -> u64 {
    if !history_rotation_sweep_complete(rotation) {
        return rotation;
    }
    pack_history_rotation(
        history_rotation_bucket(rotation),
        history_rotation_intra(rotation),
        None,
    )
}

/// Resume rotation from a durable store: idle only when coverage is Complete.
pub(crate) fn effective_history_rotation(stored: u64, coverage_complete: bool) -> u64 {
    if coverage_complete {
        stored
    } else {
        strip_sweep_complete(stored)
    }
}

/// Outcome of one recent-first Codex discovery pass.
pub struct CodexDiscoveryPass {
    pub report: FileDiscoveryReport,
    /// Packed rotation frontier after this pass; the caller persists it (the
    /// value never decreases) so the next pass resumes historical coverage
    /// where this one stopped. Bit 63 plus the packed file count are the
    /// durable sweep-complete watermark once every backlog file has been
    /// visited.
    pub next_history_rotation: u64,
}

fn count_jsonl_in_dir(dir: &Path) -> u64 {
    crate::runtime::pipeline_metrics::record_dir_enumerated();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if path.is_file() {
            total = total.saturating_add(1);
        }
    }
    total
}

/// Per-bucket jsonl counts for one discovery pass.
///
/// Both consumers — the sweep watermark's `total` and the history phase's
/// remainder — need the same unbounded per-bucket counts, and each used to
/// re-`read_dir` every bucket to get them. Counting once per pass and indexing
/// the result keeps one directory enumeration per bucket instead of two, which
/// on a 25k-file tree is the difference between one metadata sweep and three.
struct JsonlBucketCounts {
    /// Positionally aligned with the `buckets` slice it was measured from, so
    /// a sub-slice of those buckets indexes the matching sub-slice here.
    per_bucket: Vec<u64>,
    total: u64,
}

impl JsonlBucketCounts {
    fn measure(buckets: &[PathBuf]) -> Self {
        let per_bucket: Vec<u64> = buckets
            .iter()
            .map(|bucket| count_jsonl_in_dir(bucket))
            .collect();
        let total = per_bucket
            .iter()
            .fold(0u64, |total, count| total.saturating_add(*count));
        Self { per_bucket, total }
    }
}

/// jsonl files still ahead of the history frontier on this wrap — the
/// remainder of an incomplete current bucket plus later older buckets
/// before the slice ends. Already-visited buckets earlier in `older` are
/// not recounted (modulo wrap-around would treat them as still pending).
///
/// `older_counts` is the slice of [`JsonlBucketCounts::per_bucket`] covering
/// the same buckets as `older`, so this reads counts already taken this pass.
fn remaining_older_jsonl(
    older_counts: &[u64],
    start: usize,
    completed_buckets: u64,
    intra_offset: u64,
) -> u64 {
    if older_counts.is_empty() || start >= older_counts.len() {
        return 0;
    }
    let completed = usize::try_from(completed_buckets).unwrap_or(older_counts.len());
    let mut total = 0u64;
    let first_idx = if completed == 0 {
        total = total.saturating_add(older_counts[start].saturating_sub(intra_offset));
        start.saturating_add(1)
    } else {
        start.saturating_add(completed)
    };
    for count in older_counts.iter().skip(first_idx) {
        total = total.saturating_add(*count);
    }
    total
}

/// Bounded breadth-first enumeration of transcript directories. Directory
/// symlinks are not followed; the root itself is a bucket so stray files
/// directly under `sessions/` are still discovered.
fn collect_bucket_dirs(root: &Path, max_depth: u8, max_dirs: usize) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut dirs = vec![(root.to_path_buf(), 0u8)];
    let mut index = 0;
    while index < dirs.len() && dirs.len() < max_dirs {
        let (dir, depth) = dirs[index].clone();
        index += 1;
        if depth >= max_depth {
            continue;
        }
        crate::runtime::pipeline_metrics::record_dir_enumerated();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if dirs.len() >= max_dirs {
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                dirs.push((dir.join(entry.file_name()), depth.saturating_add(1)));
            }
        }
    }
    dirs.into_iter().map(|(dir, _)| dir).collect()
}

/// Result of scanning one bucket into the shared retention state.
struct BucketScanOutcome {
    /// Every file in the bucket was listed within the budgets.
    complete: bool,
    /// Directory-order entries covered past the skip offset (retained or
    /// already retained earlier this pass); the intra-bucket frontier advance.
    inspected: usize,
}

/// Shared retention state across the recent and history phases of one pass.
struct BucketScanState {
    bounds: TranscriptDiscoveryBounds,
    paths: Vec<PathBuf>,
    seen: std::collections::HashSet<PathBuf>,
    truncated: Option<crate::runtime::source::FileDiscoveryLimit>,
    recent_phase_truncated: bool,
    skipped_oversized_entries: u64,
    bytes_charged: u64,
}

impl BucketScanState {
    fn new(bounds: TranscriptDiscoveryBounds) -> Self {
        Self {
            bounds,
            paths: Vec::new(),
            seen: std::collections::HashSet::new(),
            truncated: None,
            recent_phase_truncated: false,
            skipped_oversized_entries: 0,
            bytes_charged: 0,
        }
    }

    /// Scans one bucket directory (files directly inside it, newest-first by
    /// name) into the shared retention state, stopping at `max_total_files`
    /// retained paths overall. Returns whether the bucket was fully scanned.
    fn scan_bucket(&mut self, bucket: &Path, max_total_files: usize) -> bool {
        self.scan_bucket_skipping(bucket, max_total_files, 0)
            .complete
    }

    /// [`Self::scan_bucket`] resuming past the bucket's first `skip` directory
    /// entries — the intra-bucket offset of the packed history rotation.
    ///
    /// The offset addresses raw directory order (stable for an unchanged
    /// directory), not sorted order: consecutive passes then cover disjoint,
    /// contiguous slices of the listing, and entries the recent phase already
    /// retained this pass still advance the offset (`inspected`) because their
    /// ingestion is covered. If the directory mutates between passes the
    /// slices shift; the wrap-around revisit recovers anything missed —
    /// at-least-once coverage.
    fn scan_bucket_skipping(
        &mut self,
        bucket: &Path,
        max_total_files: usize,
        skip: usize,
    ) -> BucketScanOutcome {
        let max_total_files = max_total_files.min(self.bounds.max_files);
        if self.paths.len() >= max_total_files {
            self.truncated = Some(crate::runtime::source::FileDiscoveryLimit::FileCount);
            return BucketScanOutcome {
                complete: false,
                inspected: 0,
            };
        }
        let remaining_bytes = self
            .bounds
            .max_discovery_bytes
            .saturating_sub(self.bytes_charged);
        if remaining_bytes == 0 {
            self.truncated = Some(crate::runtime::source::FileDiscoveryLimit::DiscoveryBytes);
            return BucketScanOutcome {
                complete: false,
                inspected: 0,
            };
        }
        let remaining = max_total_files - self.paths.len();
        // Skipped listings are still charged against the discovery byte
        // budget, so a large resume offset truncates conservatively rather
        // than exceeding the caps.
        //
        // The enumeration bound is deliberately NOT `remaining`: entries the
        // recent phase already retained are re-listed here and rejected by
        // `seen`, consuming a listing slot without producing a retention. Bound
        // by `remaining` and every such overlap silently shrinks the history
        // phase's budget — the pass stops short of `max_files` even though the
        // bucket still holds unseen files. `seen` can hold at most `max_files`
        // entries, so `skip + max_files` is the tight upper bound on how far we
        // may need to list to find `remaining` unseen ones. Retention is capped
        // at `remaining` by the loop below, and the byte budget still bounds
        // total work.
        let bucket_bounds = TranscriptDiscoveryBounds {
            max_files: self.bounds.max_files.saturating_add(skip),
            max_discovery_bytes: remaining_bytes,
            ..self.bounds
        };
        // Depth 0 scans exactly this directory; nested buckets are their own
        // enumeration entries, so no file is charged or retained twice.
        let mut scan = collect_files_with_ext_bounded(bucket, "jsonl", 0, bucket_bounds);
        self.bytes_charged = self.bytes_charged.saturating_add(scan.bytes_charged);
        self.skipped_oversized_entries = self
            .skipped_oversized_entries
            .saturating_add(scan.skipped_oversized_entries);
        let mut retained = 0usize;
        let mut inspected = 0usize;
        let mut budget_exhausted = false;
        let mut kept: Vec<PathBuf> = Vec::new();
        for path in scan.paths.drain(..).skip(skip) {
            if retained >= remaining {
                budget_exhausted = true;
                break;
            }
            inspected += 1;
            if self.seen.insert(path.clone()) {
                kept.push(path);
                retained += 1;
            }
        }
        // Within-bucket presentation stays newest-first by name; retention is
        // decided by the directory-order slice above.
        kept.sort_unstable_by(|a, b| b.cmp(a));
        self.paths.extend(kept);
        let complete = scan.truncated.is_none() && !budget_exhausted;
        if !complete {
            self.truncated = Some(
                scan.truncated
                    .unwrap_or(crate::runtime::source::FileDiscoveryLimit::FileCount),
            );
        }
        BucketScanOutcome {
            complete,
            inspected,
        }
    }

    fn truncated_by_recent_budget(&self) -> bool {
        self.truncated.is_some()
    }

    fn is_full(&self, max_total_files: usize) -> bool {
        self.paths.len() >= max_total_files.min(self.bounds.max_files)
            || self.bytes_charged >= self.bounds.max_discovery_bytes
    }

    fn enter_history_phase(&mut self) {
        self.recent_phase_truncated = self.truncated.is_some();
        self.truncated = None;
    }

    fn into_report(self, sweep_complete: bool, files_considered: u64) -> FileDiscoveryReport {
        let truncated = if sweep_complete {
            None
        } else {
            self.truncated.or_else(|| {
                self.recent_phase_truncated
                    .then_some(crate::runtime::source::FileDiscoveryLimit::FileCount)
            })
        };
        FileDiscoveryReport {
            paths: self.paths,
            truncated,
            skipped_oversized_entries: self.skipped_oversized_entries,
            bytes_charged: self.bytes_charged,
            // Exact tree size: every jsonl in enumerated buckets, including
            // the unvisited backlog this pass did not retain.
            files_considered,
        }
    }
}

impl TranscriptSource for CodexSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.discover_transcript_paths(project_root, TranscriptDiscoveryBounds::default_walk())
            .paths
    }

    fn discover_transcript_paths(
        &self,
        _project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        // Recent-first over both the dated tree and the flat archive under one
        // shared budget. Callers without a durable rotation frontier start the
        // historical rotation at zero; the scheduler-driven catch-up threads
        // the persisted frontier through the rotation-aware pass instead.
        self.discover_transcript_paths_with_rotation(bounds, 0)
            .report
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        // `session_meta` (line 1) is authoritative for session identity and the
        // initial cwd. Later context records can move one rollout between scopes.
        let meta = session_meta(path)?;
        if self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .is_some_and(|session_id| session_id != meta.session_id)
        {
            return None;
        }

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let mut messages = Vec::new();
        // Collapses identical consecutive goal states within this parse pass:
        // `thread_goal_updated` fires on every token/time tick, so only an
        // objective- or status-change opens a new `goal` row.
        let mut last_goal_key: Option<(String, Option<String>)> = None;
        let mut structured = events::CodexStructuredState::new();
        // Namespacing follows the stored cursor generation, so every batch of a
        // rewritten file is namespaced; prior-context recovery follows this
        // batch's own resume point, which is zero only at the file head.
        let namespace_replacement = new.replacement_generation;
        let mut context_state = if new.start_offset > 0 {
            CodexContextState::scan_prior(path, new.start_offset, &meta)
        } else {
            CodexContextState::from_meta(&meta)
        };
        let scope_matcher = TranscriptScopeMatcher::for_scope_cached(
            project_root,
            self.user_scope
                .as_ref()
                .map(|scope| scope.registered_roots.as_slice()),
            &self.project_matchers,
        );
        let mut last_in_scope_cwd = None;
        let mut last_in_scope_git = None;
        let push_annotated = |messages: &mut Vec<_>,
                              mut message,
                              cwd: Option<&Path>,
                              git: Option<&serde_json::Value>| {
            context::annotate_message(&mut message, cwd, git, &self.project_matchers);
            messages.push(message);
        };
        for line in &new.lines {
            let is_context_record = context_state.observe_context_record(&line.value, path, &meta);
            // `Unknown` means a bounded git timeout left this record's scope
            // undecided: abort before any cursor can be persisted so the same
            // bytes are re-parsed (and re-resolved) on the next scan pass.
            let in_scope = match scope_matcher.membership(context_state.cwd.as_deref()) {
                ProjectMembership::Match => true,
                ProjectMembership::NoMatch => false,
                ProjectMembership::Unknown => return None,
            };
            if !in_scope {
                if compacted_summary_from_line(
                    &line.value,
                    &meta,
                    context_state.model.as_deref(),
                    path,
                    line.offset,
                    context_state.compaction_depth + 1,
                )
                .is_some()
                {
                    context_state.compaction_depth += 1;
                }
                continue;
            }
            last_in_scope_cwd.clone_from(&context_state.cwd);
            last_in_scope_git.clone_from(&context_state.git);
            let cwd = context_state.cwd.as_deref();
            let git = context_state.git.as_ref();
            // Non-consuming: harvest session-level policy/effort/rate-limit
            // summary before the line is routed to its owning handler below.
            structured.observe_summary(&line.value);
            if is_context_record {
                continue;
            }
            if let Some(rows) = structured.event_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                for message in rows {
                    push_annotated(&mut messages, message, cwd, git);
                }
                continue;
            }
            if let Some(event) = codex_goal_event_from_line(&line.value) {
                let key = event.dedup_key();
                if last_goal_key.as_ref() == Some(&key) {
                    continue;
                }
                last_goal_key = Some(key);
                push_annotated(
                    &mut messages,
                    goal_event_message(
                        &meta,
                        context_state.model.as_deref(),
                        path,
                        line.offset,
                        timestamp_from_record(&line.value),
                        &event,
                    ),
                    cwd,
                    git,
                );
                continue;
            }
            if let Some(message) = response_item_goal_context_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                continue;
            }
            if let Some(message) = response_item_tool_event_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                continue;
            }
            if let Some(message) = compacted_summary_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
                context_state.compaction_depth + 1,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                context_state.compaction_depth += 1;
                continue;
            }
            if let Some(message) = goal_context_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                continue;
            }
            if let Some(message) = message_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
            }
        }
        // Emit any `exec_command` calls whose paired output never arrived in
        // this pass so the tool call is not silently dropped.
        for message in structured.flush_pending(&meta, path) {
            push_annotated(
                &mut messages,
                message,
                last_in_scope_cwd.as_deref(),
                last_in_scope_git.as_ref(),
            );
        }

        // A truncate-and-rewrite can reuse every byte offset from the previous
        // file generation. Legacy projection keys are offset-based, so keep
        // replacement rows distinct instead of overwriting retained history.
        if namespace_replacement {
            namespace_replacement_message_ids(&mut messages, new.new_cursor.file_id);
        }

        let project = self.user_scope.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            // The summary is session-wide and may include evidence observed
            // after Codex changed cwd into a registered project. User scope
            // stores only the filtered message rows, never that mixed summary.
            metadata_json: context::session_metadata_json(
                &meta,
                self.user_scope.is_none().then_some(&structured.summary),
                &self.project_matchers,
            ),
            parent_session_id: meta.parent_session_id.clone(),
            is_subagent: meta.is_subagent,
            agent_id: meta.agent_id.clone(),
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        preflight_and_parse_new(PROVIDER, path, prev, max_new_bytes, || {
            self.parse_new(path, prev, project_root, max_new_bytes)
        })
    }
}
