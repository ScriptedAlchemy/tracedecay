use std::path::Path;

use super::*;

pub(super) async fn advance_reflog_capture<S: GitCorrelationSessionStore>(
    session_store: &S,
    project_path: &Path,
    progress: &GitHistoryProgressRow,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let cursor = cursor_from_progress(progress)?;
    let path = project_path.to_owned();
    let native_control = control.clone();
    let window_start = progress.window_start;
    let window_end = progress.window_end;
    let chunk = tokio::task::spawn_blocking(move || {
        native::scan_reflog_chunk(&path, window_start, window_end, cursor, &native_control)
    })
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
    verify_source_without_writer(project_path, &chunk.cursor, control).await?;

    let mut next = progress.clone();
    next.generation = next
        .generation
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let segments = chunk.segments;
    copy_cursor_to_progress(&mut next, chunk.cursor)?;
    if chunk.complete {
        next.scan_mode = GitHistoryScanMode::ReflogVerify;
        next.capture_target_offset = Some(next.reflog_byte_offset);
        next.verify_byte_offset = next.reflog_byte_length;
        next.verify_digest = history_progress::initial_reflog_content_chain().to_owned();
        next.segment_cursor = 0;
    }
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    for segment in segments {
        let inserted = history_progress::upsert_segment(
            &transaction,
            &GitHistorySegmentRow {
                key: progress.key,
                ordinal: u64::try_from(segment.ordinal)
                    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?,
                branch: segment.branch,
                start_ts: segment.start,
                end_ts: segment.end,
                tip_oid: segment.tip_oid,
                applied: false,
                completed: false,
            },
        )
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        if !inserted {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
    }
    if !history_progress::compare_and_swap_progress(&transaction, progress.generation, &next)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(StreamGitEvidenceOutcome::Progressed);
    }
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    *committed = true;
    Ok(StreamGitEvidenceOutcome::Progressed)
}

pub(super) async fn advance_reflog_verification<S: GitCorrelationSessionStore>(
    session_store: &S,
    project_path: &Path,
    progress: &GitHistoryProgressRow,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let source = cursor_from_progress(progress)?;
    let target = progress
        .capture_target_offset
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let verification = native::ReflogVerificationCursor {
        byte_offset: progress.verify_byte_offset,
        content_chain: progress.verify_digest.clone(),
    };
    let path = project_path.to_owned();
    let scan_source = source.clone();
    let native_control = control.clone();
    let chunk = tokio::task::spawn_blocking(move || {
        native::scan_reflog_verification_chunk(
            &path,
            &scan_source,
            target,
            verification,
            &native_control,
        )
    })
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
    verify_source_without_writer(project_path, &source, control).await?;

    let mut next = progress.clone();
    next.generation = next
        .generation
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    next.verify_byte_offset = chunk.cursor.byte_offset;
    next.verify_digest = chunk.cursor.content_chain;
    if chunk.complete {
        next.scan_mode = GitHistoryScanMode::Graph;
    }
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    if !history_progress::compare_and_swap_progress(&transaction, progress.generation, &next)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(StreamGitEvidenceOutcome::Progressed);
    }
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    *committed = true;
    Ok(StreamGitEvidenceOutcome::Progressed)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn advance_graph<S: GitCorrelationSessionStore>(
    session_store: &S,
    project_path: &Path,
    row: &SessionActivityRow,
    candidate_frontier: GitHistoryIndexFrontier,
    progress: &GitHistoryProgressRow,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
    stats: &mut BackfillStats,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let segment_ordinal = progress.segment_cursor;
    let snapshot = session_store
        .read_snapshot()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let segment = history_progress::read_segment(&snapshot, progress.key, segment_ordinal)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let pending = if segment.as_ref().is_some_and(|segment| segment.applied) {
        history_progress::read_pending_page(
            &snapshot,
            progress.key,
            segment_ordinal,
            history_progress::MAX_PENDING_PAGE_ROWS,
        )
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    } else {
        Vec::new()
    };
    drop(snapshot);
    let source = cursor_from_progress(progress)?;
    let Some(segment) = segment else {
        verify_source_without_writer(project_path, &source, control).await?;
        return finalize_session(
            session_store,
            candidate_frontier,
            progress,
            control,
            committed,
        )
        .await;
    };
    if !segment.applied {
        verify_source_without_writer(project_path, &source, control).await?;
        return apply_segment(
            session_store,
            row,
            progress,
            segment,
            opts,
            control,
            stats,
            committed,
        )
        .await;
    }
    if pending.is_empty() {
        verify_source_without_writer(project_path, &source, control).await?;
        return complete_segment(session_store, progress, segment, control, committed).await;
    }
    let remaining = opts
        .max_commits_per_repo
        .checked_sub(
            usize::try_from(progress.emitted_count)
                .map_err(|_| BoundedBackfillInterruption::HistoryLimitReached)?,
        )
        .ok_or(BoundedBackfillInterruption::HistoryLimitReached)?;
    let graph_pending = pending
        .iter()
        .map(|pending| native::GraphPending {
            oid: pending.oid.clone(),
        })
        .collect();
    let path = project_path.to_owned();
    let scan_source = source.clone();
    let native_control = control.clone();
    let window_start = progress.window_start;
    let window_end = progress.window_end;
    let chunk = tokio::task::spawn_blocking(move || {
        native::scan_graph_chunk(
            &path,
            window_start,
            window_end,
            &scan_source,
            graph_pending,
            remaining,
            &native_control,
        )
    })
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)??;
    verify_source_without_writer(project_path, &source, control).await?;
    apply_graph_chunk(
        session_store,
        row,
        progress,
        segment,
        chunk,
        control,
        stats,
        committed,
    )
    .await
}

async fn verify_source_without_writer(
    project_path: &Path,
    source: &native::ReflogCursor,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    let path = project_path.to_owned();
    let source = source.clone();
    let native_control = control.clone();
    tokio::task::spawn_blocking(move || {
        native::verify_reflog_source(&path, &source, &native_control)
    })
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
}

async fn apply_segment<S: GitCorrelationSessionStore>(
    session_store: &S,
    row: &SessionActivityRow,
    progress: &GitHistoryProgressRow,
    mut segment: GitHistorySegmentRow,
    opts: &BackfillOptions,
    control: &BoundedGitControl,
    stats: &mut BackfillStats,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let worktree_path = native::decode_path(&progress.worktree)?;
    let worktree = worktree_path
        .to_str()
        .map(normalize_worktree)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    for timestamp in [segment.start_ts, segment.end_ts] {
        record_span_in_transaction(
            &transaction,
            segment.branch.as_deref(),
            row,
            &worktree,
            timestamp,
            opts.merge_gap_secs,
            control,
        )
        .await?;
    }
    segment.applied = true;
    if !history_progress::upsert_segment(&transaction, &segment)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    if !history_progress::upsert_pending(
        &transaction,
        &GitHistoryPendingRow {
            key: progress.key,
            segment_ordinal: segment.ordinal,
            oid: segment.tip_oid.clone(),
        },
    )
    .await
    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    let mut next = progress.clone();
    next.generation = next
        .generation
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    if !history_progress::compare_and_swap_progress(&transaction, progress.generation, &next)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(StreamGitEvidenceOutcome::Progressed);
    }
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    stats.spans_written = stats.spans_written.saturating_add(1);
    *committed = true;
    Ok(StreamGitEvidenceOutcome::Progressed)
}

async fn complete_segment<S: GitCorrelationSessionStore>(
    session_store: &S,
    progress: &GitHistoryProgressRow,
    mut segment: GitHistorySegmentRow,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    segment.completed = true;
    if !history_progress::upsert_segment(&transaction, &segment)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    let mut next = progress.clone();
    next.generation = next
        .generation
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    next.segment_cursor = next
        .segment_cursor
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    if !history_progress::compare_and_swap_progress(&transaction, progress.generation, &next)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(StreamGitEvidenceOutcome::Progressed);
    }
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    *committed = true;
    Ok(StreamGitEvidenceOutcome::Progressed)
}

async fn apply_graph_chunk<S: GitCorrelationSessionStore>(
    session_store: &S,
    row: &SessionActivityRow,
    progress: &GitHistoryProgressRow,
    segment: GitHistorySegmentRow,
    chunk: native::GraphChunk,
    control: &BoundedGitControl,
    stats: &mut BackfillStats,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let worktree_path = native::decode_path(&progress.worktree)?;
    let worktree = worktree_path
        .to_str()
        .map(normalize_worktree)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    let mut inserted_commits = 0_usize;
    for commit in &chunk.commits {
        if record_commit_in_transaction(
            &transaction,
            row,
            segment.branch.as_deref(),
            &worktree,
            &commit.oid,
            commit.committed_at,
            control,
        )
        .await?
        {
            inserted_commits = inserted_commits.saturating_add(1);
        }
    }
    for oid in &chunk.newly_seen {
        if !history_progress::delete_pending(&transaction, progress.key, segment.ordinal, oid)
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
        {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
        if !history_progress::insert_seen(
            &transaction,
            &GitHistorySeenRow {
                key: progress.key,
                segment_ordinal: segment.ordinal,
                oid: oid.clone(),
            },
        )
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
        {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
    }
    for pending in chunk.pending {
        history_progress::upsert_pending(
            &transaction,
            &GitHistoryPendingRow {
                key: progress.key,
                segment_ordinal: segment.ordinal,
                oid: pending.oid,
            },
        )
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    }
    let mut next = progress.clone();
    next.generation = next
        .generation
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    next.emitted_count = next
        .emitted_count
        .checked_add(
            u64::try_from(chunk.commits.len())
                .map_err(|_| BoundedBackfillInterruption::HistoryLimitReached)?,
        )
        .ok_or(BoundedBackfillInterruption::HistoryLimitReached)?;
    if !history_progress::compare_and_swap_progress(&transaction, progress.generation, &next)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(StreamGitEvidenceOutcome::Progressed);
    }
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    stats.commits_attributed = stats.commits_attributed.saturating_add(inserted_commits);
    *committed = true;
    Ok(StreamGitEvidenceOutcome::Progressed)
}

async fn finalize_session<S: GitCorrelationSessionStore>(
    session_store: &S,
    candidate_frontier: GitHistoryIndexFrontier,
    progress: &GitHistoryProgressRow,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    let mut next = progress.clone();
    next.generation = next
        .generation
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    if !history_progress::compare_and_swap_progress(&transaction, progress.generation, &next)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(StreamGitEvidenceOutcome::Progressed);
    }
    if !history_progress::reset_progress(&transaction, progress.key)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    let persisted = super::super::advance_history_frontier(&transaction, candidate_frontier)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    *committed = true;
    Ok(StreamGitEvidenceOutcome::Applied(Some(persisted)))
}

pub(super) async fn reset_exact_progress<S: GitCorrelationSessionStore>(
    session_store: &S,
    expected: &GitHistoryProgressRow,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<(), BoundedBackfillInterruption> {
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let current = history_progress::read_progress(&transaction, expected.key)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    if current.as_ref() != Some(expected) {
        return Ok(());
    }
    if !history_progress::reset_progress(&transaction, expected.key)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Ok(());
    }
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    *committed = true;
    Ok(())
}
