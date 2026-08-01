//! Root adapter over [`RegisteredGlobalDb`] for git-correlation operations.
//!
//! Session backfill/query logic depends on the port; this module owns the
//! concrete registered-database binding, authority checks, and high-level
//! façade methods.

use std::borrow::Borrow;

use tracedecay_store::StoreShardScopeV1;

use crate::db::engine::ReadSnapshot;
use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use crate::sessions::git_correlation::{
    AnalyticsSessionTimestampSource, BackfillOptions, BackfillStats, CommitRelationFilter,
    CorrelationIndexHealth, GitCorrelationError, GitCorrelationSessionStore,
    GitCorrelationWriteTxn, GitReflogSource, SessionGitCorrelationHit, SessionsForQuery,
    SpanObservation, correlation_index_health, record_span_observation_in_transaction,
    run_backfill, run_incremental_backfill, sessions_for_with_relation,
};

/// Adapter over an already-open project-sessions database.
///
/// The holder `D` is generic so callers that own an `Arc<RegisteredGlobalDb>`
/// can build a lifetime-free (`'static`) adapter. A borrowed adapter makes the
/// `GitCorrelationSessionStore` impl apply only "for some specific lifetime",
/// so any future that holds one across an await and must then prove `Send`
/// raises a higher-ranked `for<'0> GlobalDbGitCorrelationStore<'0>: …`
/// obligation the compiler cannot discharge. Owning the handle keeps the impl
/// lifetime-free. Borrowed holders remain supported for call sites that never
/// cross such a boundary.
pub struct GlobalDbGitCorrelationStore<D> {
    db: D,
}

impl<D> GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    pub(crate) const fn new(db: D) -> Self {
        Self { db }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    pub(crate) fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        if matches!(
            &self.db().binding().shard_id.scope,
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
        self.db()
            .read_snapshot()
            .await
            .map_err(|error| GitCorrelationError::Db(error.to_string()))
    }

    pub(crate) async fn open_write_transaction(
        &self,
    ) -> Result<RegisteredGlobalDbWriteTransaction<'_>, GitCorrelationError> {
        self.db()
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

    pub(crate) async fn run_backfill<E, G>(
        &self,
        analytics_events: &[E],
        git: &G,
        opts: &BackfillOptions,
    ) -> Result<BackfillStats, GitCorrelationError>
    where
        E: AnalyticsSessionTimestampSource,
        G: GitReflogSource + ?Sized,
    {
        run_backfill(self, analytics_events, git, opts).await
    }

    pub(crate) async fn run_incremental_backfill<G: GitReflogSource + ?Sized>(
        &self,
        git: &G,
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

impl<D> GitCorrelationSessionStore for GlobalDbGitCorrelationStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
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
