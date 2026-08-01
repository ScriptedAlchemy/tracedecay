//! Root adapter over [`RegisteredGlobalDb`] for git-correlation operations.
//!
//! Session backfill/query logic depends on the port; this module owns the
//! concrete registered-database binding, authority checks, and high-level
//! façade methods.

use tracedecay_store::StoreShardScopeV1;

use crate::db::engine::ReadSnapshot;
use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use crate::sessions::git_correlation::{
    AnalyticsSessionTimestampSource, BackfillOptions, BackfillStats, CommitRelationFilter,
    CorrelationIndexHealth, GitCorrelationError, GitCorrelationSessionStore,
    GitCorrelationWriteTxn, GitReflogSource, SessionGitCorrelationHit, SessionsForQuery,
    SpanObservation, SpanScanTarget, TargetScan, correlation_index_health,
    record_span_observation_in_transaction, run_backfill, run_commit_attribution_sweep,
    run_incremental_backfill, sessions_for_with_relation,
};

/// Borrowed adapter over an already-open project-sessions database.
pub struct GlobalDbGitCorrelationStore<'a> {
    db: &'a RegisteredGlobalDb,
}

impl<'a> GlobalDbGitCorrelationStore<'a> {
    pub(crate) const fn new(db: &'a RegisteredGlobalDb) -> Self {
        Self { db }
    }

    pub(crate) fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        if matches!(
            &self.db.binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions { .. }
        ) {
            Ok(())
        } else {
            Err(GitCorrelationError::Db(
                "git correlation requires registered ProjectSessions authority".to_string(),
            ))
        }
    }

    pub(crate) async fn read_snapshot(&self) -> Result<ReadSnapshot, GitCorrelationError> {
        self.db
            .read_snapshot()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    pub(crate) async fn open_write_transaction(
        &self,
    ) -> Result<RegisteredGlobalDbWriteTransaction<'a>, GitCorrelationError> {
        self.db
            .begin_write_transaction()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    pub(crate) async fn record_span_observation(
        &self,
        observation: &SpanObservation,
        merge_gap_secs: i64,
    ) -> Result<i64, GitCorrelationError> {
        let transaction = self.open_write_transaction().await?;
        let span_id =
            record_span_observation_in_transaction(&transaction, observation, merge_gap_secs)
                .await?;
        GitCorrelationWriteTxn::commit(transaction).await?;
        Ok(span_id)
    }

    /// Attributes commits for every span touched since the last sweep, holding
    /// one write transaction so the watermark can only advance with the rows it
    /// describes. The scanner stays with the caller: reading a worktree is the
    /// caller's concern, and a target it cannot read must report
    /// [`TargetScan::Unavailable`] so the watermark waits for a retry.
    pub(crate) async fn run_commit_attribution_sweep<F>(
        &self,
        gap_secs: i64,
        scan: F,
    ) -> Result<usize, GitCorrelationError>
    where
        F: FnMut(&SpanScanTarget) -> TargetScan,
    {
        let transaction = self.open_write_transaction().await?;
        let attributed = run_commit_attribution_sweep(&transaction, gap_secs, scan).await?;
        GitCorrelationWriteTxn::commit(transaction).await?;
        Ok(attributed)
    }

    pub(crate) async fn run_backfill<E: AnalyticsSessionTimestampSource>(
        &self,
        analytics_events: &[E],
        git: &dyn GitReflogSource,
        opts: &BackfillOptions,
    ) -> Result<BackfillStats, GitCorrelationError> {
        run_backfill(self, analytics_events, git, opts).await
    }

    pub(crate) async fn run_incremental_backfill(
        &self,
        git: &dyn GitReflogSource,
        limit_sessions: usize,
    ) -> Result<BackfillStats, GitCorrelationError> {
        run_incremental_backfill(self, git, limit_sessions).await
    }

    pub(crate) async fn correlation_index_health(
        &self,
    ) -> Result<CorrelationIndexHealth, GitCorrelationError> {
        let snapshot = self.read_snapshot().await?;
        correlation_index_health(&snapshot).await
    }

    pub(crate) async fn sessions_for_with_relation(
        &self,
        query: &SessionsForQuery,
        relation: CommitRelationFilter,
    ) -> Result<Vec<SessionGitCorrelationHit>, GitCorrelationError> {
        let snapshot = self.read_snapshot().await?;
        sessions_for_with_relation(&snapshot, query, relation).await
    }
}

impl GitCorrelationSessionStore for GlobalDbGitCorrelationStore<'_> {
    type WriteTxn<'txn>
        = RegisteredGlobalDbWriteTransaction<'txn>
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        GlobalDbGitCorrelationStore::require_project_sessions_authority(self)
    }

    async fn read_snapshot(&self) -> Result<ReadSnapshot, GitCorrelationError> {
        GlobalDbGitCorrelationStore::read_snapshot(self).await
    }

    async fn open_write_transaction(&self) -> Result<Self::WriteTxn<'_>, GitCorrelationError> {
        GlobalDbGitCorrelationStore::open_write_transaction(self).await
    }
}
