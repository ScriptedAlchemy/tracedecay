//! Root adapter for workflow-index storage.

use std::borrow::Borrow;
#[cfg(any(test, feature = "test-helpers"))]
use std::path::Path;

use tracedecay_domain::ProjectId;
use tracedecay_store::StoreShardScopeV1;

use crate::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::runtime::workflow_index::{
    INGEST_WATERMARK_KEY, RegisteredWorkflowIndexSnapshot, WorkflowAgent, WorkflowIndexError,
    WorkflowIngestSink, WorkflowIngestWriteTxn, WorkflowRun, read_ingest_watermark, upsert_agent,
    upsert_run,
};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_sessions::runtime::workflow_ingest::{
    WorkflowIngestStats, ingest_workflow_runs_with_sink,
};
use tracedecay_sessions::runtime::workflow_state::{WorkflowStateItem, list_unfinished};

/// Borrowed adapter over an already-open project-sessions database.
/// The holder `D` is generic so callers that own a
/// [`crate::RegisteredGlobalDbLeaseV1`]
/// can build a lifetime-free (`'static`) adapter. A borrowed adapter makes the
/// trait impls below apply only "for some specific lifetime", which turns any
/// `Send` proof over a future holding one across an await into a higher-ranked
/// obligation the compiler cannot discharge.
pub struct GlobalDbWorkflowStore<D> {
    db: D,
}

impl<D> GlobalDbWorkflowStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    #[hotpath::skip]
    pub const fn new(db: D) -> Self {
        Self { db }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    pub fn matches_project_sessions_authority(&self, project_id: &ProjectId) -> bool {
        matches!(
            &self.db().binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions {
                project_id: authority_project_id,
            } if authority_project_id == project_id
        )
    }

    #[hotpath::measure(label = "global_db.workflow.read_watermark", future = true)]
    pub async fn read_ingest_watermark(&self) -> Option<i64> {
        let Ok(snapshot) = self.db().read_snapshot().await else {
            return None;
        };
        // `None` means the snapshot could not be opened; a missing watermark
        // key still returns `Some(0)` so the sweep can proceed.
        Some(read_ingest_watermark(&snapshot, INGEST_WATERMARK_KEY).await)
    }

    #[hotpath::measure(label = "global_db.workflow.upsert_run", future = true)]
    pub async fn upsert_workflow_run(
        &self,
        run: &WorkflowRun,
        agents: &[WorkflowAgent],
    ) -> Result<(), WorkflowIndexError> {
        let transaction = self
            .db()
            .begin_write_transaction()
            .await
            .map_err(|error| WorkflowIndexError::Db(error.to_string()))?;
        upsert_run(&transaction, run).await?;
        for agent in agents {
            upsert_agent(&transaction, agent).await?;
        }
        WorkflowIngestWriteTxn::commit(transaction).await
    }

    #[hotpath::measure(label = "global_db.workflow.bump_watermark", future = true)]
    pub async fn bump_ingest_watermark(&self, value: i64) {
        let Ok(transaction) = self.db().begin_write_transaction().await else {
            tracing::debug!("workflow ingest writer unavailable");
            return;
        };
        let write = async {
            transaction
                .execute(
                    "INSERT INTO workflow_index_meta(key, value)
                         VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET
                             value = MAX(value, excluded.value),
                             updated_at = unixepoch()",
                    params![INGEST_WATERMARK_KEY, value],
                )
                .await?;
            RegisteredGlobalDbWriteTransaction::commit(transaction).await
        }
        .await;
        if let Err(err) = write {
            tracing::debug!(error = %err, "workflow ingest watermark not advanced");
        }
    }

    #[hotpath::measure(label = "global_db.workflow.index_snapshot", future = true)]
    pub async fn open_workflow_index_snapshot(
        &self,
    ) -> Result<RegisteredWorkflowIndexSnapshot, WorkflowIndexError> {
        if !matches!(
            &self.db().binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions { .. }
        ) {
            return Err(WorkflowIndexError::InvalidArgument(
                "workflow index requires ProjectSessions authority".to_string(),
            ));
        }
        let snapshot = self
            .db()
            .read_snapshot()
            .await
            .map_err(|error| WorkflowIndexError::Db(error.to_string()))?;
        Ok(RegisteredWorkflowIndexSnapshot::from_snapshot(snapshot))
    }

    /// Ingest sweep against an explicit Claude `projects` directory, so callers
    /// that already resolved (or must isolate) that root do not re-derive it
    /// from the operator's real home.
    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn ingest_workflow_runs_from(
        &self,
        project_id: &ProjectId,
        project_root: &Path,
        projects_dir: &Path,
    ) -> WorkflowIngestStats {
        ingest_workflow_runs_with_sink(self, project_id, project_root, projects_dir).await
    }

    /// Unfinished-run evidence listing, read at one pinned generation.
    #[hotpath::measure(label = "global_db.workflow.list_unfinished", future = true)]
    pub async fn list_unfinished_workflows(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkflowStateItem>, String> {
        let snapshot = self
            .db()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        list_unfinished(&snapshot, limit).await
    }
}

impl<D> WorkflowIngestSink for GlobalDbWorkflowStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    fn matches_project_sessions_authority(&self, project_id: &ProjectId) -> bool {
        GlobalDbWorkflowStore::matches_project_sessions_authority(self, project_id)
    }

    #[hotpath::skip]
    async fn read_ingest_watermark(&self) -> Option<i64> {
        GlobalDbWorkflowStore::read_ingest_watermark(self).await
    }

    #[hotpath::skip]
    async fn bump_ingest_watermark(&self, value: i64) {
        GlobalDbWorkflowStore::bump_ingest_watermark(self, value).await;
    }

    #[hotpath::skip]
    async fn upsert_workflow_run(
        &self,
        run: &WorkflowRun,
        agents: &[WorkflowAgent],
    ) -> Result<(), WorkflowIndexError> {
        GlobalDbWorkflowStore::upsert_workflow_run(self, run, agents).await
    }
}
