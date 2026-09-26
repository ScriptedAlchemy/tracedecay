//! One convergence pass over a project's Git evidence.
//!
//! Every publication rewrites the complete projection, so evidence is folded
//! before it is published: all pending transcript receipts, every retained
//! session past the history frontier, and the commit attribution over the
//! resulting spans land in at most one generation per pass. A pass whose fold
//! changes nothing publishes nothing, so a settled project stays settled until
//! new evidence arrives.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_graph_db::GraphNamespace;

use super::attribution::{
    attribute_commits, merge_commit, merge_span, transcript_spans_from_observations,
};
use super::backfill::{advance_history_frontier, collect_incremental_backfill, scan_span_target};
use super::publication_outbox::{
    read_pending_git_evidence_publications, settle_git_evidence_publication,
};
use super::{
    BackfillOptions, BackfillStats, DEFAULT_SPAN_MERGE_GAP_SECS, GitCorrelationError,
    GitCorrelationSessionStore, GitCorrelationWriteTxn, GitHistoryIndexFrontier, GitReflogSource,
    git_evidence_projection_identity, recover_git_evidence_projection,
};

const GIT_EVIDENCE_GRAPH_NAMESPACE: &str = "project";

/// Receipt of one convergence pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidencePass {
    /// Transcript receipts folded into the head and settled.
    pub settled_receipts: usize,
    /// Retained-history counters; `spans_written` and `commits_attributed`
    /// count what the pass's generation changed relative to the head.
    pub backfill: BackfillStats,
    /// Durable `(activity, rowid)` history frontier after the pass.
    pub frontier: GitHistoryIndexFrontier,
    /// Whether the pass published a generation.
    pub published: bool,
}

impl GitEvidencePass {
    pub const fn committed_progress(&self) -> bool {
        self.published || self.settled_receipts > 0 || self.backfill.committed_progress()
    }
}

/// A pass plus a failure observed after it already committed progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEvidencePassOutcome {
    pub pass: GitEvidencePass,
    pub later_failure: Option<GitCorrelationError>,
}

/// Folds all pending evidence into the verified head, publishes at most one
/// generation, then settles the folded receipts and advances the history
/// frontier in one transaction.
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
    let snapshot = session_store.read_snapshot().await?;
    let receipts = read_pending_git_evidence_publications(&snapshot).await?;
    drop(snapshot);
    let backfill = collect_incremental_backfill(session_store, git).await?;
    let mut stats = backfill.stats;

    let identity =
        git_evidence_projection_identity(GraphNamespace::new(GIT_EVIDENCE_GRAPH_NAMESPACE)?)?;
    let head = recover_git_evidence_projection(
        session_store.graph_runtime()?,
        &identity,
        Arc::new(AtomicBool::new(false)),
    )?;
    let (mut spans, mut commits) = head.map_or_else(
        || (Vec::new(), Vec::new()),
        |store| {
            (
                store.projection().spans().to_vec(),
                store.projection().commit_sessions().to_vec(),
            )
        },
    );
    let observations = receipts
        .iter()
        .flat_map(|receipt| receipt.span_observations().iter().cloned())
        .collect::<Vec<_>>();
    let mut new_spans =
        transcript_spans_from_observations(&spans, &observations, DEFAULT_SPAN_MERGE_GAP_SECS);
    new_spans.extend(backfill.spans);
    let spans_changed = new_spans
        .iter()
        .filter(|incoming| merge_span(&mut spans, incoming))
        .count();
    let mut new_commits = receipts
        .iter()
        .flat_map(|receipt| receipt.commit_records().iter().cloned())
        .chain(backfill.commits)
        .collect::<Vec<_>>();
    let max_commits = BackfillOptions::default().max_commits_per_repo;
    let attribution_failure =
        match attribute_commits(&spans, DEFAULT_SPAN_MERGE_GAP_SECS, |target| {
            scan_span_target(git, target, DEFAULT_SPAN_MERGE_GAP_SECS, max_commits)
        }) {
            Ok(attribution) => {
                stats.unavailable_attributions = stats
                    .unavailable_attributions
                    .saturating_add(attribution.unavailable_references);
                new_commits.extend(attribution.records);
                None
            }
            Err(error) => {
                stats.skipped_git_error = stats.skipped_git_error.saturating_add(1);
                Some(error)
            }
        };
    let commits_changed = new_commits
        .iter()
        .filter(|incoming| merge_commit(&mut commits, incoming))
        .count();
    drop((spans, commits));

    // The publication re-merges onto the head it recovers under the
    // publication lock, so evidence a concurrent publisher added meanwhile is
    // kept rather than replaced.
    let published = spans_changed > 0 || commits_changed > 0;
    if published {
        (stats.spans_written, stats.commits_attributed) = session_store
            .publish_graph_evidence_owned("git-convergence".to_owned(), new_spans, new_commits)
            .await?;
    }
    crate::runtime::pipeline_metrics::record_git_backfill(
        stats.sessions_scanned,
        stats.spans_written,
    );

    let mut pass = GitEvidencePass {
        settled_receipts: 0,
        backfill: stats,
        frontier: backfill.start,
        published,
    };
    if receipts.is_empty() && backfill.settled_through.is_none() {
        return Ok(GitEvidencePassOutcome {
            pass,
            later_failure: attribution_failure,
        });
    }
    let settlement = async {
        let transaction = session_store.open_write_transaction().await?;
        for receipt in &receipts {
            settle_git_evidence_publication(&transaction, receipt).await?;
        }
        let frontier = match backfill.settled_through {
            Some(settled) => advance_history_frontier(&transaction, settled).await?,
            None => backfill.start,
        };
        GitCorrelationWriteTxn::commit(transaction).await?;
        Ok::<_, GitCorrelationError>(frontier)
    }
    .await;
    match settlement {
        Ok(frontier) => {
            pass.settled_receipts = receipts.len();
            pass.backfill.frontier_advanced = frontier != backfill.start;
            pass.frontier = frontier;
            Ok(GitEvidencePassOutcome {
                pass,
                later_failure: attribution_failure,
            })
        }
        Err(error) if pass.committed_progress() => Ok(GitEvidencePassOutcome {
            pass,
            later_failure: Some(error),
        }),
        Err(error) => Err(error),
    }
}
