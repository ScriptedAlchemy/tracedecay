use std::future::Future;

use tracedecay_sessions::runtime::workflow_index::{WorkflowAgent, WorkflowIndexError, WorkflowRun};

pub use tracedecay_sessions::runtime::workflow_ingest::*;

impl WorkflowIngestStore for crate::global_db::GlobalDb {
    fn dashboard_connection(&self) -> libsql::Connection {
        self.dashboard_connection()
    }

    fn workflow_upsert_run(
        &self,
        run: &WorkflowRun,
    ) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send {
        async move { crate::global_db::GlobalDb::workflow_upsert_run(self, run).await }
    }

    fn workflow_upsert_agent(
        &self,
        agent: &WorkflowAgent,
    ) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send {
        async move { crate::global_db::GlobalDb::workflow_upsert_agent(self, agent).await }
    }
}
