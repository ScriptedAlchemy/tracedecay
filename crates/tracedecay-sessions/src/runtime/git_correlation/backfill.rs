use tracedecay_runtime_core::db::engine::{QueryExecutor, Row, params};

use super::store::GitCorrelationSessionStore;

use super::{
    AUTO_BACKFILL_WATERMARK_KEY, AnalyticsSessionTimestampSource, CommitAttributionPublication,
    CommitEvidence, CommitRelation, CommitSessionRecord, DEFAULT_SPAN_MERGE_GAP_SECS,
    GitCorrelationError, GitCorrelationWriteTxn, GitScanFailure, SpanObservation, SpanOverlapKind,
    SpanScanTarget, SpanSource, TargetScan, normalize_worktree, parse_bounded_git_log,
    prepare_commit_attribution_sweep, publish_commit_attribution_plan_to_store,
    scan_commit_attribution_plan,
};

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

/// Millisecond/second boundary for stored session timestamps: any value at or
/// above this is treated as unix millis and divided by 1000 (mirrors
/// `RegisteredGlobalDb::latest_session_activity_secs` and
/// `kiro::normalize_timestamp`).
const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;
const BACKFILL_PUBLICATION_CHUNK: usize = 32;
const MAX_REFLOG_ENTRIES: usize = 10_000;

/// Normalizes provider timestamps to unix seconds.
fn normalize_activity_ts(ts: i64) -> i64 {
    if ts >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
        ts / 1000
    } else {
        ts
    }
}

impl SessionActivityRow {
    /// Coarse `[start, end]` window from the widest pair of known bounds, or
    /// `None` when the session carries no usable timestamp at all. Each bound is
    /// normalized to unix seconds (see [`normalize_activity_ts`]) so mixed
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
        .map(normalize_activity_ts)
        {
            lo = Some(lo.map_or(ts, |cur| cur.min(ts)));
            hi = Some(hi.map_or(ts, |cur| cur.max(ts)));
        }
        match (lo, hi) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        }
    }

    /// The activity timestamp the incremental backfill orders and watermarks by:
    /// the newest message time, else the declared end, else the start. Mirrors
    /// the `COALESCE(MAX(m.timestamp), s.ended_at, s.started_at)` key used by
    /// [`session_activity_rows_since`], so the returned value compares directly
    /// against the persisted watermark (both are raw, un-normalized bounds).
    pub fn activity_sort_key(&self) -> Option<i64> {
        self.message_max_ts.or(self.ended_at).or(self.started_at)
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
/// yielding the branch segments the session overlapped. The leading stretch —
/// before the first timeline entry that lands after `win_start` — is
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

impl BackfillSkipReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoActivityWindow => "no_activity_window",
            Self::NotAWorktree => "not_a_worktree",
            Self::GitError => "git_error",
        }
    }
}

/// Tunables for [`run_backfill`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillOptions {
    /// Inclusive lower bound (unix seconds) on session activity and commit
    /// times. Sessions whose activity ends before this are skipped.
    pub since: i64,
    /// Maximum number of sessions to scan.
    pub limit_sessions: usize,
    /// Span merge gap forwarded to [`record_span_observation`].
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
    pub git_reflog_calls: usize,
    pub git_current_branch_calls: usize,
    pub git_log_calls: usize,
    pub max_writer_hold_micros: u64,
}

impl BackfillStats {
    fn record_skip(&mut self, reason: BackfillSkipReason) {
        match reason {
            BackfillSkipReason::NoActivityWindow => self.skipped_no_window += 1,
            BackfillSkipReason::NotAWorktree => self.skipped_not_worktree += 1,
            BackfillSkipReason::GitError => self.skipped_git_error += 1,
        }
    }

    pub const fn skipped_total(&self) -> usize {
        self.skipped_no_window + self.skipped_not_worktree + self.skipped_git_error
    }

    fn observe_writer_hold(&mut self, started: std::time::Instant) {
        self.max_writer_hold_micros = self
            .max_writer_hold_micros
            .max(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
}

/// Abstracts the git subprocess surface the backfill needs, so tests can run
/// the core against a real repo ([`SystemGit`]) or a canned fixture.
///
/// `Send + Sync` so a `&dyn GitReflogSource` can be held across an `.await`
/// inside a spawned task (the startup auto-backfill runs on a tokio worker).
pub trait GitReflogSource: Send + Sync {
    /// `git reflog --date=unix HEAD` text for `worktree`.
    fn reflog(&self, worktree: &std::path::Path) -> Result<String, GitScanFailure>;
    /// The branch `HEAD` currently points at in `worktree` (`None` = detached
    /// or unknown), used as the leading-segment floor.
    fn current_branch(&self, worktree: &std::path::Path) -> Result<Option<String>, GitScanFailure>;
    /// `git log <branch> --pretty=%H %ct --since=<since>` text for `worktree`,
    /// newest-first.
    fn commit_log(
        &self,
        worktree: &std::path::Path,
        branch: &str,
        since: i64,
        max_commits: usize,
    ) -> Result<String, GitScanFailure>;
}

/// Real git-subprocess implementation of [`GitReflogSource`].
#[derive(Default)]
pub struct SystemGit {
    bounds: Option<tracedecay_runtime_core::git::GitCommandBounds>,
}

impl SystemGit {
    pub fn with_bounds(bounds: tracedecay_runtime_core::git::GitCommandBounds) -> Self {
        Self {
            bounds: Some(bounds),
        }
    }

    fn output(&self, worktree: &std::path::Path, args: &[&str]) -> Result<String, GitScanFailure> {
        let default_bounds;
        let bounds = match self.bounds.as_ref() {
            Some(bounds) => bounds,
            None => {
                default_bounds = tracedecay_runtime_core::git::GitCommandBounds::default();
                &default_bounds
            }
        };
        let output = tracedecay_runtime_core::git::bounded_git_output(worktree, args, bounds)
            .map_err(map_git_command_error)?;
        if !output.status.success() {
            return Err(GitScanFailure::CommandFailed);
        }
        String::from_utf8(output.stdout).map_err(|_| GitScanFailure::InvalidOutput)
    }
}

impl GitReflogSource for SystemGit {
    fn reflog(&self, worktree: &std::path::Path) -> Result<String, GitScanFailure> {
        let max_count = format!("--max-count={}", MAX_REFLOG_ENTRIES.saturating_add(1));
        let output = self.output(worktree, &["reflog", "--date=unix", &max_count, "HEAD"])?;
        if output.lines().count() > MAX_REFLOG_ENTRIES {
            return Err(GitScanFailure::OutputLimitExceeded);
        }
        Ok(output)
    }

    fn current_branch(&self, worktree: &std::path::Path) -> Result<Option<String>, GitScanFailure> {
        let raw = self.output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "HEAD" {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    fn commit_log(
        &self,
        worktree: &std::path::Path,
        branch: &str,
        since: i64,
        max_commits: usize,
    ) -> Result<String, GitScanFailure> {
        let max_count = format!("--max-count={}", max_commits.saturating_add(1));
        self.output(
            worktree,
            &[
                "log",
                branch,
                "--pretty=%H %ct",
                &format!("--since={since}"),
                &max_count,
            ],
        )
    }
}

fn map_git_command_error(error: tracedecay_runtime_core::git::GitCommandError) -> GitScanFailure {
    match error {
        tracedecay_runtime_core::git::GitCommandError::Cancelled => GitScanFailure::Cancelled,
        tracedecay_runtime_core::git::GitCommandError::DeadlineExceeded => {
            GitScanFailure::DeadlineExceeded
        }
        tracedecay_runtime_core::git::GitCommandError::OutputLimitExceeded { .. } => {
            GitScanFailure::OutputLimitExceeded
        }
        tracedecay_runtime_core::git::GitCommandError::Unavailable(_)
        | tracedecay_runtime_core::git::GitCommandError::ReadOutput { .. }
        | tracedecay_runtime_core::git::GitCommandError::Wait(_) => GitScanFailure::CommandFailed,
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
/// `session_store` is the per-project sessions authority (already open, and —
/// for a real run — writable). `analytics_events` contribute only
/// provider/session timestamps (via [`AnalyticsSessionTimestampSource`]);
/// branch data is never assumed present. `git` supplies the reflog/log
/// subprocess surface. Fail-open: a broken repo or session is counted and
/// skipped, never aborting the run.
///
/// When `opts.dry_run` is set no rows are written; the returned counts reflect
/// what *would* have been written.
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
    let mut stats = BackfillStats::default();
    let _progress = backfill_rows(
        session_store,
        git,
        opts,
        &rows,
        analytics_events,
        &mut stats,
        false,
    )
    .await?;
    Ok(stats)
}

/// Default number of previously-unattempted sessions the auto-backfill drains
/// per pass. Bounds a single startup/tick so the first run on a store with
/// months of history never blocks; successive passes advance the watermark and
/// drain the remainder.
pub const DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS: usize = 50;

/// Runs one incremental, idempotent pass of the historical git-span backfill,
/// advancing a persistent watermark so unattended callers (MCP server startup)
/// drain months of history a bounded batch at a time without a manual CLI
/// invocation.
///
/// The watermark ([`AUTO_BACKFILL_WATERMARK_KEY`]) records the highest session
/// activity timestamp successfully completed or permanently skipped. Each pass
/// reads up to `limit_sessions` sessions strictly newer than the watermark,
/// oldest-first, and backfills them with idempotent writes. A retryable Git
/// failure or cancellation stops the ordered batch before that row advances
/// the watermark, so a restart resumes from the failed session.
///
/// Analytics timestamps are not consulted here (the manual
/// `tracedecay sessions git-backfill` remains the exhaustive, watermark-free,
/// analytics-aware path); auto-backfill relies on session and reflog
/// timestamps alone, which is enough to populate branch/worktree spans.
pub async fn run_incremental_backfill<S: GitCorrelationSessionStore, G>(
    session_store: &S,
    git: &G,
    limit_sessions: usize,
) -> Result<BackfillStats, GitCorrelationError>
where
    G: GitReflogSource + ?Sized,
{
    session_store.require_project_sessions_authority()?;
    let mut stats = BackfillStats::default();
    if limit_sessions == 0 {
        return Ok(stats);
    }
    let snapshot = session_store.read_snapshot().await?;
    let watermark = super::read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    let rows = session_activity_rows_since(&snapshot, watermark, limit_sessions)
        .await
        .map_err(GitCorrelationError::Db)?;
    drop(snapshot);

    // `since` is left at 0: the query already excludes anything at or below the
    // watermark, so a second time floor would only drop legitimately-new spans.
    let opts = BackfillOptions {
        since: 0,
        limit_sessions,
        merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
        max_commits_per_repo: BackfillOptions::default().max_commits_per_repo,
        dry_run: false,
    };
    if !rows.is_empty() {
        let no_analytics: &[super::AnalyticsSessionTimestamp] = &[];
        let progress = backfill_rows(
            session_store,
            git,
            &opts,
            &rows,
            no_analytics,
            &mut stats,
            true,
        )
        .await?;

        // Only publish the contiguous completed prefix. Retryable Git failures
        // leave their row and every later row eligible for the next pass.
        let new_watermark = progress.completed_activity_watermark;
        if let Some(new_watermark) = new_watermark
            && new_watermark > watermark
        {
            let transaction = session_store.open_write_transaction().await?;
            let writer_started = std::time::Instant::now();
            super::write_meta_value(&transaction, AUTO_BACKFILL_WATERMARK_KEY, new_watermark)
                .await?;
            GitCorrelationWriteTxn::commit(transaction).await?;
            stats.observe_writer_hold(writer_started);
        }
    }

    // Sweep commit attribution over span targets written since the last sweep.
    // This is the only attribution path for spans recorded live by the hook
    // route: those sessions have no transcript rows, so the session-driven
    // backfill above never sees them, and without this sweep their commits
    // would stay unattributed until a transcript ingest happens to run. The
    // sweep keeps its own watermark and is idempotent, so running it on every
    // pass (including passes with zero new session rows) is safe.
    let snapshot = session_store.read_snapshot().await?;
    let plan = prepare_commit_attribution_sweep(&snapshot).await?;
    drop(snapshot);
    let scanned = scan_commit_attribution_plan(&plan, opts.merge_gap_secs, |target| {
        scan_span_target(git, target, opts.merge_gap_secs, opts.max_commits_per_repo)
    });
    let publication = publish_commit_attribution_plan_to_store(session_store, scanned).await?;
    stats.max_writer_hold_micros = stats
        .max_writer_hold_micros
        .max(publication.max_writer_hold_micros);
    if let CommitAttributionPublication::Published { inserted, .. } = publication.outcome {
        stats.commits_attributed += inserted;
    }
    Ok(stats)
}

/// Scans one span target's branch history through the backfill's git source,
/// mirroring the ingest-time sweep's scanner: commits on the recorded branch
/// (or `HEAD` for detached spans) inside the gap-widened span window. Reports
/// [`TargetScan::Unavailable`] — not an empty list — when the worktree is gone
/// or git fails, so the sweep holds its watermark and retries the target.
fn scan_span_target<G: GitReflogSource + ?Sized>(
    git: &G,
    target: &SpanScanTarget,
    gap_secs: i64,
    max_commits: usize,
) -> TargetScan {
    let worktree = std::path::Path::new(&target.worktree);
    if !worktree.is_dir() {
        return TargetScan::Unavailable(super::GitScanFailure::WorktreeUnavailable);
    }
    let since = target.window_start.saturating_sub(gap_secs);
    let until = target.window_end.saturating_add(gap_secs);
    let branch = target
        .branch
        .as_deref()
        .filter(|branch| !branch.is_empty())
        .unwrap_or("HEAD");
    let log_text = match git.commit_log(worktree, branch, since, max_commits) {
        Ok(log_text) => log_text,
        Err(reason) => return TargetScan::Unavailable(reason),
    };
    let commits = match parse_bounded_git_log(&log_text, max_commits) {
        Ok(commits) => commits,
        Err(reason) => return TargetScan::Unavailable(reason),
    };
    TargetScan::Scanned(
        commits
            .into_iter()
            .filter(|commit| commit.committed_at <= until)
            .collect(),
    )
}

/// Shared per-session backfill loop used by both the exhaustive
/// [`run_backfill`] and the incremental [`run_incremental_backfill`]. Indexes
/// the supplied analytics timestamps once, then folds each row into the span
/// and commit tables, counting skips instead of aborting.
#[derive(Debug, Default, PartialEq, Eq)]
struct BackfillRowsProgress {
    completed_activity_watermark: Option<i64>,
}

async fn backfill_rows<S, E, G: GitReflogSource + ?Sized>(
    session_store: &S,
    git: &G,
    opts: &BackfillOptions,
    rows: &[SessionActivityRow],
    analytics_events: &[E],
    stats: &mut BackfillStats,
    stop_on_git_error: bool,
) -> Result<BackfillRowsProgress, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    E: AnalyticsSessionTimestampSource,
{
    // Index analytics timestamps by (provider, session_id) for O(1) lookup.
    let mut analytics_ts: std::collections::HashMap<(String, String), Vec<i64>> =
        std::collections::HashMap::new();
    for event in analytics_events {
        if let Some(timestamp) = event.as_analytics_session_timestamp() {
            analytics_ts
                .entry((timestamp.provider, timestamp.session_id))
                .or_default()
                .push(timestamp.timestamp);
        }
    }

    let mut git_batch = BackfillGitBatch::new(git);
    let mut progress = BackfillRowsProgress::default();
    for row in rows {
        stats.sessions_scanned += 1;
        let outcome = backfill_one_session(
            session_store,
            &mut git_batch,
            opts,
            row,
            &analytics_ts,
            stats,
        )
        .await;
        match outcome {
            Ok(()) => {}
            Err(reason) => {
                stats.record_skip(reason);
                if stop_on_git_error && reason == BackfillSkipReason::GitError {
                    break;
                }
            }
        }
        if let Some(activity) = row.activity_sort_key() {
            progress.completed_activity_watermark = Some(
                progress
                    .completed_activity_watermark
                    .map_or(activity, |current| current.max(activity)),
            );
        }
    }
    stats.git_reflog_calls = stats
        .git_reflog_calls
        .saturating_add(git_batch.reflog_calls);
    stats.git_current_branch_calls = stats
        .git_current_branch_calls
        .saturating_add(git_batch.current_branch_calls);
    stats.git_log_calls = stats.git_log_calls.saturating_add(git_batch.log_calls);
    Ok(progress)
}

#[derive(Clone)]
struct CachedRepositoryState {
    worktree_root: std::path::PathBuf,
    worktree: String,
    timeline: Vec<BranchTimelineEntry>,
    current_branch: Option<String>,
}

struct CachedCommitLog {
    since: i64,
    text: String,
}

struct BackfillGitBatch<'git, G: GitReflogSource + ?Sized> {
    git: &'git G,
    project_worktrees:
        std::collections::HashMap<String, Result<(String, std::path::PathBuf), BackfillSkipReason>>,
    repositories:
        std::collections::HashMap<String, Result<CachedRepositoryState, BackfillSkipReason>>,
    logs: std::collections::HashMap<(String, String), CachedCommitLog>,
    reflog_calls: usize,
    current_branch_calls: usize,
    log_calls: usize,
}

impl<'git, G: GitReflogSource + ?Sized> BackfillGitBatch<'git, G> {
    fn new(git: &'git G) -> Self {
        Self {
            git,
            project_worktrees: std::collections::HashMap::new(),
            repositories: std::collections::HashMap::new(),
            logs: std::collections::HashMap::new(),
            reflog_calls: 0,
            current_branch_calls: 0,
            log_calls: 0,
        }
    }

    fn repository(
        &mut self,
        project_path: &str,
    ) -> Result<CachedRepositoryState, BackfillSkipReason> {
        let project_path = project_path.trim();
        if project_path.is_empty() {
            return Err(BackfillSkipReason::NotAWorktree);
        }
        let (key, worktree_root) = if let Some(cached) = self.project_worktrees.get(project_path) {
            cached.clone()?
        } else {
            let resolved = tracedecay_runtime_core::worktree::git_worktree_root(
                std::path::Path::new(project_path),
            )
            .ok_or(BackfillSkipReason::NotAWorktree)
            .map(|worktree_root| {
                (
                    normalize_worktree(&worktree_root.to_string_lossy()),
                    worktree_root,
                )
            });
            self.project_worktrees
                .insert(project_path.to_owned(), resolved.clone());
            resolved?
        };
        if let Some(cached) = self.repositories.get(&key) {
            return cached.clone();
        }
        let result = (|| {
            self.reflog_calls = self.reflog_calls.saturating_add(1);
            let reflog_text = self
                .git
                .reflog(&worktree_root)
                .map_err(|_| BackfillSkipReason::GitError)?;
            self.current_branch_calls = self.current_branch_calls.saturating_add(1);
            let current_branch = self
                .git
                .current_branch(&worktree_root)
                .map_err(|_| BackfillSkipReason::GitError)?;
            Ok(CachedRepositoryState {
                worktree: key.clone(),
                worktree_root,
                timeline: branch_timeline_from_reflog(&reflog_text),
                current_branch,
            })
        })();
        self.repositories.insert(key, result.clone());
        result
    }

    fn commit_log(
        &mut self,
        repository: &CachedRepositoryState,
        branch: &str,
        since: i64,
        max_commits: usize,
    ) -> Result<String, BackfillSkipReason> {
        let key = (repository.worktree.clone(), branch.to_owned());
        if let Some(cached) = self.logs.get(&key)
            && cached.since <= since
        {
            return Ok(cached.text.clone());
        }
        self.log_calls = self.log_calls.saturating_add(1);
        let text = self
            .git
            .commit_log(&repository.worktree_root, branch, since, max_commits)
            .map_err(|_| BackfillSkipReason::GitError)?;
        self.logs.insert(
            key,
            CachedCommitLog {
                since,
                text: text.clone(),
            },
        );
        Ok(text)
    }
}

async fn backfill_one_session<S: GitCorrelationSessionStore, G: GitReflogSource + ?Sized>(
    session_store: &S,
    git_batch: &mut BackfillGitBatch<'_, G>,
    opts: &BackfillOptions,
    row: &SessionActivityRow,
    analytics_ts: &std::collections::HashMap<(String, String), Vec<i64>>,
    stats: &mut BackfillStats,
) -> Result<(), BackfillSkipReason> {
    let (mut win_start, win_end) = row.window().ok_or(BackfillSkipReason::NoActivityWindow)?;
    if win_end < opts.since {
        return Err(BackfillSkipReason::NoActivityWindow);
    }
    win_start = win_start.max(opts.since);
    if win_start > win_end {
        return Err(BackfillSkipReason::NoActivityWindow);
    }

    let repository = git_batch.repository(&row.project_path)?;
    let worktree = repository.worktree.clone();

    // Extra observation timestamps: analytics event times inside the
    // (since-clamped) window, which refine span boundaries within a segment.
    let mut analytics_within: Vec<i64> = Vec::new();
    if let Some(times) = analytics_ts.get(&(row.provider.clone(), row.session_id.clone())) {
        for &ts in times {
            if ts >= win_start && ts <= win_end {
                analytics_within.push(ts);
            }
        }
    }

    let segments = window_branch_segments(
        win_start,
        win_end,
        &repository.timeline,
        repository.current_branch.as_deref(),
    );

    for segment in &segments {
        // Every segment yields a span: seed it with its own clamped edges so an
        // interior segment (e.g. a mid-session branch switch) is recorded even
        // when the global window edges fall outside it. Analytics timestamps
        // inside the segment refine the boundaries; record_span_observation
        // merges observations on the same branch within the merge gap.
        let mut segment_ts = vec![segment.start, segment.end];
        segment_ts.extend(
            analytics_within
                .iter()
                .copied()
                .filter(|&ts| ts >= segment.start && ts <= segment.end),
        );
        if !opts.dry_run {
            for timestamp_chunk in segment_ts.chunks(BACKFILL_PUBLICATION_CHUNK) {
                let transaction = session_store
                    .open_write_transaction()
                    .await
                    .map_err(|_| BackfillSkipReason::GitError)?;
                let writer_started = std::time::Instant::now();
                for &ts in timestamp_chunk {
                    super::record_span_observation_in_transaction(
                        &transaction,
                        &SpanObservation {
                            provider: row.provider.clone(),
                            session_id: row.session_id.clone(),
                            thread_id: None,
                            branch: segment.branch.clone(),
                            worktree: worktree.clone(),
                            ts,
                            source: SpanSource::Backfill,
                        },
                        opts.merge_gap_secs,
                    )
                    .await
                    .map_err(|_| BackfillSkipReason::GitError)?;
                }
                GitCorrelationWriteTxn::commit(transaction)
                    .await
                    .map_err(|_| BackfillSkipReason::GitError)?;
                stats.observe_writer_hold(writer_started);
            }
        }
        stats.spans_written += 1;

        // Attribute commits on this segment's branch within the segment window.
        let Some(branch) = segment.branch.as_deref() else {
            continue;
        };
        let log_text = git_batch.commit_log(
            &repository,
            branch,
            segment.start,
            opts.max_commits_per_repo,
        )?;
        let commits = parse_bounded_git_log(&log_text, opts.max_commits_per_repo)
            .map_err(|_| BackfillSkipReason::GitError)?;
        let records = commits
            .into_iter()
            .filter(|commit| {
                commit.committed_at >= segment.start && commit.committed_at <= segment.end
            })
            .map(|commit| CommitSessionRecord {
                commit_sha: commit.sha,
                provider: row.provider.clone(),
                session_id: row.session_id.clone(),
                branch: Some(branch.to_string()),
                worktree: Some(worktree.clone()),
                committed_at: commit.committed_at,
                span_overlap_kind: SpanOverlapKind::WithinSpan,
                span_id: None,
                relation: CommitRelation::Observed,
                evidence: CommitEvidence::ReflogOverlap,
                confidence: 30,
                evidence_message_id: None,
            })
            .collect::<Vec<_>>();
        if opts.dry_run {
            stats.commits_attributed = stats.commits_attributed.saturating_add(records.len());
            continue;
        }
        for record_chunk in records.chunks(BACKFILL_PUBLICATION_CHUNK) {
            let transaction = session_store
                .open_write_transaction()
                .await
                .map_err(|_| BackfillSkipReason::GitError)?;
            let writer_started = std::time::Instant::now();
            for record in record_chunk {
                if super::upsert_commit_session(&transaction, record)
                    .await
                    .map_err(|_| BackfillSkipReason::GitError)?
                {
                    stats.commits_attributed += 1;
                }
            }
            GitCorrelationWriteTxn::commit(transaction)
                .await
                .map_err(|_| BackfillSkipReason::GitError)?;
            stats.observe_writer_hold(writer_started);
        }
    }
    Ok(())
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
             LEFT JOIN session_messages m
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

/// Reads per-session activity windows whose activity timestamp is strictly
/// greater than `since_exclusive`, oldest-first and capped at `limit`. Backs the
/// incremental auto-backfill: paired with a persisted watermark it drains
/// history forward in bounded batches. Sessions with no timestamp at all are
/// excluded (their `COALESCE` key is `NULL`, so the `HAVING` filter drops them —
/// they carry no derivable activity window anyway).
pub(super) async fn session_activity_rows_since(
    conn: &(impl QueryExecutor + ?Sized),
    since_exclusive: i64,
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
             LEFT JOIN session_messages m
                    ON m.provider = s.provider AND m.session_id = s.session_id
             GROUP BY s.provider, s.session_id
             HAVING COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) > ?1
             ORDER BY COALESCE(MAX(m.timestamp), s.ended_at, s.started_at) ASC
             LIMIT ?2",
            params![since_exclusive, i64::try_from(limit).unwrap_or(i64::MAX)],
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
