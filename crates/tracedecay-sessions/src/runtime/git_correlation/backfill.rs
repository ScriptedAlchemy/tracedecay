use std::future::Future;

use libsql::{Connection, params};

use super::{
    AUTO_BACKFILL_WATERMARK_KEY, CommitEvidence, CommitRelation, CommitSessionRecord,
    DEFAULT_SPAN_MERGE_GAP_SECS, GitCorrelationError, SpanObservation, SpanOverlapKind, SpanSource,
    normalize_worktree,
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
/// `GlobalDb::latest_session_activity_secs` and `kiro::normalize_timestamp`).
const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

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

/// Tunables for [`run_backfill_with_analytics`].
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
}

/// Session-store operations needed by historical git correlation backfills.
pub trait GitBackfillStore {
    fn session_activity_rows(
        &self,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<SessionActivityRow>, String>> + Send;

    fn session_activity_rows_since(
        &self,
        since_exclusive: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<SessionActivityRow>, String>> + Send;

    fn git_correlation_meta_get(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<i64>, GitCorrelationError>> + Send;

    fn git_correlation_meta_set(
        &self,
        key: &str,
        value: i64,
    ) -> impl Future<Output = Result<(), GitCorrelationError>> + Send;

    fn git_record_span_observation(
        &self,
        observation: &SpanObservation,
        merge_gap_secs: i64,
    ) -> impl Future<Output = Result<i64, GitCorrelationError>> + Send;

    fn git_upsert_commit_session(
        &self,
        record: &CommitSessionRecord,
    ) -> impl Future<Output = Result<bool, GitCorrelationError>> + Send;
}

/// Analytics fields used to refine historical session activity spans.
pub trait GitBackfillAnalytics {
    fn provider(&self) -> &str;
    fn session_id(&self) -> Option<&str>;
    fn timestamp(&self) -> i64;
}

struct NoAnalytics;

impl GitBackfillAnalytics for NoAnalytics {
    fn provider(&self) -> &str {
        ""
    }

    fn session_id(&self) -> Option<&str> {
        None
    }

    fn timestamp(&self) -> i64 {
        0
    }
}

/// Abstracts the git subprocess surface the backfill needs, so tests can run
/// the core against a real repo ([`SystemGit`]) or a canned fixture.
///
/// `Send + Sync` so a `&dyn GitReflogSource` can be held across an `.await`
/// inside a spawned task (the startup auto-backfill runs on a tokio worker).
pub trait GitReflogSource: Send + Sync {
    /// `git reflog --date=unix HEAD` text for `worktree`, or `None` on error.
    fn reflog(&self, worktree: &std::path::Path) -> Option<String>;
    /// The branch `HEAD` currently points at in `worktree` (`None` = detached
    /// or unknown), used as the leading-segment floor.
    fn current_branch(&self, worktree: &std::path::Path) -> Option<String>;
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

impl GitReflogSource for SystemGit {
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

    fn commit_log(&self, worktree: &std::path::Path, branch: &str, since: i64) -> Option<String> {
        Self::output(
            worktree,
            &[
                "log",
                branch,
                "--pretty=%H %ct",
                &format!("--since={since}"),
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
/// `session_store` is the per-project `sessions.db` (already open, and — for a
/// real run — writable). `analytics_events` are global analytics rows whose
/// `session_id` matches a scanned session; only their timestamps are consumed
/// (branch data is never assumed present). `git` supplies the reflog/log
/// subprocess surface. Fail-open: a broken repo or session is counted and
/// skipped, never aborting the run.
///
/// When `opts.dry_run` is set no rows are written; the returned counts reflect
/// what *would* have been written.
pub async fn run_backfill_with_analytics<S, E>(
    session_store: &S,
    analytics_events: &[E],
    git: &dyn GitReflogSource,
    opts: &BackfillOptions,
) -> Result<BackfillStats, GitCorrelationError>
where
    S: GitBackfillStore,
    E: GitBackfillAnalytics,
{
    let rows = session_store
        .session_activity_rows(opts.limit_sessions)
        .await
        .map_err(GitCorrelationError::Db)?;
    let mut stats = BackfillStats::default();
    backfill_rows(
        session_store,
        git,
        opts,
        &rows,
        analytics_events,
        &mut stats,
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
/// activity timestamp already attempted. Each pass reads up to `limit_sessions`
/// sessions strictly newer than the watermark, oldest-first, backfills them
/// (span/commit writes are idempotent), then advances the watermark to the
/// newest activity in the batch. Fresh sessions recorded after a pass are
/// picked up by a later pass; a fully-drained store scans nothing.
///
/// Analytics timestamps are not consulted here (the manual
/// `tracedecay sessions git-backfill` remains the exhaustive, watermark-free,
/// analytics-aware path); auto-backfill relies on session and reflog
/// timestamps alone, which is enough to populate branch/worktree spans.
pub async fn run_incremental_backfill<S>(
    session_store: &S,
    git: &dyn GitReflogSource,
    limit_sessions: usize,
) -> Result<BackfillStats, GitCorrelationError>
where
    S: GitBackfillStore,
{
    let mut stats = BackfillStats::default();
    if limit_sessions == 0 {
        return Ok(stats);
    }
    let watermark = session_store
        .git_correlation_meta_get(AUTO_BACKFILL_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    let rows = session_store
        .session_activity_rows_since(watermark, limit_sessions)
        .await
        .map_err(GitCorrelationError::Db)?;
    if rows.is_empty() {
        return Ok(stats);
    }

    // `since` is left at 0: the query already excludes anything at or below the
    // watermark, so a second time floor would only drop legitimately-new spans.
    let opts = BackfillOptions {
        since: 0,
        limit_sessions,
        merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
        max_commits_per_repo: BackfillOptions::default().max_commits_per_repo,
        dry_run: false,
    };
    backfill_rows(
        session_store,
        git,
        &opts,
        &rows,
        &[] as &[NoAnalytics],
        &mut stats,
    )
    .await?;

    // Advance the watermark to the newest activity attempted this pass. Rows are
    // ordered oldest-first, so the last row carries the max; fall back to a scan
    // to stay correct if the query's ordering ever changes.
    let new_watermark = rows
        .iter()
        .filter_map(SessionActivityRow::activity_sort_key)
        .max();
    if let Some(new_watermark) = new_watermark {
        if new_watermark > watermark {
            session_store
                .git_correlation_meta_set(AUTO_BACKFILL_WATERMARK_KEY, new_watermark)
                .await?;
        }
    }
    Ok(stats)
}

/// Shared per-session backfill loop used by both the exhaustive
/// [`run_backfill`] and the incremental [`run_incremental_backfill`]. Indexes
/// the supplied analytics timestamps once, then folds each row into the span
/// and commit tables, counting skips instead of aborting.
async fn backfill_rows<S, E>(
    session_store: &S,
    git: &dyn GitReflogSource,
    opts: &BackfillOptions,
    rows: &[SessionActivityRow],
    analytics_events: &[E],
    stats: &mut BackfillStats,
) -> Result<(), GitCorrelationError>
where
    S: GitBackfillStore,
    E: GitBackfillAnalytics,
{
    // Index analytics timestamps by (provider, session_id) for O(1) lookup.
    let mut analytics_ts: std::collections::HashMap<(String, String), Vec<i64>> =
        std::collections::HashMap::new();
    for event in analytics_events {
        if let Some(session_id) = event.session_id() {
            analytics_ts
                .entry((event.provider().to_string(), session_id.to_string()))
                .or_default()
                .push(event.timestamp());
        }
    }

    for row in rows {
        stats.sessions_scanned += 1;
        if let Err(reason) =
            backfill_one_session(session_store, git, opts, row, &analytics_ts, stats).await
        {
            stats.record_skip(reason);
        }
    }
    Ok(())
}

async fn backfill_one_session<S>(
    session_store: &S,
    git: &dyn GitReflogSource,
    opts: &BackfillOptions,
    row: &SessionActivityRow,
    analytics_ts: &std::collections::HashMap<(String, String), Vec<i64>>,
    stats: &mut BackfillStats,
) -> Result<(), BackfillSkipReason>
where
    S: GitBackfillStore,
{
    let (mut win_start, win_end) = row.window().ok_or(BackfillSkipReason::NoActivityWindow)?;
    if win_end < opts.since {
        return Err(BackfillSkipReason::NoActivityWindow);
    }
    win_start = win_start.max(opts.since);
    if win_start > win_end {
        return Err(BackfillSkipReason::NoActivityWindow);
    }

    if row.project_path.trim().is_empty() {
        return Err(BackfillSkipReason::NotAWorktree);
    }
    let worktree_path = std::path::Path::new(row.project_path.trim());
    let worktree_root = tracedecay_runtime_core::worktree::git_worktree_root(worktree_path)
        .ok_or(BackfillSkipReason::NotAWorktree)?;
    let worktree = normalize_worktree(&worktree_root.to_string_lossy());

    let reflog_text = git
        .reflog(&worktree_root)
        .ok_or(BackfillSkipReason::GitError)?;
    let timeline = branch_timeline_from_reflog(&reflog_text);
    let current_branch = git.current_branch(&worktree_root);

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

    let segments = window_branch_segments(win_start, win_end, &timeline, current_branch.as_deref());

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
        for &ts in &segment_ts {
            if !opts.dry_run {
                session_store
                    .git_record_span_observation(
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
        }
        stats.spans_written += 1;

        // Attribute commits on this segment's branch within the segment window.
        let Some(branch) = segment.branch.as_deref() else {
            continue;
        };
        let Some(log_text) = git.commit_log(&worktree_root, branch, segment.start) else {
            continue;
        };
        for (sha, committed_at) in parse_commit_log(&log_text, opts.max_commits_per_repo) {
            if committed_at < segment.start || committed_at > segment.end {
                continue;
            }
            if opts.dry_run {
                stats.commits_attributed += 1;
                continue;
            }
            let inserted = session_store
                .git_upsert_commit_session(&CommitSessionRecord {
                    commit_sha: sha,
                    provider: row.provider.clone(),
                    session_id: row.session_id.clone(),
                    branch: Some(branch.to_string()),
                    worktree: Some(worktree.clone()),
                    committed_at,
                    span_overlap_kind: SpanOverlapKind::WithinSpan,
                    span_id: None,
                    relation: CommitRelation::Observed,
                    evidence: CommitEvidence::ReflogOverlap,
                    confidence: 30,
                    evidence_message_id: None,
                })
                .await
                .map_err(|_| BackfillSkipReason::GitError)?;
            if inserted {
                stats.commits_attributed += 1;
            }
        }
    }
    Ok(())
}

/// Reads per-session activity windows for the [`GitBackfillStore`] adapter.
pub async fn session_activity_rows(
    conn: &Connection,
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
pub async fn session_activity_rows_since(
    conn: &Connection,
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
fn decode_session_activity_row(row: &libsql::Row) -> Result<SessionActivityRow, String> {
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
