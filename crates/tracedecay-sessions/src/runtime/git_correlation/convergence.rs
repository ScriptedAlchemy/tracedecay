//! One convergence pass over a project's Git evidence.
//!
//! A pass derives span and commit evidence for every retained session past
//! the history frontier, appends it to the per-session rows, attributes
//! commits to the spans no attribution has covered yet, and advances the
//! frontier, all in one transaction that installs at most one generation. A
//! pass that finds nothing new changes no row and installs nothing, so a
//! settled project stays settled until new evidence arrives.

use std::time::{SystemTime, UNIX_EPOCH};

use super::attribution::attribute_commits;
use super::backfill::{advance_history_frontier, collect_incremental_backfill, scan_span_target};
use super::rows::{GitEvidenceBatch, GitEvidenceGeneration, GitEvidenceWrite, GitEvidenceWriter};
use super::{
    BackfillOptions, BackfillStats, DEFAULT_SPAN_MERGE_GAP_SECS, GitCorrelationError,
    GitCorrelationSessionStore, GitCorrelationWriteTxn, GitHistoryIndexFrontier, GitReflogSource,
};

/// Receipt of one convergence pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidencePass {
    /// Retained-history counters; `spans_written` and `commits_attributed`
    /// count the rows the pass changed.
    pub backfill: BackfillStats,
    /// Durable `(activity, rowid)` history frontier after the pass.
    pub frontier: GitHistoryIndexFrontier,
    /// The generation the pass installed; `None` when no row changed.
    pub generation: Option<GitEvidenceGeneration>,
}

impl GitEvidencePass {
    pub const fn committed_progress(&self) -> bool {
        self.generation.is_some() || self.backfill.committed_progress()
    }
}

/// A committed pass plus an attribution failure the next pass retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidencePassOutcome {
    pub pass: GitEvidencePass,
    pub later_failure: Option<GitCorrelationError>,
}

#[hotpath::measure(label = "sessions.git_correlation.converge_pass", future = true)]
pub async fn converge_git_evidence_pass<S, G>(
    session_store: &S,
    git: &G,
) -> Result<GitEvidencePassOutcome, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
    G: GitReflogSource + ?Sized,
{
    session_store.require_project_sessions_authority()?;
    let backfill = collect_incremental_backfill(session_store, git).await?;
    let mut stats = backfill.stats;
    // Spans still inside their merge gap can gain commits made after their
    // last observation, so attribution revisits them until the gap closes.
    let open_since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .ok_or_else(|| {
            GitCorrelationError::Contract("system clock precedes the Unix epoch".to_owned())
        })?
        .saturating_sub(DEFAULT_SPAN_MERGE_GAP_SECS);

    let transaction = session_store.open_write_transaction().await?;
    let mut writer = GitEvidenceWriter::open(&transaction).await?;
    let backfilled = writer
        .apply(GitEvidenceBatch {
            spans: backfill.spans,
            commits: backfill.commits,
            merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
            ..GitEvidenceBatch::default()
        })
        .await?;
    let pending = writer.spans_pending_attribution(open_since).await?;
    let max_commits = BackfillOptions::default().max_commits_per_repo;
    let mut attributed = GitEvidenceWrite::default();
    let later_failure = match attribute_commits(&pending, DEFAULT_SPAN_MERGE_GAP_SECS, |target| {
        scan_span_target(git, target, DEFAULT_SPAN_MERGE_GAP_SECS, max_commits)
    }) {
        Ok(attribution) => {
            stats.unavailable_attributions = stats
                .unavailable_attributions
                .saturating_add(attribution.unavailable_references);
            attributed = writer
                .apply(GitEvidenceBatch {
                    commits: attribution.records,
                    merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
                    ..GitEvidenceBatch::default()
                })
                .await?;
            writer.mark_attributed();
            None
        }
        Err(error) => {
            stats.skipped_git_error = stats.skipped_git_error.saturating_add(1);
            Some(error)
        }
    };
    stats.spans_written = backfilled.spans_changed + attributed.spans_changed;
    stats.commits_attributed = backfilled.commits_changed + attributed.commits_changed;
    let generation = writer.finish().await?;
    let frontier = match backfill.settled_through {
        Some(settled) => advance_history_frontier(&transaction, settled).await?,
        None => backfill.start,
    };
    GitCorrelationWriteTxn::commit(transaction).await?;
    stats.frontier_advanced = frontier != backfill.start;
    crate::runtime::pipeline_metrics::record_git_backfill(
        stats.sessions_scanned,
        stats.spans_written,
    );
    Ok(GitEvidencePassOutcome {
        pass: GitEvidencePass {
            backfill: stats,
            frontier,
            generation,
        },
        later_failure,
    })
}
