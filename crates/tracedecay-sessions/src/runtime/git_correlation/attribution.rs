use libsql::{Connection, params};

use super::{
    CommitEvidence, CommitRelation, CommitSessionRecord, GitCorrelationError, SpanOverlapKind,
    correlation_tables_present, opt_text, upsert_commit_session,
};

const COMMIT_SWEEP_WATERMARK_KEY: &str = "commit_attribution_watermark";

pub async fn read_meta_value(
    conn: &Connection,
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
    conn: &Connection,
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
    conn: &Connection,
    since_ts: i64,
) -> Result<Vec<SpanScanTarget>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT branch, worktree, MIN(first_ts), MAX(last_ts), MAX(updated_at)
             FROM session_git_spans
             WHERE updated_at >= ?1
             GROUP BY branch, worktree",
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
    conn: &Connection,
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

/// Runs commit attribution for span targets touched since the last sweep.
pub async fn run_commit_attribution_sweep<F>(
    conn: &Connection,
    gap_secs: i64,
    mut scan: F,
) -> Result<usize, GitCorrelationError>
where
    F: FnMut(&SpanScanTarget) -> Vec<ScannedCommit>,
{
    if !correlation_tables_present(conn).await? {
        return Ok(0);
    }
    let watermark = read_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY)
        .await?
        .unwrap_or(0);
    let targets = scan_targets_since(conn, watermark).await?;
    let mut inserted = 0usize;
    let mut new_watermark = watermark;
    for target in &targets {
        // Advance on write time, not event time, so the watermark reflects how
        // far ingestion has progressed rather than how recent the commits are.
        new_watermark = new_watermark.max(target.max_updated_at);
        let spans = span_windows_for(conn, target.branch.as_deref(), &target.worktree).await?;
        if spans.is_empty() {
            continue;
        }
        for commit in scan(target) {
            let records = match_commit_to_spans(
                &commit.sha,
                target.branch.as_deref(),
                &target.worktree,
                commit.committed_at,
                &spans,
                gap_secs,
            );
            for record in &records {
                if upsert_commit_session(conn, record).await? {
                    inserted += 1;
                }
            }
        }
    }
    if new_watermark > watermark {
        write_meta_value(conn, COMMIT_SWEEP_WATERMARK_KEY, new_watermark).await?;
    }
    Ok(inserted)
}
