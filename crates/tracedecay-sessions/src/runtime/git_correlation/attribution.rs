use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::store::{GitCorrelationSessionStore, GitCorrelationWriteTxn};
use super::{
    CommitEvidence, CommitRelation, CommitSessionRecord, GitCorrelationError, SpanOverlapKind,
    correlation_tables_present, opt_text, upsert_commit_session,
};

const COMMIT_SWEEP_WATERMARK_KEY: &str = "commit_attribution_watermark";

pub async fn read_meta_value(
    conn: &(impl QueryExecutor + ?Sized),
    key: &str,
) -> Result<Option<i64>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT value FROM git_correlation_meta WHERE key = ?1",
            params![key],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub async fn write_meta_value(
    conn: &(impl Executor + ?Sized),
    key: &str,
    value: i64,
) -> Result<(), GitCorrelationError> {
    conn.execute(
        "INSERT INTO git_correlation_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
        params![key, value],
    )
    .await?;
    Ok(())
}

/// A `(branch, worktree)` pair a session was observed on, with the widest span
/// window recorded for it. Commit scans run once per pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanScanTarget {
    pub branch: Option<String>,
    pub worktree: String,
    pub window_start: i64,
    pub window_end: i64,
    /// Newest span write time (`updated_at`) in this target. The sweep
    /// watermark advances on this — ingest/write order — not on `window_end`
    /// (event time), so a session ingested late for old commits is still
    /// scanned even though its events predate the watermark.
    pub max_updated_at: i64,
}

/// One span row a candidate commit may fall inside. Kept minimal so the
/// matching logic ([`match_commit_to_spans`]) is a pure function testable
/// without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanWindow {
    pub span_id: i64,
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: String,
    pub first_ts: i64,
    pub last_ts: i64,
}

/// Classifies a commit at `committed_at` against one span: `Some(WithinSpan)`
/// when strictly inside `[first_ts, last_ts]`, `Some(ExtendedWindow)` when
/// inside the span widened by `gap_secs` on either edge, `None` otherwise.
pub fn commit_overlap_kind(
    first_ts: i64,
    last_ts: i64,
    committed_at: i64,
    gap_secs: i64,
) -> Option<SpanOverlapKind> {
    if committed_at >= first_ts && committed_at <= last_ts {
        Some(SpanOverlapKind::WithinSpan)
    } else if committed_at >= first_ts.saturating_sub(gap_secs)
        && committed_at <= last_ts.saturating_add(gap_secs)
    {
        Some(SpanOverlapKind::ExtendedWindow)
    } else {
        None
    }
}

/// Records that every matching span observed a commit. Time overlap is
/// candidate evidence only: concurrent sessions must never be labelled as
/// producers without a direct tool/host event.
pub fn match_commit_to_spans(
    commit_sha: &str,
    branch: Option<&str>,
    worktree: &str,
    committed_at: i64,
    spans: &[SpanWindow],
    gap_secs: i64,
) -> Vec<CommitSessionRecord> {
    let mut records = Vec::new();
    for span in spans {
        if span.branch.as_deref() != branch || span.worktree != worktree {
            continue;
        }
        let Some(kind) = commit_overlap_kind(span.first_ts, span.last_ts, committed_at, gap_secs)
        else {
            continue;
        };
        records.push(CommitSessionRecord {
            commit_sha: commit_sha.to_string(),
            provider: span.provider.clone(),
            session_id: span.session_id.clone(),
            branch: span.branch.clone(),
            worktree: Some(span.worktree.clone()),
            committed_at,
            span_overlap_kind: kind,
            span_id: Some(span.span_id),
            relation: CommitRelation::Observed,
            evidence: CommitEvidence::TimeOverlap,
            confidence: match kind {
                SpanOverlapKind::Direct => 100,
                SpanOverlapKind::WithinSpan => 20,
                SpanOverlapKind::ExtendedWindow => 10,
                SpanOverlapKind::Reflog => 30,
            },
            evidence_message_id: None,
        });
    }
    records
}

/// Loads the `(branch, worktree)` scan targets whose spans were *written*
/// (`updated_at`) at or after `since_ts` (the sweep watermark), each carrying
/// the widest span window observed for it so the git scan can be time-bounded.
///
/// Selecting on `updated_at` (ingest/write time) rather than `last_ts` (event
/// time) is deliberate: historical sessions ingested after the watermark carry
/// old event times but a fresh `updated_at`, so they still get attributed.
async fn scan_targets_since(
    conn: &(impl QueryExecutor + ?Sized),
    since_ts: i64,
) -> Result<Vec<SpanScanTarget>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT branch, worktree, MIN(first_ts), MAX(last_ts), MAX(updated_at)
             FROM session_git_spans
             WHERE updated_at >= ?1
             GROUP BY branch, worktree
             ORDER BY MAX(updated_at) ASC, worktree ASC, branch ASC",
            params![since_ts],
        )
        .await?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await? {
        targets.push(SpanScanTarget {
            branch: row.get(0)?,
            worktree: row.get(1)?,
            window_start: row.get(2)?,
            window_end: row.get(3)?,
            max_updated_at: row.get(4)?,
        });
    }
    Ok(targets)
}

/// Loads span windows for one `(branch, worktree)` pair, used to attribute
/// each scanned commit.
async fn span_windows_for(
    conn: &(impl QueryExecutor + ?Sized),
    branch: Option<&str>,
    worktree: &str,
) -> Result<Vec<SpanWindow>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT span_id, provider, session_id, branch, worktree, first_ts, last_ts
             FROM session_git_spans
             WHERE branch IS ?1 AND worktree = ?2",
            params![opt_text(branch), worktree],
        )
        .await?;
    let mut spans = Vec::new();
    while let Some(row) = rows.next().await? {
        spans.push(SpanWindow {
            span_id: row.get(0)?,
            provider: row.get(1)?,
            session_id: row.get(2)?,
            branch: row.get(3)?,
            worktree: row.get(4)?,
            first_ts: row.get(5)?,
            last_ts: row.get(6)?,
        });
    }
    canonicalize_span_providers(&mut spans);
    Ok(spans)
}

/// Collapses the multiple provider identities of one session onto its single
/// real provider. A live session is span-observed by the hook route (which
/// stores `provider ''`, since it cannot know the provider) and again by
/// transcript ingest (which stores the real provider). Left split, both
/// windows attribute the same commit under different `(commit_sha, provider,
/// session_id)` keys, double-counting the session. Resolving every window to
/// the session's non-empty provider makes those attributions collapse onto one
/// `commit_sessions` row via the upsert primary key.
fn canonicalize_span_providers(spans: &mut [SpanWindow]) {
    let mut canonical: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for span in spans.iter() {
        if !span.provider.is_empty() {
            canonical
                .entry(span.session_id.clone())
                .or_insert_with(|| span.provider.clone());
        }
    }
    for span in spans.iter_mut() {
        if let Some(provider) = canonical.get(&span.session_id) {
            span.provider = provider.clone();
        }
    }
}

/// One commit observed by the bounded git scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedCommit {
    pub sha: String,
    pub committed_at: i64,
}

/// Outcome of scanning one span target's git history for candidate commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetScan {
    /// The scan ran; these are the commits it found (possibly none).
    Scanned(Vec<ScannedCommit>),
    /// The scan could not run — the worktree is gone, `git log` failed, or the
    /// repository was unreadable. Distinct from `Scanned(vec![])`: the target's
    /// commits are unknown, not absent, so the sweep watermark must not move
    /// past it or the target would never be revisited.
    Unavailable(GitScanFailure),
}

/// Typed reason a bounded Git scan could not produce complete evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitScanFailure {
    Cancelled,
    DeadlineExceeded,
    OutputLimitExceeded,
    WorktreeUnavailable,
    CommandFailed,
    InvalidOutput,
}

/// Strictly parses bounded `%H %ct` output from `git log`.
///
/// Callers request `max + 1` rows from Git. Seeing that sentinel row is a
/// typed output-limit failure, while any malformed row makes the evidence
/// incomplete rather than silently disappearing from the scan.
pub(crate) fn parse_bounded_git_log(
    stdout: &str,
    max: usize,
) -> Result<Vec<ScannedCommit>, GitScanFailure> {
    if stdout.is_empty() {
        return Ok(Vec::new());
    }
    let mut commits = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else {
            return Err(GitScanFailure::InvalidOutput);
        };
        let Some(committed_at) = parts.next().and_then(|value| value.parse::<i64>().ok()) else {
            return Err(GitScanFailure::InvalidOutput);
        };
        if parts.next().is_some()
            || !(7..=64).contains(&sha.len())
            || !sha.chars().all(|character| character.is_ascii_hexdigit())
        {
            return Err(GitScanFailure::InvalidOutput);
        }
        if commits.len() >= max {
            return Err(GitScanFailure::OutputLimitExceeded);
        }
        commits.push(ScannedCommit {
            sha: sha.to_ascii_lowercase(),
            committed_at,
        });
    }
    Ok(commits)
}

/// Read-only database snapshot used by the Git phase after the snapshot closes.
#[derive(Debug, Clone)]
pub struct CommitAttributionPlan {
    expected_watermark: i64,
    targets: Vec<SpanScanTarget>,
    spans: Vec<Vec<SpanWindow>>,
}

impl CommitAttributionPlan {
    pub fn targets(&self) -> &[SpanScanTarget] {
        &self.targets
    }
}

/// Whether one prepared sweep covered every target in its read snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitAttributionCoverage {
    Complete,
    Partial,
}

/// Candidate rows derived entirely outside the database writer lane.
pub struct ScannedCommitAttributionPlan {
    expected_watermark: i64,
    new_watermark: i64,
    records: Vec<CommitSessionRecord>,
    coverage: CommitAttributionCoverage,
}

/// Result of the short compare-and-set publication phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitAttributionPublication {
    Published {
        inserted: usize,
        coverage: CommitAttributionCoverage,
    },
    Stale,
}

/// Store-level publication receipt including the measured longest writer hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAttributionStorePublication {
    pub outcome: CommitAttributionPublication,
    pub max_writer_hold_micros: u64,
}

const ATTRIBUTION_PUBLICATION_CHUNK: usize = 32;

/// Takes the short read phase of a commit-attribution sweep. The returned plan
/// owns every span window needed by Git scanning, so callers can close the
/// snapshot before invoking any subprocess.
pub async fn prepare_commit_attribution_sweep(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<CommitAttributionPlan, GitCorrelationError> {
    if !correlation_tables_present(conn).await? {
        return Ok(CommitAttributionPlan {
            expected_watermark: 0,
            targets: Vec::new(),
            spans: Vec::new(),
        });
    }
    let watermark = read_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    let mut targets = scan_targets_since(conn, watermark).await?;
    targets.sort_by_key(|target| {
        (
            target.max_updated_at,
            target.worktree.clone(),
            target.branch.clone(),
        )
    });
    let mut spans = Vec::with_capacity(targets.len());
    for target in &targets {
        spans.push(span_windows_for(conn, target.branch.as_deref(), &target.worktree).await?);
    }
    Ok(CommitAttributionPlan {
        expected_watermark: watermark,
        targets,
        spans,
    })
}

/// Runs bounded Git scans and pure span matching with no database handle live.
///
/// A failed target ends this pass at a contiguous prefix. That makes
/// cancellation and transient failures restartable without advancing past
/// evidence that was never read.
pub fn scan_commit_attribution_plan<F>(
    plan: &CommitAttributionPlan,
    gap_secs: i64,
    mut scan: F,
) -> ScannedCommitAttributionPlan
where
    F: FnMut(&SpanScanTarget) -> TargetScan,
{
    let mut records = Vec::new();
    let mut new_watermark = plan.expected_watermark;
    let mut coverage = CommitAttributionCoverage::Complete;
    for (target, spans) in plan.targets.iter().zip(&plan.spans) {
        if spans.is_empty() {
            new_watermark = new_watermark.max(target.max_updated_at);
            continue;
        }
        let commits = match scan(target) {
            TargetScan::Scanned(commits) => commits,
            TargetScan::Unavailable(_) => {
                coverage = CommitAttributionCoverage::Partial;
                break;
            }
        };
        for commit in commits {
            records.extend(match_commit_to_spans(
                &commit.sha,
                target.branch.as_deref(),
                &target.worktree,
                commit.committed_at,
                spans,
                gap_secs,
            ));
        }
        new_watermark = new_watermark.max(target.max_updated_at);
    }
    ScannedCommitAttributionPlan {
        expected_watermark: plan.expected_watermark,
        new_watermark,
        records,
        coverage,
    }
}

/// Publishes one scanned plan under an exact watermark compare-and-set.
///
/// Production callers pass a newly-opened write transaction here only after
/// [`scan_commit_attribution_plan`] has returned.
pub async fn publish_commit_attribution_plan(
    conn: &(impl Executor + ?Sized),
    scanned: ScannedCommitAttributionPlan,
) -> Result<CommitAttributionPublication, GitCorrelationError> {
    let current = read_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    if current != scanned.expected_watermark {
        return Ok(CommitAttributionPublication::Stale);
    }
    let mut inserted = 0;
    for record in &scanned.records {
        if upsert_commit_session(conn, record).await? {
            inserted += 1;
        }
    }
    if scanned.new_watermark > current {
        write_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY, scanned.new_watermark).await?;
    }
    Ok(CommitAttributionPublication::Published {
        inserted,
        coverage: scanned.coverage,
    })
}

/// Publishes candidate rows in short idempotent chunks, then advances the
/// watermark in a final CAS transaction. A crash between chunks leaves valid
/// rows but no advanced watermark, so restart safely resumes and converges.
pub async fn publish_commit_attribution_plan_to_store<S: GitCorrelationSessionStore>(
    store: &S,
    scanned: ScannedCommitAttributionPlan,
) -> Result<CommitAttributionStorePublication, GitCorrelationError> {
    let ScannedCommitAttributionPlan {
        expected_watermark,
        new_watermark,
        records,
        coverage,
    } = scanned;
    let mut inserted = 0usize;
    let mut max_writer_hold_micros = 0u64;
    for chunk in records.chunks(ATTRIBUTION_PUBLICATION_CHUNK) {
        let transaction = store.open_write_transaction().await?;
        let writer_started = std::time::Instant::now();
        let current = read_meta_value(&transaction, COMMIT_SWEEP_WATERMARK_KEY)
            .await?
            .unwrap_or(0);
        if current != expected_watermark {
            GitCorrelationWriteTxn::commit(transaction).await?;
            max_writer_hold_micros = max_writer_hold_micros.max(elapsed_micros(writer_started));
            return Ok(CommitAttributionStorePublication {
                outcome: CommitAttributionPublication::Stale,
                max_writer_hold_micros,
            });
        }
        for record in chunk {
            if upsert_commit_session(&transaction, record).await? {
                inserted += 1;
            }
        }
        GitCorrelationWriteTxn::commit(transaction).await?;
        max_writer_hold_micros = max_writer_hold_micros.max(elapsed_micros(writer_started));
    }

    let transaction = store.open_write_transaction().await?;
    let writer_started = std::time::Instant::now();
    let current = read_meta_value(&transaction, COMMIT_SWEEP_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    let outcome = if current != expected_watermark {
        CommitAttributionPublication::Stale
    } else {
        if new_watermark > current {
            write_meta_value(&transaction, COMMIT_SWEEP_WATERMARK_KEY, new_watermark).await?;
        }
        CommitAttributionPublication::Published { inserted, coverage }
    };
    GitCorrelationWriteTxn::commit(transaction).await?;
    max_writer_hold_micros = max_writer_hold_micros.max(elapsed_micros(writer_started));
    Ok(CommitAttributionStorePublication {
        outcome,
        max_writer_hold_micros,
    })
}

fn elapsed_micros(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}
