use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tracedecay_capture::normalize_timestamp_secs;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

#[cfg(test)]
use super::attribution::{attribute_commits, publish_graph_evidence};
use super::attribution::{publish_graph_evidence_controlled, stable_backfill_span};
use super::store::GitCorrelationSessionStore;

use super::{
    AUTO_BACKFILL_WATERMARK_KEY, AnalyticsSessionTimestampSource, CommitEvidence, CommitRelation,
    CommitSessionRecord, DEFAULT_SPAN_MERGE_GAP_SECS, GIT_HISTORY_ROWID_FRONTIER_KEY,
    GitCorrelationError, GitCorrelationWriteTxn, ScannedCommit, SessionGitSpan, SpanOverlapKind,
    SpanScanTarget, TargetScan, normalize_worktree,
};

mod bounded;
pub(super) mod history_failures;
pub(super) mod history_progress;
pub use bounded::{
    BoundedBackfillInterruption, BoundedBackfillOutcome, BoundedGitControl,
    GitHistoryIndexFrontier, run_bounded_history_index_page,
};

#[derive(Debug)]
pub(super) struct SessionActivityPageRow {
    pub source_rowid: i64,
    pub activity_timestamp: i64,
    pub session: SessionActivityRow,
}

// Historical backfill for sessions that predate live span recording.

/// One session's declared and message-derived activity bounds, read from the
/// per-project session store. Any field may be `None` when the source row left
/// it unset; [`SessionActivityRow::window`] collapses them into a single
/// `[start, end]` when at least one bound is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionActivityRow {
    pub provider: String,
    pub session_id: String,
    pub project_path: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub message_min_ts: Option<i64>,
    pub message_max_ts: Option<i64>,
}

impl SessionActivityRow {
    /// Coarse `[start, end]` window from the widest pair of known bounds, or
    /// `None` when the session carries no usable timestamp at all. Each bound is
    /// normalized to unix seconds (see [`normalize_timestamp_secs`]) so mixed
    /// seconds/millis rows on legacy stores produce a seconds-scale window.
    pub fn window(&self) -> Option<(i64, i64)> {
        let mut lo: Option<i64> = None;
        let mut hi: Option<i64> = None;
        for ts in [
            self.started_at,
            self.ended_at,
            self.message_min_ts,
            self.message_max_ts,
        ]
        .into_iter()
        .flatten()
        .map(normalize_timestamp_secs)
        {
            lo = Some(lo.map_or(ts, |cur| cur.min(ts)));
            hi = Some(hi.map_or(ts, |cur| cur.max(ts)));
        }
        match (lo, hi) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        }
    }
}

/// One `HEAD` position in a worktree's reflog timeline: the branch `HEAD`
/// pointed at starting from `from_ts`, or `None` for a detached-HEAD checkout
/// (the target was a raw sha, not a branch name).
pub type BranchTimelineEntry = (i64, Option<String>);

/// Reconstructs a worktree's branch timeline from `git reflog --date=unix`
/// output on `HEAD`. Only `checkout: moving from X to Y` entries advance the
/// timeline; each yields `(entry_ts, branch_of(Y))`, where a target that looks
/// like a raw commit sha is treated as detached HEAD (`None`).
///
/// Returned oldest-first (reflog output is newest-first, so this reverses it),
/// which is the order [`window_branch_segments`] expects. Pure: no IO.
pub fn branch_timeline_from_reflog(reflog_text: &str) -> Vec<BranchTimelineEntry> {
    let mut entries: Vec<BranchTimelineEntry> = Vec::new();
    for line in reflog_text.lines() {
        if let Some(entry) = parse_reflog_checkout_line(line) {
            entries.push(entry);
        }
    }
    entries.reverse();
    entries
}

/// Parses one `git reflog --date=unix` line, returning `(ts, branch)` when it
/// is a `checkout: moving from X to Y` entry (`branch` = `None` when `Y` is a
/// detached sha). Non-checkout and unparseable lines return `None`.
///
/// Expected shape: `<sha> HEAD@{<unix>}: checkout: moving from <X> to <Y>`.
fn parse_reflog_checkout_line(line: &str) -> Option<BranchTimelineEntry> {
    let ts_open = line.find("HEAD@{")? + "HEAD@{".len();
    let ts_close = line[ts_open..].find('}')? + ts_open;
    let ts: i64 = line[ts_open..ts_close].trim().parse().ok()?;

    let rest = line.get(ts_close + 1..)?.trim_start();
    let message = rest.strip_prefix(':').map_or(rest, str::trim_start);
    let moving = message.strip_prefix("checkout: moving from ")?;
    // Split on the last ` to ` so a branch name containing ` to ` in X does
    // not confuse the split of the target Y.
    let (_from, to) = moving.rsplit_once(" to ")?;
    let target = to.trim();
    if target.is_empty() {
        return None;
    }
    Some((ts, branch_from_checkout_target(target)))
}

/// Classifies a checkout target: a 7–64 char all-hex token is a detached-HEAD
/// commit (`None`); anything else is treated as a branch name.
fn branch_from_checkout_target(target: &str) -> Option<String> {
    let looks_like_sha =
        (7..=64).contains(&target.len()) && target.chars().all(|c| c.is_ascii_hexdigit());
    if looks_like_sha {
        None
    } else {
        Some(target.to_string())
    }
}

/// One branch segment of a session's activity window: the branch `HEAD`
/// pointed at (per the reflog timeline) over `[start, end]`, clamped to the
/// window. `None` branch = detached HEAD during that stretch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowBranchSegment {
    pub branch: Option<String>,
    pub start: i64,
    pub end: i64,
}

/// Intersects an activity window `[win_start, win_end]` with a worktree's
/// branch `timeline` (oldest-first, from [`branch_timeline_from_reflog`]),
/// yielding the branch segments the session overlapped. The leading stretch,
/// before the first timeline entry that lands after `win_start`, is
/// attributed to `initial_branch` (callers pass the branch `HEAD` currently
/// points at as the floor).
///
/// Pure: no IO. Segments are clamped to the window and returned oldest-first.
pub fn window_branch_segments(
    win_start: i64,
    win_end: i64,
    timeline: &[BranchTimelineEntry],
    initial_branch: Option<&str>,
) -> Vec<WindowBranchSegment> {
    if win_start > win_end {
        return Vec::new();
    }
    let mut current_branch: Option<String> = initial_branch.map(str::to_string);
    // Advance to the last timeline entry at or before win_start; that entry's
    // target is the branch HEAD held when the window opened.
    let mut idx = 0;
    while idx < timeline.len() && timeline[idx].0 <= win_start {
        current_branch.clone_from(&timeline[idx].1);
        idx += 1;
    }
    let mut segments: Vec<WindowBranchSegment> = Vec::new();
    let mut seg_start = win_start;
    while idx < timeline.len() && timeline[idx].0 <= win_end {
        let (change_ts, next_branch) = &timeline[idx];
        // Only a *real* branch change ends the current segment; a checkout that
        // lands back on the same branch (or reflog noise) must not fragment it.
        if *next_branch != current_branch && *change_ts > seg_start {
            segments.push(WindowBranchSegment {
                branch: current_branch.clone(),
                start: seg_start,
                end: *change_ts,
            });
            seg_start = *change_ts;
        }
        current_branch.clone_from(next_branch);
        idx += 1;
    }
    if seg_start <= win_end {
        segments.push(WindowBranchSegment {
            branch: current_branch,
            start: seg_start,
            end: win_end,
        });
    }
    segments
}

/// Reason a session was skipped by the backfill (counted and reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillSkipReason {
    /// Session had no usable timestamp in any signal source.
    NoActivityWindow,
    /// `project_path` was empty or not a resolvable git worktree.
    NotAWorktree,
    /// A git command for this session's repo failed; failed open.
    GitError,
}

/// Tunables for [`run_backfill`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillOptions {
    /// Inclusive lower bound (unix seconds) on session activity and commit
    /// times. Sessions whose activity ends before this are skipped.
    pub since: i64,
    /// Maximum number of sessions to scan.
    pub limit_sessions: usize,
    /// Span merge gap forwarded to `record_span_observation`.
    pub merge_gap_secs: i64,
    /// Hard cap on commits parsed from a single `git log` invocation.
    pub max_commits_per_repo: usize,
    /// When true, derive and count everything but write nothing.
    pub dry_run: bool,
}

impl Default for BackfillOptions {
    fn default() -> Self {
        Self {
            since: 0,
            limit_sessions: 500,
            merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
            max_commits_per_repo: 5_000,
            dry_run: false,
        }
    }
}

/// Outcome counters for one backfill run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillStats {
    pub sessions_scanned: usize,
    pub spans_written: usize,
    pub commits_attributed: usize,
    pub skipped_no_window: usize,
    pub skipped_not_worktree: usize,
    pub skipped_git_error: usize,
    /// Retained spans whose archived branch no longer resolves. Their observed
    /// identity remains authoritative; only inferred historical attribution is
    /// unavailable.
    pub unavailable_attributions: usize,
    /// Whether this pass durably advanced the incremental session tuple.
    pub frontier_advanced: bool,
}

impl BackfillStats {
    fn record_skip(&mut self, reason: BackfillSkipReason) {
        match reason {
            BackfillSkipReason::NoActivityWindow => self.skipped_no_window += 1,
            BackfillSkipReason::NotAWorktree => self.skipped_not_worktree += 1,
            BackfillSkipReason::GitError => self.skipped_git_error += 1,
        }
    }

    #[hotpath::skip]
    pub const fn skipped_total(&self) -> usize {
        self.skipped_no_window
            + self.skipped_not_worktree
            + self.skipped_git_error
            + self.unavailable_attributions
    }

    /// Whether this pass durably published evidence or advanced its source
    /// tuple. Skip counters alone are observations, not committed progress.
    pub const fn committed_progress(&self) -> bool {
        self.frontier_advanced || self.spans_written > 0 || self.commits_attributed > 0
    }
}

/// Abstracts the git subprocess surface the backfill needs, so tests can run
/// the core against a real repo ([`SystemGit`]) or a canned fixture.
///
/// `Send + Sync` so a `&dyn GitReflogSource` can be held across an `.await`
/// inside a spawned task (the startup auto-backfill runs on a tokio worker).
pub trait GitReflogSource: Send + Sync {
    /// Proves that this source has no history to publish. Sources without an
    /// unborn-repository authority retain normal reflog reads and error handling.
    fn has_verified_empty_history(
        &self,
        _worktree: &std::path::Path,
    ) -> Result<bool, GitCorrelationError> {
        Ok(false)
    }
    /// `git reflog --date=unix HEAD` text for `worktree`, or `None` on error.
    fn reflog(&self, worktree: &std::path::Path) -> Option<String>;
    /// The branch `HEAD` currently points at in `worktree` (`None` = detached
    /// or unknown), used as the leading-segment floor.
    fn current_branch(&self, worktree: &std::path::Path) -> Option<String>;
    /// Whether `reference` still resolves to a commit. Archived transcript
    /// branches may have been deleted while their observed identity remains
    /// valid evidence.
    fn commit_reference_exists(
        &self,
        _worktree: &std::path::Path,
        _reference: &str,
    ) -> Result<bool, GitCorrelationError>;
    /// `git log <branch> --pretty=%H %ct --since=<since>` text for `worktree`,
    /// newest-first. `None` on error.
    fn commit_log(&self, worktree: &std::path::Path, branch: &str, since: i64) -> Option<String>;
}

/// Real git-subprocess implementation of [`GitReflogSource`].
pub struct SystemGit;

impl SystemGit {
    fn output(worktree: &std::path::Path, args: &[&str]) -> Option<String> {
        let output = tracedecay_runtime_core::git::git_output(worktree, args)?;
        String::from_utf8(output.stdout).ok()
    }
}

pub fn git_commit_reference_exists(
    worktree: &std::path::Path,
    reference: &str,
) -> Result<bool, GitCorrelationError> {
    let resolved = format!("{reference}^{{commit}}");
    let output = tracedecay_runtime_core::git::bounded_git_output(
        worktree,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &resolved,
        ],
        &tracedecay_runtime_core::git::GitCommandBounds::default(),
    )
    .map_err(|error| GitCorrelationError::Unavailable(error.to_string()))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => {
            let detail = String::from_utf8_lossy(&output.stderr);
            Err(GitCorrelationError::Unavailable(format!(
                "cannot resolve Git commit reference {reference:?}: {}",
                detail.trim()
            )))
        }
    }
}

impl GitReflogSource for SystemGit {
    fn has_verified_empty_history(
        &self,
        worktree: &std::path::Path,
    ) -> Result<bool, GitCorrelationError> {
        bounded::verified_empty_history(worktree).map_err(|interruption| {
            GitCorrelationError::Unavailable(format!(
                "Git history source verification failed: {interruption:?}"
            ))
        })
    }

    fn reflog(&self, worktree: &std::path::Path) -> Option<String> {
        Self::output(worktree, &["reflog", "--date=unix", "HEAD"])
    }

    fn current_branch(&self, worktree: &std::path::Path) -> Option<String> {
        let raw = Self::output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "HEAD" {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn commit_reference_exists(
        &self,
        worktree: &std::path::Path,
        reference: &str,
    ) -> Result<bool, GitCorrelationError> {
        git_commit_reference_exists(worktree, reference)
    }

    fn commit_log(&self, worktree: &std::path::Path, branch: &str, since: i64) -> Option<String> {
        // --since parses approximate dates: small epoch values become wall-clock
        // times. --max-age accepts the raw timestamp; Git dates are unsigned.
        Self::output(
            worktree,
            &[
                "log",
                branch,
                "--pretty=%H %ct",
                &format!("--max-age={}", since.max(0)),
            ],
        )
    }
}

/// Parses `git log --pretty=%H %ct` output into `(sha, committed_at)` pairs,
/// capping at `max`. Malformed and non-hex lines are skipped. Pure.
pub fn parse_commit_log(log_text: &str, max: usize) -> Vec<(String, i64)> {
    let mut commits = Vec::new();
    for line in log_text.lines() {
        if commits.len() >= max {
            break;
        }
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else { continue };
        let Some(ts) = parts.next().and_then(|t| t.parse::<i64>().ok()) else {
            continue;
        };
        if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            commits.push((sha.to_ascii_lowercase(), ts));
        }
    }
    commits
}

/// Runs the historical backfill against one project's session store.
///
/// `session_store` is the per-project sessions authority (already open, and,
/// for a real run, writable). `analytics_events` contribute only
/// provider/session timestamps (via [`AnalyticsSessionTimestampSource`]);
/// branch data is never assumed present. `git` supplies the reflog/log
/// subprocess surface. A broken repo or session is counted and skipped; the
/// derived evidence publishes as one generation.
///
/// When `opts.dry_run` is set no rows are written; the returned counts reflect
/// what *would* have been written.
#[hotpath::measure(label = "sessions.git_correlation.backfill", future = true)]
pub async fn run_backfill<S, E, G>(
    session_store: &S,
    analytics_events: &[E],
    git: &G,
    opts: &BackfillOptions,
) -> Result<BackfillStats, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    E: AnalyticsSessionTimestampSource,
    G: GitReflogSource + ?Sized,
{
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let rows = session_activity_rows(&snapshot, opts.limit_sessions)
        .await
        .map_err(GitCorrelationError::Db)?;
    drop(snapshot);
    let derived = derive_backfill_evidence(git, opts, &rows, analytics_events);
    let mut stats = derived.stats;
    if opts.dry_run {
        stats.spans_written = derived.spans.len();
        stats.commits_attributed = derived.commits.len();
    } else if !derived.spans.is_empty() || !derived.commits.is_empty() {
        (stats.spans_written, stats.commits_attributed) = session_store
            .publish_graph_evidence_owned("git-backfill".to_owned(), derived.spans, derived.commits)
            .await?;
    }
    crate::runtime::pipeline_metrics::record_git_backfill(
        stats.sessions_scanned,
        stats.spans_written,
    );
    Ok(stats)
}

/// Session-history evidence derived for every session past the durable
/// frontier. Nothing is written: the convergence pass publishes it in its one
/// generation and only then advances the frontier to `settled_through`.
#[derive(Debug)]
pub struct CollectedBackfill {
    pub spans: Vec<SessionGitSpan>,
    pub commits: Vec<CommitSessionRecord>,
    pub stats: BackfillStats,
    /// Durable frontier the collection started from.
    pub start: GitHistoryIndexFrontier,
    /// Last session tuple before any transient Git failure; `None` when no
    /// session past `start` settled.
    pub settled_through: Option<GitHistoryIndexFrontier>,
}

/// Derives span and commit evidence for every retained session newer than the
/// durable `(activity, rowid)` frontier, oldest first. Evidence stops at the
/// first transient Git failure so the frontier never passes an unresolved
/// session; permanent exclusions (no activity window, not a worktree,
/// verified empty history) settle without evidence.
#[hotpath::measure(label = "sessions.git_correlation.backfill.collect", future = true)]
pub async fn collect_incremental_backfill<S, G>(
    session_store: &S,
    git: &G,
) -> Result<CollectedBackfill, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    G: GitReflogSource + ?Sized,
{
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let start = GitHistoryIndexFrontier {
        activity_timestamp: super::read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY)
            .await?
            .unwrap_or(0),
        source_rowid: super::read_meta_value(&snapshot, GIT_HISTORY_ROWID_FRONTIER_KEY)
            .await?
            .unwrap_or(0),
    };
    // The whole backlog in one read: it all folds into one generation, and a
    // bounded page would repeat the grouped session scan once per page.
    let page = session_activity_page_after(
        &snapshot,
        start.activity_timestamp,
        start.source_rowid,
        usize::MAX,
    )
    .await
    .map_err(GitCorrelationError::Db)?;
    drop(snapshot);
    let (frontiers, rows): (Vec<_>, Vec<_>) = page
        .into_iter()
        .map(|row| {
            (
                GitHistoryIndexFrontier {
                    activity_timestamp: row.activity_timestamp,
                    source_rowid: row.source_rowid,
                },
                row.session,
            )
        })
        .unzip();
    // `since` is left at 0: the query already excludes anything at or below
    // the frontier, so a second time floor would only drop legitimate spans.
    let opts = BackfillOptions {
        since: 0,
        limit_sessions: rows.len(),
        merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
        max_commits_per_repo: BackfillOptions::default().max_commits_per_repo,
        dry_run: false,
    };
    let no_analytics: &[super::AnalyticsSessionTimestamp] = &[];
    let derived = derive_backfill_evidence(git, &opts, &rows, no_analytics);
    let settled_through = derived
        .settled_rows
        .checked_sub(1)
        .and_then(|index| frontiers.get(index))
        .copied();
    Ok(CollectedBackfill {
        spans: derived.spans,
        commits: derived.commits,
        stats: derived.stats,
        start,
        settled_through,
    })
}

pub(super) async fn advance_history_frontier(
    transaction: &(impl Executor + ?Sized),
    candidate: GitHistoryIndexFrontier,
) -> Result<GitHistoryIndexFrontier, GitCorrelationError> {
    let current = GitHistoryIndexFrontier {
        activity_timestamp: super::read_meta_value(transaction, AUTO_BACKFILL_WATERMARK_KEY)
            .await?
            .unwrap_or(0),
        source_rowid: super::read_meta_value(transaction, GIT_HISTORY_ROWID_FRONTIER_KEY)
            .await?
            .unwrap_or(0),
    };
    if (candidate.activity_timestamp, candidate.source_rowid)
        <= (current.activity_timestamp, current.source_rowid)
    {
        return Ok(current);
    }
    super::write_meta_value(
        transaction,
        AUTO_BACKFILL_WATERMARK_KEY,
        candidate.activity_timestamp,
    )
    .await?;
    super::write_meta_value(
        transaction,
        GIT_HISTORY_ROWID_FRONTIER_KEY,
        candidate.source_rowid,
    )
    .await?;
    Ok(candidate)
}

/// Scans one span target's branch history through the backfill's git source,
/// mirroring the ingest-time sweep's scanner: commits on the recorded branch
/// (or `HEAD` for detached spans) inside the gap-widened span window. Reports
/// [`TargetScan::MissingReference`] for an archived branch that no longer
/// exists, and [`TargetScan::Unavailable`] for a missing worktree or Git
/// failure that a later sweep may recover.
pub(super) fn scan_span_target<G: GitReflogSource + ?Sized>(
    git: &G,
    target: &SpanScanTarget,
    gap_secs: i64,
    max_commits: usize,
) -> TargetScan {
    let worktree = std::path::Path::new(&target.worktree);
    if !worktree.is_dir() {
        return TargetScan::Unavailable;
    }
    let since = target.window_start.saturating_sub(gap_secs);
    let until = target.window_end.saturating_add(gap_secs);
    let branch = target
        .branch
        .as_deref()
        .filter(|branch| !branch.is_empty())
        .unwrap_or("HEAD");
    match git.commit_reference_exists(worktree, branch) {
        Ok(true) => {}
        Ok(false) => return TargetScan::MissingReference,
        Err(_) => return TargetScan::Unavailable,
    }
    let Some(log_text) = git.commit_log(worktree, branch, since) else {
        return TargetScan::Unavailable;
    };
    TargetScan::Scanned(
        parse_commit_log(&log_text, max_commits)
            .into_iter()
            .filter(|&(_, committed_at)| committed_at <= until)
            .map(|(sha, committed_at)| ScannedCommit { sha, committed_at })
            .collect(),
    )
}

/// Evidence derived for a run of session rows, before publication.
struct DerivedBackfill {
    spans: Vec<SessionGitSpan>,
    commits: Vec<CommitSessionRecord>,
    stats: BackfillStats,
    /// Rows before the first transient Git failure. Only these settled.
    settled_rows: usize,
}

/// HEAD's branch history for one worktree root.
struct WorktreeTimeline {
    timeline: Vec<BranchTimelineEntry>,
    current_branch: Option<String>,
}

/// One session's branch segments on its resolved worktree.
struct PlannedSession<'r> {
    row: &'r SessionActivityRow,
    worktree_root: std::path::PathBuf,
    worktree: String,
    segments: Vec<WindowBranchSegment>,
    analytics_within: Vec<i64>,
}

/// Git state one derivation reads once per worktree root: which root a
/// project path resolves to and HEAD's branch timeline there. Every session
/// of a pass observes the same repository snapshot, so re-running these
/// subprocesses per session only multiplies the pass.
struct WorktreeHistories<'g, G: ?Sized> {
    git: &'g G,
    roots: HashMap<String, Option<std::path::PathBuf>>,
    timelines: HashMap<std::path::PathBuf, Option<Arc<WorktreeTimeline>>>,
}

impl<'g, G: GitReflogSource + ?Sized> WorktreeHistories<'g, G> {
    fn new(git: &'g G) -> Self {
        Self {
            git,
            roots: HashMap::new(),
            timelines: HashMap::new(),
        }
    }

    fn worktree_root(&mut self, project_path: &str) -> Option<std::path::PathBuf> {
        self.roots
            .entry(project_path.to_owned())
            .or_insert_with(|| {
                tracedecay_runtime_core::worktree::git_worktree_root(std::path::Path::new(
                    project_path,
                ))
            })
            .clone()
    }

    /// `Ok(None)` is verified empty history: nothing to publish, and nothing
    /// to retry.
    fn timeline(
        &mut self,
        worktree_root: &std::path::Path,
    ) -> Result<Option<Arc<WorktreeTimeline>>, BackfillSkipReason> {
        if let Some(known) = self.timelines.get(worktree_root) {
            return Ok(known.clone());
        }
        let timeline = if self
            .git
            .has_verified_empty_history(worktree_root)
            .map_err(|_| BackfillSkipReason::GitError)?
        {
            None
        } else {
            let reflog = self
                .git
                .reflog(worktree_root)
                .ok_or(BackfillSkipReason::GitError)?;
            Some(Arc::new(WorktreeTimeline {
                timeline: branch_timeline_from_reflog(&reflog),
                current_branch: self.git.current_branch(worktree_root),
            }))
        };
        self.timelines
            .insert(worktree_root.to_path_buf(), timeline.clone());
        Ok(timeline)
    }
}

/// Derives span and commit evidence for `rows` in order, stopping at the
/// first transient Git failure. Each `(worktree, branch)` commit log is read
/// once, from the earliest segment start any settled session needs; every
/// segment then keeps only the commits inside its own window, exactly what a
/// per-segment log read from that segment's start would have yielded.
fn derive_backfill_evidence<E, G>(
    git: &G,
    opts: &BackfillOptions,
    rows: &[SessionActivityRow],
    analytics_events: &[E],
) -> DerivedBackfill
where
    E: AnalyticsSessionTimestampSource,
    G: GitReflogSource + ?Sized,
{
    // Index analytics timestamps by (provider, session_id) for O(1) lookup.
    let mut analytics_ts: HashMap<(String, String), Vec<i64>> = HashMap::new();
    for event in analytics_events {
        if let Some(timestamp) = event.as_analytics_session_timestamp() {
            analytics_ts
                .entry((timestamp.provider, timestamp.session_id))
                .or_default()
                .push(timestamp.timestamp);
        }
    }

    let mut stats = BackfillStats::default();
    let mut histories = WorktreeHistories::new(git);
    let mut planned = Vec::new();
    let mut settled_rows = rows.len();
    for (index, row) in rows.iter().enumerate() {
        stats.sessions_scanned += 1;
        match plan_session(row, opts, &analytics_ts, &mut histories) {
            Ok(Some(plan)) => planned.push((index, plan)),
            Ok(None) => {}
            Err(reason) => {
                stats.record_skip(reason);
                if reason == BackfillSkipReason::GitError {
                    settled_rows = index;
                    break;
                }
            }
        }
    }

    let mut log_reads: BTreeMap<(std::path::PathBuf, String), (i64, usize)> = BTreeMap::new();
    for (index, plan) in &planned {
        for segment in &plan.segments {
            if let Some(branch) = &segment.branch {
                log_reads
                    .entry((plan.worktree_root.clone(), branch.clone()))
                    .and_modify(|(since, _)| *since = (*since).min(segment.start))
                    .or_insert((segment.start, *index));
            }
        }
    }
    let mut logs = HashMap::new();
    for ((worktree_root, branch), (since, first_row)) in log_reads {
        match git.commit_log(&worktree_root, &branch, since) {
            Some(log_text) => {
                let commits = parse_commit_log(&log_text, opts.max_commits_per_repo);
                logs.insert((worktree_root, branch), commits);
            }
            None if first_row < settled_rows => {
                if settled_rows == rows.len() {
                    stats.record_skip(BackfillSkipReason::GitError);
                }
                settled_rows = first_row;
            }
            None => {}
        }
    }

    let mut spans = Vec::new();
    let mut commits = Vec::new();
    for (_, plan) in planned.iter().filter(|(index, _)| *index < settled_rows) {
        let row = plan.row;
        for segment in &plan.segments {
            // Every segment yields a span seeded with its own clamped edges,
            // so an interior segment (a mid-session branch switch) is
            // recorded even when the window edges fall outside it.
            let mut span = stable_backfill_span(
                &row.provider,
                &row.session_id,
                segment.branch.as_deref(),
                &plan.worktree,
                segment.start,
                segment.end,
            );
            let analytics_inside = plan
                .analytics_within
                .iter()
                .filter(|&&ts| ts >= segment.start && ts <= segment.end)
                .count();
            span.event_count =
                i64::try_from(analytics_inside.saturating_add(2)).unwrap_or(i64::MAX);
            spans.push(span);

            let Some(branch) = segment.branch.as_deref() else {
                continue;
            };
            let Some(log) = logs.get(&(plan.worktree_root.clone(), branch.to_owned())) else {
                continue;
            };
            for (sha, committed_at) in log {
                if *committed_at < segment.start || *committed_at > segment.end {
                    continue;
                }
                commits.push(CommitSessionRecord {
                    commit_sha: sha.clone(),
                    provider: row.provider.clone(),
                    session_id: row.session_id.clone(),
                    branch: Some(branch.to_string()),
                    worktree: Some(plan.worktree.clone()),
                    committed_at: *committed_at,
                    span_overlap_kind: SpanOverlapKind::WithinSpan,
                    span_id: None,
                    relation: CommitRelation::Observed,
                    evidence: CommitEvidence::ReflogOverlap,
                    confidence: 30,
                    evidence_message_id: None,
                });
            }
        }
    }
    DerivedBackfill {
        spans,
        commits,
        stats,
        settled_rows,
    }
}

/// Resolves one session's worktree and branch segments. `Ok(None)` settles a
/// session on verified empty history without evidence.
fn plan_session<'r, G: GitReflogSource + ?Sized>(
    row: &'r SessionActivityRow,
    opts: &BackfillOptions,
    analytics_ts: &HashMap<(String, String), Vec<i64>>,
    histories: &mut WorktreeHistories<'_, G>,
) -> Result<Option<PlannedSession<'r>>, BackfillSkipReason> {
    let (mut win_start, win_end) = row.window().ok_or(BackfillSkipReason::NoActivityWindow)?;
    if win_end < opts.since {
        return Err(BackfillSkipReason::NoActivityWindow);
    }
    win_start = win_start.max(opts.since);
    if win_start > win_end {
        return Err(BackfillSkipReason::NoActivityWindow);
    }
    let project_path = row.project_path.trim();
    if project_path.is_empty() {
        return Err(BackfillSkipReason::NotAWorktree);
    }
    let worktree_root = histories
        .worktree_root(project_path)
        .ok_or(BackfillSkipReason::NotAWorktree)?;
    let Some(history) = histories.timeline(&worktree_root)? else {
        return Ok(None);
    };
    // Analytics event times inside the (since-clamped) window refine span
    // boundaries within a segment.
    let analytics_within = analytics_ts
        .get(&(row.provider.clone(), row.session_id.clone()))
        .map(|times| {
            times
                .iter()
                .copied()
                .filter(|&ts| ts >= win_start && ts <= win_end)
                .collect()
        })
        .unwrap_or_default();
    let segments = window_branch_segments(
        win_start,
        win_end,
        &history.timeline,
        history.current_branch.as_deref(),
    );
    Ok(Some(PlannedSession {
        row,
        worktree: normalize_worktree(&worktree_root.to_string_lossy()),
        worktree_root,
        segments,
        analytics_within,
    }))
}

/// Reads per-session activity windows for the backfill from a project-sessions
/// snapshot opened through [`GitCorrelationStore`].
pub(super) async fn session_activity_rows(
    conn: &(impl QueryExecutor + ?Sized),
    limit: usize,
) -> Result<Vec<SessionActivityRow>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT s.provider, s.session_id, s.project_path,
                    s.started_at, s.ended_at,
                    MIN(m.timestamp), MAX(m.timestamp)
             FROM sessions s
             LEFT JOIN lcm_raw_messages m
                    ON m.provider = s.provider AND m.session_id = s.session_id
             GROUP BY s.provider, s.session_id
             ORDER BY COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) DESC
             LIMIT ?1",
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
        .map_err(|e| format!("failed to query session activity rows: {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("failed to read session activity row: {e}"))?
    {
        out.push(decode_session_activity_row(&row)?);
    }
    Ok(out)
}

pub(super) async fn session_activity_page_after(
    conn: &(impl QueryExecutor + ?Sized),
    activity_timestamp: i64,
    source_rowid: i64,
    limit: usize,
) -> Result<Vec<SessionActivityPageRow>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT s.provider, s.session_id, s.project_path,
                    s.started_at, s.ended_at,
                    MIN(m.timestamp), MAX(m.timestamp),
                    s.rowid,
                    COALESCE(MAX(m.timestamp), s.ended_at, s.started_at)
             FROM sessions s
             LEFT JOIN lcm_raw_messages m
                    ON m.provider = s.provider AND m.session_id = s.session_id
             GROUP BY s.rowid, s.provider, s.session_id
             HAVING COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) > ?1
                 OR (
                    COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) = ?1
                    AND s.rowid > ?2
                 )
             ORDER BY COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) ASC,
                      s.rowid ASC
             LIMIT ?3",
            params![
                activity_timestamp,
                source_rowid,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
        )
        .await
        .map_err(|error| format!("failed to query git history session page: {error}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("failed to read git history session page: {error}"))?
    {
        out.push(SessionActivityPageRow {
            source_rowid: row
                .get(7)
                .map_err(|error| format!("failed to decode session rowid: {error}"))?,
            activity_timestamp: row
                .get(8)
                .map_err(|error| format!("failed to decode session activity: {error}"))?,
            session: decode_session_activity_row(&row)?,
        });
    }
    Ok(out)
}

/// Decodes one `session_activity_rows*` result row into a [`SessionActivityRow`].
fn decode_session_activity_row(row: &Row) -> Result<SessionActivityRow, String> {
    Ok(SessionActivityRow {
        provider: row
            .get(0)
            .map_err(|e| format!("failed to decode provider: {e}"))?,
        session_id: row
            .get(1)
            .map_err(|e| format!("failed to decode session_id: {e}"))?,
        project_path: row
            .get(2)
            .map_err(|e| format!("failed to decode project_path: {e}"))?,
        started_at: row
            .get(3)
            .map_err(|e| format!("failed to decode started_at: {e}"))?,
        ended_at: row
            .get(4)
            .map_err(|e| format!("failed to decode ended_at: {e}"))?,
        message_min_ts: row
            .get(5)
            .map_err(|e| format!("failed to decode message_min_ts: {e}"))?,
        message_max_ts: row
            .get(6)
            .map_err(|e| format!("failed to decode message_max_ts: {e}"))?,
    })
}
