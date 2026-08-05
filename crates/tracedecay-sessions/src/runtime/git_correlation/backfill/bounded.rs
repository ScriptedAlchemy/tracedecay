use std::io::BufRead;
use std::time::{Duration, Instant};

use crate::observation::ObservationCancellation;

use super::*;

mod native;

use native::PreparedGitEvent;

#[derive(Clone)]
pub struct BoundedGitControl {
    cancellation: ObservationCancellation,
    deadline: Option<Instant>,
}

impl BoundedGitControl {
    pub fn new(cancellation: ObservationCancellation, command_timeout: Duration) -> Self {
        Self {
            cancellation,
            deadline: Instant::now().checked_add(command_timeout),
        }
    }

    fn check(&self) -> Result<(), BoundedBackfillInterruption> {
        if self.cancellation.is_cancelled() {
            return Err(BoundedBackfillInterruption::Cancelled);
        }
        if match self.deadline {
            Some(deadline) => Instant::now() >= deadline,
            None => true,
        } {
            return Err(BoundedBackfillInterruption::CommandTimedOut);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundedBackfillInterruption {
    Cancelled,
    CommandTimedOut,
    HistoryLimitReached,
    SourceUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitHistoryIndexFrontier {
    pub activity_timestamp: i64,
    pub source_rowid: i64,
}

#[derive(Debug)]
pub struct BoundedBackfillOutcome {
    pub stats: BackfillStats,
    pub committed: bool,
    pub frontier: GitHistoryIndexFrontier,
    pub remaining_sessions: u64,
    pub interruption: Option<BoundedBackfillInterruption>,
}

pub async fn run_bounded_history_index_page<S>(
    session_store: &S,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
) -> Result<BoundedBackfillOutcome, GitCorrelationError>
where
    S: GitCorrelationSessionStore,
{
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let stored_activity = super::super::read_meta_value(&snapshot, AUTO_BACKFILL_WATERMARK_KEY)
        .await?
        .unwrap_or_else(|| opts.since.saturating_sub(1));
    let stored_rowid = super::super::read_meta_value(&snapshot, GIT_HISTORY_ROWID_FRONTIER_KEY)
        .await?
        .unwrap_or(0);
    let mut frontier = GitHistoryIndexFrontier {
        activity_timestamp: stored_activity.max(opts.since.saturating_sub(1)),
        source_rowid: if stored_activity >= opts.since.saturating_sub(1) {
            stored_rowid
        } else {
            0
        },
    };
    if let Err(interruption) = control.check() {
        drop(snapshot);
        return Ok(interrupted_outcome(
            BackfillStats::default(),
            false,
            frontier,
            interruption,
        ));
    }
    let requested = opts.limit_sessions.saturating_add(1);
    let mut rows = session_activity_page_after(
        &snapshot,
        frontier.activity_timestamp,
        frontier.source_rowid,
        requested,
    )
    .await
    .map_err(GitCorrelationError::Db)?;
    drop(snapshot);
    if let Err(interruption) = control.check() {
        return Ok(interrupted_outcome(
            BackfillStats::default(),
            false,
            frontier,
            interruption,
        ));
    }
    let has_more = bounded_page_has_more(rows.len(), opts.limit_sessions);
    rows.truncate(opts.limit_sessions);

    let mut stats = BackfillStats::default();
    let mut committed = false;
    for row in &rows {
        if let Err(interruption) = control.check() {
            return Ok(interrupted_outcome(
                stats,
                committed,
                frontier,
                interruption,
            ));
        }
        stats.sessions_scanned = stats.sessions_scanned.saturating_add(1);
        let candidate_frontier = GitHistoryIndexFrontier {
            activity_timestamp: row.activity_timestamp,
            source_rowid: row.source_rowid,
        };
        let mut frontier_pending = false;
        match stream_git_evidence(
            session_store,
            &row.session,
            candidate_frontier,
            opts,
            control,
            &mut stats,
            &mut committed,
        )
        .await
        {
            Ok(StreamGitEvidenceOutcome::Applied(Some(persisted))) => {
                frontier = persisted;
            }
            Ok(StreamGitEvidenceOutcome::Applied(None)) => {}
            Ok(StreamGitEvidenceOutcome::Skip(reason)) => {
                stats.record_skip(reason);
                frontier_pending = true;
            }
            Err(BoundedBackfillInterruption::SourceUnavailable) => {
                stats.record_skip(BackfillSkipReason::GitError);
                return Ok(BoundedBackfillOutcome {
                    stats,
                    committed,
                    frontier,
                    remaining_sessions: 1,
                    interruption: Some(BoundedBackfillInterruption::SourceUnavailable),
                });
            }
            Err(interruption) => {
                return Ok(BoundedBackfillOutcome {
                    stats,
                    committed,
                    frontier,
                    remaining_sessions: 1,
                    interruption: Some(interruption),
                });
            }
        }
        if frontier_pending && !opts.dry_run {
            if let Err(interruption) = control.check() {
                return Ok(interrupted_outcome(
                    stats,
                    committed,
                    frontier,
                    interruption,
                ));
            }
            frontier = match persist_frontier(session_store, candidate_frontier, control).await {
                Ok(frontier) => frontier,
                Err(interruption) => {
                    return Ok(interrupted_outcome(
                        stats,
                        committed,
                        frontier,
                        interruption,
                    ));
                }
            };
            committed = true;
        }
    }
    if let Err(interruption) = control.check() {
        return Ok(interrupted_outcome(
            stats,
            committed,
            frontier,
            interruption,
        ));
    }
    Ok(BoundedBackfillOutcome {
        stats,
        committed,
        frontier,
        remaining_sessions: u64::from(has_more),
        interruption: None,
    })
}

fn interrupted_outcome(
    stats: BackfillStats,
    committed: bool,
    frontier: GitHistoryIndexFrontier,
    interruption: BoundedBackfillInterruption,
) -> BoundedBackfillOutcome {
    BoundedBackfillOutcome {
        stats,
        committed,
        frontier,
        remaining_sessions: 1,
        interruption: Some(interruption),
    }
}

const fn bounded_page_has_more(row_count: usize, page_size: usize) -> bool {
    row_count > page_size
}

enum StreamGitEvidenceOutcome {
    Applied(Option<GitHistoryIndexFrontier>),
    Skip(BackfillSkipReason),
}

async fn stream_git_evidence<S: GitCorrelationSessionStore>(
    session_store: &S,
    row: &SessionActivityRow,
    candidate_frontier: GitHistoryIndexFrontier,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
    stats: &mut BackfillStats,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let Some((mut window_start, window_end)) = row.window() else {
        return Ok(StreamGitEvidenceOutcome::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    };
    if window_end < opts.since {
        return Ok(StreamGitEvidenceOutcome::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    }
    window_start = window_start.max(opts.since);
    if window_start > window_end {
        return Ok(StreamGitEvidenceOutcome::Skip(
            BackfillSkipReason::NoActivityWindow,
        ));
    }
    if row.project_path.trim().is_empty() {
        return Ok(StreamGitEvidenceOutcome::Skip(
            BackfillSkipReason::NotAWorktree,
        ));
    }
    control.check()?;
    let project_path = std::path::PathBuf::from(row.project_path.trim());
    let producer_control = control.clone();
    let max_commits = opts.max_commits_per_repo;
    let spool = tokio::task::spawn_blocking(move || {
        native::produce(
            &project_path,
            window_start,
            window_end,
            max_commits,
            &producer_control,
        )
    })
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
    control.check()?;
    let mut lines = std::io::BufReader::new(spool).lines();
    let first = lines
        .next()
        .transpose()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let PreparedGitEvent::Begin { worktree } =
        serde_json::from_str(&first).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    else {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    };
    let worktree = normalize_worktree(&worktree.to_string_lossy());
    control.check()?;
    let transaction = if opts.dry_run {
        None
    } else {
        let transaction = session_store
            .open_write_transaction()
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        control.check()?;
        Some(transaction)
    };
    let mut row_stats = BackfillStats::default();
    for line in lines {
        control.check()?;
        let line = line.map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        let event = serde_json::from_str(&line)
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        apply_git_event(
            event,
            row,
            opts,
            control,
            Some(&worktree),
            transaction.as_ref(),
            &mut row_stats,
        )
        .await?;
    }
    control.check()?;
    let persisted_frontier = if let Some(transaction) = transaction {
        control.check()?;
        let persisted = super::advance_history_frontier(&transaction, candidate_frontier)
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        control.check()?;
        GitCorrelationWriteTxn::commit(transaction)
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        *committed = true;
        Some(persisted)
    } else {
        None
    };
    stats.spans_written = stats.spans_written.saturating_add(row_stats.spans_written);
    stats.commits_attributed = stats
        .commits_attributed
        .saturating_add(row_stats.commits_attributed);
    Ok(StreamGitEvidenceOutcome::Applied(persisted_frontier))
}

async fn apply_git_event<T: GitCorrelationWriteTxn>(
    event: PreparedGitEvent,
    row: &SessionActivityRow,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
    worktree: Option<&str>,
    transaction: Option<&T>,
    stats: &mut BackfillStats,
) -> Result<(), BoundedBackfillInterruption> {
    control.check()?;
    match event {
        PreparedGitEvent::Begin { .. } => {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
        PreparedGitEvent::Segment { branch, start, end } => {
            let worktree = worktree.ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
            for timestamp in [start, end] {
                if !opts.dry_run {
                    record_span_in_transaction(
                        transaction.ok_or(BoundedBackfillInterruption::SourceUnavailable)?,
                        branch.as_deref(),
                        row,
                        worktree,
                        timestamp,
                        opts.merge_gap_secs,
                        control,
                    )
                    .await?;
                }
            }
            stats.spans_written = stats.spans_written.saturating_add(1);
        }
        PreparedGitEvent::Commit {
            branch,
            sha,
            committed_at,
        } => {
            let worktree = worktree.ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
            if opts.dry_run {
                stats.commits_attributed = stats.commits_attributed.saturating_add(1);
            } else {
                if record_commit_in_transaction(
                    transaction.ok_or(BoundedBackfillInterruption::SourceUnavailable)?,
                    row,
                    &branch,
                    worktree,
                    &sha,
                    committed_at,
                    control,
                )
                .await?
                {
                    stats.commits_attributed = stats.commits_attributed.saturating_add(1);
                }
            }
        }
    }
    control.check()
}

async fn record_span_in_transaction<T: GitCorrelationWriteTxn>(
    transaction: &T,
    branch: Option<&str>,
    row: &SessionActivityRow,
    worktree: &str,
    timestamp: i64,
    merge_gap_secs: i64,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    control.check()?;
    super::record_span_observation_in_transaction(
        transaction,
        &SpanObservation {
            provider: row.provider.clone(),
            session_id: row.session_id.clone(),
            thread_id: None,
            branch: branch.map(str::to_owned),
            worktree: worktree.to_owned(),
            ts: timestamp,
            source: SpanSource::Backfill,
        },
        merge_gap_secs,
    )
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()
}

async fn record_commit_in_transaction<T: GitCorrelationWriteTxn>(
    transaction: &T,
    row: &SessionActivityRow,
    branch: &str,
    worktree: &str,
    sha: &str,
    committed_at: i64,
    control: &BoundedGitControl,
) -> Result<bool, BoundedBackfillInterruption> {
    control.check()?;
    let inserted = super::upsert_commit_session(
        transaction,
        &CommitSessionRecord {
            commit_sha: sha.to_owned(),
            provider: row.provider.clone(),
            session_id: row.session_id.clone(),
            branch: Some(branch.to_owned()),
            worktree: Some(worktree.to_owned()),
            committed_at,
            span_overlap_kind: SpanOverlapKind::WithinSpan,
            span_id: None,
            relation: CommitRelation::Observed,
            evidence: CommitEvidence::ReflogOverlap,
            confidence: 30,
            evidence_message_id: None,
        },
    )
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    Ok(inserted)
}

async fn persist_frontier<S: GitCorrelationSessionStore>(
    session_store: &S,
    candidate: GitHistoryIndexFrontier,
    control: &BoundedGitControl,
) -> Result<GitHistoryIndexFrontier, BoundedBackfillInterruption> {
    control.check()?;
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    let persisted = super::advance_history_frontier(&transaction, candidate)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    Ok(persisted)
}

#[cfg(test)]
mod tests {
    use super::{
        BackfillStats, BoundedBackfillInterruption, BoundedGitControl, GitHistoryIndexFrontier,
        bounded_page_has_more, interrupted_outcome,
    };

    #[test]
    fn cancellation_precedes_deadline() {
        let cancellation = crate::observation::ObservationCancellation::default();
        cancellation.cancel();
        let control = BoundedGitControl::new(cancellation, std::time::Duration::ZERO);

        assert_eq!(
            control.check().unwrap_err(),
            BoundedBackfillInterruption::Cancelled
        );
    }

    #[test]
    fn one_absolute_deadline_expires_all_later_checks() {
        let control = BoundedGitControl::new(
            crate::observation::ObservationCancellation::default(),
            std::time::Duration::ZERO,
        );

        assert_eq!(
            control.check().unwrap_err(),
            BoundedBackfillInterruption::CommandTimedOut
        );
        assert_eq!(
            control.check().unwrap_err(),
            BoundedBackfillInterruption::CommandTimedOut
        );
    }

    #[test]
    fn interrupted_evidence_keeps_the_completed_row_frontier() {
        let frontier = GitHistoryIndexFrontier {
            activity_timestamp: 100,
            source_rowid: 7,
        };

        let outcome = interrupted_outcome(
            BackfillStats::default(),
            false,
            frontier,
            BoundedBackfillInterruption::CommandTimedOut,
        );

        assert_eq!(outcome.frontier, frontier);
        assert_eq!(outcome.remaining_sessions, 1);
    }

    #[test]
    fn history_limit_keeps_the_completed_row_frontier() {
        let frontier = GitHistoryIndexFrontier {
            activity_timestamp: 100,
            source_rowid: 7,
        };

        let outcome = interrupted_outcome(
            BackfillStats::default(),
            false,
            frontier,
            BoundedBackfillInterruption::HistoryLimitReached,
        );

        assert_eq!(outcome.frontier, frontier);
        assert_eq!(
            outcome.interruption,
            Some(BoundedBackfillInterruption::HistoryLimitReached)
        );
        assert_eq!(outcome.remaining_sessions, 1);
    }

    #[test]
    fn bounded_history_page_reports_unconsumed_session_suffix() {
        assert!(bounded_page_has_more(51, 50));
        assert!(!bounded_page_has_more(50, 50));
    }
}
