//! Root adapter for workflow-index storage.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::ProjectId;
use tracedecay_store::StoreShardScopeV1;

use crate::db::engine::params;
use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use crate::sessions::workflow_index::{
    INGEST_WATERMARK_KEY, RegisteredWorkflowIndexSnapshot, WorkflowAgent, WorkflowIndexError,
    WorkflowIngestWriteTxn, WorkflowRun, read_ingest_watermark, upsert_agent, upsert_run,
};
use crate::sessions::workflow_ingest::{WorkflowIngestStats, ingest_workflow_runs_with_sink};
use crate::sessions::workflow_state::{WorkflowStateItem, list_unfinished};

/// Borrowed adapter over an already-open project-sessions database.
pub struct GlobalDbWorkflowStore<'a> {
    db: &'a RegisteredGlobalDb,
}

impl<'a> GlobalDbWorkflowStore<'a> {
    pub(crate) const fn new(db: &'a RegisteredGlobalDb) -> Self {
        Self { db }
    }

    pub(crate) fn matches_project_sessions_authority(&self, project_id: &ProjectId) -> bool {
        matches!(
            &self.db.binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions {
                project_id: authority_project_id,
            } if authority_project_id == project_id
        )
    }

    pub(crate) async fn read_ingest_watermark(&self) -> Option<i64> {
        let Ok(snapshot) = self.db.read_snapshot().await else {
            return None;
        };
        // `None` means the snapshot could not be opened; a missing watermark
        // key still returns `Some(0)` so the sweep can proceed.
        Some(read_ingest_watermark(&snapshot, INGEST_WATERMARK_KEY).await)
    }

    pub(crate) async fn upsert_workflow_run(
        &self,
        run: &WorkflowRun,
        agents: &[WorkflowAgent],
    ) -> Result<(), WorkflowIndexError> {
        let transaction = self
            .db
            .begin_write_transaction()
            .await
            .map_err(|error| WorkflowIndexError::Db(error.to_string()))?;
        upsert_run(&transaction, run).await?;
        for agent in agents {
            upsert_agent(&transaction, agent).await?;
        }
        WorkflowIngestWriteTxn::commit(transaction).await
    }

    pub(crate) async fn bump_ingest_watermark(&self, value: i64) {
        let Ok(transaction) = self.db.begin_write_transaction().await else {
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

    pub(crate) async fn open_workflow_index_snapshot(
        &self,
    ) -> Result<RegisteredWorkflowIndexSnapshot, WorkflowIndexError> {
        if !matches!(
            &self.db.binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions { .. }
        ) {
            return Err(WorkflowIndexError::InvalidArgument(
                "workflow index requires ProjectSessions authority".to_string(),
            ));
        }
        let snapshot = self.db.read_snapshot().await?;
        Ok(RegisteredWorkflowIndexSnapshot::from_snapshot(snapshot))
    }

    pub(crate) async fn ingest_workflow_runs(
        &self,
        project_id: &ProjectId,
        project_root: &Path,
    ) -> WorkflowIngestStats {
        let Some(home) = crate::sessions::home_dir() else {
            return WorkflowIngestStats::default();
        };
        self.ingest_workflow_runs_from(
            project_id,
            project_root,
            &home.join(".claude").join("projects"),
        )
        .await
    }

    /// Ingest sweep against an explicit Claude `projects` directory, so callers
    /// that already resolved (or must isolate) that root do not re-derive it
    /// from the operator's real home.
    pub(crate) async fn ingest_workflow_runs_from(
        &self,
        project_id: &ProjectId,
        project_root: &Path,
        projects_dir: &Path,
    ) -> WorkflowIngestStats {
        ingest_workflow_runs_with_sink(self, project_id, project_root, projects_dir).await
    }

    /// Unfinished-run evidence listing, read at one pinned generation.
    pub(crate) async fn list_unfinished_workflows(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkflowStateItem>, String> {
        let snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        list_unfinished(&snapshot, limit).await
    }
}

impl WorkflowIngestWriteTxn for RegisteredGlobalDbWriteTransaction<'_> {
    fn commit(self) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send {
        async move {
            RegisteredGlobalDbWriteTransaction::commit(self)
                .await
                .map_err(|error| WorkflowIndexError::Db(error.to_string()))
        }
    }
}
