use tracedecay_sessions::runtime::workflow_index::{
    WorkflowAgent, WorkflowIndexError, WorkflowRun,
};

pub(crate) use tracedecay_sessions::runtime::workflow_ingest::WorkflowIngestStore;
pub use tracedecay_sessions::runtime::workflow_ingest::{
    WorkflowIngestStats, ingest_workflow_runs,
};

impl WorkflowIngestStore for crate::global_db::GlobalDb {
    fn dashboard_connection(&self) -> libsql::Connection {
        self.dashboard_connection()
    }

    async fn workflow_upsert_run(&self, run: &WorkflowRun) -> Result<(), WorkflowIndexError> {
        crate::global_db::GlobalDb::workflow_upsert_run(self, run).await
    }

    async fn workflow_upsert_agent(&self, agent: &WorkflowAgent) -> Result<(), WorkflowIndexError> {
        crate::global_db::GlobalDb::workflow_upsert_agent(self, agent).await
    }
}
