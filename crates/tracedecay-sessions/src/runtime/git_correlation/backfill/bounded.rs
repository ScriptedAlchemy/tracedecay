use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::history_progress::{
    self, GitHistoryCursorHeadState, GitHistoryPendingRow, GitHistoryProgressKey,
    GitHistoryProgressRow, GitHistoryScanMode, GitHistorySeenRow, GitHistorySegmentRow,
};
use crate::observation::ObservationCancellation;

use super::*;

mod native;
mod state;

use state::{
    advance_graph, advance_reflog_capture, advance_reflog_verification, reset_exact_progress,
};
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

    fn should_soft_stop(&self, reserve: Duration) -> Result<bool, BoundedBackfillInterruption> {
        self.check()?;
        Ok(self
            .deadline
            .is_none_or(|deadline| deadline.saturating_duration_since(Instant::now()) <= reserve))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundedBackfillInterruption {
    Cancelled,
    CommandTimedOut,
    HistoryLimitReached,
    DryRunFrontierLimitReached,
    UnsupportedSourceFraming,
    SourceChanged,
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
            Ok(StreamGitEvidenceOutcome::Progressed) => {
                return Ok(BoundedBackfillOutcome {
                    stats,
                    committed,
                    frontier,
                    remaining_sessions: 1,
                    interruption: None,
                });
            }
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
    Progressed,
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
    if opts.dry_run {
        dry_run_native_history(
            &project_path,
            window_start,
            window_end,
            opts.max_commits_per_repo,
            control,
            stats,
        )
        .await?;
        return Ok(StreamGitEvidenceOutcome::Applied(None));
    }
    let key = GitHistoryProgressKey {
        activity_timestamp: candidate_frontier.activity_timestamp,
        source_rowid: candidate_frontier.source_rowid,
    };
    loop {
        let snapshot = session_store
            .read_snapshot()
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        let progress = history_progress::read_progress(&snapshot, key)
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        drop(snapshot);
        let Some(progress) = progress else {
            let native_control = control.clone();
            let native_path = project_path.clone();
            let cursor = tokio::task::spawn_blocking(move || {
                native::initialize_reflog_cursor(&native_path, window_end, &native_control)
            })
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
            let progress = progress_from_cursor(key, row, window_start, window_end, cursor)?;
            let transaction = session_store
                .open_write_transaction()
                .await
                .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
            control.check()?;
            let inserted = history_progress::insert_progress(&transaction, &progress)
                .await
                .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
            if inserted {
                control.check()?;
                GitCorrelationWriteTxn::commit(transaction)
                    .await
                    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
                *committed = true;
            }
            if control.should_soft_stop(Duration::from_millis(750))? {
                return Ok(StreamGitEvidenceOutcome::Progressed);
            }
            continue;
        };
        if progress.provider != row.provider
            || progress.session_id != row.session_id
            || progress.project_path != row.project_path
            || progress.window_start != window_start
            || progress.window_end != window_end
        {
            reset_exact_progress(session_store, &progress, control, committed).await?;
            return Ok(StreamGitEvidenceOutcome::Progressed);
        }
        let result = match progress.scan_mode {
            GitHistoryScanMode::ReflogCapture => {
                advance_reflog_capture(session_store, &project_path, &progress, control, committed)
                    .await
            }
            GitHistoryScanMode::ReflogVerify => {
                advance_reflog_verification(
                    session_store,
                    &project_path,
                    &progress,
                    control,
                    committed,
                )
                .await
            }
            GitHistoryScanMode::Graph => {
                advance_graph(
                    session_store,
                    &project_path,
                    row,
                    candidate_frontier,
                    &progress,
                    opts,
                    control,
                    stats,
                    committed,
                )
                .await
            }
        };
        if matches!(result, Err(BoundedBackfillInterruption::SourceChanged)) {
            reset_exact_progress(session_store, &progress, control, committed).await?;
            return Ok(StreamGitEvidenceOutcome::Progressed);
        }
        match result {
            Ok(StreamGitEvidenceOutcome::Progressed)
                if !control.should_soft_stop(Duration::from_millis(750))? =>
            {
                continue;
            }
            result => return result,
        }
    }
}

fn progress_from_cursor(
    key: GitHistoryProgressKey,
    row: &SessionActivityRow,
    window_start: i64,
    window_end: i64,
    cursor: native::ReflogCursor,
) -> Result<GitHistoryProgressRow, BoundedBackfillInterruption> {
    let (cursor_head_state, cursor_head_branch) = match cursor.state {
        native::ReflogHeadState::LocalBranch(branch) => {
            (GitHistoryCursorHeadState::LocalBranch, Some(branch))
        }
        native::ReflogHeadState::Detached => (GitHistoryCursorHeadState::Detached, None),
    };
    let reflog_byte_length = cursor.byte_offset;
    Ok(GitHistoryProgressRow {
        key,
        provider: row.provider.clone(),
        session_id: row.session_id.clone(),
        project_path: row.project_path.clone(),
        window_start,
        window_end,
        worktree: native::encode_path(&cursor.worktree)?,
        generation: 0,
        scan_mode: GitHistoryScanMode::ReflogCapture,
        reflog_path: native::encode_path(&cursor.reflog_path)?,
        reflog_byte_offset: cursor.byte_offset,
        reflog_byte_length,
        source_generation: cursor.source_generation,
        reflog_digest: cursor.content_chain,
        capture_target_offset: None,
        verify_byte_offset: reflog_byte_length,
        verify_digest: history_progress::initial_reflog_content_chain().to_owned(),
        source_head_referent: cursor.source_head_referent,
        source_head_oid: cursor.source_head_oid,
        cursor_head_state,
        cursor_head_branch,
        cursor_oid: cursor.state_oid,
        segment_end: cursor.segment_end,
        segment_tip_oid: cursor.segment_tip_oid,
        segment_cursor: 0,
        emitted_count: 0,
        consulted_refs: cursor.consulted_refs,
    })
}

fn cursor_from_progress(
    progress: &GitHistoryProgressRow,
) -> Result<native::ReflogCursor, BoundedBackfillInterruption> {
    let state = match progress.cursor_head_state {
        GitHistoryCursorHeadState::LocalBranch => native::ReflogHeadState::LocalBranch(
            progress
                .cursor_head_branch
                .clone()
                .ok_or(BoundedBackfillInterruption::SourceUnavailable)?,
        ),
        GitHistoryCursorHeadState::Detached => native::ReflogHeadState::Detached,
    };
    Ok(native::ReflogCursor {
        worktree: native::decode_path(&progress.worktree)?,
        reflog_path: native::decode_path(&progress.reflog_path)?,
        source_generation: progress.source_generation.clone(),
        source_head_referent: progress.source_head_referent.clone(),
        source_head_oid: progress.source_head_oid.clone(),
        byte_offset: progress.reflog_byte_offset,
        state,
        state_oid: progress.cursor_oid.clone(),
        segment_end: progress.segment_end,
        segment_tip_oid: progress.segment_tip_oid.clone(),
        next_segment_ordinal: i64::try_from(progress.segment_cursor)
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?,
        consulted_refs: progress.consulted_refs.clone(),
        content_chain: progress.reflog_digest.clone(),
    })
}

fn copy_cursor_to_progress(
    progress: &mut GitHistoryProgressRow,
    cursor: native::ReflogCursor,
) -> Result<(), BoundedBackfillInterruption> {
    progress.reflog_byte_offset = cursor.byte_offset;
    progress.reflog_digest = cursor.content_chain;
    (progress.cursor_head_state, progress.cursor_head_branch) = match cursor.state {
        native::ReflogHeadState::LocalBranch(branch) => {
            (GitHistoryCursorHeadState::LocalBranch, Some(branch))
        }
        native::ReflogHeadState::Detached => (GitHistoryCursorHeadState::Detached, None),
    };
    progress.cursor_oid = cursor.state_oid;
    progress.segment_end = cursor.segment_end;
    progress.segment_tip_oid = cursor.segment_tip_oid;
    progress.segment_cursor = u64::try_from(cursor.next_segment_ordinal)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    progress.consulted_refs = cursor.consulted_refs;
    Ok(())
}

async fn dry_run_native_history(
    project_path: &std::path::Path,
    window_start: i64,
    window_end: i64,
    max_commits: usize,
    control: &BoundedGitControl,
    stats: &mut BackfillStats,
) -> Result<(), BoundedBackfillInterruption> {
    let path = project_path.to_owned();
    let native_control = control.clone();
    let mut cursor = tokio::task::spawn_blocking(move || {
        native::initialize_reflog_cursor(&path, window_end, &native_control)
    })
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
    let initial_cursor = cursor.clone();
    let source_length = cursor.byte_offset;
    loop {
        let path = project_path.to_owned();
        let native_control = control.clone();
        let scan_cursor = cursor;
        let chunk = tokio::task::spawn_blocking(move || {
            native::scan_reflog_chunk(
                &path,
                window_start,
                window_end,
                scan_cursor,
                &native_control,
            )
        })
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
        cursor = chunk.cursor;
        if chunk.complete {
            break;
        }
    }
    let target = cursor.byte_offset;
    let mut verification = native::ReflogVerificationCursor {
        byte_offset: source_length,
        content_chain: history_progress::initial_reflog_content_chain().to_owned(),
    };
    loop {
        let path = project_path.to_owned();
        let source = cursor.clone();
        let native_control = control.clone();
        let chunk = tokio::task::spawn_blocking(move || {
            native::scan_reflog_verification_chunk(
                &path,
                &source,
                target,
                verification,
                &native_control,
            )
        })
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
        verification = chunk.cursor;
        if chunk.complete {
            break;
        }
    }
    let sealed_source = cursor;
    let mut replay = initial_cursor;
    let mut emitted = 0_usize;
    let mut spans = 0_usize;
    loop {
        let path = project_path.to_owned();
        let native_control = control.clone();
        let replay_cursor = replay;
        let chunk = tokio::task::spawn_blocking(move || {
            native::scan_reflog_chunk(
                &path,
                window_start,
                window_end,
                replay_cursor,
                &native_control,
            )
        })
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
        for segment in chunk.segments {
            emitted = dry_run_segment(
                project_path,
                window_start,
                window_end,
                &sealed_source,
                segment.tip_oid,
                max_commits,
                emitted,
                control,
            )
            .await?;
            spans = spans.saturating_add(1);
        }
        replay = chunk.cursor;
        if chunk.complete {
            if replay.byte_offset != target
                || replay.content_chain != sealed_source.content_chain
                || replay.consulted_refs != sealed_source.consulted_refs
            {
                return Err(BoundedBackfillInterruption::SourceChanged);
            }
            break;
        }
    }
    stats.spans_written = stats.spans_written.saturating_add(spans);
    stats.commits_attributed = stats.commits_attributed.saturating_add(emitted);
    Ok(())
}

async fn dry_run_segment(
    project_path: &std::path::Path,
    window_start: i64,
    window_end: i64,
    source: &native::ReflogCursor,
    tip_oid: String,
    max_commits: usize,
    mut emitted: usize,
    control: &BoundedGitControl,
) -> Result<usize, BoundedBackfillInterruption> {
    const MAX_DRY_RUN_FRONTIER_ITEMS: usize = 4096;
    const MAX_DRY_RUN_FRONTIER_BYTES: usize = 256 * 1024;

    let mut pending = BTreeMap::from([(tip_oid.clone(), native::GraphPending { oid: tip_oid })]);
    let mut seen = BTreeSet::new();
    let mut seen_bytes = 0_usize;
    while !pending.is_empty() {
        let page = pending
            .values()
            .take(history_progress::MAX_PENDING_PAGE_ROWS)
            .cloned()
            .collect::<Vec<_>>();
        for item in &page {
            pending.remove(&item.oid);
        }
        let path = project_path.to_owned();
        let source = source.clone();
        let native_control = control.clone();
        let remaining = max_commits.saturating_sub(emitted);
        let chunk = tokio::task::spawn_blocking(move || {
            native::scan_graph_chunk(
                &path,
                window_start,
                window_end,
                &source,
                page,
                remaining,
                &native_control,
            )
        })
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
        emitted = emitted.saturating_add(chunk.commits.len());
        for oid in chunk.newly_seen {
            if seen.insert(oid.clone()) {
                seen_bytes = seen_bytes
                    .checked_add(oid.len())
                    .ok_or(BoundedBackfillInterruption::DryRunFrontierLimitReached)?;
            }
        }
        for item in chunk.pending {
            if !seen.contains(&item.oid) {
                pending.entry(item.oid.clone()).or_insert(item);
            }
        }
        let frontier_bytes = pending
            .keys()
            .try_fold(0_usize, |total, oid| total.checked_add(oid.len()))
            .ok_or(BoundedBackfillInterruption::DryRunFrontierLimitReached)?;
        if pending.len() > MAX_DRY_RUN_FRONTIER_ITEMS
            || frontier_bytes > MAX_DRY_RUN_FRONTIER_BYTES
            || seen.len() > MAX_DRY_RUN_FRONTIER_ITEMS
            || seen_bytes > MAX_DRY_RUN_FRONTIER_BYTES
        {
            return Err(BoundedBackfillInterruption::DryRunFrontierLimitReached);
        }
    }
    Ok(emitted)
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
    super::super::record_span_observation_in_transaction(
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
    branch: Option<&str>,
    worktree: &str,
    sha: &str,
    committed_at: i64,
    control: &BoundedGitControl,
) -> Result<bool, BoundedBackfillInterruption> {
    control.check()?;
    let inserted = super::super::upsert_commit_session(
        transaction,
        &CommitSessionRecord {
            commit_sha: sha.to_owned(),
            provider: row.provider.clone(),
            session_id: row.session_id.clone(),
            branch: branch.map(str::to_owned),
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
mod tests;
