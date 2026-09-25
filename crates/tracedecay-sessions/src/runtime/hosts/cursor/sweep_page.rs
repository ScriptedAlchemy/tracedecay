//! Bounded, resumable pages over the Cursor transcript corpus.
//!
//! A catch-up pass sweeps one page: the sessions after the durable resume
//! position, in session-id order, until the discovery file budget is spent.
//! The next pass resumes where this one stopped, and a pass that reaches the
//! last session starts the next lap from the first. Every transcript is
//! therefore visited within one lap however large the corpus is, while each
//! pass admits at most one discovery budget of files.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::runtime::source::{
    TranscriptDiscoveryBounds, collect_files_with_ext_bounded, path_byte_len,
};

use super::{MAX_SWEEP_SCAN_DEPTH, is_subagent_transcript};

/// Durable resume position of the Cursor transcript sweep in its admission
/// scope: the session-order index of the next session to sweep.
pub(super) const CURSOR_SWEEP_FRONTIER_KEY: &str = "tracedecay-internal:cursor-sweep-frontier:v1";

/// How much of the Cursor transcript corpus a sweep pass has covered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::runtime) enum CursorSweepCoverage {
    /// Every session was swept in this lap; the next pass starts a new lap.
    #[default]
    Complete,
    /// Sessions before `resume_at` of `total` are swept in this lap; the next
    /// pass resumes at `resume_at`.
    Continuing { resume_at: u64, total: u64 },
}

impl CursorSweepCoverage {
    pub(in crate::runtime) const fn is_continuing(self) -> bool {
        matches!(self, Self::Continuing { .. })
    }

    /// Sessions this lap has not swept yet.
    pub(in crate::runtime) const fn deferred_sessions(self) -> u64 {
        match self {
            Self::Complete => 0,
            Self::Continuing { resume_at, total } => total.saturating_sub(resume_at),
        }
    }

    /// Resume position to persist after the pass.
    pub(super) const fn next_position(self) -> u64 {
        match self {
            Self::Complete => 0,
            Self::Continuing { resume_at, .. } => resume_at,
        }
    }
}

/// One `agent-transcripts` child owned by a session: the flat `<id>.jsonl`
/// or the `<id>/` directory holding `<id>.jsonl` and `subagents/`.
struct SessionEntry {
    path: PathBuf,
    is_dir: bool,
}

/// The whole swept corpus, listed without descending into session trees.
pub(super) struct CursorSweepCorpus {
    /// Sessions in id order. Copies of one session across projects share a
    /// key, so they always fall in the same page.
    sessions: Vec<Vec<SessionEntry>>,
    /// Stems of every `<session>/subagents/*.jsonl`. Cursor also writes a
    /// top-level `<id>/<id>.jsonl` copy of a subagent session whose content
    /// drifts; both copies share one native session identity, so ingesting
    /// both duplicates observations, overwrites the parent linkage, and
    /// refuses byte-identical repeated lines as identity collisions. The
    /// subagent copy is the authority (it carries parentage, and the live
    /// hook path ingests it). The stems cover the whole corpus because the
    /// two copies usually land in different pages.
    subagent_stems: HashSet<OsString>,
}

/// Lists every session under `transcripts_dirs`.
///
/// ponytail: each pass lists all session names (one `read_dir` per project
/// plus one `subagents/` probe per session directory) to order them; only
/// the page is walked and admitted. Keep the listing O(sessions) names; a
/// corpus where that dominates needs a persisted session index instead.
#[hotpath::measure(label = "sessions.hosts.cursor.sweep_list_sessions")]
pub(super) fn list_cursor_sweep_corpus(transcripts_dirs: &[PathBuf]) -> CursorSweepCorpus {
    let mut sessions = BTreeMap::<OsString, Vec<SessionEntry>>::new();
    let mut subagent_stems = HashSet::new();
    for transcripts_dir in transcripts_dirs {
        let Ok(entries) = std::fs::read_dir(transcripts_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let path = transcripts_dir.join(&name);
            let (session, is_dir) = if kind.is_dir() {
                subagent_stems.extend(jsonl_stems(&path.join("subagents")));
                (name, true)
            } else if path.extension() == Some(OsStr::new("jsonl"))
                && let Some(stem) = path.file_stem()
            {
                (stem.to_os_string(), false)
            } else {
                continue;
            };
            sessions
                .entry(session)
                .or_default()
                .push(SessionEntry { path, is_dir });
        }
    }
    CursorSweepCorpus {
        sessions: sessions.into_values().collect(),
        subagent_stems,
    }
}

fn jsonl_stems(dir: &Path) -> impl Iterator<Item = OsString> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|child| {
            let name = PathBuf::from(child.file_name());
            (name.extension() == Some(OsStr::new("jsonl")))
                .then(|| name.file_stem().map(OsStr::to_os_string))
                .flatten()
        })
}

/// The sessions one sweep pass admits.
pub(super) struct CursorSweepPage {
    /// Transcript files per session, in session order.
    pub(super) sessions: Vec<Vec<PathBuf>>,
    /// Corpus index of the first session in `sessions`.
    pub(super) start: usize,
    /// Sessions in the corpus when the page was cut.
    pub(super) total: usize,
}

impl CursorSweepPage {
    /// Coverage after the pass, given the first session (by corpus index)
    /// whose files it could not finish.
    pub(super) fn coverage(&self, unfinished: Option<usize>) -> CursorSweepCoverage {
        let resume_at = unfinished.unwrap_or(self.start + self.sessions.len());
        if resume_at >= self.total {
            CursorSweepCoverage::Complete
        } else {
            CursorSweepCoverage::Continuing {
                resume_at: u64::try_from(resume_at).unwrap_or(u64::MAX),
                total: u64::try_from(self.total).unwrap_or(u64::MAX),
            }
        }
    }

    pub(super) fn into_paths(self) -> Vec<PathBuf> {
        self.sessions.into_iter().flatten().collect()
    }
}

impl CursorSweepCorpus {
    /// Cuts the page that starts at `resume_at` (or at the first session when
    /// the corpus shrank below it) and spends at most one discovery budget.
    /// A page always holds whole sessions, except that a single session over
    /// the whole budget is admitted truncated so the sweep still advances.
    #[hotpath::measure(label = "sessions.hosts.cursor.sweep_page")]
    pub(super) fn page(
        &self,
        resume_at: u64,
        bounds: TranscriptDiscoveryBounds,
        keep: impl Fn(&Path) -> bool,
    ) -> CursorSweepPage {
        let total = self.sessions.len();
        let start = usize::try_from(resume_at)
            .ok()
            .filter(|&start| start < total)
            .unwrap_or(0);
        let mut remaining_files = bounds.max_files;
        let mut remaining_bytes = bounds.max_discovery_bytes;
        let mut sessions = Vec::new();
        for entries in &self.sessions[start..] {
            let mut files = Vec::new();
            let mut bytes_charged = 0u64;
            let mut fits = true;
            for entry in entries {
                if entry.is_dir {
                    let report = collect_files_with_ext_bounded(
                        &entry.path,
                        "jsonl",
                        MAX_SWEEP_SCAN_DEPTH.saturating_sub(1),
                        TranscriptDiscoveryBounds {
                            max_files: remaining_files.saturating_sub(files.len()),
                            max_discovery_bytes: remaining_bytes.saturating_sub(bytes_charged),
                            ..bounds
                        },
                    );
                    bytes_charged = bytes_charged.saturating_add(report.bytes_charged);
                    fits &= !report.is_truncated();
                    files.extend(report.paths);
                } else {
                    let charge = u64::try_from(path_byte_len(&entry.path)).unwrap_or(u64::MAX);
                    if files.len() >= remaining_files
                        || bytes_charged.saturating_add(charge) > remaining_bytes
                    {
                        fits = false;
                        continue;
                    }
                    bytes_charged = bytes_charged.saturating_add(charge);
                    files.push(entry.path.clone());
                }
            }
            if !fits && !sessions.is_empty() {
                break;
            }
            if !fits {
                tracing::warn!(
                    session = %entries[0].path.display(),
                    max_files = bounds.max_files,
                    "Cursor transcript session exceeds one sweep page; admitting its first page"
                );
            }
            remaining_files = remaining_files.saturating_sub(files.len());
            remaining_bytes = remaining_bytes.saturating_sub(bytes_charged);
            files.retain(|path| {
                (is_subagent_transcript(path)
                    || path
                        .file_stem()
                        .is_none_or(|stem| !self.subagent_stems.contains(stem)))
                    && keep(path)
            });
            sessions.push(files);
            if !fits || remaining_files == 0 || remaining_bytes == 0 {
                break;
            }
        }
        CursorSweepPage {
            sessions,
            start,
            total,
        }
    }
}
