use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::bounded::{
    BoundedBackfillInterruption, BoundedBackfillOutcome, BoundedGitControl,
    StreamGitEvidenceOutcome,
};
use super::history_progress::{self, GitHistoryProgressRow};
use super::{
    AUTO_BACKFILL_WATERMARK_KEY, GIT_HISTORY_ROWID_FRONTIER_KEY, GitCorrelationError,
    GitCorrelationSessionStore, GitCorrelationWriteTxn, GitHistoryIndexFrontier,
    SessionActivityRow,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitHistoryFailureReason {
    UnsupportedSourceFraming,
    UnsupportedCanonicalWorktreeEncoding,
}

impl GitHistoryFailureReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedSourceFraming => "unsupported_source_framing",
            Self::UnsupportedCanonicalWorktreeEncoding => "unsupported_canonical_worktree_encoding",
        }
    }

    fn from_interruption(
        interruption: BoundedBackfillInterruption,
    ) -> Result<Self, BoundedBackfillInterruption> {
        match interruption {
            BoundedBackfillInterruption::UnsupportedSourceFraming => {
                Ok(Self::UnsupportedSourceFraming)
            }
            BoundedBackfillInterruption::UnsupportedCanonicalWorktreeEncoding => {
                Ok(Self::UnsupportedCanonicalWorktreeEncoding)
            }
            other => Err(other),
        }
    }
}

pub(in super::super) async fn install_final_schema(
    conn: &(impl Executor + ?Sized),
) -> Result<(), GitCorrelationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS git_history_index_failures (
            source_rowid INTEGER NOT NULL PRIMARY KEY,
            activity_timestamp INTEGER NOT NULL,
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_path TEXT NOT NULL,
            window_start INTEGER NOT NULL,
            window_end INTEGER NOT NULL,
            reason TEXT NOT NULL
                CHECK(reason IN (
                    'unsupported_source_framing',
                    'unsupported_canonical_worktree_encoding'
                )),
            source_generation TEXT,
            reflog_digest TEXT,
            CHECK(window_start <= window_end),
            CHECK(
                (source_generation IS NULL AND reflog_digest IS NULL)
                OR
                (
                    source_generation IS NOT NULL
                    AND length(source_generation) > 0
                    AND reflog_digest IS NOT NULL
                    AND length(reflog_digest) > 0
                )
            )
        );",
    )
    .await?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHistoryFailureRow {
    pub source_rowid: i64,
    pub activity_timestamp: i64,
    pub provider: String,
    pub session_id: String,
    pub project_path: String,
    pub window_start: i64,
    pub window_end: i64,
    pub reason: GitHistoryFailureReason,
    pub source_generation: Option<String>,
    pub reflog_digest: Option<String>,
}

impl GitHistoryFailureRow {
    pub(super) fn from_candidate(
        row: &SessionActivityRow,
        frontier: GitHistoryIndexFrontier,
        window_start: i64,
        window_end: i64,
    ) -> Self {
        Self {
            source_rowid: frontier.source_rowid,
            activity_timestamp: frontier.activity_timestamp,
            provider: row.provider.clone(),
            session_id: row.session_id.clone(),
            project_path: row.project_path.clone(),
            window_start,
            window_end,
            reason: GitHistoryFailureReason::UnsupportedSourceFraming,
            source_generation: None,
            reflog_digest: None,
        }
    }

    pub(super) fn from_progress(
        progress: &GitHistoryProgressRow,
        reason: GitHistoryFailureReason,
    ) -> Self {
        Self {
            source_rowid: progress.key.source_rowid,
            activity_timestamp: progress.activity_timestamp,
            provider: progress.provider.clone(),
            session_id: progress.session_id.clone(),
            project_path: progress.project_path.clone(),
            window_start: progress.window_start,
            window_end: progress.window_end,
            reason,
            source_generation: Some(progress.source_generation.clone()),
            reflog_digest: Some(progress.reflog_digest.clone()),
        }
    }

    pub(super) const fn frontier(&self) -> GitHistoryIndexFrontier {
        GitHistoryIndexFrontier {
            activity_timestamp: self.activity_timestamp,
            source_rowid: self.source_rowid,
        }
    }
}

pub(super) async fn with_unresolved_count<S: GitCorrelationSessionStore>(
    session_store: &S,
    mut outcome: BoundedBackfillOutcome,
) -> Result<BoundedBackfillOutcome, GitCorrelationError> {
    let snapshot = session_store.read_snapshot().await?;
    outcome.unresolved_failures = count_unresolved(&snapshot).await?;
    drop(snapshot);
    Ok(outcome)
}

pub(super) async fn record_candidate<S: GitCorrelationSessionStore>(
    session_store: &S,
    row: &SessionActivityRow,
    frontier: GitHistoryIndexFrontier,
    window_start: i64,
    window_end: i64,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let failure = GitHistoryFailureRow::from_candidate(row, frontier, window_start, window_end);
    record(session_store, &failure, None, control, committed).await
}

pub(super) async fn record_progress<S: GitCorrelationSessionStore>(
    session_store: &S,
    progress: &GitHistoryProgressRow,
    interruption: BoundedBackfillInterruption,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    let reason = GitHistoryFailureReason::from_interruption(interruption)?;
    let failure = GitHistoryFailureRow::from_progress(progress, reason);
    record(session_store, &failure, Some(progress), control, committed).await
}

async fn record<S: GitCorrelationSessionStore>(
    session_store: &S,
    failure: &GitHistoryFailureRow,
    expected_progress: Option<&GitHistoryProgressRow>,
    control: &BoundedGitControl,
    committed: &mut bool,
) -> Result<StreamGitEvidenceOutcome, BoundedBackfillInterruption> {
    match persist_unresolved(session_store, failure, expected_progress, control).await? {
        Some(frontier) => {
            *committed = true;
            Ok(StreamGitEvidenceOutcome::Failed(frontier))
        }
        None => Ok(StreamGitEvidenceOutcome::Progressed),
    }
}

pub(super) async fn count_unresolved(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<u64, GitCorrelationError> {
    let mut rows = conn
        .query("SELECT COUNT(*) FROM git_history_index_failures", ())
        .await?;
    let count: i64 = rows.next().await?.map_or(Ok(0), |row| row.get(0))?;
    u64::try_from(count)
        .map_err(|_| GitCorrelationError::Db("negative git history failure count".to_string()))
}

pub(super) async fn persist_unresolved<S: GitCorrelationSessionStore>(
    session_store: &S,
    failure: &GitHistoryFailureRow,
    expected_progress: Option<&GitHistoryProgressRow>,
    control: &BoundedGitControl,
) -> Result<Option<GitHistoryIndexFrontier>, BoundedBackfillInterruption> {
    control.check()?;
    let transaction = session_store
        .open_write_transaction()
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let current_frontier = GitHistoryIndexFrontier {
        activity_timestamp: super::super::read_meta_value(
            &transaction,
            AUTO_BACKFILL_WATERMARK_KEY,
        )
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
        .unwrap_or(0),
        source_rowid: super::super::read_meta_value(&transaction, GIT_HISTORY_ROWID_FRONTIER_KEY)
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
            .unwrap_or(0),
    };
    if (failure.activity_timestamp, failure.source_rowid)
        <= (
            current_frontier.activity_timestamp,
            current_frontier.source_rowid,
        )
    {
        return Ok(None);
    }
    let current = history_progress::read_progress(&transaction, history_progress_key(failure))
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let progress_matches = match expected_progress {
        Some(expected) => current.as_ref() == Some(expected),
        None => current.is_none(),
    };
    if !progress_matches {
        return Ok(None);
    }
    upsert_unresolved(&transaction, failure)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    if expected_progress.is_some()
        && !history_progress::reset_progress(&transaction, history_progress_key(failure))
            .await
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
    {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    let frontier = super::advance_history_frontier(&transaction, failure.frontier())
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    GitCorrelationWriteTxn::commit(transaction)
        .await
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    Ok(Some(frontier))
}

pub(super) async fn clear_unresolved(
    conn: &(impl Executor + ?Sized),
    source_rowid: i64,
) -> Result<bool, GitCorrelationError> {
    Ok(conn
        .execute(
            "DELETE FROM git_history_index_failures WHERE source_rowid = ?1",
            params![source_rowid],
        )
        .await?
        == 1)
}

async fn upsert_unresolved(
    conn: &(impl Executor + ?Sized),
    failure: &GitHistoryFailureRow,
) -> Result<(), GitCorrelationError> {
    conn.execute(
        "INSERT INTO git_history_index_failures (
            source_rowid, activity_timestamp, provider, session_id,
            project_path, window_start, window_end, reason,
            source_generation, reflog_digest
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(source_rowid) DO UPDATE SET
            activity_timestamp = excluded.activity_timestamp,
            provider = excluded.provider,
            session_id = excluded.session_id,
            project_path = excluded.project_path,
            window_start = excluded.window_start,
            window_end = excluded.window_end,
            reason = excluded.reason,
            source_generation = excluded.source_generation,
            reflog_digest = excluded.reflog_digest
         WHERE excluded.activity_timestamp > git_history_index_failures.activity_timestamp
            OR (
                excluded.activity_timestamp = git_history_index_failures.activity_timestamp
                AND excluded.provider = git_history_index_failures.provider
                AND excluded.session_id = git_history_index_failures.session_id
                AND excluded.project_path = git_history_index_failures.project_path
                AND excluded.window_start = git_history_index_failures.window_start
                AND excluded.window_end = git_history_index_failures.window_end
                AND git_history_index_failures.source_generation IS NULL
                AND excluded.source_generation IS NOT NULL
            )",
        params![
            failure.source_rowid,
            failure.activity_timestamp,
            &failure.provider,
            &failure.session_id,
            &failure.project_path,
            failure.window_start,
            failure.window_end,
            failure.reason.as_str(),
            failure.source_generation.as_deref(),
            failure.reflog_digest.as_deref(),
        ],
    )
    .await?;
    Ok(())
}

const fn history_progress_key(
    failure: &GitHistoryFailureRow,
) -> history_progress::GitHistoryProgressKey {
    history_progress::GitHistoryProgressKey {
        source_rowid: failure.source_rowid,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
