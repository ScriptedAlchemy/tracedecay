use super::super::SessionActivityRow;
use super::super::history_progress::{
    self, GitHistoryCursorHeadState, GitHistoryProgressKey, GitHistoryProgressRow,
    GitHistoryScanMode,
};
use super::{BoundedBackfillInterruption, GitHistoryIndexFrontier, native};
use crate::runtime::git_correlation::normalize_worktree;

pub(super) fn session_row_from_progress(progress: &GitHistoryProgressRow) -> SessionActivityRow {
    SessionActivityRow {
        provider: progress.provider.clone(),
        session_id: progress.session_id.clone(),
        project_path: progress.project_path.clone(),
        started_at: Some(progress.window_start),
        ended_at: Some(progress.window_end),
        message_min_ts: None,
        message_max_ts: None,
    }
}

pub(super) const fn progress_frontier(progress: &GitHistoryProgressRow) -> GitHistoryIndexFrontier {
    GitHistoryIndexFrontier {
        activity_timestamp: progress.activity_timestamp,
        source_rowid: progress.key.source_rowid,
    }
}

pub(super) fn progress_from_cursor(
    key: GitHistoryProgressKey,
    activity_timestamp: i64,
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
        activity_timestamp,
        provider: row.provider.clone(),
        session_id: row.session_id.clone(),
        project_path: row.project_path.clone(),
        window_start,
        window_end,
        worktree: native::encode_path(&cursor.worktree)?,
        worktree_identity: cursor.worktree_identity,
        git_dir: native::encode_path(&cursor.git_dir)?,
        git_dir_identity: cursor.git_dir_identity,
        common_dir: native::encode_path(&cursor.common_dir)?,
        common_dir_identity: cursor.common_dir_identity,
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

pub(super) fn cursor_from_progress(
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
        worktree_identity: progress.worktree_identity.clone(),
        git_dir: native::decode_path(&progress.git_dir)?,
        git_dir_identity: progress.git_dir_identity.clone(),
        common_dir: native::decode_path(&progress.common_dir)?,
        common_dir_identity: progress.common_dir_identity.clone(),
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

pub(super) fn repository_seal_from_progress(
    progress: &GitHistoryProgressRow,
) -> Result<native::RepositorySeal, BoundedBackfillInterruption> {
    Ok(native::RepositorySeal {
        worktree: native::decode_path(&progress.worktree)?,
        worktree_identity: progress.worktree_identity.clone(),
        git_dir: native::decode_path(&progress.git_dir)?,
        git_dir_identity: progress.git_dir_identity.clone(),
        common_dir: native::decode_path(&progress.common_dir)?,
        common_dir_identity: progress.common_dir_identity.clone(),
    })
}

pub(super) fn canonical_worktree_path(
    progress: &GitHistoryProgressRow,
) -> Result<std::path::PathBuf, BoundedBackfillInterruption> {
    native::decode_path(&progress.worktree)
}

pub(super) fn canonical_worktree_evidence(
    progress: &GitHistoryProgressRow,
) -> Result<String, BoundedBackfillInterruption> {
    let worktree = canonical_worktree_path(progress)?;
    let exact = worktree
        .to_str()
        .ok_or(BoundedBackfillInterruption::UnsupportedCanonicalWorktreeEncoding)?;
    Ok(normalize_worktree(exact))
}

pub(super) fn copy_cursor_to_progress(
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
