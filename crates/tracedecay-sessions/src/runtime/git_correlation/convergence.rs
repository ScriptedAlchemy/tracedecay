//! One convergence pass over a project's Git evidence.
//!
//! A pass derives span and commit evidence for every retained session past
//! the history frontier, appends it to the per-session rows, attributes
//! commits to the spans no attribution has covered yet, and advances the
//! frontier, all in one transaction that installs at most one generation. A
//! pass that finds nothing new changes no row and installs nothing, so a
//! settled project stays settled until new evidence arrives.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::attribution::attribute_commits;
use super::backfill::{advance_history_frontier, collect_incremental_backfill, scan_span_target};
use super::rows::{GitEvidenceBatch, GitEvidenceGeneration, GitEvidenceWriter};
use super::{
    BackfillOptions, BackfillStats, DEFAULT_SPAN_MERGE_GAP_SECS, GitCorrelationError,
    GitCorrelationSessionStore, GitCorrelationWriteTxn, GitHistoryIndexFrontier, GitReflogSource,
    read_meta_value, write_meta_value,
};

/// Consecutive passes whose attribution a transient Git failure held back.
const GIT_ATTRIBUTION_FAILED_PASSES_KEY: &str = "git_attribution_failed_passes";
/// Passes a span target whose Git reads fail may hold attribution back before
/// it is given up as unavailable. A failure that never clears must not pin
/// every later drain (#3984).
const MAX_ATTRIBUTION_ATTEMPTS: i64 = 5;

/// Receipt of one convergence pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidencePass {
    /// Retained-history counters; `spans_written` and `commits_attributed`
    /// count the rows the pass changed.
    pub backfill: BackfillStats,
    /// Durable `(change sequence, rowid)` history frontier after the pass.
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

#[tracing::instrument(
    name = "sessions.git_correlation.converge_pass",
    level = "trace",
    skip_all
)]
pub async fn converge_git_evidence_pass<S>(
    session_store: &S,
    git: Arc<dyn GitReflogSource>,
    project_root: Option<&std::path::Path>,
) -> Result<GitEvidencePassOutcome, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
{
    session_store.require_project_sessions_authority()?;
    let backfill =
        collect_incremental_backfill(session_store, Arc::clone(&git), project_root).await?;
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
    for (provider, session_id, revision) in &backfill.settled_sessions {
        writer
            .replace_backfill_session(provider, session_id, *revision)
            .await?;
    }
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
    // Each span target scan runs `git`, so attribution waits on the blocking
    // pool rather than a native worker.
    let attribution = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        attribute_commits(&pending, DEFAULT_SPAN_MERGE_GAP_SECS, |target| {
            scan_span_target(
                git.as_ref(),
                target,
                DEFAULT_SPAN_MERGE_GAP_SECS,
                max_commits,
            )
        })
    })
    .await
    .map_err(GitCorrelationError::Unavailable)?;
    stats.unavailable_attributions = stats
        .unavailable_attributions
        .saturating_add(attribution.unavailable_references);
    let failed_passes = read_meta_value(&transaction, GIT_ATTRIBUTION_FAILED_PASSES_KEY)
        .await?
        .unwrap_or(0);
    let later_failure = match attribution.retryable_targets.first() {
        Some(target) if failed_passes.saturating_add(1) < MAX_ATTRIBUTION_ATTEMPTS => {
            stats.skipped_git_error = stats.skipped_git_error.saturating_add(1);
            Some(GitCorrelationError::Unavailable(format!(
                "cannot scan Git history for retained span target {target}"
            )))
        }
        Some(_) => {
            tracing::warn!(
                targets = ?attribution.retryable_targets,
                attempts = MAX_ATTRIBUTION_ATTEMPTS,
                "Git history of retained span targets stayed unreadable; their historical commit attribution is unavailable"
            );
            stats.unavailable_attributions = stats
                .unavailable_attributions
                .saturating_add(attribution.retryable_targets.len());
            None
        }
        None => None,
    };
    let attributed = writer
        .apply(GitEvidenceBatch {
            commits: attribution.records,
            merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
            ..GitEvidenceBatch::default()
        })
        .await?;
    let next_failed_passes = if later_failure.is_some() {
        failed_passes.saturating_add(1)
    } else {
        writer.mark_attributed();
        0
    };
    stats.spans_written = backfilled.spans_changed + attributed.spans_changed;
    stats.commits_attributed = backfilled.commits_changed + attributed.commits_changed;
    let generation = writer.finish().await?;
    if next_failed_passes != failed_passes {
        write_meta_value(
            &transaction,
            GIT_ATTRIBUTION_FAILED_PASSES_KEY,
            next_failed_passes,
        )
        .await?;
    }
    let frontier = match backfill.settled_through {
        Some(settled) => advance_history_frontier(&transaction, settled).await?,
        None => backfill.start,
    };
    GitCorrelationWriteTxn::commit(transaction).await?;
    stats.frontier_advanced = frontier != backfill.start;
    Ok(GitEvidencePassOutcome {
        pass: GitEvidencePass {
            backfill: stats,
            frontier,
            generation,
        },
        later_failure,
    })
}
